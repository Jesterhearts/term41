use unicode_width::UnicodeWidthChar;

use super::*;
use crate::screen::BackspaceGuard;

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
            screen.backspace_guard = None;
            if newline_mode {
                screen.cursor.col = 0;
            }
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    grid::scroll_up_in_region_op(
                        &mut screen.grid,
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
            screen.backspace_guard = None;
            screen.cursor.col = 0;
        }
        BS => {
            if consume_backspace_guard(screen, viewport) {
                return;
            }
            let prev_col = screen.cursor.col;
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
            set_backspace_guard_after_move(screen, viewport, prev_col);
        }
        b'\t' => {
            screen.backspace_guard = None;
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
        }
        SO => {
            screen.backspace_guard = None;
            screen.charset.set_gl(GraphicSetSlot::G1);
        }
        SI => {
            screen.backspace_guard = None;
            screen.charset.set_gl(GraphicSetSlot::G0);
        }
        BEL => {
            screen.backspace_guard = None;
            *bell_pending = true;
        }
        NUL => {
            screen.backspace_guard = None;
        }
        _ => {
            screen.backspace_guard = None;
        }
    }
}

fn consume_backspace_guard(
    screen: &mut Screen,
    viewport: &Viewport,
) -> bool {
    let Some(mut guard) = screen.backspace_guard else {
        return false;
    };
    let row = screen::active_row_index(screen, viewport);
    if row != guard.row || screen.cursor.col != guard.col || guard.remaining == 0 {
        screen.backspace_guard = None;
        return false;
    }
    guard.remaining -= 1;
    screen.backspace_guard = (guard.remaining > 0).then_some(guard);
    true
}

fn set_backspace_guard_after_move(
    screen: &mut Screen,
    viewport: &Viewport,
    prev_col: u32,
) {
    screen.backspace_guard = None;
    if prev_col == 0 {
        return;
    }

    let row = screen::active_row_index(screen, viewport);
    let row_cells = &screen.grid.rows[row].cells;
    let new_col = screen.cursor.col as usize;
    let prev_col = prev_col as usize;
    if prev_col != new_col + 1 || prev_col >= row_cells.len() || new_col >= row_cells.len() {
        return;
    }

    if !row_cells[prev_col].is_empty() {
        return;
    }
    let anchor = row_cells[new_col].as_str();
    if !anchor.contains('\u{200d}') {
        return;
    }

    let mut span = 1usize;
    while new_col + span < row_cells.len() && row_cells[new_col + span].is_empty() {
        span += 1;
    }
    let host_width: usize = anchor
        .chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum();
    let overshoot = host_width.saturating_sub(span);
    if overshoot > 0 {
        screen.backspace_guard = Some(BackspaceGuard {
            row,
            col: screen.cursor.col,
            remaining: overshoot as u32,
        });
    }
}
