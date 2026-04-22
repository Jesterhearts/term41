use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::Row;
use crate::Screen;
use crate::Viewport;
use crate::charset;
use crate::parser::ascii_cell;
use crate::parser::current_row_display_cols;
use crate::screen;
use crate::screen::StatusLine;
use crate::screen::grid;

#[derive(Clone, Copy)]
enum WriteTarget {
    Main {
        preserve_top_origin_scrollback: bool,
    },
    Status,
}

#[derive(Clone, Copy)]
struct CellStyle {
    fg: palette::Srgb<u8>,
    bg: palette::Srgb<u8>,
    attrs: CellAttrs,
    underline: UnderlineStyle,
    underline_color: Option<palette::Srgb<u8>>,
    link: Option<screen::hyperlink::HyperlinkId>,
}

pub(super) fn status_line_mut(screen: &mut Screen) -> Option<&mut StatusLine> {
    screen::status_line_writable(screen)
        .then_some(screen.status_line.as_mut())
        .flatten()
}

pub(super) fn status_shift_chars(
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

pub(super) fn status_delete_chars(
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

pub(super) fn status_erase_chars(
    status: &mut StatusLine,
    count: usize,
) {
    let cols = status.row.cells.len();
    let col = status.cursor.col as usize;
    let end = (col + count).min(cols);
    status.row.clear_range(col..end, status.fg, status.bg);
}

#[cfg(test)]
pub(crate) fn put_ascii_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &[u8],
    insert_mode: bool,
) {
    put_ascii_run_with_scrollback_policy(screen, viewport, run, insert_mode, true);
}

#[inline(always)]
pub(crate) fn put_ascii_run_with_scrollback_policy(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &[u8],
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
) {
    put_ascii_run_impl(
        screen,
        viewport,
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        },
        run,
        insert_mode,
    );
}

pub(crate) fn put_status_ascii_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &[u8],
    insert_mode: bool,
) {
    put_ascii_run_impl(screen, viewport, WriteTarget::Status, run, insert_mode);
}

#[cfg(test)]
pub(crate) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    put_char_with_scrollback_policy(screen, viewport, s, insert_mode, true);
}

#[inline(always)]
pub(crate) fn put_char_with_scrollback_policy(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
) {
    put_char_with_scrollback_policy_and_emoji_compat(
        screen,
        viewport,
        s,
        insert_mode,
        preserve_top_origin_scrollback,
        false,
    );
}

#[inline(always)]
fn put_char_with_scrollback_policy_and_emoji_compat(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
    legacy_emoji_compatibility: bool,
) {
    put_char_impl(
        screen,
        viewport,
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        },
        s,
        insert_mode,
        legacy_emoji_compatibility,
    );
}

pub(super) fn put_status_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    put_char_impl(screen, viewport, WriteTarget::Status, s, insert_mode, false);
}

