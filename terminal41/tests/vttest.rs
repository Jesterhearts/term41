//! Integration tests that simulate vttest sequences through the full
//! `Terminal::apply` path. Each test constructs a 80x24 terminal with
//! a persistent VTE parser, feeds escape sequences, and inspects the
//! grid to verify correct behavior.

use terminal41::ColorPalette;
use terminal41::FeaturePermissions;
use terminal41::LineAttr;
use terminal41::Terminal;
use vtepp::Parser;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

struct VtTerm {
    terminal: Terminal,
    parser: Parser,
}

impl VtTerm {
    fn new(
        cols: u32,
        rows: u32,
    ) -> Self {
        Self {
            terminal: Terminal::new(
                cols,
                rows,
                1000,
                terminal41::StatusDisplayKind::None,
                false,
                FeaturePermissions::default(),
                16,
                8,
                ColorPalette::default(),
            ),
            parser: Parser::new(),
        }
    }

    fn new_80x24() -> Self {
        Self::new(80, 24)
    }

    fn process(
        &mut self,
        data: &[u8],
    ) {
        for action in self.parser.parse(data) {
            self.terminal.apply(action);
        }
    }

    fn pending_output(&mut self) -> Vec<u8> {
        self.terminal.take_pending_output()
    }

    fn row_text(
        &self,
        row: u32,
    ) -> String {
        let r = self.terminal.visible_row(row);
        let mut s = String::new();
        for cell in &r.cells {
            s.push_str(cell);
        }
        s
    }

    fn cell_char(
        &self,
        row: u32,
        col: u32,
    ) -> char {
        let r = self.terminal.visible_row(row);
        r.cells[col as usize].as_str().chars().next().unwrap_or(' ')
    }

    fn cursor(&self) -> (u32, u32) {
        (
            self.terminal.active.cursor.row,
            self.terminal.active.cursor.col,
        )
    }
}

// ---------------------------------------------------------------------------
// 1. Cursor movement
// ---------------------------------------------------------------------------

#[test]
fn cup_positions_cursor_one_based() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;10H");
    assert_eq!(t.cursor(), (4, 9));
}

#[test]
fn cup_defaults_to_home() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;10H");
    t.process(b"\x1b[H");
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn cup_clamps_to_viewport() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[999;999H");
    assert_eq!(t.cursor(), (23, 79));
}

#[test]
fn cuu_cud_cuf_cub_move_relative() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[10;10H"); // row 9, col 9
    t.process(b"\x1b[3A"); // up 3 → row 6
    assert_eq!(t.cursor(), (6, 9));
    t.process(b"\x1b[2B"); // down 2 → row 8
    assert_eq!(t.cursor(), (8, 9));
    t.process(b"\x1b[4C"); // right 4 → col 13
    assert_eq!(t.cursor(), (8, 13));
    t.process(b"\x1b[5D"); // left 5 → col 8
    assert_eq!(t.cursor(), (8, 8));
}

#[test]
fn cnl_cpl_move_and_home_column() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;20H"); // row 4, col 19
    t.process(b"\x1b[2E"); // CNL 2: down 2, col 0
    assert_eq!(t.cursor(), (6, 0));
    t.process(b"\x1b[10;40H");
    t.process(b"\x1b[3F"); // CPL 3: up 3, col 0
    assert_eq!(t.cursor(), (6, 0));
}

#[test]
fn pending_wrap_cancelled_by_cursor_movement() {
    let mut t = VtTerm::new_80x24();
    // Fill row 0 to put cursor in pending wrap (col == 80)
    t.process(b"\x1b[H");
    for _ in 0..80 {
        t.process(b"X");
    }
    assert_eq!(t.terminal.active.cursor.col, 80);
    // CUB should cancel pending wrap then move back
    t.process(b"\x1b[D");
    assert_eq!(t.terminal.active.cursor.col, 78);
}

#[test]
fn pending_wrap_cancelled_by_erase() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[H");
    for _ in 0..80 {
        t.process(b"A");
    }
    // ED 0 from pending wrap should erase the last column
    t.process(b"\x1b[J");
    assert_eq!(t.cell_char(0, 79), ' ');
    assert_eq!(t.cell_char(0, 78), 'A');
}

// ---------------------------------------------------------------------------
// 2. Scroll regions
// ---------------------------------------------------------------------------

