use unicode_width::UnicodeWidthChar;

use super::*;
use crate::screen::BackspaceGuard;

#[cfg(test)]
pub(crate) fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    execute_with_scrollback_policy(screen, viewport, byte, bell_pending, newline_mode, true);
}

pub(crate) fn execute_with_scrollback_policy(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
    preserve_top_origin_scrollback: bool,
) {
    clamp_cursor_to_row_width(screen, viewport);

    let Ok(byte) = AsciiControlBytes::try_from(byte) else {
        screen.backspace_guard = None;
        return;
    };

    match byte {
        AsciiControlBytes::LineFeed
        | AsciiControlBytes::VerticalTab
        | AsciiControlBytes::FormFeed => {
            screen.backspace_guard = None;
            if newline_mode {
                screen.cursor.col = 0;
            }
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    grid::scroll_up_in_region_with_scrollback_policy_op(
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
                clamp_cursor_to_row_width(screen, viewport);
            }
        }
        AsciiControlBytes::CarriageReturn => {
            screen.backspace_guard = None;
            screen.cursor.col = 0;
        }
        AsciiControlBytes::Backspace => {
            if consume_backspace_guard(screen, viewport) {
                return;
            }
            let prev_col = screen.cursor.col;
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
            set_backspace_guard_after_move(screen, viewport, prev_col);
        }
        AsciiControlBytes::HorizontalTab => {
            screen.backspace_guard = None;
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
        }
        AsciiControlBytes::ShiftOut => {
            screen.backspace_guard = None;
            screen.charset.set_gl(GraphicSetSlot::G1);
        }
        AsciiControlBytes::ShiftIn => {
            screen.backspace_guard = None;
            screen.charset.set_gl(GraphicSetSlot::G0);
        }
        AsciiControlBytes::Bell => {
            screen.backspace_guard = None;
            *bell_pending = true;
        }
        AsciiControlBytes::Nul => {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

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
        execute(
            &mut screen,
            &viewport,
            AsciiControlBytes::Backspace as u8,
            &mut false,
            false,
        );
        assert_eq!(screen.cursor.col, 1);
        execute(
            &mut screen,
            &viewport,
            AsciiControlBytes::Backspace as u8,
            &mut false,
            false,
        );
        execute(
            &mut screen,
            &viewport,
            AsciiControlBytes::Backspace as u8,
            &mut false,
            false,
        );
        execute(
            &mut screen,
            &viewport,
            AsciiControlBytes::Backspace as u8,
            &mut false,
            false,
        );
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
        execute(
            &mut screen,
            &viewport,
            AsciiControlBytes::Bell as u8,
            &mut bell,
            false,
        );
        assert!(bell);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn execute_nul_is_noop() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(
            &mut screen,
            &viewport,
            AsciiControlBytes::Nul as u8,
            &mut false,
            false,
        );
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
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
}
