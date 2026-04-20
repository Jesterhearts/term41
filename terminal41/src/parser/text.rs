use super::*;

pub(crate) fn put_ascii_run(
    screen: &mut Screen,
    viewport: &Viewport,
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

    let last_byte = *run.last().unwrap();
    screen.last_char = Some(ASCII_CELLS[(last_byte - 0x20) as usize].clone());

    let mut i = 0;
    while i < run.len() {
        let cols = current_row_display_cols(screen, viewport);
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

        if insert_mode {
            screen
                .grid
                .insert_chars(&screen.cursor, viewport, chunk_len as u16);
        }

        let row = &mut screen.grid.rows[r];
        break_wide_glyphs_around_write(row, col, chunk_len);
        let chunk = &run[i..i + chunk_len];
        let table: &[SmolStr; 95] = &ASCII_CELLS;
        for (cell, &b) in row.cells[col..col + chunk_len].iter_mut().zip(chunk) {
            *cell = unsafe { table.get_unchecked((b - 0x20) as usize) }.clone();
        }
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

pub(crate) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    let raw_width = UnicodeWidthStr::width(s.as_str());

    if raw_width == 0 {
        try_extend_prev_cell(screen, viewport, &s);
        return;
    }

    screen.charset.single_shift = None;

    let width = raw_width.max(1);
    let cols = current_row_display_cols(screen, viewport);

    if screen.cursor.col + width as u32 > cols {
        if screen.autowrap {
            soft_wrap(screen, viewport);
        } else {
            screen.cursor.col = cols.saturating_sub(width as u32);
        }
    }

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

pub(crate) fn put_8bit_byte(
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

pub(crate) fn put_text_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &str,
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }

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

    let mut i = 0;
    while i < bytes.len() {
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
        let len = utf8_char_len(bytes[i]);
        let mut builder = SmolStrBuilder::new();
        builder.push_str(&run[i..i + len]);

        put_char(screen, viewport, builder.finish(), insert_mode);
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

    while prev_col > 0 && screen.grid.rows[prev_row].cells[prev_col].is_empty() {
        prev_col -= 1;
    }

    let prev = &screen.grid.rows[prev_row].cells[prev_col];
    if prev.as_str() == " " || prev.is_empty() {
        return;
    }

    let mut combined = SmolStrBuilder::new();
    combined.push_str(prev);
    combined.push_str(s);
    let combined = combined.finish();
    if combined.graphemes(true).count() != 1 {
        return;
    }

    screen.grid.rows[prev_row].cells[prev_col] = combined;
}

pub(crate) fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    clamp_cursor_to_row_width(screen, viewport);

    match byte {
        b'\n' | VT | FF => {
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
            screen.charset.set_gl(GraphicSetSlot::G1);
        }
        SI => {
            screen.charset.set_gl(GraphicSetSlot::G0);
        }
        BEL => {
            *bell_pending = true;
        }
        NUL => {}
        _ => {}
    }
}
