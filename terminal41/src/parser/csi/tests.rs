use config41::default_bg;
use config41::default_fg;
use palette::Srgb;

use super::*;
use crate::parser::execute;
use crate::parser::test_support::*;

fn set_cursor_col(
    screen: &mut Screen,
    col: u32,
) {
    screen.cursor.col = col;
}

#[test]
fn csi_parse_maps_private_mode_query_semantically() {
    assert!(with_csi_action(b"\x1b[?7$p", |action| matches!(
        action,
        ParsedCsiAction::QueryPrivateMode { mode: 7 }
    )));
}

#[test]
fn csi_parse_maps_ansi_mode_query_semantically() {
    assert!(with_csi_action(b"\x1b[4$p", |action| matches!(
        action,
        ParsedCsiAction::QueryAnsiMode { mode: 4 }
    )));
}

#[test]
fn csi_parse_maps_status_display_semantically() {
    assert!(with_csi_action(b"\x1b[2$~", |action| matches!(
        action,
        ParsedCsiAction::SetStatusDisplay { mode: 2 }
    )));
}

#[test]
fn csi_parse_maps_private_mode_set_semantically() {
    assert!(with_csi_action(b"\x1b[?2004h", |action| matches!(
        action,
        ParsedCsiAction::SetPrivateModes { enable: true, .. }
    )));
}

#[test]
fn csi_parse_maps_attr_change_extent_semantically() {
    assert!(with_csi_action(b"\x1b[2*x", |action| matches!(
        action,
        ParsedCsiAction::SetAttrChangeExtent {
            extent: grid::AttrChangeExtent::Rectangle
        }
    )));
}

#[test]
fn csi_parse_maps_cursor_style_semantically() {
    assert!(with_csi_action(b"\x1b[5 q", |action| matches!(
        action,
        ParsedCsiAction::SetCursorStyle { style: 5 }
    )));
}

#[test]
fn csi_parse_maps_soft_reset_semantically() {
    assert!(with_csi_action(b"\x1b[!p", |action| matches!(
        action,
        ParsedCsiAction::SoftReset
    )));
}

#[test]
fn csi_parse_uses_declrmm_to_disambiguate_csi_s() {
    let (screen, _) = setup();
    let mut modes = TerminalModes::new();
    modes.declrmm = true;
    assert!(with_csi_action_and(
        b"\x1b[2;8s",
        &screen,
        &modes,
        |action| matches!(
            action,
            ParsedCsiAction::Main(MainCsiAction::SetLeftRightMargins {
                left: 2,
                right: Some(8)
            })
        )
    ));
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
    set_cursor_col(&mut screen, 2);
    feed(b"\x1b[5D", &mut screen, &mut viewport);
    assert_eq!(screen.cursor.col, 0);
}

// -- CNL / CPL -----------------------------------------------------------

#[test]
fn csi_e_moves_down_and_homes_column() {
    let (mut screen, mut viewport) = setup();
    screen.cursor.row = 0;
    set_cursor_col(&mut screen, 5);
    feed(b"\x1b[2E", &mut screen, &mut viewport);
    assert_eq!(screen.cursor.row, 2);
    assert_eq!(screen.cursor.col, 0);
}

#[test]
fn csi_e_clamps_at_bottom() {
    let (mut screen, mut viewport) = setup();
    set_cursor_col(&mut screen, 3);
    feed(b"\x1b[99E", &mut screen, &mut viewport);
    assert_eq!(screen.cursor.row, TEST_ROWS - 1);
    assert_eq!(screen.cursor.col, 0);
}

#[test]
fn csi_f_moves_up_and_homes_column() {
    let (mut screen, mut viewport) = setup();
    screen.cursor.row = 3;
    set_cursor_col(&mut screen, 7);
    feed(b"\x1b[2F", &mut screen, &mut viewport);
    assert_eq!(screen.cursor.row, 1);
    assert_eq!(screen.cursor.col, 0);
}