#[test]
fn decstbm_sets_region_and_homes_cursor() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;10H"); // move away from home
    t.process(b"\x1b[5;20r"); // scroll region rows 5-20
    assert_eq!(t.terminal.active.scroll_top, 4);
    assert_eq!(t.terminal.active.scroll_bottom, 19);
    assert_eq!(t.cursor(), (0, 0)); // cursor homed
}

#[test]
fn lf_scrolls_within_region() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;6r"); // 2-line scroll region (rows 5-6, 1-based)
    t.process(b"\x1b[5;1H"); // position at region top
    t.process(b"Line1\r\nLine2\r\n"); // scroll within region
    // Line1 should have scrolled off, Line2 on first region row
    let row4 = t.row_text(4); // row 5 (1-based) = row 4 (0-based)
    assert!(row4.starts_with("Line2"));
}

#[test]
fn ind_at_scroll_bottom_scrolls_region() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[10;15r"); // region rows 10-15
    t.process(b"\x1b[15;1H"); // bottom of region
    t.process(b"bottom");
    t.process(b"\x1b[15;1H"); // back to bottom
    t.process(b"\x1bD"); // IND — should scroll region up
    // cursor stays at row 14 (0-based), region scrolled
    assert_eq!(t.terminal.active.cursor.row, 14);
}

#[test]
fn ri_at_scroll_top_scrolls_region_down() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[10;15r"); // region rows 10-15
    t.process(b"\x1b[10;1H"); // top of region
    t.process(b"\x1bM"); // RI — should scroll region down
    assert_eq!(t.terminal.active.cursor.row, 9);
}

// ---------------------------------------------------------------------------
// 3. Origin mode (DECOM)
// ---------------------------------------------------------------------------

#[test]
fn origin_mode_cup_relative_to_region() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;20r"); // region rows 5-20
    t.process(b"\x1b[?6h"); // origin mode ON
    t.process(b"\x1b[1;1H"); // should be row 5 (0-based: 4), col 0
    assert_eq!(t.cursor(), (4, 0));
    t.process(b"\x1b[3;5H"); // row 3 relative = row 7 (0-based: 6)
    assert_eq!(t.cursor(), (6, 4));
}

#[test]
fn origin_mode_cud_clamps_to_region() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[12;13r"); // tiny 2-line region
    t.process(b"\x1b[?6h"); // origin mode ON
    t.process(b"\x1b[99B"); // CUD 99 — should clamp to scroll_bottom
    assert_eq!(t.terminal.active.cursor.row, 12); // row 13 (1-based) = 12 (0-based)
}

#[test]
fn origin_mode_decstbm_homes_to_region_top() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[?6h"); // origin mode ON
    t.process(b"\x1b[20;23r"); // region rows 20-23
    assert_eq!(t.terminal.active.cursor.row, 19); // homed to scroll_top
}

// ---------------------------------------------------------------------------
// 4. DECALN (Screen Alignment Display)
// ---------------------------------------------------------------------------

#[test]
fn decaln_fills_screen_with_e() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b#8");
    for row in 0..24 {
        let text = t.row_text(row);
        assert!(
            text.chars().all(|c| c == 'E'),
            "row {} not all E: {:?}",
            row,
            &text[..10]
        );
    }
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn decaln_resets_scroll_region() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;10r"); // restricted region
    t.process(b"\x1b#8"); // DECALN resets it
    assert_eq!(t.terminal.active.scroll_top, 0);
    assert_eq!(t.terminal.active.scroll_bottom, 23);
}

// ---------------------------------------------------------------------------
// 5. DECCOLM (80/132 column mode)
// ---------------------------------------------------------------------------

#[test]
fn deccolm_set_resizes_to_132() {
    let mut t = VtTerm::new_80x24();
    // Mode 40 must be enabled before DECCOLM is honoured.
    t.process(b"\x1b[?40h\x1b[?3h");
    assert_eq!(t.terminal.viewport.cols, 132);
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn deccolm_reset_restores_80() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[?40h");
    t.process(b"\x1b[?3h");
    t.process(b"\x1b[?3l");
    assert_eq!(t.terminal.viewport.cols, 80);
}

// ---------------------------------------------------------------------------
// 6. Erase operations
// ---------------------------------------------------------------------------

#[test]
fn ed_0_erases_from_cursor_to_end() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b#8"); // fill with E
    t.process(b"\x1b[12;40H"); // middle of screen
    t.process(b"\x1b[0J"); // erase below
    // Cell before cursor should still be E
    assert_eq!(t.cell_char(11, 38), 'E');
    // Cell at cursor and after should be space
    assert_eq!(t.cell_char(11, 39), ' ');
    assert_eq!(t.cell_char(23, 79), ' ');
}

