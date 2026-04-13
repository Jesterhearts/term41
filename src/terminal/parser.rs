use crate::terminal::color::apply_sgr;
use crate::terminal::grid::Viewport;
use crate::terminal::screen::Screen;
use crate::vte;

// C0 control bytes (ECMA-48 / ASCII).
const NUL: u8 = 0x00;
const BEL: u8 = 0x07;
const BS: u8 = 0x08;

/// Hardware tab stop width in columns.
const TAB_WIDTH: u32 = 8;

/// SCS (Select Character Set) intermediate bytes that designate G0..G3.
/// We accept and silently ignore these rather than treating the sequence as
/// unknown.
const SCS_INTERMEDIATES: &[u8] = b"()*+";

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
        BS => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        b'\t' => {
            let next = (screen.cursor.col / TAB_WIDTH + 1) * TAB_WIDTH;
            screen.cursor.col = next.min(viewport.cols - 1);
        }
        BEL | NUL => {}
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
    if intermediates
        .first()
        .is_some_and(|&b| SCS_INTERMEDIATES.contains(&b))
    {
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

#[cfg(test)]
mod tests {
    use palette::Srgb;

    use super::*;
    use crate::terminal::screen::Screen;
    use crate::vte::Action;
    use crate::vte::Parser;

    const TEST_COLS: u32 = 10;
    const TEST_ROWS: u32 = 4;

    fn setup() -> (Screen, Viewport) {
        let screen = Screen::new(TEST_COLS, TEST_ROWS, 100);
        let viewport = Viewport {
            rows: TEST_ROWS,
            cols: TEST_COLS,
        };
        (screen, viewport)
    }

    /// Drive `input` through a VTE parser and dispatch each action through the
    /// parser module under test. This is the same pipeline the live terminal
    /// uses, so tests exercise the same paths callers actually take.
    fn feed(
        input: &[u8],
        screen: &mut Screen,
        viewport: &Viewport,
    ) {
        let mut parser = Parser::new();
        for action in parser.parse(input) {
            match action {
                Action::Print(ch) => put_char(screen, viewport, ch),
                Action::Execute(b) => execute(screen, viewport, b),
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    csi_dispatch(screen, viewport, &params, intermediates.as_slice(), action);
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    esc_dispatch(screen, viewport, intermediates.as_slice(), byte);
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
        screen.grid.rows[r].chars.iter().collect()
    }

    // -- put_char -----------------------------------------------------------

    #[test]
    fn put_char_writes_with_current_colors_and_advances() {
        let (mut screen, viewport) = setup();
        screen.fg = Srgb::new(1, 2, 3);
        screen.bg = Srgb::new(4, 5, 6);

        put_char(&mut screen, &viewport, 'A');

        assert_eq!(row_text(&screen, &viewport, 0).chars().next(), Some('A'));
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].fg[0], Srgb::new(1, 2, 3));
        assert_eq!(screen.grid.rows[r].bg[0], Srgb::new(4, 5, 6));
        assert_eq!(screen.cursor.col, 1);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn put_char_soft_wraps_at_right_edge() {
        let (mut screen, viewport) = setup();
        feed(b"abcdefghij", &mut screen, &viewport);

        // Cursor sits past the right edge; the next char should wrap.
        assert_eq!(screen.cursor.col, TEST_COLS);
        feed(b"k", &mut screen, &viewport);

        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
        assert!(
            screen.grid.rows[screen.grid.active_row_index(&screen.cursor, &viewport) - 1].wrapped
        );
        assert_eq!(&row_text(&screen, &viewport, 1)[..1], "k");
    }

    #[test]
    fn put_char_resets_scrollback_offset() {
        let (mut screen, viewport) = setup();
        screen.offset = 5;
        put_char(&mut screen, &viewport, 'x');
        assert_eq!(screen.offset, 0);
    }

    // -- execute ------------------------------------------------------------

    #[test]
    fn execute_lf_moves_cursor_down() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\n');
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn execute_lf_at_scroll_bottom_scrolls_up() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();

        execute(&mut screen, &viewport, b'\n');

        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert_eq!(screen.grid.rows.len(), rows_before + 1);
    }

    #[test]
    fn execute_cr_resets_col_to_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 5;
        execute(&mut screen, &viewport, b'\r');
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_bs_saturates_at_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 2;
        execute(&mut screen, &viewport, BS);
        assert_eq!(screen.cursor.col, 1);
        execute(&mut screen, &viewport, BS);
        execute(&mut screen, &viewport, BS);
        execute(&mut screen, &viewport, BS);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_tab_advances_to_next_tab_stop() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\t');
        assert_eq!(screen.cursor.col, TAB_WIDTH);

        screen.cursor.col = 3;
        execute(&mut screen, &viewport, b'\t');
        assert_eq!(screen.cursor.col, TAB_WIDTH);
    }

    #[test]
    fn execute_tab_clamps_at_rightmost_column() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = TEST_COLS - 1;
        execute(&mut screen, &viewport, b'\t');
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn execute_bel_and_nul_are_noops() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, BEL);
        execute(&mut screen, &viewport, NUL);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    // -- csi_dispatch cursor movement --------------------------------------

    #[test]
    fn csi_a_moves_cursor_up_by_count() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 3;
        feed(b"\x1b[2A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_defaults_to_one() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_zero_parameter_treated_as_one() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[0A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_saturates_at_top() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 1;
        feed(b"\x1b[99A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn csi_b_moves_cursor_down_clamped() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[99B", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
    }

    #[test]
    fn csi_c_moves_cursor_right_clamped() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[99C", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_d_moves_cursor_left_saturating() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 2;
        feed(b"\x1b[5D", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_positions_cursor_one_based() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[3;5H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_h_defaults_to_origin() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 5;
        feed(b"\x1b[H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_clamps_to_viewport() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[99;99H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_f_is_alias_of_h() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;3f", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_g_sets_column_only() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[5G", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_d_lowercase_sets_row_only() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1b[3d", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 5);
    }

    // -- csi_dispatch erase / SGR / scroll region --------------------------

    #[test]
    fn csi_j_2_erases_entire_display() {
        let (mut screen, viewport) = setup();
        feed(b"hello\nworld", &mut screen, &viewport);
        feed(b"\x1b[2J", &mut screen, &viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim(), "");
    }

    #[test]
    fn csi_k_erases_to_end_of_line() {
        let (mut screen, viewport) = setup();
        feed(b"hello", &mut screen, &viewport);
        feed(b"\x1b[3G", &mut screen, &viewport); // col=2
        feed(b"\x1b[K", &mut screen, &viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim_end(), "he");
    }

    #[test]
    fn csi_m_applies_sgr_colors() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[31m", &mut screen, &viewport);
        // SGR 31 = ANSI red fg, which is (205, 0, 0) in the standard palette.
        assert_eq!(screen.fg, Srgb::new(205, 0, 0));
    }

    #[test]
    fn csi_r_sets_scroll_region_and_homes_cursor() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 3;
        screen.cursor.col = 5;
        feed(b"\x1b[2;3r", &mut screen, &viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_r_clamps_bounds_to_viewport() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[1;99r", &mut screen, &viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, TEST_ROWS - 1);
    }

    #[test]
    fn csi_with_intermediate_is_ignored() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // Intermediate ` ` before action `q` is a valid CSI shape but not one
        // we handle — we must leave state untouched.
        feed(b"\x1b[1 q", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn csi_unknown_action_is_ignored() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 1;
        screen.cursor.col = 1;
        feed(b"\x1b[1Z", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
    }

    // -- esc_dispatch ------------------------------------------------------

    #[test]
    fn esc_m_at_scroll_top_scrolls_down() {
        let (mut screen, viewport) = setup();
        feed(b"top\nmid\nbot", &mut screen, &viewport);
        // Cursor is at scroll_top (row 0) after moving back there.
        feed(b"\x1b[H", &mut screen, &viewport);
        feed(b"\x1bM", &mut screen, &viewport);
        // After scroll-down, the old top row shifts down one and row 0 blanks.
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim_end(), "top");
    }

    #[test]
    fn esc_m_above_scroll_top_moves_cursor_up() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1bM", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn esc_m_at_row_zero_outside_region_is_noop() {
        // scroll_top defaults to 0, so row 0 triggers scroll_down_in_region
        // above. Force a non-zero scroll_top to exercise the cursor.row > 0
        // branch at exactly row 0 of the viewport.
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;4r", &mut screen, &viewport); // scroll_top = 1
        screen.cursor.row = 0;
        feed(b"\x1bM", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn esc_scs_designator_is_ignored() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // ESC ( B designates US-ASCII as G0. Parser should no-op without
        // dropping state or panicking on the `B` byte (which would otherwise
        // land in the unknown-byte arm).
        feed(b"\x1b(B", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn esc_keypad_modes_are_noop() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        feed(b"\x1b=", &mut screen, &viewport);
        feed(b"\x1b>", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }
}
