use smol_str::SmolStrBuilder;
use unicode_width::UnicodeWidthStr;

use super::*;

#[derive(Clone, Copy)]
enum WriteTarget<'a> {
    Main(&'a Viewport),
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

pub(super) fn status_insert_chars(
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

pub(crate) fn put_ascii_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &[u8],
    insert_mode: bool,
) {
    put_ascii_run_impl(screen, WriteTarget::Main(viewport), run, insert_mode);
}

pub(crate) fn put_status_ascii_run(
    screen: &mut Screen,
    run: &[u8],
    insert_mode: bool,
) {
    put_ascii_run_impl(screen, WriteTarget::Status, run, insert_mode);
}

pub(crate) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    put_char_impl(screen, WriteTarget::Main(viewport), s, insert_mode);
}

pub(super) fn put_status_char(
    screen: &mut Screen,
    s: SmolStr,
    insert_mode: bool,
) {
    put_char_impl(screen, WriteTarget::Status, s, insert_mode);
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

pub(crate) fn put_printable(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    put_printable_impl(screen, WriteTarget::Main(viewport), s, insert_mode);
}

pub(crate) fn put_status_printable(
    screen: &mut Screen,
    s: SmolStr,
    insert_mode: bool,
) {
    put_printable_impl(screen, WriteTarget::Status, s, insert_mode);
}

pub(crate) fn put_8bit_byte(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    insert_mode: bool,
) {
    put_8bit_byte_impl(screen, WriteTarget::Main(viewport), byte, insert_mode);
}

pub(crate) fn put_status_8bit_byte(
    screen: &mut Screen,
    byte: u8,
    insert_mode: bool,
) {
    put_8bit_byte_impl(screen, WriteTarget::Status, byte, insert_mode);
}

pub(crate) fn put_text_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
) {
    put_text_run_impl(screen, WriteTarget::Main(viewport), run, insert_mode);
}

pub(crate) fn put_status_text_run(
    screen: &mut Screen,
    run: &str,
    insert_mode: bool,
) {
    put_text_run_impl(screen, WriteTarget::Status, run, insert_mode);
}

fn put_ascii_run_impl(
    screen: &mut Screen,
    target: WriteTarget<'_>,
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
        put_char_impl(screen, target, ch, insert_mode);
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
            put_char_impl(screen, target, ch, insert_mode);
        }
        return;
    }

    put_ascii_run_fast(screen, target, run, insert_mode);
}

fn put_char_impl(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    s: SmolStr,
    insert_mode: bool,
) {
    let raw_width = UnicodeWidthStr::width(s.as_str());
    if raw_width == 0 {
        try_extend_prev_cell(screen, target, &s);
        return;
    }

    screen.charset.single_shift = None;
    let width = raw_width.max(1);

    put_char_to_target(screen, target, s, width, insert_mode);
}

fn put_printable_impl(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    s: SmolStr,
    insert_mode: bool,
) {
    let mut chars = s.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
        && let Some(mapped) = translated_codepoint(screen, ch)
    {
        put_char_impl(screen, target, mapped, insert_mode);
        return;
    }

    put_char_impl(screen, target, s, insert_mode);
}

fn put_8bit_byte_impl(
    screen: &mut Screen,
    target: WriteTarget<'_>,
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
    put_char_impl(screen, target, ch, insert_mode);
}

fn put_text_run_impl(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    run: &str,
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }

    let mut chars = run.chars();
    let run = if screen.charset.single_shift.take().is_some() {
        let ch = chars.next().unwrap();
        put_char_impl(
            screen,
            target,
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
            put_printable_impl(
                screen,
                target,
                SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4])),
                insert_mode,
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
            put_ascii_run_impl(screen, target, &bytes[start..i], insert_mode);
        }
        if i >= bytes.len() {
            break;
        }
        let len = utf8_char_len(bytes[i]);
        let mut builder = SmolStrBuilder::new();
        builder.push_str(&run[i..i + len]);
        put_char_impl(screen, target, builder.finish(), insert_mode);
        i += len;
    }
}

#[inline]
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn put_ascii_run_fast(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    run: &[u8],
    insert_mode: bool,
) {
    let Some(style) = target_style(screen, target) else {
        return;
    };
    let last_byte = *run.last().unwrap();
    set_target_last_char(
        screen,
        target,
        ASCII_CELLS[(last_byte - 0x20) as usize].clone(),
    );

    let mut i = 0;
    while i < run.len() {
        if !prepare_ascii_cursor(screen, target) {
            return;
        }

        let cols = target_cols(screen, target).unwrap() as usize;
        let col = target_cursor_col(screen, target).unwrap() as usize;
        let remaining_cols = cols - col;
        let chunk_len = (run.len() - i).min(remaining_cols);

        if insert_mode {
            target_insert_chars(screen, target, chunk_len);
        }

        write_ascii_chunk(
            target_row_mut(screen, target).unwrap(),
            col,
            &run[i..i + chunk_len],
            style,
        );
        advance_target_cursor(screen, target, chunk_len as u32);
        i += chunk_len;
    }
}

fn put_char_to_target(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    s: SmolStr,
    width: usize,
    insert_mode: bool,
) {
    if !fit_char_to_target(screen, target, width) {
        return;
    }

    let style = target_style(screen, target).unwrap();
    if insert_mode {
        target_insert_chars(screen, target, width);
    }

    let col = target_cursor_col(screen, target).unwrap() as usize;
    write_styled_glyph(
        target_row_mut(screen, target).unwrap(),
        col,
        width,
        style,
        &s,
    );
    set_target_last_char(screen, target, s);
    advance_target_cursor(screen, target, width as u32);
}

