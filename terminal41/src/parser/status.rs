use super::*;

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

pub(crate) fn put_status_ascii_run(
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

pub(crate) fn put_status_text_run(
    screen: &mut Screen,
    run: &str,
    insert_mode: bool,
) {
    for ch in run.chars() {
        put_status_printable(
            screen,
            SmolStr::new_inline(ch.encode_utf8(&mut [0u8; 4])),
            insert_mode,
        );
    }
}

pub(crate) fn put_status_printable(
    screen: &mut Screen,
    s: SmolStr,
    insert_mode: bool,
) {
    let mut chars = s.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
        && let Some(mapped) = translated_codepoint(screen, ch)
    {
        status_put_char(screen, mapped, insert_mode);
        return;
    }
    status_put_char(screen, s, insert_mode);
}

pub(crate) fn put_status_8bit_byte(
    screen: &mut Screen,
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
    status_put_char(screen, ch, insert_mode);
}

pub(crate) fn execute_status(
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

pub(crate) fn apply_status_line_csi(
    screen: &mut Screen,
    palette: &mut color::ColorPalette,
    insert_mode: bool,
    action: StatusLineCsiAction,
) {
    let Some(status) = status_line_mut(screen) else {
        return;
    };
    let cols = status.row.len().max(1);
    let cursor = &mut status.cursor;

    match action {
        StatusLineCsiAction::SetGraphicsRendition { params } => {
            let mut palette = palette.clone();
            palette.fg = palette.status_line_fg;
            palette.bg = palette.status_line_bg;
            apply_sgr_groups(
                &mut status.fg,
                &mut status.bg,
                &mut status.attrs,
                &mut status.underline,
                &mut status.underline_color,
                params.as_groups(),
                &palette,
            );
        }
        StatusLineCsiAction::InsertChars { count } => {
            status_insert_chars(status, count as usize);
        }
        StatusLineCsiAction::HomeRow => {
            cursor.row = 0;
        }
        StatusLineCsiAction::CursorForward { count } => {
            cursor.col = (cursor.col + count as u32).min(cols - 1);
        }
        StatusLineCsiAction::CursorBackward { count } => {
            cursor.col = cursor.col.saturating_sub(count as u32);
        }
        StatusLineCsiAction::CursorHorizontalAbsolute { col } => {
            cursor.col = (col.max(1) as u32 - 1).min(cols - 1);
        }
        StatusLineCsiAction::CursorPosition { col } => {
            cursor.row = 0;
            cursor.col = (col.max(1) as u32 - 1).min(cols - 1);
        }
        StatusLineCsiAction::EraseDisplay => {
            status.row.clear(status.fg, status.bg);
        }
        StatusLineCsiAction::EraseInLine { mode } => {
            let col = cursor.col as usize;
            let len = cols as usize;
            match mode {
                0 => status.row.clear_range(col..len, status.fg, status.bg),
                1 => status.row.clear_range(0..(col + 1), status.fg, status.bg),
                2 => status.row.clear(status.fg, status.bg),
                _ => {}
            }
        }
        StatusLineCsiAction::DeleteChars { count } => {
            status_delete_chars(status, count as usize);
        }
        StatusLineCsiAction::EraseChars { count } => {
            status_erase_chars(status, count as usize);
        }
        StatusLineCsiAction::RepeatLastChar { count } => {
            if let Some(last) = status.last_char.clone() {
                for _ in 0..count {
                    status_put_char(screen, last.clone(), insert_mode);
                }
            }
        }
    }
}
