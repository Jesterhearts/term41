use crate::terminal::color::apply_sgr;
use crate::terminal::grid::Viewport;
use crate::terminal::screen::Screen;
use crate::vte;

pub(super) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    ch: char,
) {
    let fg = screen.fg;
    let bg = screen.bg;

    if screen.cursor.col >= viewport.cols {
        // Soft wrap: mark the current row as a continuation.
        screen.cursor.col = 0;
        let r = screen.grid.active_row_index(&screen.cursor, viewport);
        screen.grid.rows[r].wrapped = true;
        if screen.cursor.row == screen.scroll_bottom {
            if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                screen.grid.push_visible_row(viewport);
            } else {
                screen.grid.scroll_up_in_region(
                    viewport,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    1,
                );
            }
        } else if screen.cursor.row < viewport.rows - 1 {
            screen.cursor.row += 1;
        }
    }

    // New output resets the viewport to the live edge.
    screen.offset = 0;

    let r = screen.grid.active_row_index(&screen.cursor, viewport);
    let c = screen.cursor.col as usize;
    screen.grid.rows[r].chars[c] = ch;
    screen.grid.rows[r].fg[c] = fg;
    screen.grid.rows[r].bg[c] = bg;
    screen.cursor.col += 1;
}

pub(super) fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
) {
    match byte {
        b'\n' => {
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    screen.grid.scroll_up_in_region(
                        viewport,
                        screen.scroll_top,
                        screen.scroll_bottom,
                        1,
                    );
                }
            } else if screen.cursor.row < viewport.rows - 1 {
                screen.cursor.row += 1;
            }
        }
        b'\r' => {
            screen.cursor.col = 0;
        }
        0x08 => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        b'\t' => {
            let next = (screen.cursor.col / 8 + 1) * 8;
            screen.cursor.col = next.min(viewport.cols - 1);
        }
        0x07 | 0x00 => {}
        _ => {}
    }
}

pub(super) fn csi_dispatch(
    screen: &mut Screen,
    viewport: &Viewport,
    params: &vte::Params,
    intermediates: &[u8],
    action: char,
) {
    if !intermediates.is_empty() {
        return;
    }

    let p: Vec<u16> = params.iter().map(|p| p[0]).collect();
    let cursor = &mut screen.cursor;

    match action {
        'A' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = cursor.row.saturating_sub(n);
        }
        'B' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = (cursor.row + n).min(viewport.rows - 1);
        }
        'C' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = (cursor.col + n).min(viewport.cols - 1);
        }
        'D' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = cursor.col.saturating_sub(n);
        }
        'H' | 'f' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.row = row.min(viewport.rows - 1);
            cursor.col = col.min(viewport.cols - 1);
        }
        'J' => {
            let mode = p.first().copied().unwrap_or(0);
            screen.grid.erase_in_display(&screen.cursor, viewport, mode);
        }
        'K' => {
            let mode = p.first().copied().unwrap_or(0);
            screen.grid.erase_in_line(&screen.cursor, viewport, mode);
        }
        'm' => apply_sgr(&mut screen.fg, &mut screen.bg, params),
        'd' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.row = row.min(viewport.rows - 1);
        }
        'G' => {
            let col = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.col = col.min(viewport.cols - 1);
        }
        'L' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= screen.scroll_top && cursor.row <= screen.scroll_bottom {
                screen
                    .grid
                    .scroll_down_in_region(viewport, cursor.row, screen.scroll_bottom, n);
            }
        }
        'M' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= screen.scroll_top && cursor.row <= screen.scroll_bottom {
                screen
                    .grid
                    .scroll_up_in_region(viewport, cursor.row, screen.scroll_bottom, n);
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
            if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                for _ in 0..n {
                    screen.grid.push_visible_row(viewport);
                }
            } else {
                screen.grid.scroll_up_in_region(
                    viewport,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'T' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen
                .grid
                .scroll_down_in_region(viewport, screen.scroll_top, screen.scroll_bottom, n);
        }
        'r' => {
            let top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let bottom = p.get(1).copied().unwrap_or(viewport.rows as u16).max(1) as u32 - 1;
            screen.scroll_top = top.min(viewport.rows - 1);
            screen.scroll_bottom = bottom.min(viewport.rows - 1).max(screen.scroll_top);
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        'n' | 'c' => {}
        _ => {}
    }
}

pub(super) fn esc_dispatch(
    screen: &mut Screen,
    viewport: &Viewport,
    intermediates: &[u8],
    byte: u8,
) {
    if intermediates.first().is_some_and(|&b| b"()*+".contains(&b)) {
        return;
    }

    match byte {
        b'c' => {
            todo!()
        }
        b'M' => {
            if screen.cursor.row == screen.scroll_top {
                screen.grid.scroll_down_in_region(
                    viewport,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    1,
                );
            } else if screen.cursor.row > 0 {
                screen.cursor.row -= 1;
            }
        }
        b'=' | b'>' => {}
        _ => {}
    }
}