#[test]
fn csi_f_saturates_at_top() {
    let (mut screen, mut viewport) = setup();
    screen.cursor.row = 1;
    set_cursor_col(&mut screen, 5);
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
    set_cursor_col(&mut screen, 5);
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
    set_cursor_col(&mut screen, 5);
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
    set_cursor_col(&mut screen, 5);
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
    set_cursor_col(&mut screen, 3);
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
    set_cursor_col(&mut screen, 1);
    // Use a genuinely unrecognized CSI action (not Z, which is now CBT).
    feed(b"\x1b[1~", &mut screen, &mut viewport);
    assert_eq!(screen.cursor.row, 1);
    assert_eq!(screen.cursor.col, 1);
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
    assert_ne!(screen.fg, default_fg());
    // Soft reset.
    feed(b"\x1b[!p", &mut screen, &mut viewport);
    assert_eq!(screen.attrs, CellAttrs::default());
    assert_eq!(screen.fg, default_fg());
    assert_eq!(screen.bg, default_bg());
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

// -- DECSR / DECSRC (CSI Pr + p / CSI Pr * q) -------------------------

#[test]
fn decsr_resets_screen_and_reports_confirmation() {
    let (mut screen, mut viewport) = setup();
    feed(b"Hello\x1b[?7l", &mut screen, &mut viewport);
    assert!(!screen.autowrap);
    let out = feed_with_output(b"\x1b[123+p", &mut screen, &mut viewport);
    assert_eq!(out, b"\x1b[123*q");
    assert!(screen.autowrap);
    let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
    assert_eq!(row.cells[0].as_str(), " ");
    assert_eq!(screen.cursor.row, 0);
    assert_eq!(screen.cursor.col, 0);
}

#[test]
fn decsr_without_parameter_does_not_report_confirmation() {
    let (mut screen, mut viewport) = setup();
    let out = feed_with_output(b"\x1b[+p", &mut screen, &mut viewport);
    assert!(out.is_empty());
}

#[test]
fn decsr_confirmation_uses_reset_c1_mode() {
    let (mut screen, mut viewport) = setup();
    let out = feed_with_output(b"\x1b[64;2\"p\x1b[9+p", &mut screen, &mut viewport);
    assert_eq!(out, b"\x1b[9*q");
}

// -- DECRQM (CSI ? Ps $ p) -----------------------------------------------

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
fn decrqm_reports_sgr_pixels_mouse_encoding_set() {
    let (mut screen, mut viewport) = setup();
    let out = feed_with_output(b"\x1b[?1016h\x1b[?1016$p", &mut screen, &mut viewport);
    assert_eq!(out, b"\x1b[?1016;1$y");
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
    set_cursor_col(&mut screen, 3);
    feed(b"\x1bH", &mut screen, &mut viewport);
    let out = feed_with_output(b"\x1b[2$w", &mut screen, &mut viewport);
    assert_eq!(out, b"\x1bP2$u4;9\x1b\\");
}

#[test]
fn np_switches_page_and_homes_cursor() {
    let (mut screen, mut viewport) = setup();
    screen.cursor.row = 5;
    set_cursor_col(&mut screen, 7);
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
    set_cursor_col(&mut screen, 3);
    execute(&mut screen, &viewport, b'\t', &mut false, false);
    assert_eq!(screen.cursor.col, 8);
}

#[test]
fn tab_at_last_column_stays() {
    let (mut screen, viewport) = setup();
    set_cursor_col(&mut screen, TEST_COLS - 1);
    execute(&mut screen, &viewport, b'\t', &mut false, false);
    assert_eq!(screen.cursor.col, TEST_COLS - 1);
}

#[test]
fn hts_sets_custom_tab_stop() {
    let (mut screen, mut viewport) = setup();
    // Move to col 3, set a tab stop with ESC H, then tab from col 0.
    feed(b"\x1b[1;4H\x1bH", &mut screen, &mut viewport);
    assert!(screen.tab_stops[3]);
    set_cursor_col(&mut screen, 0);
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
        default_fg(),
        default_bg(),
        default_fg(),
        default_bg(),
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
        default_fg(),
        default_bg(),
        default_fg(),
        default_bg(),
    );
    let mut viewport = Viewport {
        rows: TEST_ROWS,
        cols: screen_cols,
        top: 0,
    };
    // Park at col 20, then CSI 2 Z (back 2 stops) should land at 8.
    set_cursor_col(&mut screen, 20);
    feed(b"\x1b[2Z", &mut screen, &mut viewport);
    assert_eq!(screen.cursor.col, 8);
}

#[test]
fn tbc_0_clears_at_cursor() {
    let (mut screen, mut viewport) = setup();
    // Default stop at col 8. Move there and clear it.
    set_cursor_col(&mut screen, 8);
    feed(b"\x1b[0g", &mut screen, &mut viewport);
    assert!(!screen.tab_stops[8]);
    // Tab from col 0 should now go to the last column.
    set_cursor_col(&mut screen, 0);
    execute(&mut screen, &viewport, b'\t', &mut false, false);
    assert_eq!(screen.cursor.col, TEST_COLS - 1);
}

#[test]
fn tbc_3_clears_all_tab_stops() {
    let (mut screen, mut viewport) = setup();
    feed(b"\x1b[3g", &mut screen, &mut viewport);
    assert!(screen.tab_stops.iter().all(|&s| !s));
    // Tab from col 0 should go to last column.
    set_cursor_col(&mut screen, 0);
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
