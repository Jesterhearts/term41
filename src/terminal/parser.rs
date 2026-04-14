use smol_str::SmolStr;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::terminal::color::apply_sgr;
use crate::terminal::grid::Viewport;
use crate::terminal::row::Row;
use crate::terminal::screen::Screen;
use crate::vte;

/// Sentinel for the second (and beyond) cell of a wide glyph. Distinct from
/// the default blank (`" "`) so neighbour cleanup can tell them apart.
fn continuation_cell() -> SmolStr {
    SmolStr::default()
}

fn blank_cell() -> SmolStr {
    SmolStr::new_inline(" ")
}

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
    s: SmolStr,
) {
    let raw_width = UnicodeWidthStr::width(s.as_str());

    // Fold only zero-width codepoints (combining marks, ZWJ, variation
    // selectors) into the prior anchor. Folding a *wide* codepoint into a
    // wide anchor would mean the host's wcswidth and our cursor disagree on
    // the cluster's width — e.g. `👨‍💻` is 4 cells per wcswidth (2+0+2) but
    // folding would advance our cursor by only 2, so every subsequent
    // redraw lands two columns off and backspace walks into the prompt.
    // Keeping each wide codepoint in its own cell range matches wcswidth;
    // the font shaper still sees the ZWJ sequence in `row_text` (empty
    // continuations contribute 0 bytes) and renders the ligature if the
    // font has one.
    if raw_width == 0 {
        if try_extend_prev_cell(screen, viewport, &s) {
            screen.offset = 0;
        }
        return;
    }

    let width = raw_width.max(1);

    // Soft-wrap when the incoming cluster (possibly wide) would overhang the
    // right edge. The existing pre-wrap also covers the cursor-past-end case
    // left behind by the previous character.
    if screen.cursor.col + width as u32 > viewport.cols {
        soft_wrap(screen, viewport);
    }

    // New output resets the viewport to the live edge.
    screen.offset = 0;

    let fg = screen.fg;
    let bg = screen.bg;
    let link = screen.current_hyperlink;
    let r = screen.grid.active_row_index(&screen.cursor, viewport);
    let col = screen.cursor.col as usize;

    // Preserve the "a cell is a continuation iff its left neighbour is a wide
    // anchor" invariant by blanking any wide-anchor/continuation pair the new
    // write would sever. See design note: we only fix this at put_char, not
    // at clear/erase/reflow.
    break_wide_glyphs_around_write(&mut screen.grid.rows[r], col, width);

    screen.grid.rows[r].cells[col] = s;
    screen.grid.rows[r].fg[col] = fg;
    screen.grid.rows[r].bg[col] = bg;
    screen.grid.rows[r].links[col] = link;
    for i in 1..width {
        screen.grid.rows[r].cells[col + i] = continuation_cell();
        screen.grid.rows[r].fg[col + i] = fg;
        screen.grid.rows[r].bg[col + i] = bg;
        screen.grid.rows[r].links[col + i] = link;
    }
    screen.cursor.col += width as u32;
}

/// True if the cell at `col` is the anchor of a wide glyph — it holds
/// non-blank text and its right neighbour is the empty continuation
/// sentinel we placed when laying out the wide glyph. Consulting the grid
/// state is more robust than re-measuring the cell text: `unicode-width`
/// disagrees with glibc `wcswidth` on VS16-upgraded emoji (e.g. `❤️`, which
/// `unicode-width` reports as width 2 but `wcswidth` reports as 1), and we
/// keep such clusters single-cell to stay in sync with the shell's cursor
/// tracking. Checking neighbour emptiness reflects the physical invariant.
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

/// Keep the wide-anchor/continuation invariant intact across an overwrite.
/// Left edge: if the cell to our left was a wide anchor, our write lands on
/// its continuation, so blank the orphaned anchor. Right edge: if the last
/// cell we're writing *is* a wide anchor, its continuation (at `col + width`)
/// won't be touched by the write and would dangle, so blank it.
fn break_wide_glyphs_around_write(
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
    let r = screen.grid.active_row_index(&screen.cursor, viewport);
    screen.grid.rows[r].wrapped = true;
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
    }
}