pub(crate) fn translated_codepoint(
    screen: &mut Screen,
    ch: char,
) -> Option<SmolStr> {
    let cp = ch as u32;
    if (0x20..=0x7E).contains(&cp) {
        if let Some(charset) = screen.charset.take_single_shift_charset() {
            let b = cp as u8;
            return Some(
                charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
                    .unwrap_or_else(|| ascii_cell(b)),
            );
        }
        if charset::gl_charset_requires_translation(&screen.charset, screen.nrc_mode) {
            let charset = screen.charset.gl_charset();
            let b = cp as u8;
            return Some(
                charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
                    .unwrap_or_else(|| ascii_cell(b)),
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

#[cfg(test)]
pub(crate) fn put_printable(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    put_printable_with_scrollback_policy(screen, viewport, s, insert_mode, true);
}

#[inline(always)]
#[cfg(test)]
pub(crate) fn put_printable_with_scrollback_policy(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
) {
    put_printable_with_scrollback_policy_and_emoji_compat(
        screen,
        viewport,
        s,
        insert_mode,
        preserve_top_origin_scrollback,
        false,
    );
}

#[inline(always)]
pub(crate) fn put_printable_with_scrollback_policy_and_emoji_compat(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
    legacy_emoji_compatibility: bool,
) {
    put_printable_impl(
        screen,
        viewport,
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        },
        s,
        insert_mode,
        legacy_emoji_compatibility,
    );
}

pub(crate) fn put_status_printable(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    put_printable_impl(screen, viewport, WriteTarget::Status, s, insert_mode, false);
}

#[cfg(test)]
pub(crate) fn put_8bit_byte(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    insert_mode: bool,
) {
    put_8bit_byte_with_scrollback_policy(screen, viewport, byte, insert_mode, true);
}

#[inline(always)]
pub(crate) fn put_8bit_byte_with_scrollback_policy(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
) {
    put_8bit_byte_impl(
        screen,
        viewport,
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        },
        byte,
        insert_mode,
    );
}

pub(crate) fn put_status_8bit_byte(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    insert_mode: bool,
) {
    put_8bit_byte_impl(screen, viewport, WriteTarget::Status, byte, insert_mode);
}

#[cfg(test)]
pub(crate) fn put_text_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
) {
    put_text_run_with_scrollback_policy(screen, viewport, run, insert_mode, true);
}

#[inline(always)]
#[cfg(test)]
pub(crate) fn put_text_run_with_scrollback_policy(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
) {
    put_text_run_with_scrollback_policy_and_emoji_compat(
        screen,
        viewport,
        run,
        insert_mode,
        preserve_top_origin_scrollback,
        false,
    );
}

#[inline(always)]
pub(crate) fn put_text_run_with_scrollback_policy_and_emoji_compat(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
    preserve_top_origin_scrollback: bool,
    legacy_emoji_compatibility: bool,
) {
    put_text_run_impl(
        screen,
        viewport,
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        },
        run,
        insert_mode,
        legacy_emoji_compatibility,
    );
}

pub(crate) fn put_status_text_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
) {
    put_text_run_impl(
        screen,
        viewport,
        WriteTarget::Status,
        run,
        insert_mode,
        false,
    );
}

#[inline(always)]
fn put_ascii_run_impl(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    run: &[u8],
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }
    debug_assert_printable_ascii_run(run);

    let run = if let Some(charset) = screen.charset.take_single_shift_charset() {
        let b = run[0];
        let ch = charset::translate_ascii_byte(b, charset, screen.nrc_mode, screen.upss)
            .unwrap_or_else(|| ascii_cell(b));
        put_char_impl(screen, viewport, target, ch, insert_mode, false);
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
                .unwrap_or_else(|| ascii_cell(b));
            put_char_impl(screen, viewport, target, ch, insert_mode, false);
        }
        return;
    }

    put_ascii_run_fast(screen, viewport, target, run, insert_mode);
}

#[inline(always)]
fn put_char_impl(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    s: SmolStr,
    insert_mode: bool,
    legacy_emoji_compatibility: bool,
) {
    if legacy_emoji_compatibility && is_legacy_emoji_zero_width_control(&s) {
        screen.charset.single_shift = None;
        return;
    }

    if let Some(combined) =
        try_extend_prev_cell(screen, viewport, target, &s, legacy_emoji_compatibility)
    {
        debug!("Combining with previous cell to form {:?}", combined);
        set_target_last_char(screen, target, combined);
        return;
    }

    debug!("Not combining with previous cell");
    screen.charset.single_shift = None;

    let width_override = legacy_emoji_scalar_width_override(&s);
    put_char_to_target(screen, viewport, target, s, insert_mode, width_override);
}

#[inline(always)]
fn put_printable_impl(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    s: SmolStr,
    insert_mode: bool,
    legacy_emoji_compatibility: bool,
) {
    let mut chars = s.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
        && let Some(mapped) = translated_codepoint(screen, ch)
    {
        put_char_impl(
            screen,
            viewport,
            target,
            mapped,
            insert_mode,
            legacy_emoji_compatibility,
        );
        return;
    }

    put_char_impl(
        screen,
        viewport,
        target,
        s,
        insert_mode,
        legacy_emoji_compatibility,
    );
}

#[inline(always)]
fn put_8bit_byte_impl(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
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
    put_char_impl(screen, viewport, target, ch, insert_mode, false);
}

