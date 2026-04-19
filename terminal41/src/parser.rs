use std::sync::LazyLock;
use std::time::Instant;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use pty_pipe41::ForegroundProcessSet;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vtepp::Params;

use crate::C1Mode;
use crate::ConformanceLevel;
use crate::TerminalModes;
use crate::TextMode;
use crate::charset;
use crate::charset::CharacterSet;
use crate::charset::GraphicSetSlot;
use crate::color;
use crate::color::apply_sgr;
use crate::conformance;
use crate::cursor::CursorStyle;
use crate::dec_color::DecColorState;
use crate::decmacro::MacroStore;
use crate::drcs::Store as DrcsStore;
use crate::feature::FeaturePermissions;
use crate::grid;
use crate::grid::Viewport;
use crate::keyboard::KittyKeyboardState;
use crate::keyboard::handle_kitty_keyboard;
use crate::mode;
use crate::mouse::MouseTracking;
use crate::mouse::apply_mouse_mode;
use crate::row::LineAttr;
use crate::row::Row;
use crate::screen;
use crate::screen::ActiveDisplay;
use crate::screen::Screen;
use crate::screen::StatusDisplayKind;
use crate::screen::StatusLine;

/// Bundles the bits of [`Terminal`](super::Terminal) state that CSI handlers
/// need beyond the active screen. Keeps the call signature stable as new CSI
/// sequences get wired in.
pub(super) struct CsiContext<'a> {
    pub screen: &'a mut Screen,
    pub stash: &'a mut Screen,
    pub viewport: &'a mut Viewport,
    pub on_alt_screen: &'a mut bool,
    pub modes: &'a mut TerminalModes,
    pub kitty_keyboard: &'a mut KittyKeyboardState,
    pub pending_output: &'a mut Vec<u8>,
    pub pending_resize: &'a mut Option<(u32, u32)>,
    pub cursor_style: &'a mut CursorStyle,
    pub cell_width: u32,
    pub cell_height: u32,
    pub palette: &'a mut color::ColorPalette,
    pub base_palette: &'a color::ColorPalette,
    pub default_status_display: &'a mut StatusDisplayKind,
    pub title_stack: &'a mut Vec<Option<String>>,
    pub current_title: &'a mut Option<String>,
    pub saved_modes: &'a mut std::collections::HashMap<u16, bool>,
    pub current_prompt_row: &'a mut Option<u64>,
    pub bell_pending: &'a mut bool,
    pub vt52_cursor_addr: &'a mut crate::Vt52CursorAddr,
    pub macros: &'a mut MacroStore,
    pub dec_color: &'a mut DecColorState,
    pub feature_permissions: &'a FeaturePermissions,
    pub foreground_processes: &'a Option<ForegroundProcessSet>,
    pub drcs: &'a mut DrcsStore,
}

/// Bundles the bits of [`Terminal`](super::Terminal) state that ESC handlers
/// need beyond the active screen. RIS in particular resets nearly everything.
pub(super) struct EscContext<'a> {
    pub screen: &'a mut Screen,
    pub stash: &'a mut Screen,
    pub viewport: &'a mut Viewport,
    pub on_alt_screen: &'a mut bool,
    pub modes: &'a mut TerminalModes,
    pub kitty_keyboard: &'a mut KittyKeyboardState,
    pub cursor_style: &'a mut CursorStyle,
    pub current_title: &'a mut Option<String>,
    pub title_stack: &'a mut Vec<Option<String>>,
    pub saved_modes: &'a mut std::collections::HashMap<u16, bool>,
    pub current_prompt_row: &'a mut Option<u64>,
    pub bell_pending: &'a mut bool,
    pub palette: &'a mut color::ColorPalette,
    pub base_palette: &'a color::ColorPalette,
    pub default_status_display: &'a mut StatusDisplayKind,
    /// Bytes to write back to the PTY (e.g. VT52 identify response `ESC / Z`).
    pub pending_output: &'a mut Vec<u8>,
    /// State machine for VT52 `ESC Y Pr Pc`. Set to `AwaitingRow` when the
    /// `ESC Y` byte is dispatched; the subsequent bytes are consumed in
    /// [`Terminal::apply`] before any other dispatch occurs.
    pub vt52_cursor_addr: &'a mut crate::Vt52CursorAddr,
    pub macros: &'a mut MacroStore,
    pub dec_color: &'a mut DecColorState,
    pub drcs: &'a mut DrcsStore,
}

/// Pre-built inline `SmolStr` for every printable ASCII byte (0x20..=0x7E).
/// `put_ascii_run` clones out of this table instead of constructing a fresh
/// `SmolStr` per byte — inline-backed clones are a short memcpy, so the table
/// eliminates repeated `from_utf8` validation and the inline copy constructor
/// call per cell.
static ASCII_CELLS: LazyLock<[SmolStr; 95]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let b = 0x20u8 + i as u8;
        // SAFETY: b is in 0x20..=0x7E which is valid single-byte UTF-8.
        SmolStr::new_inline(unsafe { std::str::from_utf8_unchecked(std::slice::from_ref(&b)) })
    })
});

fn line_attr_display_cols(
    line_attr: LineAttr,
    viewport: &Viewport,
) -> u32 {
    match line_attr {
        LineAttr::Normal => viewport.cols,
        LineAttr::DoubleWidth | LineAttr::DoubleHeightTop | LineAttr::DoubleHeightBottom => {
            (viewport.cols / 2).max(1)
        }
    }
}

fn visible_row_index(
    screen: &Screen,
    viewport: &Viewport,
    row: u32,
) -> usize {
    if screen::page_memory_active(screen) {
        viewport.top_index(screen.grid.rows.len()) + row as usize
    } else {
        screen
            .grid
            .rows
            .len()
            .saturating_sub(viewport.rows as usize)
            + row as usize
    }
}

fn row_display_cols(
    screen: &Screen,
    viewport: &Viewport,
    row: u32,
) -> u32 {
    let row_index = visible_row_index(screen, viewport, row);
    let line_attr = screen
        .grid
        .rows
        .get(row_index)
        .map(|row| row.line_attr)
        .unwrap_or(LineAttr::Normal);
    line_attr_display_cols(line_attr, viewport)
}

fn current_row_display_cols(
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    row_display_cols(screen, viewport, screen.cursor.row)
}

fn clamp_cursor_to_row_width(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    let cols = current_row_display_cols(screen, viewport);
    if screen.cursor.col >= cols {
        screen.cursor.col = cols.saturating_sub(1);
    }
}

/// Sentinel for the second (and beyond) cell of a wide glyph. Distinct from
/// the default blank (`" "`) so neighbour cleanup can tell them apart.
fn continuation_cell() -> SmolStr {
    SmolStr::default()
}

fn blank_cell() -> SmolStr {
    SmolStr::new_inline(" ")
}

fn status_line_mut(screen: &mut Screen) -> Option<&mut StatusLine> {
    screen::status_line_writable(screen)
        .then_some(screen.status_line.as_mut())
        .flatten()
}

fn status_insert_chars(
    status: &mut StatusLine,
    count: usize,
) {
    let cols = status.row.cells.len();
    let col = status.cursor.col as usize;
    let count = count.min(cols.saturating_sub(col));
    if count == 0 {
        return;
    }
    status.row.copy_within(col..cols - count, col + count);
    status
        .row
        .clear_range(col..col + count, status.fg, status.bg);
}

fn status_delete_chars(
    status: &mut StatusLine,
    count: usize,
) {
    let cols = status.row.cells.len();
    let col = status.cursor.col as usize;
    let count = count.min(cols.saturating_sub(col));
    if count == 0 {
        return;
    }
    status.row.copy_within(col + count..cols, col);
    status
        .row
        .clear_range(cols - count..cols, status.fg, status.bg);
}

fn status_erase_chars(
    status: &mut StatusLine,
    count: usize,
) {
    let cols = status.row.cells.len();
    let col = status.cursor.col as usize;
    let end = (col + count).min(cols);
    status.row.clear_range(col..end, status.fg, status.bg);
}

fn status_break_wide_glyphs_around_write(
    status: &mut StatusLine,
    col: usize,
    width: usize,
) {
    break_wide_glyphs_around_write(&mut status.row, col, width);
}

fn status_put_char(
    screen: &mut Screen,
    s: SmolStr,
    insert_mode: bool,
) {
    screen.charset.single_shift = None;
    let Some(status) = status_line_mut(screen) else {
        return;
    };
    let cols = status.row.len().max(1);
    let raw_width = UnicodeWidthStr::width(s.as_str());
    if raw_width == 0 {
        let col = status.cursor.col as usize;
        if col > 0 {
            let mut builder = SmolStrBuilder::new();
            builder.push_str(status.row.cells[col - 1].as_str());
            builder.push_str(&s);
            status.row.cells[col - 1] = builder.finish();
        }
        return;
    }

    let width = raw_width.max(1);
    if status.cursor.col + width as u32 > cols {
        status.cursor.col = cols.saturating_sub(width as u32);
    }

    if insert_mode {
        status_insert_chars(status, width);
    }

    let col = status.cursor.col as usize;
    status_break_wide_glyphs_around_write(status, col, width);
    status.row.cells[col] = s.clone();
    status.row.fg[col] = status.fg;
    status.row.bg[col] = status.bg;
    status.row.attrs[col] = status.attrs;
    status.row.underline[col] = status.underline;
    status.row.underline_color[col] = status.underline_color;
    status.row.links[col] = status.current_hyperlink;
    for i in 1..width {
        status.row.cells[col + i] = continuation_cell();
        status.row.fg[col + i] = status.fg;
        status.row.bg[col + i] = status.bg;
        status.row.attrs[col + i] = status.attrs;
        status.row.underline[col + i] = status.underline;
        status.row.underline_color[col + i] = status.underline_color;
        status.row.links[col + i] = status.current_hyperlink;
    }
    status.last_char = Some(s);
    status.cursor.col += width as u32;
}

pub(super) fn put_status_ascii_run(
    screen: &mut Screen,
    run: &[u8],
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }
    let run = if let Some(charset) = screen.charset.take_single_shift_charset() {
        let b = run[0];
        let ch = charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
            .unwrap_or_else(|| ASCII_CELLS[(b - 0x20) as usize].clone());
        status_put_char(screen, ch, insert_mode);
        &run[1..]
    } else {
        run
    };

    if charset::gl_charset_requires_translation(&screen.charset, screen.nrc_mode) {
        let charset = screen.charset.gl_charset();
        for &b in run {
            let ch = charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
                .unwrap_or_else(|| ASCII_CELLS[(b - 0x20) as usize].clone());
            status_put_char(screen, ch, insert_mode);
        }
        return;
    }

    for &b in run {
        status_put_char(
            screen,
            ASCII_CELLS[(b - 0x20) as usize].clone(),
            insert_mode,
        );
    }
}

pub(super) fn put_status_text_run(
    screen: &mut Screen,
    run: &str,
    insert_mode: bool,
) {
    for grapheme in run.graphemes(true) {
        status_put_char(screen, SmolStr::new(grapheme), insert_mode);
    }
}

pub(super) fn put_status_printable(
    screen: &mut Screen,
    s: SmolStr,
    insert_mode: bool,
) {
    let mut chars = s.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
        && let Some(translated) = translated_codepoint(screen, ch)
    {
        status_put_char(screen, translated, insert_mode);
        return;
    }
    status_put_char(screen, s, insert_mode);
}

pub(super) fn put_status_8bit_byte(
    screen: &mut Screen,
    byte: u8,
    insert_mode: bool,
) {
    let ch = char::from_u32(byte as u32).expect("8-bit byte always maps to valid Unicode scalar");
    if let Some(translated) = translated_codepoint(screen, ch) {
        status_put_char(screen, translated, insert_mode);
    } else {
        let mut buf = [0u8; 4];
        status_put_char(
            screen,
            SmolStr::new_inline(ch.encode_utf8(&mut buf)),
            insert_mode,
        );
    }
}

pub(super) fn execute_status(
    screen: &mut Screen,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    match byte {
        NUL => {}
        BEL => *bell_pending = true,
        b'\x08' => {
            if let Some(status) = status_line_mut(screen) {
                status.cursor.col = status.cursor.col.saturating_sub(1);
            }
        }
        b'\x09' => {
            let tab_stops = screen.tab_stops.clone();
            if let Some(status) = status_line_mut(screen) {
                let cols = status.row.len().max(1);
                status.cursor.col = next_tab_stop(&tab_stops, status.cursor.col, cols);
            }
        }
        b'\n' | VT | FF => {
            if newline_mode && let Some(status) = status_line_mut(screen) {
                status.cursor.col = 0;
            }
        }
        b'\r' => {
            if let Some(status) = status_line_mut(screen) {
                status.cursor.col = 0;
            }
        }
        b'\x0e' => screen.charset.set_gl(charset::GraphicSetSlot::G1),
        b'\x0f' => screen.charset.set_gl(charset::GraphicSetSlot::G0),
        _ => {}
    }
}

fn status_line_csi_dispatch(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    intermediates: &[u8],
    action: char,
) -> bool {
    let Some(status) = status_line_mut(ctx.screen) else {
        return false;
    };
    let cols = status.row.len().max(1);
    let cursor = &mut status.cursor;

    if intermediates.is_empty() && action == 'm' {
        let mut palette = ctx.palette.clone();
        palette.fg = palette.status_line_fg;
        palette.bg = palette.status_line_bg;
        apply_sgr(
            &mut status.fg,
            &mut status.bg,
            &mut status.attrs,
            &mut status.underline,
            &mut status.underline_color,
            params,
            &palette,
        );
        return true;
    }

    if !intermediates.is_empty() {
        return false;
    }

    match action {
        '@' => {
            let count = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1) as usize;
            status_insert_chars(status, count);
            true
        }
        'A' | 'B' | 'd' => {
            cursor.row = 0;
            true
        }
        'C' => {
            let n = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1) as u32;
            cursor.col = (cursor.col + n).min(cols - 1);
            true
        }
        'D' => {
            let n = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1) as u32;
            cursor.col = cursor.col.saturating_sub(n);
            true
        }
        'G' | '`' => {
            let col = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1)
                .max(1);
            cursor.col = (col as u32 - 1).min(cols - 1);
            true
        }
        'H' | 'f' => {
            let col = params
                .iter()
                .nth(1)
                .and_then(|g| g.first().copied())
                .unwrap_or(1)
                .max(1);
            cursor.row = 0;
            cursor.col = (col as u32 - 1).min(cols - 1);
            true
        }
        'J' => {
            status.row.clear(status.fg, status.bg);
            true
        }
        'K' => {
            let mode = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            let col = cursor.col as usize;
            let len = cols as usize;
            match mode {
                0 => status.row.clear_range(col..len, status.fg, status.bg),
                1 => status.row.clear_range(0..(col + 1), status.fg, status.bg),
                2 => status.row.clear(status.fg, status.bg),
                _ => {}
            }
            true
        }
        'P' => {
            let count = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1) as usize;
            status_delete_chars(status, count);
            true
        }
        'X' => {
            let count = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1) as usize;
            status_erase_chars(status, count);
            true
        }
        'b' => {
            let count = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1);
            if let Some(last) = status.last_char.clone() {
                for _ in 0..count {
                    status_put_char(ctx.screen, last.clone(), ctx.modes.insert_mode);
                }
            }
            true
        }
        _ => false,
    }
}

fn csi_dispatch_star_intermediate(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    action: char,
) -> bool {
    match action {
        '|' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(24);
            if let Some(rows) = valid_screen_lines(ps) {
                let page_rows =
                    screen::page_rows(ctx.screen).unwrap_or(rows.max(ctx.viewport.rows));
                for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                    screen::activate_page_memory(
                        screen,
                        &Viewport {
                            rows,
                            cols: ctx.viewport.cols,
                            top: 0,
                        },
                        page_rows,
                    );
                }
                let old_cols = ctx.viewport.cols;
                let old_total_rows = ctx.viewport.rows + screen::status_line_rows(ctx.screen);
                let new_total_rows = rows + screen::status_line_rows(ctx.screen);
                for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                    let old_rows = old_total_rows.saturating_sub(screen::status_line_rows(screen));
                    let new_rows = new_total_rows.saturating_sub(screen::status_line_rows(screen));
                    screen::resize_screen(screen, old_cols, old_rows, old_cols, new_rows);
                }
                ctx.viewport.rows = rows;
                *ctx.pending_resize = Some((
                    ctx.viewport.cols,
                    rows + screen::status_line_rows(ctx.screen),
                ));
                ctx.screen.scroll_top = 0;
                ctx.screen.scroll_bottom = rows.saturating_sub(1);
                ctx.screen.cursor.row = ctx.screen.cursor.row.min(rows.saturating_sub(1));
            }
            true
        }
        'x' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            ctx.screen.attr_change_extent = match ps {
                2 => grid::AttrChangeExtent::Rectangle,
                0 | 1 => grid::AttrChangeExtent::Stream,
                _ => ctx.screen.attr_change_extent,
            };
            true
        }
        _ => false,
    }
}

fn csi_dispatch_space_intermediate(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    action: char,
) -> bool {
    match action {
        'q' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            ctx.cursor_style.apply_decscusr(ps);
            true
        }
        '@' => {
            let view = screen::screen_viewport(ctx.screen, ctx.viewport);
            let n = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1)
                .max(1) as u32;
            ctx.screen
                .grid
                .scroll_left(&view, ctx.screen.scroll_top, ctx.screen.scroll_bottom, n);
            true
        }
        'A' => {
            let view = screen::screen_viewport(ctx.screen, ctx.viewport);
            let n = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1)
                .max(1) as u32;
            ctx.screen
                .grid
                .scroll_right(&view, ctx.screen.scroll_top, ctx.screen.scroll_bottom, n);
            true
        }
        'P' | 'Q' | 'R' => {
            let n = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1)
                .max(1) as u32;
            let view = screen::screen_viewport(ctx.screen, ctx.viewport);
            screen::activate_page_memory(ctx.screen, &view, view.rows);
            if let Some(page) = ctx.screen.page_memory.as_mut() {
                match action {
                    'P' => {
                        page.active_page =
                            (n.saturating_sub(1)).min(page.page_count().saturating_sub(1))
                    }
                    'Q' => {
                        page.active_page =
                            (page.active_page + n).min(page.page_count().saturating_sub(1))
                    }
                    'R' => page.active_page = page.active_page.saturating_sub(n),
                    _ => unreachable!(),
                }
            }
            true
        }
        _ => false,
    }
}