fn write_ascii_chunk(
    row: &mut Row,
    col: usize,
    chunk: &[u8],
    style: CellStyle,
) {
    break_wide_glyphs_around_write(row, col, chunk.len());
    let table: &[SmolStr; 95] = &ASCII_CELLS;
    for (cell, &b) in row.cells[col..col + chunk.len()].iter_mut().zip(chunk) {
        *cell = unsafe { table.get_unchecked((b - 0x20) as usize) }.clone();
    }
    fill_row_style(row, col, chunk.len(), style);
}

fn write_styled_glyph(
    row: &mut Row,
    col: usize,
    width: usize,
    style: CellStyle,
    s: &SmolStr,
) {
    break_wide_glyphs_around_write(row, col, width);
    row.cells[col] = s.clone();
    fill_row_style(row, col, width, style);
    for i in 1..width {
        row.cells[col + i] = continuation_cell();
    }
}

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

fn status_line_ref(screen: &Screen) -> Option<&StatusLine> {
    screen::status_line_writable(screen)
        .then_some(screen.status_line.as_ref())
        .flatten()
}

fn target_style(
    screen: &Screen,
    target: WriteTarget<'_>,
) -> Option<CellStyle> {
    match target {
        WriteTarget::Main(_) => Some(screen_style(screen)),
        WriteTarget::Status => status_line_ref(screen).map(status_style),
    }
}

fn target_cols(
    screen: &Screen,
    target: WriteTarget<'_>,
) -> Option<u32> {
    match target {
        WriteTarget::Main(viewport) => Some(current_row_display_cols(screen, viewport)),
        WriteTarget::Status => status_line_ref(screen).map(|status| status.row.len().max(1)),
    }
}

fn target_cursor_col(
    screen: &Screen,
    target: WriteTarget<'_>,
) -> Option<u32> {
    match target {
        WriteTarget::Main(_) => Some(screen.cursor.col),
        WriteTarget::Status => status_line_ref(screen).map(|status| status.cursor.col),
    }
}

fn set_target_last_char(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    last: SmolStr,
) {
    match target {
        WriteTarget::Main(_) => screen.last_char = Some(last),
        WriteTarget::Status => {
            if let Some(status) = status_line_mut(screen) {
                status.last_char = Some(last);
            }
        }
    }
}

fn prepare_ascii_cursor(
    screen: &mut Screen,
    target: WriteTarget<'_>,
) -> bool {
    match target {
        WriteTarget::Main(viewport) => {
            let cols = current_row_display_cols(screen, viewport);
            if screen.cursor.col >= cols {
                if screen.autowrap {
                    soft_wrap(screen, viewport);
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

fn fit_char_to_target(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    width: usize,
) -> bool {
    match target {
        WriteTarget::Main(viewport) => {
            let cols = current_row_display_cols(screen, viewport);
            if screen.cursor.col + width as u32 > cols {
                if screen.autowrap {
                    soft_wrap(screen, viewport);
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

fn target_insert_chars(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    count: usize,
) {
    match target {
        WriteTarget::Main(viewport) => {
            screen
                .grid
                .insert_chars(&screen.cursor, viewport, count as u16);
        }
        WriteTarget::Status => {
            if let Some(status) = status_line_mut(screen) {
                status_insert_chars(status, count);
            }
        }
    }
}

fn target_row_mut<'a>(
    screen: &'a mut Screen,
    target: WriteTarget<'_>,
) -> Option<&'a mut Row> {
    match target {
        WriteTarget::Main(viewport) => {
            let row = screen::active_row_index(screen, viewport);
            Some(&mut screen.grid.rows[row])
        }
        WriteTarget::Status => status_line_mut(screen).map(|status| &mut status.row),
    }
}

fn advance_target_cursor(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    delta: u32,
) {
    match target {
        WriteTarget::Main(_) => screen.cursor.col += delta,
        WriteTarget::Status => {
            if let Some(status) = status_line_mut(screen) {
                status.cursor.col += delta;
            }
        }
    }
}

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

pub(crate) fn break_wide_glyphs_around_write(
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

fn try_extend_prev_cell(
    screen: &mut Screen,
    target: WriteTarget<'_>,
    s: &str,
) {
    match target {
        WriteTarget::Main(viewport) => try_extend_prev_main_cell(screen, viewport, s),
        WriteTarget::Status => try_extend_prev_status_cell(screen, s),
    }
}

fn try_extend_prev_main_cell(
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

    while prev_col > 0 && screen.grid.rows[prev_row].cells[prev_col].is_empty() {
        prev_col -= 1;
    }

    let row = &mut screen.grid.rows[prev_row];
    let _ = try_extend_row_cell(row, prev_col, s);
}

fn try_extend_prev_status_cell(
    screen: &mut Screen,
    s: &str,
) {
    let Some(status) = status_line_mut(screen) else {
        return;
    };
    let col = status.cursor.col as usize;
    if col == 0 {
        return;
    }
    let _ = try_extend_row_cell(&mut status.row, col - 1, s);
}

fn try_extend_row_cell(
    row: &mut Row,
    col: usize,
    s: &str,
) -> bool {
    let Some(prev) = row.cells.get(col) else {
        return false;
    };
    if prev.as_str() == " " || prev.is_empty() {
        return false;
    }

    let mut combined = SmolStrBuilder::new();
    combined.push_str(prev);
    combined.push_str(s);
    let combined = combined.finish();
    if combined.graphemes(true).count() != 1 {
        return false;
    }

    row.cells[col] = combined;
    true
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