#[inline(always)]
fn put_text_run_impl(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    run: &str,
    insert_mode: bool,
    legacy_emoji_compatibility: bool,
) {
    if run.is_empty() {
        return;
    }

    let mut chars = run.chars();
    let run = if screen.charset.single_shift.take().is_some() {
        let ch = chars.next().unwrap();
        put_char_impl(
            screen,
            viewport,
            target,
            SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4])),
            insert_mode,
            legacy_emoji_compatibility,
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
            put_printable_impl(
                screen,
                viewport,
                target,
                SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4])),
                insert_mode,
                legacy_emoji_compatibility,
            );
        }
        return;
    }

    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] <= 0x7E {
            i += 1;
        }
        if i > start {
            put_ascii_run_fast(screen, viewport, target, &bytes[start..i], insert_mode);
        }
        if i >= bytes.len() {
            break;
        }
        let len = utf8_char_len(bytes[i]);
        put_char_impl(
            screen,
            viewport,
            target,
            SmolStr::new_inline(&run[i..i + len]),
            insert_mode,
            legacy_emoji_compatibility,
        );
        i += len;
    }
}

#[inline(always)]
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[inline(always)]
fn put_ascii_run_fast(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    run: &[u8],
    insert_mode: bool,
) {
    debug_assert_printable_ascii_run(run);
    let Some(style) = target_style(screen, target) else {
        return;
    };
    let last_byte = *run.last().unwrap();
    set_target_last_char(screen, target, ascii_cell(last_byte));

    let mut i = 0;
    while i < run.len() {
        if !prepare_ascii_cursor(screen, viewport, target) {
            return;
        }

        let cols = target_cols(screen, viewport, target).unwrap() as usize;
        let col = target_cursor_col(screen, target).unwrap() as usize;
        let remaining_cols = cols - col;
        let chunk_len = (run.len() - i).min(remaining_cols);

        if insert_mode {
            target_shift_chars(screen, viewport, target, chunk_len);
        }

        write_ascii_chunk(
            target_row_mut(screen, viewport, target).unwrap(),
            col,
            &run[i..i + chunk_len],
            style,
        );
        advance_target_cursor(screen, target, chunk_len as u32);
        i += chunk_len;
    }
}

#[inline(always)]
fn put_char_to_target(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    s: SmolStr,
    insert_mode: bool,
    width_override: Option<usize>,
) {
    let width = width_override.unwrap_or_else(|| s.width());
    if width == 0 {
        screen.charset.single_shift = None;
        return;
    }
    if !fit_char_to_target(screen, viewport, target, width) {
        return;
    }

    let style = target_style(screen, target).unwrap();
    if insert_mode {
        target_shift_chars(screen, viewport, target, width);
    }

    let col = target_cursor_col(screen, target).unwrap() as usize;
    write_styled_glyph(
        target_row_mut(screen, viewport, target).unwrap(),
        col,
        width,
        style,
        &s,
    );
    set_target_last_char(screen, target, s);
    advance_target_cursor(screen, target, width as u32);
}

#[inline(always)]
fn write_ascii_chunk(
    row: &mut Row,
    col: usize,
    chunk: &[u8],
    style: CellStyle,
) {
    let end = col + chunk.len();

    let mut idx = col;
    while idx < end {
        if row.cells[idx].is_empty() {
            row.cells[idx - 1] = SmolStr::new_inline(" ");
        } else if row.cells[idx].width() > 1 {
            for i in idx..idx + row.cells[idx].width() {
                row.cells[i] = SmolStr::new_inline(" ");
            }
        }
        idx += 1;
    }

    for (cell, &b) in row.cells[col..col + chunk.len()].iter_mut().zip(chunk) {
        *cell = ascii_cell(b);
    }
    fill_row_style(row, col, chunk.len(), style);
}

#[inline(always)]
fn debug_assert_printable_ascii_run(run: &[u8]) {
    debug_assert!(
        run.iter().all(|&b| (0x20..=0x7E).contains(&b)),
        "put_ascii_run_fast requires printable ASCII bytes"
    );
}

#[inline(always)]
fn write_styled_glyph(
    row: &mut Row,
    col: usize,
    width: usize,
    style: CellStyle,
    s: &SmolStr,
) {
    let old_w = row.cells[col].width();
    let jbc_offset = if col > 0 {
        usize::from(row.cells[col - 1].width() > 1)
    } else {
        0
    };
    let mut idx = col - jbc_offset;
    while idx < col + old_w {
        row.cells[idx] = SmolStr::new_inline(" ");
        idx += 1;
    }

    row.cells[col] = s.clone();

    let mut idx = col + 1;
    while idx < col + width {
        if row.cells[idx].width() > 1 {
            for i in idx..idx + row.cells[idx].width() {
                row.cells[i] = SmolStr::new_inline(" ");
            }
        }
        row.cells[idx] = SmolStr::new_inline("");
        idx += 1;
    }

    fill_row_style(row, col, width, style);
}