#[test]
fn ed_1_erases_from_start_to_cursor() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b#8"); // fill with E
    t.process(b"\x1b[12;40H");
    t.process(b"\x1b[1J"); // erase above
    assert_eq!(t.cell_char(0, 0), ' ');
    assert_eq!(t.cell_char(11, 39), ' '); // inclusive
    assert_eq!(t.cell_char(11, 40), 'E'); // after cursor
}

#[test]
fn ed_2_erases_entire_screen() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b#8");
    t.process(b"\x1b[2J");
    for row in 0..24 {
        assert_eq!(t.cell_char(row, 0), ' ');
    }
}

#[test]
fn el_0_erases_to_end_of_line() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b#8");
    t.process(b"\x1b[1;40H");
    t.process(b"\x1b[0K");
    assert_eq!(t.cell_char(0, 38), 'E');
    assert_eq!(t.cell_char(0, 39), ' ');
    assert_eq!(t.cell_char(0, 79), ' ');
}

#[test]
fn el_2_erases_entire_line() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b#8");
    t.process(b"\x1b[5;1H\x1b[2K");
    assert!(t.row_text(4).chars().all(|c| c == ' '));
    // Adjacent rows untouched
    assert_eq!(t.cell_char(3, 0), 'E');
    assert_eq!(t.cell_char(5, 0), 'E');
}

// ---------------------------------------------------------------------------
// 7. Insert / Delete operations
// ---------------------------------------------------------------------------

#[test]
fn ich_inserts_blank_characters() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[HABCDEF");
    t.process(b"\x1b[1;3H"); // col 2 (0-based)
    t.process(b"\x1b[2@"); // insert 2 blanks
    assert_eq!(t.cell_char(0, 0), 'A');
    assert_eq!(t.cell_char(0, 1), 'B');
    assert_eq!(t.cell_char(0, 2), ' ');
    assert_eq!(t.cell_char(0, 3), ' ');
    assert_eq!(t.cell_char(0, 4), 'C');
}

#[test]
fn dch_deletes_characters() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[HABCDEF");
    t.process(b"\x1b[1;3H"); // col 2
    t.process(b"\x1b[2P"); // delete 2
    assert_eq!(t.cell_char(0, 0), 'A');
    assert_eq!(t.cell_char(0, 1), 'B');
    assert_eq!(t.cell_char(0, 2), 'E');
    assert_eq!(t.cell_char(0, 3), 'F');
}