fn csi_dispatch_quote_intermediate(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    action: char,
) -> bool {
    match action {
        'p' => {
            let ps1 = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            let Some(level) = ConformanceLevel::from_decscl(ps1) else {
                return true;
            };
            let ps2 = params.iter().nth(1).and_then(|g| g.first().copied());
            let c1_mode = if level.supports_c1_negotiation() {
                C1Mode::from_decscl(ps2)
            } else {
                C1Mode::SevenBit
            };
            let mut esc_ctx = EscContext {
                screen: ctx.screen,
                stash: ctx.stash,
                viewport: ctx.viewport,
                on_alt_screen: ctx.on_alt_screen,
                modes: ctx.modes,
                kitty_keyboard: ctx.kitty_keyboard,
                cursor_style: ctx.cursor_style,
                current_title: ctx.current_title,
                title_stack: ctx.title_stack,
                saved_modes: ctx.saved_modes,
                current_prompt_row: ctx.current_prompt_row,
                bell_pending: ctx.bell_pending,
                palette: ctx.palette,
                base_palette: ctx.base_palette,
                default_status_display: ctx.default_status_display,
                pending_output: ctx.pending_output,
                vt52_cursor_addr: ctx.vt52_cursor_addr,
                macros: ctx.macros,
                dec_color: ctx.dec_color,
                drcs: ctx.drcs,
            };
            apply_hard_reset(&mut esc_ctx, level, c1_mode);
            true
        }
        'q' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            match ps {
                1 => ctx.screen.attrs.insert(CellAttrs::PROTECTED),
                0 | 2 => ctx.screen.attrs.remove(CellAttrs::PROTECTED),
                _ => {}
            }
            true
        }
        _ => false,
    }
}

fn csi_dispatch_apostrophe_intermediate(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    action: char,
) -> bool {
    let view = screen::screen_viewport(ctx.screen, ctx.viewport);
    let n = params
        .iter()
        .next()
        .and_then(|g| g.first().copied())
        .unwrap_or(1)
        .max(1) as u32;
    match action {
        '}' => {
            ctx.screen.grid.insert_cols(
                &view,
                ctx.screen.cursor.col,
                ctx.screen.scroll_top,
                ctx.screen.scroll_bottom,
                n,
            );
            true
        }
        '~' => {
            ctx.screen.grid.delete_cols(
                &view,
                ctx.screen.cursor.col,
                ctx.screen.scroll_top,
                ctx.screen.scroll_bottom,
                n,
            );
            true
        }
        _ => false,
    }
}

fn csi_dispatch_gt_lt_eq_intermediate(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    intermediates: &[u8],
    action: char,
) -> bool {
    match (intermediates, action) {
        (b">" | b"<" | b"=", 'u') => {
            handle_kitty_keyboard(
                intermediates[0],
                params,
                ctx.kitty_keyboard,
                ctx.modes.c1_mode,
                ctx.pending_output,
            );
            true
        }
        (b">", 'q') => {
            conformance::write_dcs(
                ctx.pending_output,
                ctx.modes.c1_mode,
                format_args!(">|term41 {}", env!("CARGO_PKG_VERSION")),
            );
            true
        }
        (b">", 'c') => {
            conformance::write_csi(
                ctx.pending_output,
                ctx.modes.c1_mode,
                format_args!(">41;0;0c"),
            );
            true
        }
        (b"=", 'c') => {
            if ctx.modes.vt52_mode || !ctx.modes.conformance_level.supports_c1_negotiation() {
                return true;
            }
            conformance::write_dcs(
                ctx.pending_output,
                ctx.modes.c1_mode,
                format_args!("!|000000000"),
            );
            true
        }
        _ => false,
    }
}

fn csi_dispatch_bang_intermediate(
    ctx: &mut CsiContext<'_>,
    action: char,
) -> bool {
    if action != 'p' {
        return false;
    }
    if ctx.modes.vt52_mode || !ctx.modes.conformance_level.supports_c1_negotiation() {
        return true;
    }
    let screen = &mut *ctx.screen;
    screen.fg = ctx.palette.fg;
    screen.bg = ctx.palette.bg;
    screen.attrs = CellAttrs::default();
    screen.underline = UnderlineStyle::None;
    screen.underline_color = None;
    screen.scroll_top = 0;
    screen.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
    screen.left_margin = 0;
    screen.right_margin = ctx.viewport.cols.saturating_sub(1);
    screen.saved_cursor = None;
    screen.current_hyperlink = None;
    screen.cursor_visible = true;
    screen.last_char = None;
    screen.tab_stops = screen::init_tab_stops(ctx.viewport.cols);
    screen.origin_mode = false;
    screen.nrc_mode = false;
    screen.upss = charset::UserPreferredSupplementalSet::DecSupplemental;
    screen.autowrap = true;
    screen.app_cursor_keys = false;
    screen.attr_change_extent = grid::AttrChangeExtent::Stream;
    screen.app_keypad = false;
    screen.charset = charset::CharsetState::new();
    let conformance_level = ctx.modes.conformance_level;
    let c1_mode = ctx.modes.c1_mode;
    *ctx.modes = TerminalModes::new();
    ctx.modes.conformance_level = conformance_level;
    ctx.modes.c1_mode = c1_mode;
    *ctx.kitty_keyboard = KittyKeyboardState::new();
    *ctx.cursor_style = CursorStyle::default();
    true
}

fn csi_dispatch_amp_intermediate(
    ctx: &mut CsiContext<'_>,
    action: char,
) -> bool {
    if action != 'u' {
        return false;
    }
    conformance::write_dcs(
        ctx.pending_output,
        ctx.modes.c1_mode,
        format_args!("{}", charset::decaupss_report(ctx.screen.upss)),
    );
    true
}

// C0 control bytes (ECMA-48 / ASCII).
const NUL: u8 = 0x00;
const BEL: u8 = 0x07;
const BS: u8 = 0x08;
const VT: u8 = 0x0B;
const FF: u8 = 0x0C;
const SO: u8 = 0x0E;
const SI: u8 = 0x0F;

// DSR (Device Status Report) parameter values.
const DSR_OK: u16 = 5;
const DSR_CPR: u16 = 6;

// CSI Ps t — window manipulation parameter values.
const WINOP_TITLE_PUSH: u16 = 22;
const WINOP_TITLE_POP: u16 = 23;
const WINOP_REPORT_PIXELS: u16 = 14;
const WINOP_REPORT_CELL_SIZE: u16 = 16;
const WINOP_REPORT_TEXT_SIZE: u16 = 18;

// TBC (Tab Clear) parameter values.
const TBC_CURRENT: u16 = 0;
const TBC_ALL: u16 = 3;

const VALID_SCREEN_LINE_COUNTS: &[u16] = &[24, 25, 36, 48];
const VALID_PAGE_LINE_COUNTS: &[u16] = &[24, 25, 36, 48, 72, 144];

fn valid_screen_lines(ps: u16) -> Option<u32> {
    VALID_SCREEN_LINE_COUNTS.contains(&ps).then_some(ps as u32)
}

fn valid_page_lines(ps: u16) -> Option<u32> {
    VALID_PAGE_LINE_COUNTS.contains(&ps).then_some(ps as u32)
}

fn can_negotiate_c1(modes: &TerminalModes) -> bool {
    !modes.vt52_mode && modes.conformance_level.supports_c1_negotiation()
}

fn sync_screen_erase_defaults(
    screen: &mut Screen,
    dec_color: &DecColorState,
) {
    screen.grid.default_bg = crate::dec_color::erase_background_color(dec_color, screen.bg);
}

fn apply_hard_reset_state(
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    cursor_style: &mut CursorStyle,
    current_title: &mut Option<String>,
    title_stack: &mut Vec<Option<String>>,
    saved_modes: &mut std::collections::HashMap<u16, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    palette: &mut color::ColorPalette,
    base_palette: &color::ColorPalette,
    default_status_display: &StatusDisplayKind,
    dec_color: &mut DecColorState,
    macros: &mut MacroStore,
    drcs: &mut DrcsStore,
    conformance_level: ConformanceLevel,
    c1_mode: C1Mode,
) {
    *dec_color = crate::dec_color::state_from_palette(base_palette);
    *palette = crate::dec_color::effective_palette(base_palette, dec_color);
    if *on_alt_screen {
        std::mem::swap(screen, stash);
        *on_alt_screen = false;
    }
    let total_rows = viewport.rows + screen::status_line_rows(screen);
    for s in [&mut *screen, &mut *stash] {
        s.grid.default_fg = palette.fg;
        s.grid.default_bg = palette.bg;
        s.cursor = grid::Cursor::default();
        s.fg = palette.fg;
        s.bg = palette.bg;
        s.attrs = CellAttrs::default();
        s.underline = UnderlineStyle::None;
        s.underline_color = None;
        s.scroll_top = 0;
        s.scroll_bottom = viewport.rows.saturating_sub(1);
        s.left_margin = 0;
        s.right_margin = viewport.cols.saturating_sub(1);
        s.offset = 0;
        s.saved_cursor = None;
        s.current_hyperlink = None;
        s.cursor_visible = true;
        s.last_char = None;
        s.tab_stops = screen::init_tab_stops(viewport.cols);
        s.origin_mode = false;
        s.nrc_mode = false;
        s.upss = charset::UserPreferredSupplementalSet::DecSupplemental;
        s.autowrap = true;
        s.app_cursor_keys = false;
        s.attr_change_extent = grid::AttrChangeExtent::Stream;
        s.app_keypad = false;
        s.charset = charset::CharsetState::new();
        s.active_display = ActiveDisplay::Main;
        s.status_display = StatusDisplayKind::None;
        s.status_line = None;
        crate::apply_status_display_mode(
            s,
            total_rows,
            viewport.cols,
            *default_status_display,
            palette,
        );
        sync_screen_erase_defaults(s, dec_color);
        screen::clear_visible(s, viewport);
    }
    viewport.rows = total_rows.saturating_sub(screen::status_line_rows(screen));
    *modes = TerminalModes::new();
    modes.conformance_level = conformance_level;
    modes.c1_mode = c1_mode;
    *kitty_keyboard = KittyKeyboardState::new();
    *cursor_style = CursorStyle::default();
    *current_title = None;
    title_stack.clear();
    saved_modes.clear();
    *current_prompt_row = None;
    *bell_pending = false;
    *vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
    macros.clear();
    drcs.clear();
}

fn apply_hard_reset(
    ctx: &mut EscContext<'_>,
    conformance_level: ConformanceLevel,
    c1_mode: C1Mode,
) {
    apply_hard_reset_state(
        ctx.screen,
        ctx.stash,
        ctx.viewport,
        ctx.on_alt_screen,
        ctx.modes,
        ctx.kitty_keyboard,
        ctx.cursor_style,
        ctx.current_title,
        ctx.title_stack,
        ctx.saved_modes,
        ctx.current_prompt_row,
        ctx.bell_pending,
        ctx.vt52_cursor_addr,
        ctx.palette,
        ctx.base_palette,
        ctx.default_status_display,
        ctx.dec_color,
        ctx.macros,
        ctx.drcs,
        conformance_level,
        c1_mode,
    );
}

/// Forward-scan tab stops from `start_col + 1`. Returns the column of the
/// next set tab stop, or `cols - 1` if none is found.
fn next_tab_stop(
    tab_stops: &[bool],
    start_col: u32,
    cols: u32,
) -> u32 {
    let start = start_col as usize + 1;
    let end = cols as usize;
    if let Some(offset) = tab_stops
        .get(start..end)
        .and_then(|s| s.iter().position(|&v| v))
    {
        (start + offset) as u32
    } else {
        cols - 1
    }
}

/// Backward-scan tab stops from `start_col - 1`. Returns the column of the
/// previous set tab stop, or 0 if none is found.
fn prev_tab_stop(
    tab_stops: &[bool],
    start_col: u32,
) -> u32 {
    if start_col == 0 {
        return 0;
    }
    for c in (0..start_col as usize).rev() {
        if tab_stops[c] {
            return c as u32;
        }
    }
    0
}

/// Fast path for a batched run of printable ASCII bytes (0x20..=0x7E).
///
/// Skips the grapheme/width machinery `put_char` needs — every byte is
/// width-1 and can't fold into a neighbour. Breaks wide-anchor invariants at
/// only the run's two edges (interior cells are entirely overwritten, so any
/// anchors they held are destroyed outright).
pub(super) fn put_ascii_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &[u8],
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }

    // Single-shift (SS2/SS3): the first character uses G2 or G3 for one
    // character only, then GL snaps back to its previous mapping.
    let run = if let Some(charset) = screen.charset.take_single_shift_charset() {
        let b = run[0];
        let ch = charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
            .unwrap_or_else(|| ASCII_CELLS[(b - 0x20) as usize].clone());
        put_char(screen, viewport, ch, insert_mode);
        &run[1..]
    } else {
        run
    };
    if run.is_empty() {
        return;
    }

    if charset::gl_charset_requires_translation(&screen.charset, screen.nrc_mode) {
        let charset = screen.charset.gl_charset();
        for &b in run {
            let ch = charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
                .unwrap_or_else(|| ASCII_CELLS[(b - 0x20) as usize].clone());
            put_char(screen, viewport, ch, insert_mode);
        }
        return;
    }

    let fg = screen.fg;
    let bg = screen.bg;
    let attrs = screen.attrs;
    let ul = screen.underline;
    let ul_color = screen.underline_color;
    let link = screen.current_hyperlink;

    // Record the last byte of the run for REP (CSI Ps b).
    let last_byte = *run.last().unwrap();
    screen.last_char = Some(ASCII_CELLS[(last_byte - 0x20) as usize].clone());

    let mut i = 0;
    while i < run.len() {
        let cols = current_row_display_cols(screen, viewport);
        // Pre-wrap: a cursor parked past the last column wraps before
        // writing when DECAWM is on. When off, clamp to the last column
        // so subsequent writes overwrite in place.
        if screen.cursor.col >= cols {
            if screen.autowrap {
                soft_wrap(screen, viewport);
            } else {
                screen.cursor.col = cols - 1;
            }
        }

        let r = screen::active_row_index(screen, viewport);
        let col = screen.cursor.col as usize;
        let remaining_cols = (cols - screen.cursor.col) as usize;
        let chunk_len = (run.len() - i).min(remaining_cols);

        // IRM: shift existing content right before overwriting.
        if insert_mode {
            screen
                .grid
                .insert_chars(&screen.cursor, viewport, chunk_len as u16);
        }

        // Break a wide anchor severed by the left edge of the chunk. The
        // right-edge case is covered by passing chunk_len to
        // break_wide_glyphs_around_write.
        let row = &mut screen.grid.rows[r];
        break_wide_glyphs_around_write(row, col, chunk_len);
        let chunk = &run[i..i + chunk_len];
        // Hoist the LazyLock deref so the inner loop sees a plain
        // `&[SmolStr; 95]`; the parser guarantees each byte is 0x20..=0x7E
        // so the bounds check on the table index is provably redundant.
        let table: &[SmolStr; 95] = &ASCII_CELLS;
        for (cell, &b) in row.cells[col..col + chunk_len].iter_mut().zip(chunk) {
            // SAFETY: parser emits PrintAscii only for bytes in 0x20..=0x7E,
            // so (b - 0x20) is in 0..95 and the table index is in range.
            *cell = unsafe { table.get_unchecked((b - 0x20) as usize) }.clone();
        }
        // Attributes are homogeneous across the run — let the compiler lower
        // each of these to a single memset-style fill.
        row.fg[col..col + chunk_len].fill(fg);
        row.bg[col..col + chunk_len].fill(bg);
        row.attrs[col..col + chunk_len].fill(attrs);
        row.underline[col..col + chunk_len].fill(ul);
        row.underline_color[col..col + chunk_len].fill(ul_color);
        row.links[col..col + chunk_len].fill(link);

        screen.cursor.col += chunk_len as u32;
        i += chunk_len;
    }
}

pub(super) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    let raw_width = UnicodeWidthStr::width(s.as_str());

    // Fold only zero-width codepoints (combining marks, ZWJ, variation
    // selectors) into the prior anchor. Folding a *wide* codepoint into a
    // wide anchor would mean the host's wcswidth and our cursor disagree on
    // the cluster's width — e.g. `👨‍💻` is 4 cells per wcswidth (2+0+2) but
    // folding would advance our cursor by only 2, so every subsequent
    // redraw lands two columns off and backspace walks into the prompt.
    // Keeping each wide codepoint in its own cell range matches wcswidth;
    // the font shaper still sees the ZWJ sequence in `row_text` (empty
    // continuations contribute 0 bytes) and renders the ligature if the
    // font has one.
    if raw_width == 0 {
        try_extend_prev_cell(screen, viewport, &s);
        return;
    }

    // Single-shift applies to the next 7-bit graphic byte only.
    screen.charset.single_shift = None;

    let width = raw_width.max(1);
    let cols = current_row_display_cols(screen, viewport);

    // Soft-wrap when the incoming cluster (possibly wide) would overhang the
    // right edge. When DECAWM is off, clamp instead of wrapping.
    if screen.cursor.col + width as u32 > cols {
        if screen.autowrap {
            soft_wrap(screen, viewport);
        } else {
            screen.cursor.col = cols.saturating_sub(width as u32);
        }
    }

    // IRM: shift existing content right before overwriting.
    if insert_mode {
        screen
            .grid
            .insert_chars(&screen.cursor, viewport, width as u16);
    }

    let fg = screen.fg;
    let bg = screen.bg;
    let attrs = screen.attrs;
    let ul = screen.underline;
    let ul_color = screen.underline_color;
    let link = screen.current_hyperlink;
    let r = screen::active_row_index(screen, viewport);
    let col = screen.cursor.col as usize;

    // Preserve the "a cell is a continuation iff its left neighbour is a wide
    // anchor" invariant by blanking any wide-anchor/continuation pair the new
    // write would sever. See design note: we only fix this at put_char, not
    // at clear/erase/reflow.
    break_wide_glyphs_around_write(&mut screen.grid.rows[r], col, width);

    screen.grid.rows[r].cells[col] = s.clone();
    screen.grid.rows[r].fg[col] = fg;
    screen.grid.rows[r].bg[col] = bg;
    screen.grid.rows[r].attrs[col] = attrs;
    screen.grid.rows[r].underline[col] = ul;
    screen.grid.rows[r].underline_color[col] = ul_color;
    screen.grid.rows[r].links[col] = link;
    for i in 1..width {
        screen.grid.rows[r].cells[col + i] = continuation_cell();
        screen.grid.rows[r].fg[col + i] = fg;
        screen.grid.rows[r].bg[col + i] = bg;
        screen.grid.rows[r].attrs[col + i] = attrs;
        screen.grid.rows[r].underline[col + i] = ul;
        screen.grid.rows[r].underline_color[col + i] = ul_color;
        screen.grid.rows[r].links[col + i] = link;
    }
    screen.last_char = Some(s);
    screen.cursor.col += width as u32;
}