#[inline(always)]
fn fill_row_style(
    row: &mut Row,
    col: usize,
    width: usize,
    style: CellStyle,
) {
    row.fg[col..col + width].fill(style.fg);
    row.bg[col..col + width].fill(style.bg);
    row.attrs[col..col + width].fill(style.attrs);
    row.underline[col..col + width].fill(style.underline);
    row.underline_color[col..col + width].fill(style.underline_color);
    row.links[col..col + width].fill(style.link);
}

#[inline(always)]
fn screen_style(screen: &Screen) -> CellStyle {
    CellStyle {
        fg: screen.fg,
        bg: screen.bg,
        attrs: screen.attrs,
        underline: screen.underline,
        underline_color: screen.underline_color,
        link: screen.current_hyperlink,
    }
}

#[inline(always)]
fn status_style(status: &StatusLine) -> CellStyle {
    CellStyle {
        fg: status.fg,
        bg: status.bg,
        attrs: status.attrs,
        underline: status.underline,
        underline_color: status.underline_color,
        link: status.current_hyperlink,
    }
}

#[inline(always)]
fn status_line_ref(screen: &Screen) -> Option<&StatusLine> {
    screen::status_line_writable(screen)
        .then_some(screen.status_line.as_ref())
        .flatten()
}

#[inline(always)]
fn target_style(
    screen: &Screen,
    target: WriteTarget,
) -> Option<CellStyle> {
    match target {
        WriteTarget::Main { .. } => Some(screen_style(screen)),
        WriteTarget::Status => status_line_ref(screen).map(status_style),
    }
}

#[inline(always)]
fn target_cols(
    screen: &Screen,
    viewport: &Viewport,
    target: WriteTarget,
) -> Option<u32> {
    match target {
        WriteTarget::Main { .. } => Some(current_row_display_cols(screen, viewport)),
        WriteTarget::Status => status_line_ref(screen).map(|status| status.row.len().max(1)),
    }
}

#[inline(always)]
fn target_cursor_col(
    screen: &Screen,
    target: WriteTarget,
) -> Option<u32> {
    match target {
        WriteTarget::Main { .. } => Some(screen.cursor.col),
        WriteTarget::Status => status_line_ref(screen).map(|status| status.cursor.col),
    }
}

#[inline(always)]
fn set_target_last_char(
    screen: &mut Screen,
    target: WriteTarget,
    last: SmolStr,
) {
    match target {
        WriteTarget::Main { .. } => screen.last_char = Some(last),
        WriteTarget::Status => {
            if let Some(status) = status_line_mut(screen) {
                status.last_char = Some(last);
            }
        }
    }
}

#[inline(always)]
fn prepare_ascii_cursor(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
) -> bool {
    match target {
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        } => {
            let cols = current_row_display_cols(screen, viewport);
            if screen.cursor.col >= cols {
                if screen.autowrap {
                    soft_wrap(screen, viewport, preserve_top_origin_scrollback);
                } else {
                    screen.cursor.col = cols - 1;
                }
            }
            true
        }
        WriteTarget::Status => {
            let Some(status) = status_line_mut(screen) else {
                return false;
            };
            let cols = status.row.len().max(1);
            if status.cursor.col >= cols {
                status.cursor.col = cols - 1;
            }
            true
        }
    }
}

#[inline(always)]
fn fit_char_to_target(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    width: usize,
) -> bool {
    match target {
        WriteTarget::Main {
            preserve_top_origin_scrollback,
        } => {
            let cols = current_row_display_cols(screen, viewport);
            if screen.cursor.col + width as u32 > cols {
                if screen.autowrap {
                    soft_wrap(screen, viewport, preserve_top_origin_scrollback);
                } else {
                    screen.cursor.col = cols.saturating_sub(width as u32);
                }
            }
            true
        }
        WriteTarget::Status => {
            let Some(status) = status_line_mut(screen) else {
                return false;
            };
            let cols = status.row.len().max(1);
            if status.cursor.col + width as u32 > cols {
                status.cursor.col = cols.saturating_sub(width as u32);
            }
            true
        }
    }
}