#[test]
fn il_inserts_blank_lines() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;1Hrow1\x1b[2;1Hrow2\x1b[3;1Hrow3");
    t.process(b"\x1b[2;1H\x1b[1L"); // insert 1 line at row 2
    assert!(t.row_text(0).starts_with("row1"));
    assert!(t.row_text(1).starts_with("    ")); // blank inserted line
    assert!(t.row_text(2).starts_with("row2"));
}

#[test]
fn dl_deletes_lines() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;1Hrow1\x1b[2;1Hrow2\x1b[3;1Hrow3");
    t.process(b"\x1b[2;1H\x1b[1M"); // delete 1 line at row 2
    assert!(t.row_text(0).starts_with("row1"));
    assert!(t.row_text(1).starts_with("row3"));
}

// ---------------------------------------------------------------------------
// 8. Tab stops
// ---------------------------------------------------------------------------

#[test]
fn default_tab_stops_every_8_columns() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[H\t");
    assert_eq!(t.terminal.active.cursor.col, 8);
    t.process(b"\t");
    assert_eq!(t.terminal.active.cursor.col, 16);
}

#[test]
fn hts_sets_custom_tab_stop() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;5H"); // col 4 (0-based)
    t.process(b"\x1bH"); // HTS
    t.process(b"\x1b[1;1H\t"); // tab from col 0
    assert_eq!(t.terminal.active.cursor.col, 4);
}

#[test]
fn tbc_3_clears_all_tab_stops() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[3g"); // clear all
    t.process(b"\x1b[H\t"); // tab with no stops → last col
    assert_eq!(t.terminal.active.cursor.col, 79);
}

// ---------------------------------------------------------------------------
// 9. DEC Special Graphics (SCS)
// ---------------------------------------------------------------------------

#[test]
fn scs_g0_drawing_translates_box_chars() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b(0"); // G0 = DEC drawing
    t.process(b"\x1b[Hlqqk"); // ┌──┐
    assert_eq!(t.cell_char(0, 0), '\u{250C}'); // ┌
    assert_eq!(t.cell_char(0, 1), '\u{2500}'); // ─
    assert_eq!(t.cell_char(0, 2), '\u{2500}'); // ─
    assert_eq!(t.cell_char(0, 3), '\u{2510}'); // ┐
}

#[test]
fn scs_g0_ascii_restores_normal() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b(0"); // drawing
    t.process(b"\x1b(B"); // back to ASCII
    t.process(b"\x1b[Hq"); // should be literal 'q'
    assert_eq!(t.cell_char(0, 0), 'q');
}

// ---------------------------------------------------------------------------
// 10. Device queries
// ---------------------------------------------------------------------------

#[test]
fn da1_responds() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[c");
    let out = t.pending_output();
    assert!(out.starts_with(b"\x1b[?63;"));
}

#[test]
fn da2_responds() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[>c");
    let out = t.pending_output();
    assert!(out.starts_with(b"\x1b[>41;"));
}

#[test]
fn dsr_cursor_position_report() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;10H");
    t.process(b"\x1b[6n");
    let out = t.pending_output();
    assert_eq!(out, b"\x1b[5;10R");
}

#[test]
fn decrqm_reports_known_modes() {
    let mut t = VtTerm::new_80x24();
    // Mode 25 (cursor visible) default = set
    t.process(b"\x1b[?25$p");
    assert_eq!(t.pending_output(), b"\x1b[?25;1$y");
    // Mode 7 (autowrap) default = set
    t.process(b"\x1b[?7$p");
    assert_eq!(t.pending_output(), b"\x1b[?7;1$y");
}

// ---------------------------------------------------------------------------
// 10b. iTerm2 OSC 1337 ReportCellSize
// ---------------------------------------------------------------------------

#[test]
fn osc_1337_report_cell_size() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b]1337;ReportCellSize\x1b\\");
    let out = t.pending_output();
    // Response: OSC 1337 ; ReportCellSize=<h>;<w> ST
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.starts_with("\x1b]1337;ReportCellSize="),
        "unexpected response: {s:?}"
    );
    assert!(s.ends_with("\x1b\\"));
}

// ---------------------------------------------------------------------------
// 11. DECCKM (Application Cursor Keys)
// ---------------------------------------------------------------------------

#[test]
fn decckm_tracked_by_screen() {
    let mut t = VtTerm::new_80x24();
    assert!(!t.terminal.active.app_cursor_keys);
    t.process(b"\x1b[?1h");
    assert!(t.terminal.active.app_cursor_keys);
    t.process(b"\x1b[?1l");
    assert!(!t.terminal.active.app_cursor_keys);
}

// ---------------------------------------------------------------------------
// 12. Title stacking
// ---------------------------------------------------------------------------

#[test]
fn title_push_pop() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b]2;Original\x1b\\"); // set title
    assert_eq!(t.terminal.current_title.as_deref(), Some("Original"));
    t.process(b"\x1b[22;0t"); // push
    t.process(b"\x1b]2;Temporary\x1b\\"); // change
    assert_eq!(t.terminal.current_title.as_deref(), Some("Temporary"));
    t.process(b"\x1b[23;0t"); // pop
    assert_eq!(t.terminal.current_title.as_deref(), Some("Original"));
}

// ---------------------------------------------------------------------------
// 13. Private mode save/restore
// ---------------------------------------------------------------------------

#[test]
fn xtsave_xtrestore_round_trips_mode() {
    let mut t = VtTerm::new_80x24();
    // Default: autowrap on
    assert!(t.terminal.active.autowrap);
    t.process(b"\x1b[?7s"); // save mode 7
    t.process(b"\x1b[?7l"); // disable autowrap
    assert!(!t.terminal.active.autowrap);
    t.process(b"\x1b[?7r"); // restore → should be back on
    assert!(t.terminal.active.autowrap);
}