fn translated_codepoint(
    screen: &mut Screen,
    ch: char,
) -> Option<SmolStr> {
    let cp = ch as u32;
    if (0x20..=0x7E).contains(&cp) {
        if let Some(charset) = screen.charset.take_single_shift_charset() {
            let b = cp as u8;
            return Some(
                charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
                    .unwrap_or_else(|| ASCII_CELLS[(b - 0x20) as usize].clone()),
            );
        }
        if charset::gl_charset_requires_translation(&screen.charset, screen.nrc_mode) {
            let charset = screen.charset.gl_charset();
            let b = cp as u8;
            return Some(
                charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
                    .unwrap_or_else(|| ASCII_CELLS[(b - 0x20) as usize].clone()),
            );
        }
        return None;
    }

    if (0xA0..=0xFF).contains(&cp)
        && charset::gr_charset_requires_translation(&screen.charset, screen.nrc_mode)
    {
        let charset = screen.charset.gr_charset();
        return Some(
            charset::translate_gr_codepoint(ch, charset, screen.nrc_mode, screen.upss)
                .unwrap_or_else(|| SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4]))),
        );
    }

    None
}

pub(super) fn put_printable(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    let mut chars = s.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
        && let Some(mapped) = translated_codepoint(screen, ch)
    {
        put_char(screen, viewport, mapped, insert_mode);
        return;
    }

    put_char(screen, viewport, s, insert_mode);
}

pub(super) fn put_8bit_byte(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    insert_mode: bool,
) {
    let ch = if charset::gr_charset_requires_translation(&screen.charset, screen.nrc_mode) {
        let charset = screen.charset.gr_charset();
        charset::translate_gr_byte(byte, charset, screen.nrc_mode, screen.upss).unwrap_or_else(
            || {
                let ch = char::from_u32(byte as u32).expect("0xA0..=0xFF is valid Unicode scalar");
                SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4]))
            },
        )
    } else {
        let ch = char::from_u32(byte as u32).expect("0xA0..=0xFF is valid Unicode scalar");
        SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4]))
    };
    put_char(screen, viewport, ch, insert_mode);
}

/// Process a [`vtepp::Action::PrintText`] run: a validated UTF-8 `&str`
/// that may contain a mix of printable ASCII (0x20..=0x7E) and multi-byte
/// UTF-8 codepoints.
///
/// The fast path splits the str into ASCII sub-runs (delegated to
/// [`put_ascii_run`]) and individual UTF-8 codepoints (delegated to
/// [`put_char`]).  The slow paths for single-shift and DEC Special Graphics
/// mirror the equivalent handling in [`put_ascii_run`].
pub(super) fn put_text_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }

    // Single-shift (SS2/SS3): the next 7-bit graphic byte uses G2/G3.
    // A UTF-8 run starts with a multibyte codepoint, so the shift cannot
    // apply and must be discarded.
    let mut chars = run.chars();
    let run = if screen.charset.single_shift.take().is_some() {
        let ch = chars.next().unwrap();
        put_char(
            screen,
            viewport,
            SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4])),
            insert_mode,
        );
        chars.as_str()
    } else {
        run
    };
    if run.is_empty() {
        return;
    }

    let bytes = run.as_bytes();

    if charset::gl_charset_requires_translation(&screen.charset, screen.nrc_mode)
        || charset::gr_charset_requires_translation(&screen.charset, screen.nrc_mode)
    {
        for ch in run.chars() {
            put_printable(
                screen,
                viewport,
                SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4])),
                insert_mode,
            );
        }
        return;
    }

    // Fast path: dispatch ASCII sub-runs to put_ascii_run and UTF-8
    // codepoints to put_char.  The input is validated UTF-8 so we can
    // derive byte lengths directly from lead bytes.
    let mut i = 0;
    while i < bytes.len() {
        // ASCII sub-run.
        let start = i;
        while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] <= 0x7E {
            i += 1;
        }
        if i > start {
            put_ascii_run(screen, viewport, &bytes[start..i], insert_mode);
        }
        if i >= bytes.len() {
            break;
        }
        // UTF-8 codepoint — input is validated, so just compute the length
        // from the lead byte and slice directly.
        let len = utf8_char_len(bytes[i]);
        let mut builder = SmolStrBuilder::new();
        builder.push_str(&run[i..i + len]);

        put_char(screen, viewport, builder.finish(), insert_mode);
        i += len;
    }
}

/// Byte length of a UTF-8 codepoint from its lead byte.
/// Only called on validated UTF-8, so the lead byte is always valid.
#[inline]
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// True if the cell at `col` is the anchor of a wide glyph — it holds
/// non-blank text and its right neighbour is the empty continuation
/// sentinel we placed when laying out the wide glyph. Consulting the grid
/// state is more robust than re-measuring the cell text: `unicode-width`
/// disagrees with glibc `wcswidth` on VS16-upgraded emoji (e.g. `❤️`, which
/// `unicode-width` reports as width 2 but `wcswidth` reports as 1), and we
/// keep such clusters single-cell to stay in sync with the shell's cursor
/// tracking. Checking neighbour emptiness reflects the physical invariant.
fn is_wide_anchor_at(
    row: &Row,
    col: usize,
) -> bool {
    let Some(anchor) = row.cells.get(col) else {
        return false;
    };
    let Some(right) = row.cells.get(col + 1) else {
        return false;
    };
    let anchor_str = anchor.as_str();
    !anchor_str.is_empty() && anchor_str != " " && right.as_str().is_empty()
}

/// Keep the wide-anchor/continuation invariant intact across an overwrite.
/// Left edge: if the cell to our left was a wide anchor, our write lands on
/// its continuation, so blank the orphaned anchor. Right edge: if the last
/// cell we're writing *is* a wide anchor, its continuation (at `col + width`)
/// won't be touched by the write and would dangle, so blank it.
fn break_wide_glyphs_around_write(
    row: &mut Row,
    col: usize,
    width: usize,
) {
    if col > 0 && is_wide_anchor_at(row, col - 1) {
        row.cells[col - 1] = blank_cell();
    }
    let last = col + width - 1;
    if is_wide_anchor_at(row, last) {
        let cont = last + 1;
        if cont < row.cells.len() {
            row.cells[cont] = blank_cell();
        }
    }
}

fn soft_wrap(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    screen.cursor.col = 0;
    let r = screen::active_row_index(screen, viewport);
    screen.grid.rows[r].wrapped = true;
    if screen.cursor.row == screen.scroll_bottom {
        if screen::page_can_scroll_down(screen, viewport) {
            screen::scroll_page_down(screen, viewport, 1);
        } else if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
            screen.grid.push_visible_row(viewport);
        } else {
            screen.grid.scroll_up_in_region(
                viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
            );
        }
    } else if screen.cursor.row < viewport.rows - 1 {
        screen.cursor.row += 1;
    }
}

/// Apply a private mode set/reset from the XTRESTORE path. Mirrors the
/// logic in the `CSI ? h`/`l` handler: terminal-level modes are handled
/// inline, screen/alt-screen modes delegate to `set_private_mode` and
/// `apply_mouse_mode`.
fn apply_private_mode(
    mode: u16,
    enable: bool,
    ctx: &mut CsiContext<'_>,
) {
    if mode == mode::DECANM {
        // `h` (enable) = ANSI mode; `l` (disable) = VT52 compatibility mode.
        // The sense is inverted: the mode *being set* means ANSI is active,
        // so VT52 is off.
        ctx.modes.vt52_mode = !enable;
    } else if mode == mode::DECSCNM {
        ctx.modes.screen_reverse = enable;
    } else if mode == mode::DECARM {
        ctx.modes.decarm = enable;
    } else if mode == mode::ATT610_BLINK {
        ctx.cursor_style.blink = enable;
    } else if mode == mode::DECNCSM {
        ctx.modes.decncsm = enable;
    } else if mode == mode::DECLRMM {
        ctx.modes.declrmm = enable;
        if !enable {
            ctx.screen.left_margin = 0;
            ctx.screen.right_margin = ctx.viewport.cols.saturating_sub(1);
        }
    } else if mode == mode::DECNRCM {
        ctx.modes.decnrcm = enable;
        for screen in [&mut *ctx.screen, &mut *ctx.stash] {
            screen.nrc_mode = enable;
            screen.charset = charset::CharsetState::new();
        }
    } else if mode == mode::BRACKETED_PASTE {
        ctx.modes.bracketed_paste = enable;
    } else if mode == mode::FOCUS_REPORTING {
        ctx.modes.focus_reporting = enable;
    } else if mode == mode::SYNCHRONIZED_UPDATE {
        ctx.modes.synchronized_update_since = enable.then(Instant::now);
    } else if mode == mode::ALLOW_DECCOLM {
        ctx.modes.allow_deccolm = enable;
    } else if mode == mode::DECATCUM {
        ctx.dec_color.alternate_underline_text = enable;
    } else if mode == mode::DECATCBM {
        ctx.dec_color.alternate_blink_text = enable;
    } else if mode == mode::DECBBSM {
        ctx.dec_color.bold_blink_affects_background = enable;
    } else if mode == mode::DECECM {
        ctx.dec_color.erase_to_screen = enable;
        for screen in [&mut *ctx.screen, &mut *ctx.stash] {
            sync_screen_erase_defaults(screen, ctx.dec_color);
        }
    } else if mode == mode::DECCOLM {
        // DECCOLM restore is tricky (resizes the grid). Skip for save/restore —
        // xterm itself ignores DECCOLM in XTSAVE/XTRESTORE.
    } else if !apply_mouse_mode(
        mode,
        enable,
        &mut ctx.modes.mouse_tracking,
        &mut ctx.modes.mouse_encoding,
    ) {
        screen::set_private_mode(
            mode,
            enable,
            ctx.screen,
            ctx.stash,
            ctx.viewport,
            ctx.on_alt_screen,
        );
    }
}

/// Map a private-mode number to its DECRQM response value:
/// 1 = set, 2 = reset, 0 = not recognized. Queries every private mode
/// we track so apps can probe capabilities without side effects.
fn query_private_mode(
    ps: u16,
    ctx: &CsiContext<'_>,
) -> u8 {
    match ps {
        mode::DECANM => {
            if !ctx.modes.vt52_mode {
                1
            } else {
                2
            }
        }
        mode::DECSCNM => {
            if ctx.modes.screen_reverse {
                1
            } else {
                2
            }
        }
        mode::DECARM => {
            if ctx.modes.decarm {
                1
            } else {
                2
            }
        }
        mode::ATT610_BLINK => {
            if ctx.cursor_style.blink {
                1
            } else {
                2
            }
        }
        mode::DECLRMM => {
            if ctx.modes.declrmm {
                1
            } else {
                2
            }
        }
        mode::DECNRCM => {
            if ctx.modes.decnrcm {
                1
            } else {
                2
            }
        }
        mode::DECNCSM => {
            if ctx.modes.decncsm {
                1
            } else {
                2
            }
        }
        mode::DECCKM => {
            if ctx.screen.app_cursor_keys {
                1
            } else {
                2
            }
        }
        mode::DECOM => {
            if ctx.screen.origin_mode {
                1
            } else {
                2
            }
        }
        mode::DECAWM => {
            if ctx.screen.autowrap {
                1
            } else {
                2
            }
        }
        mode::ALLOW_DECCOLM => {
            if ctx.modes.allow_deccolm {
                1
            } else {
                2
            }
        }
        mode::DECATCUM => {
            if ctx.dec_color.alternate_underline_text {
                1
            } else {
                2
            }
        }
        mode::DECATCBM => {
            if ctx.dec_color.alternate_blink_text {
                1
            } else {
                2
            }
        }
        mode::DECBBSM => {
            if ctx.dec_color.bold_blink_affects_background {
                1
            } else {
                2
            }
        }
        mode::DECECM => {
            if ctx.dec_color.erase_to_screen {
                1
            } else {
                2
            }
        }
        mode::DECTCEM => {
            if ctx.screen.cursor_visible {
                1
            } else {
                2
            }
        }
        mode::DECNKM => {
            if ctx.screen.app_keypad {
                1
            } else {
                2
            }
        }
        mode::ALT_SCREEN | mode::ALT_SCREEN_CLEAR | mode::ALT_SCREEN_SAVE => {
            if *ctx.on_alt_screen {
                1
            } else {
                2
            }
        }
        mode::X10_MOUSE => match_tracking(ctx.modes.mouse_tracking, MouseTracking::X10),
        mode::NORMAL_MOUSE => match_tracking(ctx.modes.mouse_tracking, MouseTracking::Normal),
        mode::BUTTON_EVENT_MOUSE => {
            match_tracking(ctx.modes.mouse_tracking, MouseTracking::ButtonEvent)
        }
        mode::ANY_EVENT_MOUSE => match_tracking(ctx.modes.mouse_tracking, MouseTracking::AnyEvent),
        mode::FOCUS_REPORTING => {
            if ctx.modes.focus_reporting {
                1
            } else {
                2
            }
        }
        mode::SAVE_CURSOR => {
            if ctx.screen.saved_cursor.is_some() {
                1
            } else {
                2
            }
        }
        60 => 4,
        mode::BRACKETED_PASTE => {
            if ctx.modes.bracketed_paste {
                1
            } else {
                2
            }
        }
        mode::SYNCHRONIZED_UPDATE => {
            if ctx.modes.synchronized_update_since.is_some() {
                1
            } else {
                2
            }
        }
        _ => 0,
    }
}

fn match_tracking(
    current: MouseTracking,
    target: MouseTracking,
) -> u8 {
    if current == target { 1 } else { 2 }
}

/// If appending `s` to the previously-written cell keeps it a single grapheme
/// cluster, do so and return `true`. Walks past continuation cells so a
/// combining mark or ZWJ piece folds into the wide anchor it visually
/// decorates, not the empty continuation sitting between them.
fn try_extend_prev_cell(
    screen: &mut Screen,
    viewport: &Viewport,
    s: &str,
) {
    let (prev_row, mut prev_col) = if screen.cursor.col > 0 && screen.cursor.col <= viewport.cols {
        let row = screen::active_row_index(screen, viewport);
        (row, (screen.cursor.col - 1) as usize)
    } else if screen.cursor.col == 0 {
        let row = screen::active_row_index(screen, viewport);
        if row == 0 || !screen.grid.rows[row].wrapped {
            return;
        }
        let prev_row = row - 1;
        let last_col = screen.grid.rows[prev_row].cells.len().saturating_sub(1);
        (prev_row, last_col)
    } else {
        return;
    };

    // Skip wide-glyph continuation cells to reach the anchor.
    while prev_col > 0 && screen.grid.rows[prev_row].cells[prev_col].is_empty() {
        prev_col -= 1;
    }

    let prev = &screen.grid.rows[prev_row].cells[prev_col];
    if prev.as_str() == " " || prev.is_empty() {
        return;
    }

    // Fold without widening the cell. VS16 etc. can bump `unicode-width` on
    // the combined string (e.g. `❤` + `VS16` → 2), but glibc `wcswidth` —
    // which the host shell uses to track cursor columns — still reports 1.
    // Matching wcswidth keeps backspace/cursor-movement in sync with
    // readline; `is_wide_anchor_at` looks at the grid state (continuation
    // cell to the right) rather than re-measuring this text, so the next
    // write won't misidentify the cell as a wide anchor and blank it.
    let mut combined = SmolStrBuilder::new();
    combined.push_str(prev);
    combined.push_str(s);
    let combined = combined.finish();
    if combined.graphemes(true).count() != 1 {
        return;
    }

    screen.grid.rows[prev_row].cells[prev_col] = combined;
}

pub(super) fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    // Cancel pending wrap for control characters that affect cursor
    // position. Without this, a BS/TAB/CR/LF after writing the last
    // column would see cursor.col == viewport.cols (one past the edge).
    clamp_cursor_to_row_width(screen, viewport);

    match byte {
        // LF, VT, FF all perform the same index-down operation. VT and FF
        // are defined as equivalent to LF by ECMA-48; vttest's "control
        // characters inside ESC sequences" test relies on VT working.
        b'\n' | VT | FF => {
            // LNM (mode 20): when enabled, LF/VT/FF imply CR.
            if newline_mode {
                screen.cursor.col = 0;
            }
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    screen.grid.scroll_up_in_region(
                        viewport,
                        &mut screen.images,
                        screen.scroll_top,
                        screen.scroll_bottom,
                        1,
                    );
                }
            } else if screen.cursor.row < viewport.rows - 1 {
                screen.cursor.row += 1;
                clamp_cursor_to_row_width(screen, viewport);
            }
        }
        b'\r' => {
            screen.cursor.col = 0;
        }
        BS => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        b'\t' => {
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
        }
        SO => {
            // Shift Out: invoke G1 into GL.
            screen.charset.set_gl(GraphicSetSlot::G1);
        }
        SI => {
            // Shift In: invoke G0 into GL (default).
            screen.charset.set_gl(GraphicSetSlot::G0);
        }
        BEL => {
            *bell_pending = true;
        }
        NUL => {}
        _ => {}
    }
}