#[inline(always)]
fn target_shift_chars(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    count: usize,
) {
    match target {
        WriteTarget::Main { .. } => {
            grid::shift_chars(&mut screen.grid, &mut screen.cursor, viewport, count as u16);
        }
        WriteTarget::Status => {
            if let Some(status) = status_line_mut(screen) {
                status_shift_chars(status, count);
            }
        }
    }
}

#[inline(always)]
fn target_row_mut<'a>(
    screen: &'a mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
) -> Option<&'a mut Row> {
    match target {
        WriteTarget::Main { .. } => {
            let row = screen::active_row_index(screen, viewport);
            Some(&mut screen.grid.rows[row])
        }
        WriteTarget::Status => status_line_mut(screen).map(|status| &mut status.row),
    }
}

#[inline(always)]
fn advance_target_cursor(
    screen: &mut Screen,
    target: WriteTarget,
    num_graphemes: u32,
) {
    match target {
        WriteTarget::Main { .. } => {
            screen.cursor.col += num_graphemes;
        }
        WriteTarget::Status => {
            if let Some(status) = status_line_mut(screen) {
                status.cursor.col += num_graphemes;
            }
        }
    }
}

#[inline(always)]
fn try_extend_prev_cell(
    screen: &mut Screen,
    viewport: &Viewport,
    target: WriteTarget,
    s: &str,
    legacy_emoji_compatibility: bool,
) -> Option<SmolStr> {
    match target {
        WriteTarget::Main { .. } => {
            try_extend_prev_main_cell(screen, viewport, s, legacy_emoji_compatibility)
        }
        WriteTarget::Status => try_extend_prev_status_cell(screen, s, legacy_emoji_compatibility),
    }
}

fn try_extend_prev_main_cell(
    screen: &mut Screen,
    viewport: &Viewport,
    s: &str,
    legacy_emoji_compatibility: bool,
) -> Option<SmolStr> {
    let (prev_row, mut prev_col) = if screen.cursor.col > 0 && screen.cursor.col <= viewport.cols {
        let row = screen::active_row_index(screen, viewport);
        (row, (screen.cursor.col - 1) as usize)
    } else if screen.cursor.col == 0 {
        let row = screen::active_row_index(screen, viewport);
        if row == 0 || !screen.grid.rows[row].wrapped {
            return None;
        }
        let prev_row = row - 1;
        let last_col = screen.grid.rows[prev_row].cells.len().saturating_sub(1);
        (prev_row, last_col)
    } else {
        return None;
    };

    while prev_col > 0 && screen.grid.rows[prev_row].cells[prev_col].is_empty() {
        prev_col -= 1;
    }

    let row = &mut screen.grid.rows[prev_row];
    debug!(
        "Trying to extend previous cell at row {}, col {} with {:?} in {:?}",
        prev_row, prev_col, s, row.cells
    );
    try_extend_row_cell(row, prev_col, s, legacy_emoji_compatibility)
}

fn try_extend_prev_status_cell(
    screen: &mut Screen,
    s: &str,
    legacy_emoji_compatibility: bool,
) -> Option<SmolStr> {
    let status = status_line_mut(screen)?;
    let prev_col = status.cursor.col as usize;
    if prev_col == 0 {
        return None;
    }

    try_extend_row_cell(&mut status.row, prev_col - 1, s, legacy_emoji_compatibility)
}

fn try_extend_row_cell(
    row: &mut Row,
    col: usize,
    s: &str,
    legacy_emoji_compatibility: bool,
) -> Option<SmolStr> {
    let prev = row.cells.get(col)?;
    if prev.as_str() == " " || prev.is_empty() {
        debug!(
            "Previous cell is {}, cannot combine",
            if prev.is_empty() { "empty" } else { "a space" }
        );
        return None;
    }

    let mut combined = SmolStrBuilder::new();
    combined.push_str(prev);
    combined.push_str(s);
    let combined = combined.finish();

    if combined.graphemes(true).count() > 1 {
        debug!(
            "Combined string {:?} is not a single grapheme, cannot combine",
            combined
        );
        return None;
    }

    if legacy_emoji_compatibility && is_legacy_emoji_cluster(&combined) {
        debug!("Legacy emoji compatibility blocks combining {:?}", combined);
        return None;
    }

    row.cells[col] = combined.clone();
    Some(combined)
}