// ---------------------------------------------------------------------------
// 14. Insert mode (IRM)
// ---------------------------------------------------------------------------

#[test]
fn insert_mode_shifts_text_right() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[HABCD");
    t.process(b"\x1b[4h"); // enable IRM
    t.process(b"\x1b[1;3HXX"); // insert XX at col 2
    assert_eq!(t.cell_char(0, 0), 'A');
    assert_eq!(t.cell_char(0, 1), 'B');
    assert_eq!(t.cell_char(0, 2), 'X');
    assert_eq!(t.cell_char(0, 3), 'X');
    assert_eq!(t.cell_char(0, 4), 'C');
    assert_eq!(t.cell_char(0, 5), 'D');
}

// ---------------------------------------------------------------------------
// 15. DECAWM (Auto-wrap mode)
// ---------------------------------------------------------------------------

#[test]
fn decawm_off_prevents_wrap() {
    let mut t = VtTerm::new(10, 4);
    t.process(b"\x1b[?7l"); // disable autowrap
    t.process(b"\x1b[H");
    t.process(b"abcdefghijXY"); // more than 10 cols
    assert_eq!(t.terminal.active.cursor.row, 0); // no wrap
    assert_eq!(t.cell_char(0, 9), 'Y'); // last col overwritten
}

// ---------------------------------------------------------------------------
// 16. LNM (Newline mode)
// ---------------------------------------------------------------------------

#[test]
fn lnm_lf_implies_cr() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[20h"); // enable LNM
    t.process(b"\x1b[1;10H"); // col 9
    t.process(b"\n");
    assert_eq!(t.cursor(), (1, 0)); // col reset to 0
}

// ---------------------------------------------------------------------------
// 17. NEL / IND / RI
// ---------------------------------------------------------------------------

#[test]
fn nel_moves_to_col_0_next_line() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;10H");
    t.process(b"\x1bE"); // NEL
    assert_eq!(t.cursor(), (1, 0));
}

#[test]
fn ind_preserves_column() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;10H");
    t.process(b"\x1bD"); // IND
    assert_eq!(t.cursor(), (1, 9)); // column preserved
}

#[test]
fn ri_moves_up_preserving_column() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[5;10H");
    t.process(b"\x1bM"); // RI
    assert_eq!(t.cursor(), (3, 9));
}

// ---------------------------------------------------------------------------
// 18. VT/FF treated as LF
// ---------------------------------------------------------------------------

#[test]
fn vt_moves_down_like_lf() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;1H");
    t.process(b"\x0b"); // VT
    assert_eq!(t.terminal.active.cursor.row, 1);
}

#[test]
fn ff_moves_down_like_lf() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[1;1H");
    t.process(b"\x0c"); // FF
    assert_eq!(t.terminal.active.cursor.row, 1);
}

// ---------------------------------------------------------------------------
// 19. Control characters inside ESC sequences
// ---------------------------------------------------------------------------

#[test]
fn bs_inside_csi_executes_and_sequence_completes() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b[10;10H"); // row 9, col 9
    // CUF with embedded BS: CSI 2 BS C
    // BS executes (col 9→8), then CUF 2 fires (col 8→10)
    t.process(b"\x1b[2\x08C");
    assert_eq!(t.terminal.active.cursor.col, 10);
}

// ---------------------------------------------------------------------------
// 20. OSC color queries
// ---------------------------------------------------------------------------

#[test]
fn osc_10_query_returns_fg() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b]10;?\x1b\\");
    let out = t.pending_output();
    assert!(out.starts_with(b"\x1b]10;rgb:"));
}

#[test]
fn osc_11_query_returns_bg() {
    let mut t = VtTerm::new_80x24();
    t.process(b"\x1b]11;?\x1b\\");
    let out = t.pending_output();
    assert!(out.starts_with(b"\x1b]11;rgb:"));
}

// ---------------------------------------------------------------------------
// 21. vttest screen 2 simulation (DECALN + borders)
// ---------------------------------------------------------------------------