/// If appending `s` to the previously-written cell keeps it a single grapheme
/// cluster, do so and return `true`. Walks past continuation cells so a
/// combining mark or ZWJ piece folds into the wide anchor it visually
/// decorates, not the empty continuation sitting between them.
fn try_extend_prev_cell(
    screen: &mut Screen,
    viewport: &Viewport,
    s: &str,
) -> bool {
    let (prev_row, mut prev_col) = if screen.cursor.col > 0 && screen.cursor.col <= viewport.cols {
        let row = screen.grid.active_row_index(&screen.cursor, viewport);
        (row, (screen.cursor.col - 1) as usize)
    } else if screen.cursor.col == 0 {
        let row = screen.grid.active_row_index(&screen.cursor, viewport);
        if row == 0 || !screen.grid.rows[row].wrapped {
            return false;
        }
        let prev_row = row - 1;
        let last_col = screen.grid.rows[prev_row].cells.len().saturating_sub(1);
        (prev_row, last_col)
    } else {
        return false;
    };

    // Skip wide-glyph continuation cells to reach the anchor.
    while prev_col > 0 && screen.grid.rows[prev_row].cells[prev_col].is_empty() {
        prev_col -= 1;
    }

    let prev = &screen.grid.rows[prev_row].cells[prev_col];
    if prev.as_str() == " " || prev.is_empty() {
        return false;
    }

    // Fold without widening the cell. VS16 etc. can bump `unicode-width` on
    // the combined string (e.g. `❤` + `VS16` → 2), but glibc `wcswidth` —
    // which the host shell uses to track cursor columns — still reports 1.
    // Matching wcswidth keeps backspace/cursor-movement in sync with
    // readline; `is_wide_anchor_at` looks at the grid state (continuation
    // cell to the right) rather than re-measuring this text, so the next
    // write won't misidentify the cell as a wide anchor and blank it.
    let mut combined = String::with_capacity(prev.len() + s.len());
    combined.push_str(prev);
    combined.push_str(s);
    if combined.graphemes(true).count() != 1 {
        return false;
    }

    screen.grid.rows[prev_row].cells[prev_col] = SmolStr::new(&combined);
    true
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
            screen
                .grid
                .erase_in_display(&screen.cursor, viewport, &mut screen.images, mode);
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
                let top = cursor.row;
                screen.grid.scroll_down_in_region(
                    viewport,
                    &mut screen.images,
                    top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'M' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= screen.scroll_top && cursor.row <= screen.scroll_bottom {
                let top = cursor.row;
                screen.grid.scroll_up_in_region(
                    viewport,
                    &mut screen.images,
                    top,
                    screen.scroll_bottom,
                    n,
                );
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
                    &mut screen.images,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'T' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen.grid.scroll_down_in_region(
                viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                n,
            );
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
                    &mut screen.images,
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
                Action::Print(s) => put_char(screen, viewport, s),
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
        let mut s = String::new();
        for cell in &screen.grid.rows[r].cells {
            s.push_str(cell);
        }
        s
    }

    // -- put_char -----------------------------------------------------------

    #[test]
    fn put_char_writes_with_current_colors_and_advances() {
        let (mut screen, viewport) = setup();
        screen.fg = Srgb::new(1, 2, 3);
        screen.bg = Srgb::new(4, 5, 6);

        put_char(&mut screen, &viewport, SmolStr::new_inline("A"));

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
        put_char(&mut screen, &viewport, SmolStr::new_inline("x"));
        assert_eq!(screen.offset, 0);
    }

    #[test]
    fn put_char_folds_combining_mark_into_previous_cell() {
        let (mut screen, viewport) = setup();
        // U+0301 COMBINING ACUTE ACCENT — feeding "e" then the combining mark
        // should store the full grapheme "é" in one cell without advancing.
        feed("e\u{0301}".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "e\u{0301}");
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_vs16_emoji_stays_in_single_cell() {
        let (mut screen, viewport) = setup();
        // `UnicodeWidthStr::width("❤\u{FE0F}") == 2`, but glibc `wcswidth`
        // reports 1 because it treats VS16 as a zero-width variation
        // selector without upgrading the base to emoji presentation. The
        // host shell tracks cursor position via wcswidth, so our grid must
        // agree — otherwise a single backspace from readline lands on the
        // continuation cell and the user can't delete the emoji. Keep the
        // cluster in one cell; the shaper still sees the full cluster
        // text and renders it scaled to that cell.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{2764}\u{FE0F}");
        assert_eq!(
            screen.grid.rows[r].cells[1].as_str(),
            " ",
            "VS16 must not widen the cell — cells[1] stays blank"
        );
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_write_after_vs16_emoji_preserves_the_emoji() {
        // Reproduces the reported "heart vanishes when you type anything
        // after it" bug. Before the fix, `is_wide_anchor` re-measured the
        // cell text with `UnicodeWidthStr` — which returns 2 for "❤\u{FE0F}"
        // — so `break_wide_glyphs_around_write` treated the single-cell
        // emoji as a misaligned wide anchor and blanked it. The grid-state
        // check in `is_wide_anchor_at` looks at the right neighbour
        // instead, matching the physical layout.
        let (mut screen, viewport) = setup();
        feed("\u{2764}\u{FE0F}X".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(
            screen.grid.rows[r].cells[0].as_str(),
            "\u{2764}\u{FE0F}",
            "heart must survive subsequent write"
        );
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "X");
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn backspace_over_vs16_emoji_moves_one_column() {
        // Readline sends a single BS to rub out `❤\u{FE0F}` because glibc
        // `wcswidth` reports its width as 1. The cursor must land on the
        // anchor column so subsequent rub-out bytes (typically `\b \b`)
        // clear the cell cleanly; widening the cell into 2 columns would
        // leave the cursor sitting on the continuation after one BS and
        // desync the shell's tracking.
        let (mut screen, viewport) = setup();
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 1);

        execute(&mut screen, &viewport, BS);
        assert_eq!(screen.cursor.col, 0);

        // A full rub-out of `\b \b` from bash lands us back at col 0 with
        // the cell erased.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &viewport);
        feed(b"\x08 \x08", &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn put_char_regional_indicators_get_separate_cells() {
        let (mut screen, viewport) = setup();
        // `unicode-width` reports width 1 for each regional indicator, so
        // "🇺🇸" advances the cursor by 2 across two 1-col cells. We do not
        // collapse the flag pair into one cell — that would disagree with
        // the host's wcswidth and desync the cursor.
        feed("🇺🇸".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "🇺");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "🇸");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- wide (2-column) glyph handling ------------------------------------

    #[test]
    fn put_char_wide_glyph_occupies_two_cells_and_advances_cursor() {
        let (mut screen, viewport) = setup();
        feed("好".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), ""); // continuation
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn put_char_wide_glyph_soft_wraps_when_it_would_overhang() {
        let (mut screen, viewport) = setup();
        // Fill 9 of 10 columns with narrow chars so only 1 column is free.
        feed(b"abcdefghi", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 9);

        feed("好".as_bytes(), &mut screen, &viewport);

        // The wide glyph didn't fit at col 9, so we soft-wrap and place it
        // on the next row.
        assert_eq!(screen.cursor.row, 1);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.cursor.col, 2);
        assert!(screen.grid.rows[r - 1].wrapped);
    }

    #[test]
    fn put_char_narrow_overwriting_wide_anchor_blanks_continuation() {
        let (mut screen, viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &viewport);
        // Move cursor back to col 0 and stomp on the anchor with a narrow char.
        feed(b"\x1b[1;1H", &mut screen, &viewport);
        feed(b"x", &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "x");
        // The continuation at col 1 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_narrow_overwriting_wide_continuation_blanks_anchor() {
        let (mut screen, viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &viewport);
        // Park cursor on the continuation (col 1) and write a narrow char.
        feed(b"\x1b[1;2H", &mut screen, &viewport);
        feed(b"x", &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // The anchor at col 0 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "x");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_wide_overwriting_wide_blanks_both_neighbours() {
        let (mut screen, viewport) = setup();
        // [好, "", 世, "", a]
        feed("好世a".as_bytes(), &mut screen, &viewport);
        // Park on col 1 (好's continuation) and write a new wide glyph that
        // straddles the old layout.
        feed(b"\x1b[1;2H", &mut screen, &viewport);
        feed("界".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // 好's anchor (col 0) is orphaned — blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        // New wide glyph at cols 1-2.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "界");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "");
        // 世's orphaned continuation (at col 3) is blanked.
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[4].as_str(), "a");
    }

    #[test]
    fn put_char_zwj_emoji_keeps_components_in_separate_wide_cells() {
        let (mut screen, viewport) = setup();
        // 👨‍💻 = 👨 ZWJ 💻. wcswidth = 2+0+2 = 4, so the shell expects the
        // cursor to advance by 4. The ZWJ folds into `👨` (width 0 → fold),
        // but the second emoji starts a new wide cell of its own. The font
        // shaper still sees the full ZWJ sequence in `row_text` and renders
        // the ligature if the font has one.
        feed("👨\u{200D}💻".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👨\u{200D}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "💻");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "");
        assert_eq!(screen.cursor.col, 4);
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