pub(super) fn csi_dispatch(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    intermediates: &[u8],
    action: char,
) {
    // Cancel the pending-wrap state. After writing the last column,
    // cursor.col sits at viewport.cols (one past the right edge). Any CSI
    // sequence — cursor movement, erase, DSR report, even SGR — cancels
    // this state so the cursor reports and behaves as if on the last column.
    clamp_cursor_to_row_width(ctx.screen, ctx.viewport);

    if ctx.screen.active_display == ActiveDisplay::Status
        && screen::status_line_writable(ctx.screen)
        && status_line_csi_dispatch(ctx, params, intermediates, action)
    {
        return;
    }

    match intermediates {
        b"?" => {
            match action {
                'h' | 'l' => {
                    let enable = action == 'h';
                    for p in params.iter() {
                        match p[0] {
                            mode::DECANM => {
                                ctx.modes.vt52_mode = !enable;
                            }
                            mode::DECSCNM => {
                                ctx.modes.screen_reverse = enable;
                            }
                            mode::DECARM => {
                                ctx.modes.decarm = enable;
                            }
                            mode::ATT610_BLINK => {
                                ctx.cursor_style.blink = enable;
                            }
                            mode::DECNCSM => {
                                ctx.modes.decncsm = enable;
                            }
                            mode::DECLRMM => {
                                ctx.modes.declrmm = enable;
                                if !enable {
                                    ctx.screen.left_margin = 0;
                                    ctx.screen.right_margin = ctx.viewport.cols.saturating_sub(1);
                                }
                            }
                            mode::DECNRCM => {
                                ctx.modes.decnrcm = enable;
                                for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                                    screen.nrc_mode = enable;
                                    screen.charset = charset::CharsetState::new();
                                }
                            }
                            mode::BRACKETED_PASTE => {
                                ctx.modes.bracketed_paste = enable;
                            }
                            mode::FOCUS_REPORTING => {
                                ctx.modes.focus_reporting = enable;
                            }
                            mode::SYNCHRONIZED_UPDATE => {
                                ctx.modes.synchronized_update_since = enable.then(Instant::now);
                            }
                            mode::ALLOW_DECCOLM => {
                                ctx.modes.allow_deccolm = enable;
                            }
                            mode::DECATCUM => {
                                ctx.dec_color.alternate_underline_text = enable;
                            }
                            mode::DECATCBM => {
                                ctx.dec_color.alternate_blink_text = enable;
                            }
                            mode::DECBBSM => {
                                ctx.dec_color.bold_blink_affects_background = enable;
                            }
                            mode::DECECM => {
                                ctx.dec_color.erase_to_screen = enable;
                                for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                                    sync_screen_erase_defaults(screen, ctx.dec_color);
                                }
                            }
                            mode::DECCOLM => {
                                if !ctx.modes.allow_deccolm {
                                    continue;
                                }
                                let new_cols = if enable {
                                    ctx.modes.deccolm_saved_cols = Some(ctx.viewport.cols);
                                    132
                                } else {
                                    ctx.modes
                                        .deccolm_saved_cols
                                        .take()
                                        .unwrap_or(ctx.viewport.cols)
                                };
                                let old_cols = ctx.viewport.cols;
                                let rows = ctx.viewport.rows;
                                for s in [&mut *ctx.screen, &mut *ctx.stash] {
                                    screen::resize_screen(s, old_cols, rows, new_cols, rows);
                                }
                                ctx.viewport.cols = new_cols;
                                if !ctx.modes.decncsm {
                                    let view = screen::screen_viewport(ctx.screen, ctx.viewport);
                                    screen::clear_visible(ctx.screen, &view);
                                }
                                ctx.screen.scroll_top = 0;
                                ctx.screen.scroll_bottom = rows.saturating_sub(1);
                                ctx.screen.left_margin = 0;
                                ctx.screen.right_margin = (ctx.viewport.cols).saturating_sub(1);
                                ctx.screen.cursor = grid::Cursor::default();
                            }
                            mode => {
                                if !apply_mouse_mode(
                                    mode,
                                    enable,
                                    &mut ctx.modes.mouse_tracking,
                                    &mut ctx.modes.mouse_encoding,
                                ) {
                                    screen::set_private_mode(
                                        mode,
                                        enable,
                                        ctx.screen,
                                        ctx.stash,
                                        ctx.viewport,
                                        ctx.on_alt_screen,
                                    );
                                }
                            }
                        }
                    }
                }
                's' => {
                    for p in params.iter() {
                        let mode = p[0];
                        let state = query_private_mode(mode, ctx);
                        ctx.saved_modes.insert(mode, state == 1);
                    }
                }
                'r' => {
                    for p in params.iter() {
                        let mode = p[0];
                        if let Some(&saved) = ctx.saved_modes.get(&mode) {
                            apply_private_mode(mode, saved, ctx);
                        }
                    }
                }
                'J' => {
                    let view = screen::screen_viewport(ctx.screen, ctx.viewport);
                    let mode = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    ctx.screen.grid.erase_in_display_selective(
                        &ctx.screen.cursor,
                        &view,
                        &mut ctx.screen.images,
                        mode,
                    );
                }
                'K' => {
                    let view = screen::screen_viewport(ctx.screen, ctx.viewport);
                    let mode = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    ctx.screen
                        .grid
                        .erase_in_line_selective(&ctx.screen.cursor, &view, mode);
                }
                'u' => {
                    handle_kitty_keyboard(
                        b'?',
                        params,
                        ctx.kitty_keyboard,
                        ctx.modes.c1_mode,
                        ctx.pending_output,
                    );
                }
                'n' => {
                    let ps = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    if ps == DSR_CPR {
                        let row = ctx.screen.cursor.row + 1;
                        let col = ctx.screen.cursor.col + 1;
                        let page = ctx
                            .screen
                            .page_memory
                            .as_ref()
                            .map(|page| page.active_page + 1)
                            .unwrap_or(1);
                        conformance::write_csi(
                            ctx.pending_output,
                            ctx.modes.c1_mode,
                            format_args!("?{row};{col};{page}R"),
                        );
                    }
                }
                _ => {}
            }
            return;
        }
        b"?$" if action == 'p' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            let pm = query_private_mode(ps, ctx);
            conformance::write_csi(
                ctx.pending_output,
                ctx.modes.c1_mode,
                format_args!("?{ps};{pm}$y"),
            );
            return;
        }
        b"$" => {
            match action {
                '}' => {
                    let ps = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    ctx.screen.active_display = match ps {
                        1 if screen::status_line_writable(ctx.screen) => ActiveDisplay::Status,
                        _ => ActiveDisplay::Main,
                    };
                }
                '~' => {
                    let ps = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    let total_rows = ctx.viewport.rows + screen::status_line_rows(ctx.screen);
                    let old_rows = ctx.viewport.rows;
                    let status_display = match ps {
                        1 => StatusDisplayKind::Indicator,
                        2 => StatusDisplayKind::HostWritable,
                        _ => StatusDisplayKind::None,
                    };
                    screen::set_status_display(
                        ctx.screen,
                        ctx.viewport.cols,
                        status_display,
                        ctx.palette.status_line_fg,
                        ctx.palette.status_line_bg,
                    );
                    let new_rows = total_rows.saturating_sub(screen::status_line_rows(ctx.screen));
                    if new_rows != old_rows {
                        let old_cols = ctx.viewport.cols;
                        screen::resize_screen(ctx.screen, old_cols, old_rows, old_cols, new_rows);
                        if screen::page_memory_active(ctx.screen)
                            && let Some(page_rows) = screen::page_rows(ctx.screen)
                        {
                            screen::resize_page_memory(
                                ctx.screen,
                                &Viewport {
                                    rows: new_rows,
                                    cols: old_cols,
                                    top: 0,
                                },
                                page_rows,
                            );
                        }
                        ctx.viewport.rows = new_rows;
                    }
                }
                'w' => {
                    let ps = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    match ps {
                        1 => {
                            if let Some(report) =
                                crate::deccir_report(ctx.screen, ctx.viewport, ctx.modes, ctx.drcs)
                            {
                                conformance::write_dcs(
                                    ctx.pending_output,
                                    ctx.modes.c1_mode,
                                    format_args!("1$u{report}"),
                                );
                            }
                        }
                        2 => {
                            let stops = crate::dectabsr_report(ctx.screen);
                            conformance::write_dcs(
                                ctx.pending_output,
                                ctx.modes.c1_mode,
                                format_args!("2$u{stops}"),
                            );
                        }
                        _ => {}
                    }
                }
                'p' => {
                    let ps = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(0);
                    let pm = match ps {
                        mode::IRM => {
                            if ctx.modes.insert_mode {
                                1
                            } else {
                                2
                            }
                        }
                        mode::LNM => {
                            if ctx.modes.newline_mode {
                                1
                            } else {
                                2
                            }
                        }
                        1 | 5 | 7 | 10 | 11 | 13 | 14 | 15 | 16 | 17 | 18 | 19 => 4,
                        _ => 0,
                    };
                    conformance::write_csi(
                        ctx.pending_output,
                        ctx.modes.c1_mode,
                        format_args!("{ps};{pm}$y"),
                    );
                }
                '|' => {
                    let ps = params
                        .iter()
                        .next()
                        .and_then(|g| g.first().copied())
                        .unwrap_or(80);
                    let Some(cols) = matches!(ps, 80 | 132).then_some(ps as u32) else {
                        return;
                    };
                    let old_cols = ctx.viewport.cols;
                    let total_rows = ctx.viewport.rows + screen::status_line_rows(ctx.screen);
                    for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                        let rows = total_rows.saturating_sub(screen::status_line_rows(screen));
                        screen::resize_screen(screen, old_cols, rows, cols, rows);
                        if screen::page_memory_active(screen) {
                            let page_rows = screen::page_rows(screen).unwrap_or(rows);
                            screen::resize_page_memory(
                                screen,
                                &Viewport { rows, cols, top: 0 },
                                page_rows,
                            );
                        }
                    }
                    ctx.viewport.cols = cols;
                    *ctx.pending_resize = Some((
                        cols,
                        ctx.viewport.rows + screen::status_line_rows(ctx.screen),
                    ));
                    ctx.screen.right_margin = cols.saturating_sub(1);
                    ctx.screen.cursor.col = ctx.screen.cursor.col.min(cols.saturating_sub(1));
                }
                'z' | '{' | 'x' | 'v' | 'r' | 't' => {
                    let view = screen::screen_viewport(ctx.screen, ctx.viewport);
                    let rows = view.rows;
                    let cols = view.cols;
                    let p: Vec<u16> = params.iter().map(|p| p[0]).collect();

                    if matches!(action, 'r' | 't')
                        && ctx.screen.attr_change_extent == grid::AttrChangeExtent::Stream
                    {
                        let start_row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
                        let start_col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
                        let end_row = (p.get(2).copied().unwrap_or(rows as u16).max(1) as u32 - 1)
                            .min(rows.saturating_sub(1));
                        let end_col = (p.get(3).copied().unwrap_or(cols as u16).max(1) as u32 - 1)
                            .min(cols.saturating_sub(1));
                        if start_row > end_row || (start_row == end_row && start_col > end_col) {
                            return;
                        }
                        let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                        match action {
                            'r' => ctx.screen.grid.change_attrs_rect(
                                &view,
                                start_row,
                                start_col,
                                end_row,
                                end_col,
                                &sgr,
                                ctx.screen.attr_change_extent,
                            ),
                            't' => ctx.screen.grid.reverse_attrs_rect(
                                &view,
                                start_row,
                                start_col,
                                end_row,
                                end_col,
                                &sgr,
                                ctx.screen.attr_change_extent,
                            ),
                            _ => {}
                        }
                        return;
                    }

                    let rect_top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
                    let rect_left = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
                    let rect_bottom = (p.get(2).copied().unwrap_or(rows as u16).max(1) as u32 - 1)
                        .min(rows.saturating_sub(1));
                    let rect_right = (p.get(3).copied().unwrap_or(cols as u16).max(1) as u32 - 1)
                        .min(cols.saturating_sub(1));

                    if rect_top > rect_bottom || rect_left > rect_right {
                        return;
                    }

                    match action {
                        'z' => {
                            ctx.screen.grid.erase_rect(
                                &view,
                                rect_top,
                                rect_left,
                                rect_bottom,
                                rect_right,
                            );
                        }
                        '{' => {
                            ctx.screen.grid.erase_rect_selective(
                                &view,
                                rect_top,
                                rect_left,
                                rect_bottom,
                                rect_right,
                            );
                        }
                        'x' => {
                            let ch_code = p.get(4).copied().unwrap_or(0x20) as u32;
                            let valid =
                                (32..=126).contains(&ch_code) || (160..=255).contains(&ch_code);
                            if valid && let Some(ch) = char::from_u32(ch_code) {
                                let mut buf = [0u8; 4];
                                let s = SmolStr::new(ch.encode_utf8(&mut buf) as &str);
                                ctx.screen.grid.fill_rect(
                                    &view,
                                    rect_top,
                                    rect_left,
                                    rect_bottom,
                                    rect_right,
                                    s,
                                    ctx.screen.fg,
                                    ctx.screen.bg,
                                    ctx.screen.attrs,
                                    ctx.screen.underline,
                                    ctx.screen.underline_color,
                                );
                            }
                        }
                        'v' => {
                            let src_page = p.get(4).copied().unwrap_or(1);
                            let dst_top = p.get(5).copied().unwrap_or(1).max(1) as u32 - 1;
                            let dst_left = p.get(6).copied().unwrap_or(1).max(1) as u32 - 1;
                            let dst_page = p.get(7).copied().unwrap_or(1);
                            if src_page > 1 || dst_page > 1 {
                                screen::ensure_page_memory(ctx.screen, ctx.viewport);
                            }
                            let Some(src_view) =
                                screen::page_viewport(ctx.screen, ctx.viewport, src_page)
                            else {
                                return;
                            };
                            let Some(dst_view) =
                                screen::page_viewport(ctx.screen, ctx.viewport, dst_page)
                            else {
                                return;
                            };
                            ctx.screen.grid.copy_rect(
                                &src_view,
                                rect_top,
                                rect_left,
                                rect_bottom,
                                rect_right,
                                dst_top,
                                dst_left,
                                &dst_view,
                            );
                        }
                        'r' => {
                            let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                            ctx.screen.grid.change_attrs_rect(
                                &view,
                                rect_top,
                                rect_left,
                                rect_bottom,
                                rect_right,
                                &sgr,
                                ctx.screen.attr_change_extent,
                            );
                        }
                        't' => {
                            let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                            ctx.screen.grid.reverse_attrs_rect(
                                &view,
                                rect_top,
                                rect_left,
                                rect_bottom,
                                rect_right,
                                &sgr,
                                ctx.screen.attr_change_extent,
                            );
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            return;
        }
        b"*" if csi_dispatch_star_intermediate(ctx, params, action) => {
            return;
        }
        b" " if csi_dispatch_space_intermediate(ctx, params, action) => {
            return;
        }
        b"\"" if csi_dispatch_quote_intermediate(ctx, params, action) => {
            return;
        }
        b"'" if csi_dispatch_apostrophe_intermediate(ctx, params, action) => {
            return;
        }
        b"!" if csi_dispatch_bang_intermediate(ctx, action) => {
            return;
        }
        b"&" if csi_dispatch_amp_intermediate(ctx, action) => {
            return;
        }
        b">" | b"<" | b"="
            if csi_dispatch_gt_lt_eq_intermediate(ctx, params, intermediates, action) =>
        {
            return;
        }
        _ => {}
    }

    if !intermediates.is_empty() {
        return;
    }

    let pending_output = &mut *ctx.pending_output;
    let screen = &mut *ctx.screen;
    let viewport = screen::screen_viewport(screen, ctx.viewport);
    let viewport = &viewport;
    let p: Vec<u16> = params.iter().map(|p| p[0]).collect();

    match action {
        'y' => {
            let mut groups = params.iter();
            let selector = groups.next().and_then(|g| g.first().copied()).unwrap_or(0);
            if selector != 4 {
                return;
            }
            let requested_tests: Vec<u16> = groups.flat_map(|g| g.iter().copied()).collect();
            let power_up_self_test = requested_tests.is_empty()
                || requested_tests.contains(&0)
                || requested_tests.contains(&1);
            if power_up_self_test {
                apply_hard_reset_state(
                    ctx.screen,
                    ctx.stash,
                    ctx.viewport,
                    ctx.on_alt_screen,
                    ctx.modes,
                    ctx.kitty_keyboard,
                    ctx.cursor_style,
                    ctx.current_title,
                    ctx.title_stack,
                    ctx.saved_modes,
                    ctx.current_prompt_row,
                    ctx.bell_pending,
                    ctx.vt52_cursor_addr,
                    ctx.palette,
                    ctx.base_palette,
                    ctx.default_status_display,
                    ctx.dec_color,
                    ctx.macros,
                    ctx.drcs,
                    ConformanceLevel::Level4,
                    C1Mode::SevenBit,
                );
            }
        }
        'c' => {
            let macro_allowed = ctx
                .feature_permissions
                .macros
                .allows_programs(ctx.foreground_processes.as_ref());
            let level = if macro_allowed {
                ctx.modes.conformance_level.da1_code()
            } else {
                ctx.modes.conformance_level.da1_code().min(63)
            };
            let macro_feature = if macro_allowed { ";32" } else { "" };
            conformance::write_csi(
                pending_output,
                ctx.modes.c1_mode,
                format_args!("?{level};7;21;22;28;29{macro_feature}c"),
            );
        }
        'n' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            match ps {
                DSR_OK => {
                    conformance::write_csi(
                        ctx.pending_output,
                        ctx.modes.c1_mode,
                        format_args!("0n"),
                    );
                }
                DSR_CPR => {
                    let row = ctx.screen.cursor.row + 1;
                    let col = ctx.screen.cursor.col + 1;
                    conformance::write_csi(
                        ctx.pending_output,
                        ctx.modes.c1_mode,
                        format_args!("{row};{col}R"),
                    );
                }
                _ => {}
            }
        }
        't' => {
            let ps = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(0);
            if params.iter().count() <= 1
                && let Some(lines_per_page) = valid_page_lines(ps)
            {
                let rows = ctx.viewport.rows.min(lines_per_page);
                for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                    screen::activate_page_memory(
                        screen,
                        &Viewport {
                            rows,
                            cols: ctx.viewport.cols,
                            top: 0,
                        },
                        lines_per_page,
                    );
                }
                if rows != ctx.viewport.rows {
                    let old_cols = ctx.viewport.cols;
                    let old_total_rows = ctx.viewport.rows + screen::status_line_rows(ctx.screen);
                    let new_total_rows = rows + screen::status_line_rows(ctx.screen);
                    for screen in [&mut *ctx.screen, &mut *ctx.stash] {
                        let old_rows =
                            old_total_rows.saturating_sub(screen::status_line_rows(screen));
                        let new_rows =
                            new_total_rows.saturating_sub(screen::status_line_rows(screen));
                        screen::resize_screen(screen, old_cols, old_rows, old_cols, new_rows);
                    }
                    ctx.viewport.rows = rows;
                    *ctx.pending_resize = Some((
                        ctx.viewport.cols,
                        rows + screen::status_line_rows(ctx.screen),
                    ));
                }
                return;
            }
            match ps {
                WINOP_TITLE_PUSH => {
                    if ctx.title_stack.len() < 16 {
                        ctx.title_stack.push(ctx.current_title.clone());
                    }
                }
                WINOP_TITLE_POP => {
                    if let Some(title) = ctx.title_stack.pop() {
                        *ctx.current_title = title;
                    }
                }
                WINOP_REPORT_PIXELS => {
                    let h = ctx.viewport.rows * ctx.cell_height;
                    let w = ctx.viewport.cols * ctx.cell_width;
                    conformance::write_csi(
                        ctx.pending_output,
                        ctx.modes.c1_mode,
                        format_args!("4;{h};{w}t"),
                    );
                }
                WINOP_REPORT_CELL_SIZE => {
                    conformance::write_csi(
                        ctx.pending_output,
                        ctx.modes.c1_mode,
                        format_args!("6;{};{}t", ctx.cell_height, ctx.cell_width),
                    );
                }
                WINOP_REPORT_TEXT_SIZE => {
                    conformance::write_csi(
                        ctx.pending_output,
                        ctx.modes.c1_mode,
                        format_args!("8;{};{}t", ctx.viewport.rows, ctx.viewport.cols),
                    );
                }
                _ => {}
            }
        }
        'b' => {
            let n = params
                .iter()
                .next()
                .and_then(|g| g.first().copied())
                .unwrap_or(1)
                .max(1);
            if let Some(ch) = ctx.screen.last_char.clone() {
                let insert = ctx.modes.insert_mode;
                let view = screen::screen_viewport(ctx.screen, ctx.viewport);
                for _ in 0..n {
                    put_char(ctx.screen, &view, ch.clone(), insert);
                }
            }
        }
        'A' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let top = if screen.origin_mode {
                screen.scroll_top
            } else {
                0
            };
            screen.cursor.row = screen.cursor.row.saturating_sub(n).max(top);
            clamp_cursor_to_row_width(screen, viewport);
        }
        'B' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let bottom = if screen.origin_mode {
                screen.scroll_bottom
            } else {
                viewport.rows - 1
            };
            screen.cursor.row = (screen.cursor.row + n).min(bottom);
            clamp_cursor_to_row_width(screen, viewport);
        }
        'C' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = (screen.cursor.col + n).min(cols - 1);
        }
        'D' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen.cursor.col = screen.cursor.col.saturating_sub(n);
        }
        // CNL — Cursor Next Line. Move down Ps lines and to column 1.
        'E' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen.cursor.row = (screen.cursor.row + n).min(viewport.rows - 1);
            screen.cursor.col = 0;
        }
        // CPL — Cursor Previous Line. Move up Ps lines and to column 1.
        'F' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen.cursor.row = screen.cursor.row.saturating_sub(n);
            screen.cursor.col = 0;
        }
        'H' | 'f' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
            let target_row = if screen.origin_mode {
                (screen.scroll_top + row).min(screen.scroll_bottom)
            } else {
                row.min(viewport.rows - 1)
            };
            let cols = row_display_cols(screen, viewport, target_row);
            screen.cursor.row = target_row;
            screen.cursor.col = col.min(cols - 1);
        }
        'J' => {
            let mode = p.first().copied().unwrap_or(0);
            screen
                .grid
                .erase_in_display(&screen.cursor, viewport, &mut screen.images, mode);
        }
        'K' => {
            let mode = p.first().copied().unwrap_or(0);
            screen.grid.erase_in_line(&screen.cursor, viewport, mode);
        }
        'm' => {
            apply_sgr(
                &mut screen.fg,
                &mut screen.bg,
                &mut screen.attrs,
                &mut screen.underline,
                &mut screen.underline_color,
                params,
                ctx.palette,
            );
            sync_screen_erase_defaults(screen, ctx.dec_color);
        }
        'd' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            if screen.origin_mode {
                screen.cursor.row = (screen.scroll_top + row).min(screen.scroll_bottom);
            } else {
                screen.cursor.row = row.min(viewport.rows - 1);
            }
            clamp_cursor_to_row_width(screen, viewport);
        }
        // CHA — Cursor Horizontal Absolute. HPA (`) is an alias.
        'G' | '`' => {
            let col = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = col.min(cols - 1);
        }
        // HPR — Horizontal Position Relative. Alias for CUF (C).
        'a' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = (screen.cursor.col + n).min(cols - 1);
        }
        // VPR — Vertical Position Relative. Alias for CUD (B).
        'e' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let bottom = if screen.origin_mode {
                screen.scroll_bottom
            } else {
                viewport.rows - 1
            };
            screen.cursor.row = (screen.cursor.row + n).min(bottom);
            clamp_cursor_to_row_width(screen, viewport);
        }
        'L' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if screen.cursor.row >= screen.scroll_top && screen.cursor.row <= screen.scroll_bottom {
                let top = screen.cursor.row;
                if ctx.modes.declrmm {
                    screen.grid.scroll_down_in_rect(
                        viewport,
                        top,
                        screen.scroll_bottom,
                        screen.left_margin,
                        screen.right_margin,
                        n,
                    );
                } else {
                    screen.grid.scroll_down_in_region(
                        viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        n,
                    );
                }
            }
        }
        'M' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if screen.cursor.row >= screen.scroll_top && screen.cursor.row <= screen.scroll_bottom {
                let top = screen.cursor.row;
                if ctx.modes.declrmm {
                    screen.grid.scroll_up_in_rect(
                        viewport,
                        top,
                        screen.scroll_bottom,
                        screen.left_margin,
                        screen.right_margin,
                        n,
                    );
                } else {
                    screen.grid.scroll_up_in_region(
                        viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        n,
                    );
                }
            }
        }
        'P' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            screen.grid.delete_chars(&screen.cursor, viewport, n);
        }
        '@' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            screen.grid.insert_chars(&screen.cursor, viewport, n);
        }
        'X' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            screen.grid.erase_chars(&screen.cursor, viewport, n);
        }
        'S' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if screen::page_can_scroll_down(screen, viewport) {
                screen::scroll_page_down(screen, viewport, n);
            } else if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                for _ in 0..n {
                    screen.grid.push_visible_row(viewport);
                }
            } else {
                screen.grid.scroll_up_in_region(
                    viewport,
                    &mut screen.images,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'T' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen.grid.scroll_down_in_region(
                viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                n,
            );
        }
        'r' => {
            let top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let bottom = p.get(1).copied().unwrap_or(viewport.rows as u16).max(1) as u32 - 1;
            screen.scroll_top = top.min(viewport.rows - 1);
            screen.scroll_bottom = bottom.min(viewport.rows - 1).max(screen.scroll_top);
            // Home cursor. In origin mode, home means the scroll region
            // top; in absolute mode, home means row 0.
            screen.cursor.row = if screen.origin_mode {
                screen.scroll_top
            } else {
                0
            };
            screen.cursor.col = 0;
        }
        's' => {
            // Disambiguation: when DECLRMM is active and parameters are
            // present, CSI Pl ; Pr s is DECSLRM (Set Left/Right Margins).
            // Otherwise it's SCOSC (save cursor position).
            if ctx.modes.declrmm && !p.is_empty() {
                let left = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
                let right = p.get(1).copied().unwrap_or(viewport.cols as u16).max(1) as u32 - 1;
                screen.left_margin = left.min(viewport.cols.saturating_sub(1));
                screen.right_margin = right
                    .min(viewport.cols.saturating_sub(1))
                    .max(screen.left_margin);
            } else {
                screen::save_cursor_slot(screen);
            }
        }
        'u' => {
            // SCORC — restore cursor position. The kitty keyboard `CSI u`
            // variants all carry an intermediate (`>`, `<`, `=`, `?`) and
            // are caught above; a plain no-intermediate `CSI u` is
            // unambiguously the restore form.
            screen::restore_cursor_slot(screen, viewport);
        }
        'U' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen::activate_page_memory(screen, viewport, viewport.rows);
            if let Some(page) = screen.page_memory.as_mut() {
                page.active_page = (page.active_page + n).min(page.page_count().saturating_sub(1));
                page.display_top = 0;
            }
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        'V' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen::activate_page_memory(screen, viewport, viewport.rows);
            if let Some(page) = screen.page_memory.as_mut() {
                page.active_page = page.active_page.saturating_sub(n);
                page.display_top = 0;
            }
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        // CHT — Cursor Forward Tabulation. Advance Ps tab stops.
        'I' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            let cols = current_row_display_cols(screen, viewport);
            for _ in 0..n {
                screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
            }
        }
        // CBT — Cursor Backward Tabulation. Move back Ps tab stops.
        'Z' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            for _ in 0..n {
                screen.cursor.col = prev_tab_stop(&screen.tab_stops, screen.cursor.col);
            }
        }
        // TBC — Tab Clear.
        'g' => {
            let ps = p.first().copied().unwrap_or(0);
            match ps {
                TBC_CURRENT => {
                    let col = screen.cursor.col as usize;
                    if col < screen.tab_stops.len() {
                        screen.tab_stops[col] = false;
                    }
                }
                TBC_ALL => screen.tab_stops.fill(false),
                _ => {}
            }
        }
        'h' | 'l' => {
            // ANSI (non-private) mode set/reset. Private modes (with `?`
            // intermediate) are handled above.
            let enable = action == 'h';
            for &m in &p {
                match m {
                    mode::IRM => ctx.modes.insert_mode = enable,
                    mode::LNM => ctx.modes.newline_mode = enable,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn esc_dispatch_vt52(
    ctx: &mut EscContext<'_>,
    byte: u8,
) -> bool {
    if !ctx.modes.vt52_mode {
        return false;
    }
    if *ctx.vt52_cursor_addr != crate::Vt52CursorAddr::Idle {
        return false;
    }

    if byte == b'Y' {
        *ctx.vt52_cursor_addr = crate::Vt52CursorAddr::AwaitingRow;
        return true;
    }

    if !matches!(
        byte,
        b'A' | b'B' | b'C' | b'D' | b'F' | b'G' | b'H' | b'I' | b'J' | b'K' | b'Z' | b'<'
    ) {
        return false;
    }

    // VT52 mode — completely different ESC vocabulary, no CSI or parameters.
    // The `/` intermediate (ESC / Z identify response) shares the intermediate
    // byte space with ANSI SCS, so we must gate on vt52_mode *first*.
    {
        // Cancel pending wrap before any cursor-moving sequence.
        clamp_cursor_to_row_width(ctx.screen, ctx.viewport);
        match byte {
            // ESC A — cursor up (no scroll).
            b'A' => {
                ctx.screen.cursor.row = ctx.screen.cursor.row.saturating_sub(1);
                clamp_cursor_to_row_width(ctx.screen, ctx.viewport);
            }
            // ESC B — cursor down (no scroll).
            b'B' if ctx.screen.cursor.row + 1 < ctx.viewport.rows => {
                ctx.screen.cursor.row += 1;
                clamp_cursor_to_row_width(ctx.screen, ctx.viewport);
            }
            // ESC C — cursor right (no scroll).
            b'C' if ctx.screen.cursor.col + 1
                < current_row_display_cols(ctx.screen, ctx.viewport) =>
            {
                ctx.screen.cursor.col += 1;
            }
            // ESC D — cursor left (no scroll).
            b'D' => {
                ctx.screen.cursor.col = ctx.screen.cursor.col.saturating_sub(1);
            }
            // ESC F — enter DEC Special Graphics mode (same as SCS G0 = 0).
            b'F' => {
                ctx.screen
                    .charset
                    .designate(GraphicSetSlot::G0, CharacterSet::DecSpecialGraphics);
            }
            // ESC G — exit DEC Special Graphics mode (same as SCS G0 = B).
            b'G' => {
                ctx.screen
                    .charset
                    .designate(GraphicSetSlot::G0, CharacterSet::Ascii);
            }
            // ESC H — cursor home (0, 0).
            b'H' => {
                ctx.screen.cursor.row = 0;
                ctx.screen.cursor.col = 0;
            }
            // ESC I — reverse index (identical to ANSI RI / ESC M): scroll
            // down if at the top of the scroll region, else cursor up.
            b'I' => {
                if ctx.screen.cursor.row == ctx.screen.scroll_top {
                    ctx.screen.grid.scroll_down_in_region(
                        ctx.viewport,
                        &mut ctx.screen.images,
                        ctx.screen.scroll_top,
                        ctx.screen.scroll_bottom,
                        1,
                    );
                } else if ctx.screen.cursor.row > 0 {
                    ctx.screen.cursor.row -= 1;
                }
            }
            // ESC J — erase to end of screen (same as ANSI ED 0).
            b'J' => {
                ctx.screen.grid.erase_in_display(
                    &ctx.screen.cursor,
                    ctx.viewport,
                    &mut ctx.screen.images,
                    0,
                );
            }
            // ESC K — erase to end of line (same as ANSI EL 0).
            b'K' => {
                ctx.screen
                    .grid
                    .erase_in_line(&ctx.screen.cursor, ctx.viewport, 0);
            }
            // ESC Y — direct cursor address. The two parameter bytes are
            // absorbed by Terminal::apply via Vt52CursorAddr state.
            b'Y' => {
                *ctx.vt52_cursor_addr = crate::Vt52CursorAddr::AwaitingRow;
            }
            // ESC Z — identify. VT52 responds ESC / Z (0x1b 0x2f 0x5a).
            b'Z' => {
                ctx.pending_output.extend_from_slice(b"\x1b/Z");
            }
            // ESC < — exit VT52 mode, return to ANSI mode (sets DECANM).
            b'<' => {
                ctx.modes.vt52_mode = false;
            }
            _ => {}
        }
    }
    true
}

fn esc_dispatch_with_space_intermediate(
    ctx: &mut EscContext<'_>,
    byte: u8,
) -> bool {
    match byte {
        b'F' if can_negotiate_c1(ctx.modes) => {
            ctx.modes.c1_mode = C1Mode::SevenBit;
            true
        }
        b'G' if can_negotiate_c1(ctx.modes) => {
            ctx.modes.c1_mode = C1Mode::EightBit;
            true
        }
        _ => false,
    }
}

fn esc_dispatch_with_percent_intermediate(
    ctx: &mut EscContext<'_>,
    byte: u8,
) -> bool {
    if let Some(mode) = TextMode::from_docs_final(byte) {
        ctx.modes.text_mode = mode;
        true
    } else {
        false
    }
}

fn esc_dispatch_designation(
    ctx: &mut EscContext<'_>,
    intermediates: &[u8],
    byte: u8,
) -> bool {
    if let Some((slot, charset)) = charset::parse_designation(intermediates, byte).or_else(|| {
        let slot = charset::slot_for_intermediates(intermediates)?;
        let charset = ctx.drcs.lookup_designation(intermediates, byte)?;
        Some((slot, charset))
    }) {
        ctx.screen.charset.designate(slot, charset);
        true
    } else {
        false
    }
}

fn esc_dispatch_with_hash_intermediate(
    ctx: &mut EscContext<'_>,
    byte: u8,
) -> bool {
    match byte {
        b'8' => {
            let first_visible = ctx
                .screen
                .grid
                .rows
                .len()
                .saturating_sub(ctx.viewport.rows as usize);
            let e_cell = SmolStr::new_inline("E");
            let fg = ctx.palette.fg;
            let bg = ctx.palette.bg;
            for r in first_visible..ctx.screen.grid.rows.len() {
                let row = &mut ctx.screen.grid.rows[r];
                row.clear(fg, bg);
                row.wrapped = false;
                row.line_attr = LineAttr::Normal;
                for cell in row.cells.iter_mut() {
                    *cell = e_cell.clone();
                }
                row.fg.fill(fg);
                row.bg.fill(bg);
            }
            ctx.screen.scroll_top = 0;
            ctx.screen.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
            ctx.screen.left_margin = 0;
            ctx.screen.right_margin = ctx.viewport.cols.saturating_sub(1);
            ctx.screen.origin_mode = false;
            ctx.screen.cursor.row = 0;
            ctx.screen.cursor.col = 0;
            true
        }
        b'3' | b'4' | b'5' | b'6' => {
            let visible_start = ctx
                .screen
                .grid
                .rows
                .len()
                .saturating_sub(ctx.viewport.rows as usize);
            let abs_row = visible_start + ctx.screen.cursor.row as usize;
            if let Some(row) = ctx.screen.grid.rows.get_mut(abs_row) {
                row.line_attr = match byte {
                    b'3' => LineAttr::DoubleHeightTop,
                    b'4' => LineAttr::DoubleHeightBottom,
                    b'5' => LineAttr::Normal,
                    _ => LineAttr::DoubleWidth,
                };
                if ctx.screen.cursor.row as usize + visible_start == abs_row {
                    clamp_cursor_to_row_width(ctx.screen, ctx.viewport);
                }
            }
            true
        }
        _ => false,
    }
}

pub(super) fn esc_dispatch(
    ctx: &mut EscContext<'_>,
    intermediates: &[u8],
    byte: u8,
) {
    if intermediates.is_empty() && esc_dispatch_vt52(ctx, byte) {
        return;
    }

    match intermediates {
        b" " if esc_dispatch_with_space_intermediate(ctx, byte) => return,
        b"%" if esc_dispatch_with_percent_intermediate(ctx, byte) => return,
        b"#" if esc_dispatch_with_hash_intermediate(ctx, byte) => return,
        b"" => {}
        _ if esc_dispatch_designation(ctx, intermediates, byte) => return,
        _ => return,
    }

    // Cancel pending wrap before ESC sequences that move the cursor.
    clamp_cursor_to_row_width(ctx.screen, ctx.viewport);
    let viewport = screen::screen_viewport(ctx.screen, ctx.viewport);
    let viewport = &viewport;

    match byte {
        b'7' => screen::save_cursor_slot(ctx.screen),
        b'8' => screen::restore_cursor_slot(ctx.screen, viewport),
        // IND — Index. Move the cursor down one line; if at the bottom of
        // the scroll region, scroll the region up.
        b'D' => {
            if ctx.screen.cursor.row == ctx.screen.scroll_bottom {
                if ctx.screen.scroll_top == 0 && ctx.screen.scroll_bottom == viewport.rows - 1 {
                    if screen::page_can_scroll_down(ctx.screen, viewport) {
                        screen::scroll_page_down(ctx.screen, viewport, 1);
                    } else {
                        ctx.screen.grid.push_visible_row(viewport);
                    }
                } else {
                    ctx.screen.grid.scroll_up_in_region(
                        viewport,
                        &mut ctx.screen.images,
                        ctx.screen.scroll_top,
                        ctx.screen.scroll_bottom,
                        1,
                    );
                }
            } else if ctx.screen.cursor.row < viewport.rows - 1 {
                ctx.screen.cursor.row += 1;
                clamp_cursor_to_row_width(ctx.screen, viewport);
            }
        }
        // NEL — Next Line. Move to column 0 of the next line; scroll if
        // at the bottom of the scroll region.
        b'E' => {
            ctx.screen.cursor.col = 0;
            if ctx.screen.cursor.row == ctx.screen.scroll_bottom {
                if ctx.screen.scroll_top == 0 && ctx.screen.scroll_bottom == viewport.rows - 1 {
                    if screen::page_can_scroll_down(ctx.screen, viewport) {
                        screen::scroll_page_down(ctx.screen, viewport, 1);
                    } else {
                        ctx.screen.grid.push_visible_row(viewport);
                    }
                } else {
                    ctx.screen.grid.scroll_up_in_region(
                        viewport,
                        &mut ctx.screen.images,
                        ctx.screen.scroll_top,
                        ctx.screen.scroll_bottom,
                        1,
                    );
                }
            } else if ctx.screen.cursor.row < viewport.rows - 1 {
                ctx.screen.cursor.row += 1;
            }
            clamp_cursor_to_row_width(ctx.screen, viewport);
        }
        b'H' => {
            // HTS — set a tab stop at the current cursor column.
            let col = ctx.screen.cursor.col as usize;
            if col < ctx.screen.tab_stops.len() {
                ctx.screen.tab_stops[col] = true;
            }
        }
        b'c' => {
            // RIS (Reset to Initial State). Drop the app's terminal state
            // back to power-on defaults — every mode the app might have
            // flipped, plus the visible screen. Scrollback is preserved: a
            // misbehaving app's reset shouldn't take the user's history.
            apply_hard_reset(ctx, ConformanceLevel::Level4, C1Mode::SevenBit);
        }
        b'M' => {
            if ctx.screen.cursor.row == ctx.screen.scroll_top {
                ctx.screen.grid.scroll_down_in_region(
                    viewport,
                    &mut ctx.screen.images,
                    ctx.screen.scroll_top,
                    ctx.screen.scroll_bottom,
                    1,
                );
            } else if ctx.screen.cursor.row > 0 {
                ctx.screen.cursor.row -= 1;
            }
        }
        // DECKPAM (ESC =) — application keypad mode.
        // DECKPNM (ESC >) — normal keypad mode.
        b'=' => ctx.screen.app_keypad = true,
        b'>' => ctx.screen.app_keypad = false,
        // SS2 — Single Shift G2. Next graphic character uses G2.
        b'N' => ctx.screen.charset.single_shift = Some(GraphicSetSlot::G2),
        // SS3 — Single Shift G3. Next graphic character uses G3.
        b'O' => ctx.screen.charset.single_shift = Some(GraphicSetSlot::G3),
        // LS2 / LS3 — lock G2/G3 into GL.
        b'n' => ctx.screen.charset.set_gl(GraphicSetSlot::G2),
        b'o' => ctx.screen.charset.set_gl(GraphicSetSlot::G3),
        // LS1R / LS2R / LS3R — lock G1/G2/G3 into GR.
        b'~' => ctx.screen.charset.set_gr(GraphicSetSlot::G1),
        b'}' => ctx.screen.charset.set_gr(GraphicSetSlot::G2),
        b'|' => ctx.screen.charset.set_gr(GraphicSetSlot::G3),
        // DECBI — Back Index. Scroll region right if at left margin, else
        // move cursor left.
        b'6' => {
            if ctx.screen.cursor.col == 0 {
                ctx.screen.grid.scroll_right(
                    ctx.viewport,
                    ctx.screen.scroll_top,
                    ctx.screen.scroll_bottom,
                    1,
                );
            } else {
                ctx.screen.cursor.col -= 1;
            }
        }
        // DECFI — Forward Index. Scroll region left if at right margin, else
        // move cursor right.
        b'9' => {
            if ctx.screen.cursor.col >= current_row_display_cols(ctx.screen, ctx.viewport) - 1 {
                ctx.screen.grid.scroll_left(
                    ctx.viewport,
                    ctx.screen.scroll_top,
                    ctx.screen.scroll_bottom,
                    1,
                );
            } else {
                ctx.screen.cursor.col += 1;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use palette::Srgb;
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::cursor::CursorStyle;
    use crate::keyboard::KittyKeyboardState;
    use crate::screen::Screen;

    const TEST_COLS: u32 = 10;
    const TEST_ROWS: u32 = 4;

    fn setup() -> (Screen, Viewport) {
        let screen = Screen::new(
            TEST_COLS,
            TEST_ROWS,
            100,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let viewport = Viewport {
            rows: TEST_ROWS,
            cols: TEST_COLS,
            top: 0,
        };
        (screen, viewport)
    }

    /// Drive `input` through a VTE parser and dispatch each action through the
    /// parser module under test. This is the same pipeline the live terminal
    /// uses, so tests exercise the same paths callers actually take.
    fn feed(
        input: &[u8],
        screen: &mut Screen,
        viewport: &mut Viewport,
    ) {
        let base_pal = color::ColorPalette::default();
        let mut dec_color = crate::dec_color::state_from_palette(&base_pal);
        let mut pal = crate::dec_color::effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let foreground_processes: Option<ForegroundProcessSet> = None;
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for action in parser.parse(input) {
            // VT52 ESC Y cursor address state machine (mirrors Terminal::apply).
            if vt52_cursor_addr != crate::Vt52CursorAddr::Idle {
                let byte_opt: Option<u8> = match &action {
                    Action::PrintAscii(run) => run.first().copied(),
                    Action::Execute(b) => Some(*b),
                    _ => None,
                };
                match (vt52_cursor_addr, byte_opt) {
                    (crate::Vt52CursorAddr::AwaitingRow, Some(b)) => {
                        vt52_cursor_addr =
                            crate::Vt52CursorAddr::AwaitingCol(b.saturating_sub(0x20));
                        if let Action::PrintAscii(run) = &action
                            && run.len() >= 2
                        {
                            let row = b.saturating_sub(0x20) as u32;
                            let col = run[1].saturating_sub(0x20) as u32;
                            screen.cursor.row = row.min(viewport.rows.saturating_sub(1));
                            screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                            vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                            if run.len() > 2 {
                                let view = screen::screen_viewport(screen, viewport);
                                put_ascii_run(screen, &view, &run[2..], modes.insert_mode);
                            }
                            continue;
                        }
                        continue;
                    }
                    (crate::Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                        let col = b.saturating_sub(0x20) as u32;
                        screen.cursor.row = (row as u32).min(viewport.rows.saturating_sub(1));
                        screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                        if let Action::PrintAscii(run) = &action
                            && run.len() > 1
                        {
                            let view = screen::screen_viewport(screen, viewport);
                            put_ascii_run(screen, &view, &run[1..], modes.insert_mode);
                        }
                        continue;
                    }
                    _ => {
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                    }
                }
            }
            // In VT52 mode, CSI sequences are invalid and must be dropped.
            if modes.vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
                continue;
            }
            match action {
                Action::PrintAscii(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_ascii_run(screen, &view, run, modes.insert_mode)
                }
                Action::PrintText(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_text_run(screen, &view, run, modes.insert_mode)
                }
                Action::Print(s) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_printable(screen, &view, s, modes.insert_mode)
                }
                Action::Print8Bit(byte) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_8bit_byte(screen, &view, byte, modes.insert_mode)
                }
                Action::Execute(b) => {
                    let view = screen::screen_viewport(screen, viewport);
                    execute(screen, &view, b, &mut bell_pending, modes.newline_mode)
                }
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    let mut ctx = CsiContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        pending_output: &mut pending_output,
                        pending_resize: &mut pending_resize,
                        cursor_style: &mut cursor_style,
                        cell_width: 8,
                        cell_height: 16,
                        palette: &mut pal,
                        base_palette: &base_pal,
                        default_status_display: &mut default_status_display,
                        title_stack: &mut title_stack,
                        current_title: &mut current_title,
                        saved_modes: &mut saved_modes,
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
                        vt52_cursor_addr: &mut vt52_cursor_addr,
                        macros: &mut macros,
                        dec_color: &mut dec_color,
                        feature_permissions: &feature_permissions,
                        foreground_processes: &foreground_processes,
                        drcs: &mut drcs,
                    };
                    csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    let mut ctx = EscContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        cursor_style: &mut cursor_style,
                        current_title: &mut current_title,
                        title_stack: &mut title_stack,
                        saved_modes: &mut saved_modes,
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
                        palette: &mut pal,
                        base_palette: &base_pal,
                        default_status_display: &mut default_status_display,
                        pending_output: &mut pending_output,
                        vt52_cursor_addr: &mut vt52_cursor_addr,
                        macros: &mut macros,
                        dec_color: &mut dec_color,
                        drcs: &mut drcs,
                    };
                    esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
                }
                _ => {}
            }
        }
    }

    fn row_text(
        screen: &Screen,
        viewport: &Viewport,
        row: u32,
    ) -> String {
        let first_visible = screen.grid.rows.len() - viewport.rows as usize;
        let r = first_visible + row as usize;
        let mut s = String::new();
        for cell in &screen.grid.rows[r].cells {
            s.push_str(cell);
        }
        s
    }

    // -- put_char -----------------------------------------------------------

    #[test]
    fn put_char_writes_with_current_colors_and_advances() {
        let (mut screen, viewport) = setup();
        screen.fg = Srgb::new(1, 2, 3);
        screen.bg = Srgb::new(4, 5, 6);

        put_char(&mut screen, &viewport, SmolStr::new_inline("A"), false);

        assert_eq!(row_text(&screen, &viewport, 0).chars().next(), Some('A'));
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].fg[0], Srgb::new(1, 2, 3));
        assert_eq!(screen.grid.rows[r].bg[0], Srgb::new(4, 5, 6));
        assert_eq!(screen.cursor.col, 1);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn put_char_soft_wraps_at_right_edge() {
        let (mut screen, mut viewport) = setup();
        feed(b"abcdefghij", &mut screen, &mut viewport);

        // Cursor sits past the right edge; the next char should wrap.
        assert_eq!(screen.cursor.col, TEST_COLS);
        feed(b"k", &mut screen, &mut viewport);

        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
        assert!(
            screen.grid.rows[screen.grid.active_row_index(&screen.cursor, &viewport) - 1].wrapped
        );
        assert_eq!(&row_text(&screen, &viewport, 1)[..1], "k");
    }

    #[test]
    fn put_char_folds_combining_mark_into_previous_cell() {
        let (mut screen, mut viewport) = setup();
        // U+0301 COMBINING ACUTE ACCENT — feeding "e" then the combining mark
        // should store the full grapheme "é" in one cell without advancing.
        feed("e\u{0301}".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "e\u{0301}");
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_vs16_emoji_stays_in_single_cell() {
        let (mut screen, mut viewport) = setup();
        // `UnicodeWidthStr::width("❤\u{FE0F}") == 2`, but glibc `wcswidth`
        // reports 1 because it treats VS16 as a zero-width variation
        // selector without upgrading the base to emoji presentation. The
        // host shell tracks cursor position via wcswidth, so our grid must
        // agree — otherwise a single backspace from readline lands on the
        // continuation cell and the user can't delete the emoji. Keep the
        // cluster in one cell; the shaper still sees the full cluster
        // text and renders it scaled to that cell.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{2764}\u{FE0F}");
        assert_eq!(
            screen.grid.rows[r].cells[1].as_str(),
            " ",
            "VS16 must not widen the cell — cells[1] stays blank"
        );
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_write_after_vs16_emoji_preserves_the_emoji() {
        // Reproduces the reported "heart vanishes when you type anything
        // after it" bug. Before the fix, `is_wide_anchor` re-measured the
        // cell text with `UnicodeWidthStr` — which returns 2 for "❤\u{FE0F}"
        // — so `break_wide_glyphs_around_write` treated the single-cell
        // emoji as a misaligned wide anchor and blanked it. The grid-state
        // check in `is_wide_anchor_at` looks at the right neighbour
        // instead, matching the physical layout.
        let (mut screen, mut viewport) = setup();
        feed("\u{2764}\u{FE0F}X".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(
            screen.grid.rows[r].cells[0].as_str(),
            "\u{2764}\u{FE0F}",
            "heart must survive subsequent write"
        );
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "X");
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn backspace_over_vs16_emoji_moves_one_column() {
        // Readline sends a single BS to rub out `❤\u{FE0F}` because glibc
        // `wcswidth` reports its width as 1. The cursor must land on the
        // anchor column so subsequent rub-out bytes (typically `\b \b`)
        // clear the cell cleanly; widening the cell into 2 columns would
        // leave the cursor sitting on the continuation after one BS and
        // desync the shell's tracking.
        let (mut screen, mut viewport) = setup();
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 1);

        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 0);

        // A full rub-out of `\b \b` from bash lands us back at col 0 with
        // the cell erased.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &mut viewport);
        feed(b"\x08 \x08", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn put_char_regional_indicators_get_separate_cells() {
        let (mut screen, mut viewport) = setup();
        // `unicode-width` reports width 1 for each regional indicator, so
        // "🇺🇸" advances the cursor by 2 across two 1-col cells. We do not
        // collapse the flag pair into one cell — that would disagree with
        // the host's wcswidth and desync the cursor.
        feed("🇺🇸".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "🇺");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "🇸");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- wide (2-column) glyph handling ------------------------------------

    #[test]
    fn put_char_wide_glyph_occupies_two_cells_and_advances_cursor() {
        let (mut screen, mut viewport) = setup();
        feed("好".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), ""); // continuation
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn put_char_wide_glyph_soft_wraps_when_it_would_overhang() {
        let (mut screen, mut viewport) = setup();
        // Fill 9 of 10 columns with narrow chars so only 1 column is free.
        feed(b"abcdefghi", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 9);

        feed("好".as_bytes(), &mut screen, &mut viewport);

        // The wide glyph didn't fit at col 9, so we soft-wrap and place it
        // on the next row.
        assert_eq!(screen.cursor.row, 1);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.cursor.col, 2);
        assert!(screen.grid.rows[r - 1].wrapped);
    }

    #[test]
    fn put_char_narrow_overwriting_wide_anchor_blanks_continuation() {
        let (mut screen, mut viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &mut viewport);
        // Move cursor back to col 0 and stomp on the anchor with a narrow char.
        feed(b"\x1b[1;1H", &mut screen, &mut viewport);
        feed(b"x", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "x");
        // The continuation at col 1 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_narrow_overwriting_wide_continuation_blanks_anchor() {
        let (mut screen, mut viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &mut viewport);
        // Park cursor on the continuation (col 1) and write a narrow char.
        feed(b"\x1b[1;2H", &mut screen, &mut viewport);
        feed(b"x", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // The anchor at col 0 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "x");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_wide_overwriting_wide_blanks_both_neighbours() {
        let (mut screen, mut viewport) = setup();
        // [好, "", 世, "", a]
        feed("好世a".as_bytes(), &mut screen, &mut viewport);
        // Park on col 1 (好's continuation) and write a new wide glyph that
        // straddles the old layout.
        feed(b"\x1b[1;2H", &mut screen, &mut viewport);
        feed("界".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // 好's anchor (col 0) is orphaned — blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        // New wide glyph at cols 1-2.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "界");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "");
        // 世's orphaned continuation (at col 3) is blanked.
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[4].as_str(), "a");
    }

    #[test]
    fn put_char_zwj_emoji_keeps_components_in_separate_wide_cells() {
        let (mut screen, mut viewport) = setup();
        // 👨‍💻 = 👨 ZWJ 💻. wcswidth = 2+0+2 = 4, so the shell expects the
        // cursor to advance by 4. The ZWJ folds into `👨` (width 0 → fold),
        // but the second emoji starts a new wide cell of its own. The font
        // shaper still sees the full ZWJ sequence in `row_text` and renders
        // the ligature if the font has one.
        feed("👨\u{200D}💻".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👨\u{200D}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "💻");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "");
        assert_eq!(screen.cursor.col, 4);
    }

    // -- execute ------------------------------------------------------------

    #[test]
    fn execute_lf_moves_cursor_down() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\n', &mut false, false);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn execute_lf_at_scroll_bottom_scrolls_up() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();

        execute(&mut screen, &viewport, b'\n', &mut false, false);

        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert_eq!(screen.grid.rows.len(), rows_before + 1);
    }

    #[test]
    fn execute_cr_resets_col_to_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 5;
        execute(&mut screen, &viewport, b'\r', &mut false, false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_bs_saturates_at_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 2;
        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 1);
        execute(&mut screen, &viewport, BS, &mut false, false);
        execute(&mut screen, &viewport, BS, &mut false, false);
        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_tab_advances_to_next_tab_stop() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);

        screen.cursor.col = 3;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn execute_tab_clamps_at_rightmost_column() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = TEST_COLS - 1;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn execute_bel_sets_bell_pending() {
        let (mut screen, viewport) = setup();
        let mut bell = false;
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, BEL, &mut bell, false);
        assert!(bell);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn execute_nul_is_noop() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, NUL, &mut false, false);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    // -- csi_dispatch cursor movement --------------------------------------

    #[test]
    fn csi_a_moves_cursor_up_by_count() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 3;
        feed(b"\x1b[2A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_defaults_to_one() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_zero_parameter_treated_as_one() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[0A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_saturates_at_top() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        feed(b"\x1b[99A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn csi_b_moves_cursor_down_clamped() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[99B", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
    }

    #[test]
    fn csi_c_moves_cursor_right_clamped() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[99C", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_d_moves_cursor_left_saturating() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 2;
        feed(b"\x1b[5D", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- CNL / CPL -----------------------------------------------------------

    #[test]
    fn csi_e_moves_down_and_homes_column() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 0;
        screen.cursor.col = 5;
        feed(b"\x1b[2E", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_e_clamps_at_bottom() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 3;
        feed(b"\x1b[99E", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_f_moves_up_and_homes_column() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 3;
        screen.cursor.col = 7;
        feed(b"\x1b[2F", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_f_saturates_at_top() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        screen.cursor.col = 5;
        feed(b"\x1b[99F", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_positions_cursor_one_based() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[3;5H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_h_defaults_to_origin() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 5;
        feed(b"\x1b[H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_clamps_to_viewport() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[99;99H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_f_is_alias_of_h() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3f", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_s_saves_and_csi_u_restores_cursor() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s", &mut screen, &mut viewport);
        // Move elsewhere after saving.
        feed(b"\x1b[4;5H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 3);
        assert_eq!(screen.cursor.col, 4);
        feed(b"\x1b[u", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_u_without_prior_save_homes_cursor() {
        // Matches DECRC semantics: no saved slot → cursor homes to 0,0.
        // Live-updating scripts that call `CSI u` on the first paint
        // before any `CSI s` get predictable behaviour instead of a
        // surprise no-op that leaves the cursor mid-screen.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3H", &mut screen, &mut viewport);
        feed(b"\x1b[u", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_s_shares_slot_with_esc_7() {
        // SCOSC and DECSC write the same slot, so an `ESC 8` after a
        // `CSI s` restores the CSI-written position.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s", &mut screen, &mut viewport);
        feed(b"\x1b[4;5H", &mut screen, &mut viewport);
        feed(b"\x1b8", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_u_does_not_trip_kitty_keyboard_path() {
        // The kitty CSI-u path requires an intermediate (`>`, `<`, `=`,
        // `?`). A plain `CSI u` must fall through to SCORC — this test
        // guards against anyone re-ordering the kitty check in front of
        // the SCORC arm.
        let (mut screen, mut viewport) = setup();
        feed(
            b"\x1b[2;3H\x1b[s\x1b[4;5H\x1b[u",
            &mut screen,
            &mut viewport,
        );
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_g_sets_column_only() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[5G", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_d_lowercase_sets_row_only() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1b[3d", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 5);
    }

    // -- csi_dispatch erase / SGR / scroll region --------------------------

    #[test]
    fn csi_j_2_erases_entire_display() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello\nworld", &mut screen, &mut viewport);
        feed(b"\x1b[2J", &mut screen, &mut viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim(), "");
    }

    #[test]
    fn csi_k_erases_to_end_of_line() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello", &mut screen, &mut viewport);
        feed(b"\x1b[3G", &mut screen, &mut viewport); // col=2
        feed(b"\x1b[K", &mut screen, &mut viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim_end(), "he");
    }

    #[test]
    fn csi_m_applies_sgr_colors() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[31m", &mut screen, &mut viewport);
        // SGR 31 = ANSI red fg, which is (205, 0, 0) in the standard palette.
        assert_eq!(screen.fg, Srgb::new(205, 0, 0));
    }

    #[test]
    fn csi_r_sets_scroll_region_and_homes_cursor() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 3;
        screen.cursor.col = 5;
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_r_clamps_bounds_to_viewport() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;99r", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, TEST_ROWS - 1);
    }

    #[test]
    fn csi_with_intermediate_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // Intermediate ` ` before action `q` is a valid CSI shape but not one
        // we handle — we must leave state untouched.
        feed(b"\x1b[1 q", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn csi_unknown_action_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        screen.cursor.col = 1;
        // Use a genuinely unrecognized CSI action (not Z, which is now CBT).
        feed(b"\x1b[1~", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
    }

    // -- esc_dispatch ------------------------------------------------------

    #[test]
    fn esc_m_at_scroll_top_scrolls_down() {
        let (mut screen, mut viewport) = setup();
        feed(b"top\nmid\nbot", &mut screen, &mut viewport);
        // Cursor is at scroll_top (row 0) after moving back there.
        feed(b"\x1b[H", &mut screen, &mut viewport);
        feed(b"\x1bM", &mut screen, &mut viewport);
        // After scroll-down, the old top row shifts down one and row 0 blanks.
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim_end(), "top");
    }

    #[test]
    fn esc_m_above_scroll_top_moves_cursor_up() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1bM", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn esc_m_at_row_zero_outside_region_is_noop() {
        // scroll_top defaults to 0, so row 0 triggers scroll_down_in_region
        // above. Force a non-zero scroll_top to exercise the cursor.row > 0
        // branch at exactly row 0 of the viewport.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;4r", &mut screen, &mut viewport); // scroll_top = 1
        screen.cursor.row = 0;
        feed(b"\x1bM", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn esc_scs_designator_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // ESC ( B designates US-ASCII as G0. Parser should no-op without
        // dropping state or panicking on the `B` byte (which would otherwise
        // land in the unknown-byte arm).
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn esc_keypad_modes_set_app_keypad() {
        let (mut screen, mut viewport) = setup();
        assert!(!screen.app_keypad);
        feed(b"\x1b=", &mut screen, &mut viewport);
        assert!(screen.app_keypad);
        feed(b"\x1b>", &mut screen, &mut viewport);
        assert!(!screen.app_keypad);
        // Cursor must not be affected.
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- REP (CSI Ps b) ---------------------------------------------------

    #[test]
    fn rep_repeats_last_printed_char() {
        let (mut screen, mut viewport) = setup();
        // Print 'A' then repeat it 3 times.
        feed(b"A\x1b[3b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "A");
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn rep_without_prior_char_is_noop() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[3b", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn rep_defaults_to_one_repetition() {
        let (mut screen, mut viewport) = setup();
        feed(b"X\x1b[b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "X");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- DECSTR (CSI ! p) -------------------------------------------------

    #[test]
    fn decstr_resets_attrs_and_colors() {
        let (mut screen, mut viewport) = setup();
        // Set bold + reverse + custom colors.
        feed(b"\x1b[1;7;31;42m", &mut screen, &mut viewport);
        assert!(screen.attrs.contains(CellAttrs::BOLD));
        assert!(screen.attrs.contains(CellAttrs::REVERSE));
        assert_ne!(screen.fg, color::default_fg());
        // Soft reset.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(screen.attrs, CellAttrs::default());
        assert_eq!(screen.fg, color::default_fg());
        assert_eq!(screen.bg, color::default_bg());
    }

    #[test]
    fn decstr_resets_scroll_region() {
        let (mut screen, mut viewport) = setup();
        // Set a restrictive scroll region.
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        // Soft reset should restore full region.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, viewport.rows - 1);
    }

    #[test]
    fn decstr_preserves_screen_contents() {
        let (mut screen, mut viewport) = setup();
        feed(b"Hello", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        let before: Vec<_> = screen.grid.rows[r].cells[..5]
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        let after: Vec<_> = screen.grid.rows[r].cells[..5]
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        assert_eq!(before, after);
    }

    // -- DECRQM (CSI ? Ps $ p) -----------------------------------------------

    /// Like `feed` but returns the `pending_output` bytes written by query
    /// responses (DECRQM, DSR, etc.).
    fn feed_with_output(
        input: &[u8],
        screen: &mut Screen,
        viewport: &mut Viewport,
    ) -> Vec<u8> {
        let base_pal = color::ColorPalette::default();
        let mut dec_color = crate::dec_color::state_from_palette(&base_pal);
        let mut pal = crate::dec_color::effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let foreground_processes: Option<ForegroundProcessSet> = None;
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for action in parser.parse(input) {
            // VT52 ESC Y cursor address state machine (mirrors Terminal::apply).
            if vt52_cursor_addr != crate::Vt52CursorAddr::Idle {
                let byte_opt: Option<u8> = match &action {
                    Action::PrintAscii(run) => run.first().copied(),
                    Action::Execute(b) => Some(*b),
                    _ => None,
                };
                match (vt52_cursor_addr, byte_opt) {
                    (crate::Vt52CursorAddr::AwaitingRow, Some(b)) => {
                        vt52_cursor_addr =
                            crate::Vt52CursorAddr::AwaitingCol(b.saturating_sub(0x20));
                        if let Action::PrintAscii(run) = &action
                            && run.len() >= 2
                        {
                            let row = b.saturating_sub(0x20) as u32;
                            let col = run[1].saturating_sub(0x20) as u32;
                            screen.cursor.row = row.min(viewport.rows.saturating_sub(1));
                            screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                            vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                            if run.len() > 2 {
                                let view = screen::screen_viewport(screen, viewport);
                                put_ascii_run(screen, &view, &run[2..], modes.insert_mode);
                            }
                            continue;
                        }
                        continue;
                    }
                    (crate::Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                        let col = b.saturating_sub(0x20) as u32;
                        screen.cursor.row = (row as u32).min(viewport.rows.saturating_sub(1));
                        screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                        if let Action::PrintAscii(run) = &action
                            && run.len() > 1
                        {
                            let view = screen::screen_viewport(screen, viewport);
                            put_ascii_run(screen, &view, &run[1..], modes.insert_mode);
                        }
                        continue;
                    }
                    _ => {
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                    }
                }
            }
            // In VT52 mode, CSI sequences are invalid and must be dropped.
            if modes.vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
                continue;
            }
            match action {
                Action::PrintAscii(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_ascii_run(screen, &view, run, modes.insert_mode)
                }
                Action::PrintText(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_text_run(screen, &view, run, modes.insert_mode)
                }
                Action::Print(s) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_printable(screen, &view, s, modes.insert_mode)
                }
                Action::Print8Bit(byte) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_8bit_byte(screen, &view, byte, modes.insert_mode)
                }
                Action::Execute(b) => {
                    let view = screen::screen_viewport(screen, viewport);
                    execute(screen, &view, b, &mut bell_pending, modes.newline_mode)
                }
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    let mut ctx = CsiContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        pending_output: &mut pending_output,
                        pending_resize: &mut pending_resize,
                        cursor_style: &mut cursor_style,
                        cell_width: 8,
                        cell_height: 16,
                        palette: &mut pal,
                        base_palette: &base_pal,
                        default_status_display: &mut default_status_display,
                        title_stack: &mut title_stack,
                        current_title: &mut current_title,
                        saved_modes: &mut saved_modes,
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
                        vt52_cursor_addr: &mut vt52_cursor_addr,
                        macros: &mut macros,
                        dec_color: &mut dec_color,
                        feature_permissions: &feature_permissions,
                        foreground_processes: &foreground_processes,
                        drcs: &mut drcs,
                    };
                    csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    let mut ctx = EscContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        cursor_style: &mut cursor_style,
                        current_title: &mut current_title,
                        title_stack: &mut title_stack,
                        saved_modes: &mut saved_modes,
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
                        palette: &mut pal,
                        base_palette: &base_pal,
                        default_status_display: &mut default_status_display,
                        pending_output: &mut pending_output,
                        vt52_cursor_addr: &mut vt52_cursor_addr,
                        macros: &mut macros,
                        dec_color: &mut dec_color,
                        drcs: &mut drcs,
                    };
                    esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
                }
                _ => {}
            }
        }
        pending_output
    }

    #[test]
    fn decrqm_reports_cursor_visible_set() {
        let (mut screen, mut viewport) = setup();
        // Cursor is visible by default.
        let out = feed_with_output(b"\x1b[?25$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?25;1$y");
    }

    #[test]
    fn decrqm_reports_cursor_visible_reset() {
        let (mut screen, mut viewport) = setup();
        screen.cursor_visible = false;
        let out = feed_with_output(b"\x1b[?25$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?25;2$y");
    }

    #[test]
    fn decsnls_resizes_visible_rows_and_activates_page_memory() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[36*|", &mut screen, &mut viewport);
        assert_eq!(viewport.rows, 36);
        assert_eq!(
            screen.page_memory.as_ref().map(|page| page.lines_per_page),
            Some(36)
        );
    }

    #[test]
    fn decslpp_extends_page_length_without_resizing_screen() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[72t", &mut screen, &mut viewport);
        assert_eq!(viewport.rows, TEST_ROWS);
        assert_eq!(
            screen.page_memory.as_ref().map(|page| page.lines_per_page),
            Some(72)
        );
    }

    #[test]
    fn decscpp_resizes_columns() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[132$|", &mut screen, &mut viewport);
        assert_eq!(viewport.cols, 132);
        assert_eq!(screen.right_margin, 131);
    }

    #[test]
    fn decrqpsr_reports_tab_stops() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 3;
        feed(b"\x1bH", &mut screen, &mut viewport);
        let out = feed_with_output(b"\x1b[2$w", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1bP2$u4;9\x1b\\");
    }

    #[test]
    fn np_switches_page_and_homes_cursor() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 5;
        screen.cursor.col = 7;
        feed(b"\x1b[2U", &mut screen, &mut viewport);
        let page = screen.page_memory.as_ref().unwrap();
        assert_eq!(page.active_page, 2);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn deccra_copies_between_pages() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1U\x1b[1;1H", &mut screen, &mut viewport);
        feed(b"Z", &mut screen, &mut viewport);
        feed(b"\x1b[1V", &mut screen, &mut viewport);
        let page1 = screen::page_viewport(&screen, &viewport, 1).unwrap();
        let page2 = screen::page_viewport(&screen, &viewport, 2).unwrap();
        assert_eq!(
            screen.grid.rows[page2.top].cells[0].as_str(),
            "Z",
            "page 2 should receive direct printable writes"
        );
        feed(b"\x1b[1;1;1;1;2;1;1;1$v", &mut screen, &mut viewport);
        assert_eq!(
            screen.grid.rows[page1.top].cells[0].as_str(),
            "Z",
            "page 1 should receive copied cell from page 2"
        );
        assert_eq!(
            screen.grid.rows[page2.top].cells[0].as_str(),
            "Z",
            "source page should remain unchanged"
        );
    }

    #[test]
    fn decsera_skips_protected_cells() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1\"qA\x1b[0\"qB", &mut screen, &mut viewport);
        feed(b"\x1b[1;1;1;2${", &mut screen, &mut viewport);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert_eq!(row.cells[0].as_str(), "A");
        assert_eq!(row.cells[1].as_str(), " ");
    }

    #[test]
    fn deccara_and_decrara_use_vt420_opcodes() {
        let (mut screen, mut viewport) = setup();
        feed(b"X", &mut screen, &mut viewport);
        feed(b"\x1b[1;1;1;1;1$r", &mut screen, &mut viewport);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert!(row.attrs[0].contains(CellAttrs::BOLD));

        feed(b"\x1b[1;1;1;1;1$t", &mut screen, &mut viewport);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert!(!row.attrs[0].contains(CellAttrs::BOLD));
    }

    #[test]
    fn decsace_switches_between_stream_and_rectangle_extent() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;2HA\x1b[3;2HB", &mut screen, &mut viewport);

        feed(b"\x1b[1;2;3;2;1$r", &mut screen, &mut viewport);
        assert!(screen.grid.rows[0].attrs[1].contains(CellAttrs::BOLD));
        assert!(!screen.grid.rows[1].attrs[1].contains(CellAttrs::BOLD));
        assert!(screen.grid.rows[2].attrs[1].contains(CellAttrs::BOLD));

        feed(b"\x1b[2*x\x1b[1;2;3;2;1$r", &mut screen, &mut viewport);
        assert!(screen.grid.rows[1].attrs[1].contains(CellAttrs::BOLD));
    }

    #[test]
    fn decrqm_reports_bracketed_paste() {
        let (mut screen, mut viewport) = setup();
        // Enable bracketed paste first, then query.
        let out = feed_with_output(b"\x1b[?2004h\x1b[?2004$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2004;1$y");
    }

    #[test]
    fn decrqm_unknown_mode_reports_zero() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?9999$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?9999;0$y");
    }

    #[test]
    fn decrqm_ansi_mode_reports_zero_for_unknown() {
        let (mut screen, mut viewport) = setup();
        // Query an unknown ANSI (non-private) mode.
        let out = feed_with_output(b"\x1b[99$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[99;0$y");
    }

    // -- Tab stops -----------------------------------------------------------

    #[test]
    fn default_tab_stops_every_8_columns() {
        // 10-col screen: only column 8 is a stop.
        let (mut screen, viewport) = setup();
        assert_eq!(screen.cursor.col, 0);
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn tab_from_mid_column_goes_to_next_stop() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn tab_at_last_column_stays() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = TEST_COLS - 1;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn hts_sets_custom_tab_stop() {
        let (mut screen, mut viewport) = setup();
        // Move to col 3, set a tab stop with ESC H, then tab from col 0.
        feed(b"\x1b[1;4H\x1bH", &mut screen, &mut viewport);
        assert!(screen.tab_stops[3]);
        screen.cursor.col = 0;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn cht_moves_forward_n_tab_stops() {
        // Use a wider screen so we have at least two default stops.
        let screen_cols = 24;
        let mut screen = Screen::new(
            screen_cols,
            TEST_ROWS,
            100,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut viewport = Viewport {
            rows: TEST_ROWS,
            cols: screen_cols,
            top: 0,
        };
        // Default stops at 8, 16. CSI 2 I from col 0 should jump to 16.
        feed(b"\x1b[2I", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 16);
    }

    #[test]
    fn cbt_moves_backward_n_tab_stops() {
        let screen_cols = 24;
        let mut screen = Screen::new(
            screen_cols,
            TEST_ROWS,
            100,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut viewport = Viewport {
            rows: TEST_ROWS,
            cols: screen_cols,
            top: 0,
        };
        // Park at col 20, then CSI 2 Z (back 2 stops) should land at 8.
        screen.cursor.col = 20;
        feed(b"\x1b[2Z", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn tbc_0_clears_at_cursor() {
        let (mut screen, mut viewport) = setup();
        // Default stop at col 8. Move there and clear it.
        screen.cursor.col = 8;
        feed(b"\x1b[0g", &mut screen, &mut viewport);
        assert!(!screen.tab_stops[8]);
        // Tab from col 0 should now go to the last column.
        screen.cursor.col = 0;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn tbc_3_clears_all_tab_stops() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[3g", &mut screen, &mut viewport);
        assert!(screen.tab_stops.iter().all(|&s| !s));
        // Tab from col 0 should go to last column.
        screen.cursor.col = 0;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    // -- Insert Mode (IRM) ---------------------------------------------------

    #[test]
    fn default_mode_is_replace() {
        let (mut screen, mut viewport) = setup();
        feed(b"abc", &mut screen, &mut viewport);
        // Overwrite at col 0.
        feed(b"\x1b[1;1H", &mut screen, &mut viewport);
        feed(b"X", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "c");
    }

    #[test]
    fn insert_mode_shifts_text_right() {
        let (mut screen, mut viewport) = setup();
        feed(b"abc", &mut screen, &mut viewport);
        // Enable insert mode (CSI 4 h), move to col 0, type 'X'.
        feed(b"\x1b[4h\x1b[1;1HX", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "a");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "c");
    }

    #[test]
    fn insert_mode_disable_returns_to_replace() {
        let (mut screen, mut viewport) = setup();
        feed(b"abc", &mut screen, &mut viewport);
        // Enable insert, then disable it.
        feed(b"\x1b[4h\x1b[4l\x1b[1;1HX", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // Replace mode: 'X' overwrites 'a'.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "c");
    }

    // -- Origin Mode (DECOM) -------------------------------------------------

    #[test]
    fn origin_mode_cup_relative_to_scroll_region() {
        let (mut screen, mut viewport) = setup();
        // Set scroll region to rows 2..3 (1-based).
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        // Enable origin mode.
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // CUP(1,1) should land at top of scroll region (row 1 in 0-based).
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
        // CUP(2,1) should land at row 2 (scroll_bottom).
        feed(b"\x1b[2;1H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn origin_mode_cup_clamps_to_scroll_region() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // CUP(99,1) should clamp to scroll_bottom.
        feed(b"\x1b[99;1H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn origin_mode_disable_returns_to_absolute() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // Disable origin mode — cursor homes to absolute (0,0).
        feed(b"\x1b[?6l", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
        // CUP(1,1) is now absolute row 0.
        feed(b"\x1b[1;1H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn origin_mode_vpa_relative_to_scroll_region() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // VPA(2) should land at scroll_top + 1 = row 2.
        feed(b"\x1b[2d", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn decrqm_reports_origin_mode() {
        let (mut screen, mut viewport) = setup();
        // Default is off.
        let out = feed_with_output(b"\x1b[?6$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?6;2$y");
        // Enable and re-query.
        let out = feed_with_output(b"\x1b[?6h\x1b[?6$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?6;1$y");
    }

    #[test]
    fn decrqm_irm_reports_insert_mode() {
        let (mut screen, mut viewport) = setup();
        // Default is replace (off) → Pm=2.
        let out = feed_with_output(b"\x1b[4$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[4;2$y");
        // Enable and re-query → Pm=1.
        let out = feed_with_output(b"\x1b[4h\x1b[4$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[4;1$y");
    }

    // -- DEC Special Graphics (SCS) ------------------------------------------

    #[test]
    fn scs_g0_drawing_translates_box_chars() {
        let (mut screen, mut viewport) = setup();
        // ESC ( 0 designates DEC drawing into G0, then print box-drawing bytes.
        // 0x6C = ┌, 0x71 = ─, 0x6B = ┐
        feed(b"\x1b(0\x6c\x71\x6b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{2500}"); // ─
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "\u{2510}"); // ┐
    }

    #[test]
    fn scs_g0_ascii_restores_normal() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing, write a box char, then switch back to ASCII.
        feed(b"\x1b(0\x6c\x1b(B\x6c", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "l"); // plain ASCII
    }

    #[test]
    fn scs_drawing_does_not_translate_below_0x60() {
        let (mut screen, mut viewport) = setup();
        // In drawing mode, bytes below 0x60 should pass through as ASCII.
        feed(b"\x1b(0ABC", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "B");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "C");
    }

    #[test]
    fn scs_so_si_switch_between_g0_g1() {
        let (mut screen, mut viewport) = setup();
        // G0 = ASCII (default), G1 = drawing.
        // SO (0x0E) invokes G1, SI (0x0F) invokes G0.
        feed(b"\x1b)0", &mut screen, &mut viewport); // G1 = drawing
        feed(b"\x0E", &mut screen, &mut viewport); // SO → GL = G1
        feed(b"\x6c", &mut screen, &mut viewport); // should translate
        feed(b"\x0F", &mut screen, &mut viewport); // SI → GL = G0
        feed(b"\x6c", &mut screen, &mut viewport); // should be plain ASCII
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "l"); // plain
    }

    #[test]
    fn scs_decstr_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
        // DECSTR should reset charset state.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::Ascii
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G0);
    }

    #[test]
    fn scs_ris_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0\x1b)0\x0E", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::DecSpecialGraphics
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G1);
        // RIS should reset everything.
        feed(b"\x1bc", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::Ascii
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G0);
    }

    #[test]
    fn scs_save_restore_cursor_preserves_charset() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing in G0, save cursor.
        feed(b"\x1b(0\x1b7", &mut screen, &mut viewport);
        // Switch back to ASCII.
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        // Restore cursor — should bring back DEC drawing.
        feed(b"\x1b8", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
    }

    #[test]
    fn scs_technical_charset_translates_math_symbols() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b)>", &mut screen, &mut viewport); // G1 = DEC Technical
        feed(b"\x0Eabc", &mut screen, &mut viewport); // SO -> GL = G1
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{03B2}");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "\u{03C7}");
    }

    #[test]
    fn scs_ls2_maps_g2_into_gl() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.A", &mut screen, &mut viewport); // G2 = ISO Latin-1 supplemental
        feed(b"\x1bn!!", &mut screen, &mut viewport); // LS2 -> GL = G2
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{00A1}");
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G2);
    }

    #[test]
    fn scs_single_shift_uses_g2_for_one_character() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.A\x1bN!!", &mut screen, &mut viewport); // G2 = ISO Latin-1 supplemental
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "!");
    }

    #[test]
    fn scs_ls1r_maps_g1_into_gr_for_utf8_text() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b)>\x1b~", &mut screen, &mut viewport); // G1 = DEC Technical, GR = G1
        feed("á".as_bytes(), &mut screen, &mut viewport); // U+00E1 -> 0x61 in GR
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.charset.gr_slot(), GraphicSetSlot::G1);
    }

    #[test]
    fn scs_ls2r_maps_g2_into_gr_for_utf8_text() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.%5\x1b}", &mut screen, &mut viewport); // G2 = DEC Supplemental, GR = G2
        feed("¨".as_bytes(), &mut screen, &mut viewport); // U+00A8 -> DEC MCS currency sign
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A4}");
        assert_eq!(screen.charset.gr_slot(), GraphicSetSlot::G2);
    }

    #[test]
    fn docs_8bit_mode_routes_raw_high_bytes_through_gr() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b%@\x1b)>\x1b~\xe1A", &mut screen, &mut viewport); // raw 0xE1 -> 0x61 in GR
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "A");
    }

    #[test]
    fn scs_gr_translation_applies_to_split_utf8_codepoint() {
        let (mut screen, mut viewport) = setup();
        let base_pal = color::ColorPalette::default();
        let mut dec_color = crate::dec_color::state_from_palette(&base_pal);
        let mut pal = crate::dec_color::effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let foreground_processes: Option<ForegroundProcessSet> = None;
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for chunk in [b"\x1b)>\x1b~\xc3".as_slice(), b"\xa1".as_slice()] {
            for action in parser.parse(chunk) {
                match action {
                    Action::PrintAscii(run) => {
                        put_ascii_run(&mut screen, &viewport, run, modes.insert_mode)
                    }
                    Action::PrintText(run) => {
                        put_text_run(&mut screen, &viewport, run, modes.insert_mode)
                    }
                    Action::Print(s) => put_printable(&mut screen, &viewport, s, modes.insert_mode),
                    Action::Print8Bit(byte) => {
                        put_8bit_byte(&mut screen, &viewport, byte, modes.insert_mode)
                    }
                    Action::Execute(b) => execute(
                        &mut screen,
                        &viewport,
                        b,
                        &mut bell_pending,
                        modes.newline_mode,
                    ),
                    Action::CsiDispatch {
                        params,
                        intermediates,
                        action,
                    } => {
                        let mut ctx = CsiContext {
                            screen: &mut screen,
                            stash: &mut stash,
                            viewport: &mut viewport,
                            on_alt_screen: &mut on_alt_screen,
                            modes: &mut modes,
                            kitty_keyboard: &mut kitty_keyboard,
                            pending_output: &mut pending_output,
                            pending_resize: &mut pending_resize,
                            cursor_style: &mut cursor_style,
                            cell_width: 8,
                            cell_height: 16,
                            palette: &mut pal,
                            base_palette: &base_pal,
                            default_status_display: &mut default_status_display,
                            title_stack: &mut title_stack,
                            current_title: &mut current_title,
                            saved_modes: &mut saved_modes,
                            current_prompt_row: &mut current_prompt_row,
                            bell_pending: &mut bell_pending,
                            vt52_cursor_addr: &mut vt52_cursor_addr,
                            macros: &mut macros,
                            dec_color: &mut dec_color,
                            feature_permissions: &feature_permissions,
                            foreground_processes: &foreground_processes,
                            drcs: &mut drcs,
                        };
                        csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
                    }
                    Action::EscDispatch {
                        intermediates,
                        byte,
                    } => {
                        let mut ctx = EscContext {
                            screen: &mut screen,
                            stash: &mut stash,
                            viewport: &mut viewport,
                            on_alt_screen: &mut on_alt_screen,
                            modes: &mut modes,
                            kitty_keyboard: &mut kitty_keyboard,
                            cursor_style: &mut cursor_style,
                            current_title: &mut current_title,
                            title_stack: &mut title_stack,
                            saved_modes: &mut saved_modes,
                            current_prompt_row: &mut current_prompt_row,
                            bell_pending: &mut bell_pending,
                            palette: &mut pal,
                            base_palette: &base_pal,
                            default_status_display: &mut default_status_display,
                            pending_output: &mut pending_output,
                            vt52_cursor_addr: &mut vt52_cursor_addr,
                            macros: &mut macros,
                            dec_color: &mut dec_color,
                            drcs: &mut drcs,
                        };
                        esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
                    }
                    _ => {}
                }
            }
        }

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
    }

    #[test]
    fn scs_decnrcm_gates_nrc_translation() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(A#", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "#");

        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?42h\x1b(A#", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A3}");
    }

    #[test]
    fn decrqupss_reports_default_upss() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[&u", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1bP0!u%5\x1b\\");
    }

    #[test]
    fn scs_full_box_top_bottom() {
        // Simulate a typical box-drawing sequence: ┌──┐ on top, └──┘ on bottom.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0", &mut screen, &mut viewport);
        feed(b"\x6c\x71\x71\x6b", &mut screen, &mut viewport); // ┌──┐
        feed(b"\r\n", &mut screen, &mut viewport);
        feed(b"\x6d\x71\x71\x6a", &mut screen, &mut viewport); // └──┘
        let top = row_text(&screen, &viewport, 0);
        assert!(top.starts_with("\u{250C}\u{2500}\u{2500}\u{2510}"));
        let bot = row_text(&screen, &viewport, 1);
        assert!(bot.starts_with("\u{2514}\u{2500}\u{2500}\u{2518}"));
    }

    // -- DECALN (ESC # 8) ---------------------------------------------------

    #[test]
    fn decaln_fills_screen_with_e() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello", &mut screen, &mut viewport);
        feed(b"\x1b#8", &mut screen, &mut viewport);
        let text = row_text(&screen, &viewport, 0);
        assert!(text.chars().all(|c| c == 'E'));
        let text2 = row_text(&screen, &viewport, TEST_ROWS - 1);
        assert!(text2.chars().all(|c| c == 'E'));
    }

    // -- IND (ESC D) and NEL (ESC E) ----------------------------------------

    #[test]
    fn ind_moves_cursor_down() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1bD", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 5); // col preserved
    }

    #[test]
    fn ind_at_scroll_bottom_scrolls_up() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();
        feed(b"\x1bD", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert!(screen.grid.rows.len() > rows_before);
    }

    #[test]
    fn nel_moves_to_col_0_of_next_line() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1bE", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- DECAWM (mode ?7) ---------------------------------------------------

    #[test]
    fn decawm_off_prevents_wrap() {
        let (mut screen, mut viewport) = setup();
        // Disable auto-wrap.
        feed(b"\x1b[?7l", &mut screen, &mut viewport);
        // Write more chars than columns — should stay on last column.
        feed(b"abcdefghijXX", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        // Last column should have the last char written.
        let text = row_text(&screen, &viewport, 0);
        assert_eq!(&text[..TEST_COLS as usize], "abcdefghiX");
    }

    #[test]
    fn decawm_on_wraps_normally() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?7l", &mut screen, &mut viewport);
        feed(b"\x1b[?7h", &mut screen, &mut viewport);
        feed(b"abcdefghijkl", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    // -- LNM (mode 20) ------------------------------------------------------

    #[test]
    fn lnm_enabled_lf_implies_cr() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        // Enable LNM and issue LF in one feed call so the modes object
        // persists across both sequences.
        feed(b"\x1b[20h\n", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0); // CR implied
    }

    // -- pending wrap cancellation -------------------------------------------

    #[test]
    fn cub_from_pending_wrap_lands_on_second_to_last() {
        let (mut screen, mut viewport) = setup();
        // Fill the row to put cursor into pending wrap (col == viewport.cols).
        feed(b"abcdefghij", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS);
        // CUB 1 should cancel pending wrap (→ last col) then move back 1.
        feed(b"\x1b[D", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS - 2);
    }

    #[test]
    fn cuu_from_pending_wrap_cancels_wrap() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        feed(b"abcdefghij", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS);
        // CUU 1 should move up without wrapping and cancel the pending
        // wrap column to the last column.
        feed(b"\x1b[A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn ed_from_pending_wrap_erases_last_column() {
        let (mut screen, mut viewport) = setup();
        feed(b"abcdefghij", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS);
        // ED 0 (erase to end) should erase the last column, not be a no-op.
        feed(b"\x1b[J", &mut screen, &mut viewport);
        let text = row_text(&screen, &viewport, 0);
        assert_eq!(&text[..TEST_COLS as usize], "abcdefghi ");
    }

    // -- VT52 mode -----------------------------------------------------------
    //
    // Each test uses a single feed() / feed_with_output() call so that mode
    // changes set by `CSI ? 2 l` remain active for the sequences that follow.
    // Separate calls create fresh TerminalModes, so VT52 state would not
    // persist across call boundaries.

    /// DECRQM reports DECANM as set (ANSI) by default.
    #[test]
    fn decrqm_reports_decanm_set_in_ansi_mode() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// DECRQM after entering and immediately exiting VT52 mode (via ESC <)
    /// reports DECANM as set again.
    #[test]
    fn decrqm_reports_decanm_restored_after_exit() {
        let (mut screen, mut viewport) = setup();
        // Enter VT52 then exit with ESC < — DECRQM should see ANSI mode.
        let out = feed_with_output(b"\x1b[?2l\x1b<\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// Enter VT52 then exit via `ESC <`; DECRQM should see ANSI mode restored.
    #[test]
    fn vt52_enter_and_exit_via_esc_lt() {
        let (mut screen, mut viewport) = setup();
        // `CSI ? 2 l` → VT52; `ESC <` → back to ANSI; DECRQM → set.
        let out = feed_with_output(b"\x1b[?2l\x1b<\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// VT52 ESC A/B/C/D cursor movement.
    #[test]
    fn vt52_cursor_up() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 2, col 3 (1-based: 3;4), then VT52 ESC A.
        feed(b"\x1b[3;4H\x1b[?2l\x1bA", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (1, 3));
    }

    #[test]
    fn vt52_cursor_down() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 1, col 0, then VT52 ESC B.
        feed(b"\x1b[2;1H\x1b[?2l\x1bB", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 0));
    }

    #[test]
    fn vt52_cursor_right() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;3H\x1b[?2l\x1bC", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    }

    #[test]
    fn vt52_cursor_left() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;5H\x1b[?2l\x1bD", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    }

    /// VT52 cursor up at row 0 does not underflow.
    #[test]
    fn vt52_cursor_up_clamps_at_top() {
        let (mut screen, mut viewport) = setup();
        // Already at row 0 (home). VT52 mode, ESC A.
        feed(b"\x1b[?2l\x1bA", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    /// VT52 ESC H homes the cursor.
    #[test]
    fn vt52_cursor_home() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 3, col 6 (1-based), then VT52 ESC H.
        feed(b"\x1b[3;6H\x1b[?2l\x1bH", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 0));
    }

    /// VT52 ESC Y <row+0x20> <col+0x20> direct cursor address — bytes split.
    #[test]
    fn vt52_direct_cursor_address() {
        let (mut screen, mut viewport) = setup();
        // Enter VT52 then ESC Y: row 2 ('"'=0x22), col 4 ('$'=0x24).
        feed(b"\x1b[?2l\x1bY\"$", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 4));
    }

    /// VT52 ESC Y where both position bytes arrive in the same PrintAscii run.
    #[test]
    fn vt52_direct_cursor_address_batched() {
        let (mut screen, mut viewport) = setup();
        // Row 1 ('!'=0x21), col 3 ('#'=0x23).
        feed(b"\x1b[?2l\x1bY!#", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (1, 3));
    }

    /// Text after ESC Y position bytes is printed normally.
    #[test]
    fn vt52_direct_cursor_address_then_text() {
        let (mut screen, mut viewport) = setup();
        // Row 0, col 0 (both 0x20 = space), then 'A'.
        feed(b"\x1b[?2l\x1bY  A", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 1));
        assert_eq!(&row_text(&screen, &viewport, 0)[..1], "A");
    }

    /// VT52 ESC J erases from cursor to end of screen (same as ED 0).
    #[test]
    fn vt52_erase_to_end_of_screen() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0 with 'a', row 1 with 'b', then enter VT52 at row 0
        // col 5 (via CUP before VT52 entry) and erase.
        feed(
            b"aaaaaaaaaa\r\nbbbbbbbbbb\x1b[1;6H\x1b[?2l\x1bJ",
            &mut screen,
            &mut viewport,
        );
        let r0 = row_text(&screen, &viewport, 0);
        let r1 = row_text(&screen, &viewport, 1);
        assert_eq!(&r0[..5], "aaaaa", "text before cursor preserved");
        assert_eq!(r0[5..].trim(), "", "text from cursor erased");
        assert_eq!(r1.trim(), "", "row 1 cleared");
    }

    /// VT52 ESC K erases from cursor to end of line (same as EL 0).
    #[test]
    fn vt52_erase_to_end_of_line() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0, position at col 3, enter VT52, erase to EOL.
        feed(
            b"aaaaaaaaaa\x1b[1;4H\x1b[?2l\x1bK",
            &mut screen,
            &mut viewport,
        );
        let r0 = row_text(&screen, &viewport, 0);
        assert_eq!(&r0[..3], "aaa");
        assert_eq!(r0[3..].trim(), "");
    }

    /// VT52 ESC F/G toggle DEC Special Graphics on G0 within one parse pass.
    #[test]
    fn vt52_graphics_mode_on() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?2l\x1bF", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
    }

    #[test]
    fn vt52_graphics_mode_off() {
        let (mut screen, mut viewport) = setup();
        // Enable then disable in the same parse pass.
        feed(b"\x1b[?2l\x1bF\x1bG", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
    }

    /// VT52 ESC Z identify returns ESC / Z.
    #[test]
    fn vt52_identify() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?2l\x1bZ", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b/Z");
    }

    /// CSI sequences are silently dropped in VT52 mode.
    #[test]
    fn vt52_csi_suppressed() {
        let (mut screen, mut viewport) = setup();
        // Position cursor at col 5 (1-based col 6), enter VT52, send CSI CUB.
        feed(b"\x1b[1;6H\x1b[?2l\x1b[3D", &mut screen, &mut viewport);
        // CSI cursor-back should have been dropped.
        assert_eq!(screen.cursor.col, 5, "cursor should not move in VT52 mode");
    }

    /// VT52 reverse index (ESC I) scrolls down at the top of the scroll region.
    #[test]
    fn vt52_reverse_index_scrolls() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0 with text, CUP to row 0, enter VT52, reverse index.
        feed(
            b"line0\r\nline1\r\nline2\x1b[1;1H\x1b[?2l\x1bI",
            &mut screen,
            &mut viewport,
        );
        // Row 0 should now be blank (scrolled down).
        let r0 = row_text(&screen, &viewport, 0);
        assert_eq!(r0.trim(), "");
    }
}
