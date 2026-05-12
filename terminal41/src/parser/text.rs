use super::AsciiControlBytes;
use super::clamp_cursor_to_row_width;
use super::current_row_display_cols;
use super::next_tab_stop;
use crate::charset::GraphicSetSlot;
use crate::screen::Screen;
use crate::screen::grid;
use crate::screen::grid::Viewport;

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
        return;
    };

    match byte {
        AsciiControlBytes::LineFeed
        | AsciiControlBytes::VerticalTab
        | AsciiControlBytes::FormFeed => {
            if newline_mode {
                screen.cursor.col = 0;
            }
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
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
                clamp_cursor_to_row_width(screen, viewport);
            }
        }
        AsciiControlBytes::CarriageReturn => {
            screen.cursor.col = 0;
        }
        AsciiControlBytes::Backspace => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        AsciiControlBytes::HorizontalTab => {
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
        }
        AsciiControlBytes::ShiftOut => {
            screen.charset.set_gl(GraphicSetSlot::G1);
        }
        AsciiControlBytes::ShiftIn => {
            screen.charset.set_gl(GraphicSetSlot::G0);
        }
        AsciiControlBytes::Bell => {
            *bell_pending = true;
        }
        AsciiControlBytes::Nul => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    fn set_cursor_col(
        screen: &mut Screen,
        col: u32,
    ) {
        screen.cursor.col = col;
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
        set_cursor_col(&mut screen, 5);
        execute(&mut screen, &viewport, b'\r', &mut false, false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_bs_saturates_at_zero() {
        let (mut screen, viewport) = setup();
        set_cursor_col(&mut screen, 2);
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

        set_cursor_col(&mut screen, 3);
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn execute_tab_clamps_at_rightmost_column() {
        let (mut screen, viewport) = setup();
        set_cursor_col(&mut screen, TEST_COLS - 1);
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn execute_bel_sets_bell_pending() {
        let (mut screen, viewport) = setup();
        let mut bell = false;
        set_cursor_col(&mut screen, 3);
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
        set_cursor_col(&mut screen, 3);
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
        set_cursor_col(&mut screen, 5);
        // Enable LNM and issue LF in one feed call so the modes object
        // persists across both sequences.
        feed(b"\x1b[20h\n", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0); // CR implied
    }
}