#[test]
fn vttest_screen2_star_borders() {
    let mut t = VtTerm::new_80x24();

    // DECCOLM reset (clears screen)
    t.process(b"\x1b[?3l");
    // DECALN
    t.process(b"\x1b#8");

    // Selective erases for E frame (inner_l=10, inner_r=71)
    t.process(b"\x1b[9;10H\x1b[1J");
    t.process(b"\x1b[18;60H\x1b[0J\x1b[1K");
    t.process(b"\x1b[9;71H\x1b[0K");
    for row in 10..=16 {
        t.process(format!("\x1b[{};10H\x1b[1K\x1b[{};71H\x1b[0K", row, row).as_bytes());
    }
    t.process(b"\x1b[17;30H\x1b[2K");

    // * top/bottom border
    for col in 1..=80u32 {
        t.process(format!("\x1b[24;{}f*\x1b[1;{}f*", col, col).as_bytes());
    }

    // + left border with IND
    t.process(b"\x1b[2;2H");
    for _ in 2..=23u32 {
        t.process(b"+\x1b[D\x1bD");
    }

    // + right border with RI
    t.process(b"\x1b[23;79H");
    for _ in (2..=23u32).rev() {
        t.process(b"+\x1b[D\x1bM");
    }

    // * left/right column
    t.process(b"\x1b[2;1H");
    for row in 2..=23u32 {
        t.process(b"*");
        t.process(format!("\x1b[{};80H", row).as_bytes());
        t.process(b"*\x1b[10D");
        if row < 10 {
            t.process(b"\x1bE");
        } else {
            t.process(b"\r\n");
        }
    }

    // Verify borders
    // Row 0 and 23 should be all *'s
    for col in 0..80 {
        assert_eq!(t.cell_char(0, col), '*', "top border col {col}");
        assert_eq!(t.cell_char(23, col), '*', "bottom border col {col}");
    }
    // Rows 1-22 should have *+...+*
    for row in 1..=22u32 {
        assert_eq!(t.cell_char(row, 0), '*', "row {row} left *");
        assert_eq!(t.cell_char(row, 1), '+', "row {row} left +");
        assert_eq!(t.cell_char(row, 78), '+', "row {row} right +");
        assert_eq!(t.cell_char(row, 79), '*', "row {row} right *");
    }
}

// ---------------------------------------------------------------------------
// DEC line attributes (ESC # 3/4/5/6)
// ---------------------------------------------------------------------------

/// ESC#6 sets DoubleWidth, ESC#3/4 set DoubleHeightTop/Bottom, ESC#5 resets.
#[test]
fn dec_line_attrs_set_and_clear() {
    let mut t = VtTerm::new_80x24();
    // Row 2 (0-based): double-width single-height
    t.process(b"\x1b[3;1H\x1b#6");
    // Row 3: double-height top half
    t.process(b"\x1b[4;1H\x1b#3");
    // Row 4: double-height bottom half
    t.process(b"\x1b[5;1H\x1b#4");
    // Row 5: reset to normal via ESC#5
    t.process(b"\x1b[6;1H\x1b#5");

    assert_eq!(t.terminal.visible_row(2).line_attr, LineAttr::DoubleWidth);
    assert_eq!(
        t.terminal.visible_row(3).line_attr,
        LineAttr::DoubleHeightTop
    );
    assert_eq!(
        t.terminal.visible_row(4).line_attr,
        LineAttr::DoubleHeightBottom
    );
    assert_eq!(t.terminal.visible_row(5).line_attr, LineAttr::Normal);
}

/// ESC#3 on the top row followed immediately by ESC#4 on the next row
/// (the typical vttest double-height pair) preserves both attrs.
#[test]
fn dec_double_height_pair_survives_write() {
    let mut t = VtTerm::new_80x24();
    // Write text and apply ESC#3, then write same text and apply ESC#4.
    t.process(b"\x1b[4;1HThis is a Double-width-and-height line\x1b#3");
    t.process(b"\x1b[5;1HThis is a Double-width-and-height line\x1b#4");

    assert_eq!(
        t.terminal.visible_row(3).line_attr,
        LineAttr::DoubleHeightTop,
        "row 3 should be DoubleHeightTop"
    );
    assert_eq!(
        t.terminal.visible_row(4).line_attr,
        LineAttr::DoubleHeightBottom,
        "row 4 should be DoubleHeightBottom"
    );
}