fn is_legacy_emoji_cluster(s: &str) -> bool {
    s.chars().any(|ch| {
        is_zero_width_emoji_control(ch)
            || is_emoji_modifier(ch)
            || is_emoji_modifier_base(ch)
            || is_emoji_presentation_scalar(ch)
    })
}

fn is_legacy_emoji_zero_width_control(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    chars.next().is_none() && is_zero_width_emoji_control(ch)
}

fn legacy_emoji_scalar_width_override(s: &str) -> Option<usize> {
    let mut chars = s.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    is_emoji_modifier(ch).then_some(2)
}

fn is_zero_width_emoji_control(ch: char) -> bool {
    matches!(ch, '\u{200D}' | '\u{FE0E}' | '\u{FE0F}')
}

fn is_emoji_modifier(ch: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&ch)
}

fn is_emoji_modifier_base(ch: char) -> bool {
    matches!(
        ch,
        '\u{261D}'
            | '\u{26F9}'
            | '\u{270A}'..='\u{270D}'
            | '\u{1F385}'
            | '\u{1F3C2}'..='\u{1F3C4}'
            | '\u{1F3C7}'
            | '\u{1F3CA}'..='\u{1F3CC}'
            | '\u{1F442}'..='\u{1F443}'
            | '\u{1F446}'..='\u{1F450}'
            | '\u{1F466}'..='\u{1F469}'
            | '\u{1F46E}'
            | '\u{1F470}'..='\u{1F478}'
            | '\u{1F47C}'
            | '\u{1F481}'..='\u{1F483}'
            | '\u{1F485}'..='\u{1F487}'
            | '\u{1F48F}'
            | '\u{1F491}'
            | '\u{1F4AA}'
            | '\u{1F574}'..='\u{1F575}'
            | '\u{1F57A}'
            | '\u{1F590}'
            | '\u{1F595}'..='\u{1F596}'
            | '\u{1F645}'..='\u{1F647}'
            | '\u{1F64B}'..='\u{1F64F}'
            | '\u{1F6A3}'
            | '\u{1F6B4}'..='\u{1F6B6}'
            | '\u{1F6C0}'
            | '\u{1F6CC}'
            | '\u{1F918}'..='\u{1F91F}'
            | '\u{1F926}'
            | '\u{1F930}'..='\u{1F939}'
            | '\u{1F93D}'..='\u{1F93E}'
            | '\u{1F9B5}'..='\u{1F9B6}'
            | '\u{1F9B8}'..='\u{1F9B9}'
            | '\u{1F9CD}'..='\u{1F9CF}'
            | '\u{1FAF0}'..='\u{1FAF8}'
    )
}

fn is_emoji_presentation_scalar(ch: char) -> bool {
    matches!(
        ch,
        '\u{1F300}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}' | '\u{2300}'..='\u{23FF}'
    )
}

fn soft_wrap(
    screen: &mut Screen,
    viewport: &Viewport,
    preserve_top_origin_scrollback: bool,
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
            grid::scroll_up_in_region_with_scrollback_policy(
                &mut screen.grid,
                viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
                preserve_top_origin_scrollback,
            );
        }
    } else if screen.cursor.row < viewport.rows - 1 {
        screen.cursor.row += 1;
    }
}

#[cfg(test)]
mod tests {
    use palette::Srgb;

    use super::super::test_support::*;
    use super::super::*;
    use super::*;

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
    fn put_char_zwj_emoji_merges_into_previous_wide_cell() {
        let (mut screen, mut viewport) = setup();
        // 👩‍💻 = 👩 ZWJ 💻. Once the ZWJ has folded into the previous cell,
        // the following emoji should also extend that same grapheme cluster
        // instead of starting a fresh wide glyph cell of its own.
        feed("👩\u{200D}💻".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👩\u{200D}💻");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), " ");
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn put_char_write_after_zwj_emoji_preserves_full_cluster() {
        let (mut screen, mut viewport) = setup();
        feed("👩\u{200D}💻".as_bytes(), &mut screen, &mut viewport);
        feed(b"X", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👩\u{200D}💻");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "X");
        assert_eq!(screen.cursor.col, 3);
    }
}
