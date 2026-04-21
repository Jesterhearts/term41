use std::collections::VecDeque;

use palette::Srgb;

use crate::screen::row::Row;

mod edit;
mod rect;
mod reflow;
mod scroll;

pub(crate) use self::edit::delete_chars as delete_chars_op;
pub(crate) use self::edit::erase_chars as erase_chars_op;
pub(crate) use self::edit::erase_in_display as erase_in_display_op;
pub(crate) use self::edit::erase_in_display_selective as erase_in_display_selective_op;
pub(crate) use self::edit::erase_in_line as erase_in_line_op;
pub(crate) use self::edit::erase_in_line_selective as erase_in_line_selective_op;
pub(crate) use self::edit::insert_chars as insert_chars_op;
pub(crate) use self::rect::change_attrs_rect as change_attrs_rect_op;
pub(crate) use self::rect::copy_rect as copy_rect_op;
pub(crate) use self::rect::erase_rect as erase_rect_op;
pub(crate) use self::rect::erase_rect_selective as erase_rect_selective_op;
pub(crate) use self::rect::fill_rect as fill_rect_op;
pub(crate) use self::rect::reverse_attrs_rect as reverse_attrs_rect_op;
pub(crate) use self::reflow::reflow as reflow_op;
pub(crate) use self::scroll::delete_cols as delete_cols_op;
pub(crate) use self::scroll::insert_cols as insert_cols_op;
pub(crate) use self::scroll::scroll_down_in_rect as scroll_down_in_rect_op;
pub(crate) use self::scroll::scroll_down_in_region as scroll_down_in_region_op;
pub(crate) use self::scroll::scroll_left as scroll_left_op;
pub(crate) use self::scroll::scroll_right as scroll_right_op;
pub(crate) use self::scroll::scroll_up_in_rect as scroll_up_in_rect_op;
#[cfg(test)]
pub(crate) use self::scroll::scroll_up_in_region as scroll_up_in_region_op;
pub(crate) use self::scroll::scroll_up_in_region_with_scrollback_policy as scroll_up_in_region_with_scrollback_policy_op;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub col: u32,
    pub row: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AttrChangeExtent {
    #[default]
    Stream,
    Rectangle,
}

/// Dimensions of the rendered terminal window, shared by both screens.
/// Per-screen state (scroll region, scrollback offset) lives on
/// [`super::Screen`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Viewport {
    pub rows: u32,
    pub cols: u32,
    /// Local-row index of the top visible row inside the backing grid.
    pub top: usize,
}

impl Viewport {
    pub fn top_index(
        &self,
        total_rows: usize,
    ) -> usize {
        self.top.min(total_rows.saturating_sub(self.rows as usize))
    }
}

#[derive(Debug)]
pub struct Grid {
    pub rows: VecDeque<Row>,
    pub scrollback_limit: u32,
    /// Running count of rows popped from the front (for image position
    /// tracking).
    pub total_popped: usize,
    /// Default foreground color for new / cleared cells (from palette).
    pub default_fg: Srgb<u8>,
    /// Default background color for new / cleared cells (from palette).
    pub default_bg: Srgb<u8>,
}

impl Grid {
    pub fn scrollback_len(
        &self,
        viewport: &Viewport,
    ) -> u32 {
        (self.rows.len() as u32).saturating_sub(viewport.rows)
    }

    pub fn push_visible_row(
        &mut self,
        viewport: &Viewport,
    ) {
        // Once the scrollback buffer is full, recycle the oldest row rather
        // than dropping its four Vec allocations and reallocating a fresh
        // row: during text-heavy output (e.g. `ls -laR`) this is a steady
        // state and the free/alloc pair shows up in profiles.
        let max_rows = viewport.rows as usize + self.scrollback_limit as usize;
        if self.rows.len() >= max_rows && max_rows > 0 {
            let mut recycled = self.rows.pop_front().expect("max_rows > 0");
            recycled.reset_for_reuse(viewport.cols, self.default_fg, self.default_bg);
            self.rows.push_back(recycled);
            self.total_popped += 1;
        } else {
            self.rows
                .push_back(Row::new(viewport.cols, self.default_fg, self.default_bg));
        }
    }

    pub fn active_row_index(
        &self,
        cursor: &Cursor,
        viewport: &Viewport,
    ) -> usize {
        viewport.top_index(self.rows.len()) + cursor.row as usize
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use palette::Srgb;

    use super::*;
    use crate::color::default_bg;
    use crate::color::default_fg;

    trait GridTestOps {
        fn reflow(
            &mut self,
            new_width: u32,
        );
        fn scroll_up_in_region(
            &mut self,
            viewport: &Viewport,
            images: &mut BTreeMap<u64, crate::image::PlacedImage>,
            top: u32,
            bottom: u32,
            n: u32,
        );
        fn scroll_down_in_region(
            &mut self,
            viewport: &Viewport,
            images: &mut BTreeMap<u64, crate::image::PlacedImage>,
            top: u32,
            bottom: u32,
            n: u32,
        );
    }

    impl GridTestOps for Grid {
        fn reflow(
            &mut self,
            new_width: u32,
        ) {
            reflow_op(self, new_width);
        }

        fn scroll_up_in_region(
            &mut self,
            viewport: &Viewport,
            images: &mut BTreeMap<u64, crate::image::PlacedImage>,
            top: u32,
            bottom: u32,
            n: u32,
        ) {
            scroll_up_in_region_op(self, viewport, images, top, bottom, n);
        }

        fn scroll_down_in_region(
            &mut self,
            viewport: &Viewport,
            images: &mut BTreeMap<u64, crate::image::PlacedImage>,
            top: u32,
            bottom: u32,
            n: u32,
        ) {
            scroll_down_in_region_op(self, viewport, images, top, bottom, n);
        }
    }

    /// Build a grid from `(text, wrapped)` pairs. Each row is padded to `width`
    /// with spaces.
    fn make_grid(
        width: u32,
        rows: &[(&str, bool)],
    ) -> Grid {
        let mut grid_rows = VecDeque::new();
        for &(text, wrapped) in rows {
            let mut row = Row::new(width, default_fg(), default_bg());
            for (i, ch) in text.chars().enumerate() {
                if i < width as usize {
                    row.cells[i] = char_cell(ch);
                }
            }
            row.wrapped = wrapped;
            grid_rows.push_back(row);
        }
        Grid {
            rows: grid_rows,
            scrollback_limit: 1000,
            total_popped: 0,
            default_fg: default_fg(),
            default_bg: default_bg(),
        }
    }

    fn char_cell(ch: char) -> smol_str::SmolStr {
        let mut buf = [0u8; 4];
        smol_str::SmolStr::new_inline(ch.encode_utf8(&mut buf))
    }

    fn row_chars(row: &Row) -> String {
        let mut s = String::new();
        for cell in &row.cells {
            s.push_str(cell);
        }
        s
    }

    // ── Reflow: grow with no wrapping ───────────────────────────────

    #[test]
    fn reflow_grow_no_wrapping() {
        let mut grid = make_grid(3, &[("abc", false), ("def", false)]);
        grid.reflow(5);
        assert_eq!(row_chars(&grid.rows[0]), "abc  ");
        assert_eq!(row_chars(&grid.rows[1]), "def  ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_same_width_is_noop() {
        let mut grid = make_grid(4, &[("abcd", false), ("efgh", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert_eq!(grid.rows.len(), 2);
    }

    // ── Reflow: grow merges wrapped rows ────────────────────────────

    #[test]
    fn reflow_grow_merges_two_wrapped_rows() {
        // "abcdef" soft-wrapped at width 3 into two rows.
        let mut grid = make_grid(
            3,
            &[
                ("abc", true),
                ("def", false), // continuation
            ],
        );
        // Growing to 6 should merge them into one row.
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_merges_three_wrapped_rows() {
        // "abcdefghi" soft-wrapped at width 3.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(9);
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghi");
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_partial_merge() {
        // "abcdefghi" at width 3, grow to 5.
        // Should become two rows: "abcde" / "fghi_".
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(5);
        assert_eq!(row_chars(&grid.rows[0]), "abcde");
        assert_eq!(row_chars(&grid.rows[1]), "fghi ");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_grow_mixed_wrapped_and_unwrapped() {
        // Two logical lines: "abcdef" (wrapped) then "ghi" (not wrapped).
        let mut grid = make_grid(3, &[("abc", true), ("def", false), ("ghi", false)]);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert_eq!(row_chars(&grid.rows[1]), "ghi   ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_grow_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), then standalone "xx", then "ghijkl" (wrapped).
        let mut grid = make_grid(
            3,
            &[
                ("abc", true),
                ("def", false),
                ("xx ", false),
                ("ghi", true),
                ("jkl", false),
            ],
        );
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert_eq!(row_chars(&grid.rows[1]), "xx    ");
        assert_eq!(row_chars(&grid.rows[2]), "ghijkl");
        assert_eq!(grid.rows.len(), 3);
    }

    // ── Reflow: single row ──────────────────────────────────────────

    #[test]
    fn reflow_single_row_grow() {
        let mut grid = make_grid(3, &[("abc", false)]);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abc   ");
        assert_eq!(grid.rows.len(), 1);
    }

    // ── Reflow: grow collision ────────────────────────────────────

    #[test]
    fn reflow_grow_collision_preserves_line_boundary() {
        // "abcdef" (wrapped at width 3) then "ghi" (unwrapped). Grow to 4.
        // The collision on "def" must not merge content from "ghi".
        let mut grid = make_grid(3, &[("abc", true), ("def", false), ("ghi", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ef  ");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi ");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_grow_collision_continues_when_wrapped() {
        // "abcdefghi" at width 3, grow to 4. Collision on row 1 which IS
        // wrapped — merging should continue through the chain.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "i   ");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    // ── Reflow: shrink splits rows ─────────────────────────────────

    #[test]
    fn reflow_shrink_no_content_overflow() {
        // "abc" and "def" padded to width 6; trailing spaces discarded.
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_full_row() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_into_three() {
        let mut grid = make_grid(9, &[("abcdefghi", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_two_logical_lines() {
        let mut grid = make_grid(6, &[("abcdef", false), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert!(grid.rows[2].wrapped);
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_already_wrapped() {
        // "abcdefghijkl" soft-wrapped at width 6, shrink to 3.
        let mut grid = make_grid(6, &[("abcdef", true), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(grid.rows[2].wrapped);
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_uneven_split() {
        // 5 chars into width 3: "abcde" -> "abc" + "de "
        let mut grid = make_grid(5, &[("abcde", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "de ");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), standalone "xx", "ghijkl" (wrapped).
        let mut grid = make_grid(
            6,
            &[("abcdef", false), ("xx    ", false), ("ghijkl", false)],
        );
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "xx ");
        assert_eq!(row_chars(&grid.rows[3]), "ghi");
        assert_eq!(row_chars(&grid.rows[4]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert!(grid.rows[3].wrapped);
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);
    }

    #[test]
    fn reflow_shrink_pulls_from_continuation() {
        // "abcde" wrapped into "fg" — overflow "de" (len 2) should pull "f"
        // from the continuation row to produce "def".
        let mut grid = make_grid(5, &[("abcde", true), ("fg   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "g  ");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_pull_fully_consumes_next() {
        // Overflow "de" (len 2) pulls "f" from a single-char continuation,
        // fully consuming it.
        let mut grid = make_grid(5, &[("abcde", true), ("f    ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_pull_chains_through_main_loop() {
        // Multiple overflow rows each pull from the next continuation,
        // cascading through the main loop.
        let mut grid = make_grid(4, &[("abcd", true), ("efgh", true), ("ij  ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "j  ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_pull_preserves_colors() {
        // Color on the next row should land at the right position after pull.
        let mut grid = make_grid(5, &[("abcde", true), ("fg   ", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // 'f' is red
        grid.reflow(3);
        // "def" in row 1 — 'f' is at col 2.
        assert_eq!(grid.rows[1].cells[2], "f");
        assert_eq!(grid.rows[1].fg[2], red);
    }

    // ── Reflow: trailing space stripping ────────────────────────────

    #[test]
    fn reflow_grow_strips_trailing_spaces() {
        // "ab" with trailing padding on a wrapped row, then "cd".
        let mut grid = make_grid(5, &[("ab   ", true), ("cd   ", false)]);
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "ab   cd   ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_shrink_drops_trailing_space_overflow() {
        // Wrapped row where overflow portion is all spaces — no split needed.
        let mut grid = make_grid(6, &[("abc   ", true), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "   ");
        assert_eq!(row_chars(&grid.rows[2]), "def");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_grow_maintains_space() {
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abc   ");
        assert_eq!(row_chars(&grid.rows[1]), "def   ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_grow_roundtrip_with_trailing_spaces() {
        // Shrink then grow should recover original content, modulo trailing spaces.
        let mut grid = make_grid(10, &[("hello     ", true), ("world     ", false)]);
        grid.reflow(5);
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "hello     ");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "world     ");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    // ── Helpers for scroll region / push_visible_row tests ──────────

    fn make_viewport(
        rows: u32,
        cols: u32,
    ) -> Viewport {
        Viewport { rows, cols, top: 0 }
    }

    /// Build a grid with `scrollback` history rows + `visible` visible rows.
    /// Each row is labeled with a single char repeated to fill the width.
    fn make_grid_with_scrollback(
        width: u32,
        visible: u32,
        labels: &[char],
    ) -> (Grid, Viewport) {
        let mut vp = make_viewport(visible, width);
        vp.top = labels.len().saturating_sub(visible as usize);
        let mut rows = VecDeque::new();
        for &ch in labels {
            let mut row = Row::new(width, default_fg(), default_bg());
            for c in row.cells.iter_mut() {
                *c = char_cell(ch);
            }
            rows.push_back(row);
        }
        let grid = Grid {
            rows,
            scrollback_limit: 1000,
            total_popped: 0,
            default_fg: default_fg(),
            default_bg: default_bg(),
        };
        (grid, vp)
    }

    fn all_chars(grid: &Grid) -> Vec<String> {
        grid.rows.iter().map(row_chars).collect()
    }

    // ── 1. Scroll region tests ──────────────────────────────────────

    #[test]
    fn scroll_up_region_full_viewport() {
        // Scroll up the full viewport: top row removed, blank inserted at bottom.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["BBB", "CCC", "   "]);
    }

    #[test]
    fn scroll_up_region_partial() {
        // Scroll region covers only rows 1-2 of a 4-row viewport.
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 1, 2, 1);
        // Row 0 and 3 unchanged; row 1 (B) removed, blank at row 2.
        assert_eq!(all_chars(&grid), vec!["AAA", "CCC", "   ", "DDD"]);
    }

    #[test]
    fn scroll_up_region_n_greater_than_1() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 0, 3, 2);
        assert_eq!(all_chars(&grid), vec!["CCC", "DDD", "   ", "   "]);
    }

    #[test]
    fn scroll_up_region_n_clamped_to_region_size() {
        // n=100 but region is only 3 rows, should clamp.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 0, 2, 100);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "   "]);
    }

    #[test]
    fn scroll_down_region_full_viewport() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_down_region_partial() {
        // Scroll region covers only rows 1-2 of a 4-row viewport.
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 1, 2, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "BBB", "DDD"]);
    }

    #[test]
    fn scroll_down_region_n_greater_than_1() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 0, 3, 2);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_down_region_n_clamped() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 0, 2, 100);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "   "]);
    }

    #[test]
    fn scroll_up_region_with_scrollback() {
        // 2 scrollback rows + 3 visible. Scroll region is rows 0-2 of the
        // viewport. Scrollback should be untouched.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['S', 'T', 'A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["SSS", "TTT", "BBB", "CCC", "   "]);
    }

    #[test]
    fn scroll_down_region_with_scrollback() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['S', 'T', 'A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["SSS", "TTT", "   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_up_preserves_colors() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // row B, first cell
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 0, 2, 1);
        // B is now row 0; its color should survive.
        assert_eq!(grid.rows[0].fg[0], red);
        // New blank row at bottom should have default colors.
        assert_eq!(grid.rows[2].fg[0], default_fg());
    }

    #[test]
    fn scroll_down_preserves_colors() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        let blue = Srgb::new(0, 0, 255);
        grid.rows[1].fg[0] = blue; // row B
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 0, 2, 1);
        // B moved from row 1 to row 2.
        assert_eq!(grid.rows[2].fg[0], blue);
        // New blank row at top should have default colors.
        assert_eq!(grid.rows[0].fg[0], default_fg());
    }

    #[test]
    fn scroll_up_single_row_region() {
        // A 1-row region: scrolling should just blank it.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, &mut BTreeMap::new(), 1, 1, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "CCC"]);
    }

    #[test]
    fn scroll_down_single_row_region() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, &mut BTreeMap::new(), 1, 1, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "CCC"]);
    }

    // ── 2. Reflow with scrollback ───────────────────────────────────

    #[test]
    fn reflow_grow_with_scrollback_unwrapped() {
        // Scrollback rows should be resized but not merged with visible rows.
        let mut grid = make_grid(
            5,
            &[
                ("SSSSS", false), // scrollback
                ("AAAAA", false), // visible
                ("BBBBB", false),
            ],
        );
        grid.reflow(8);
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(row_chars(&grid.rows[0]), "SSSSS   ");
        assert_eq!(row_chars(&grid.rows[1]), "AAAAA   ");
    }

    #[test]
    fn reflow_grow_with_scrollback_wrapped() {
        // Wrapped rows in the scrollback should merge just like visible ones.
        let mut grid = make_grid(
            5,
            &[
                ("hello", true),  // scrollback, wraps into next
                ("world", false), // scrollback
                ("AAAAA", false), // visible
            ],
        );
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "helloworld");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_with_scrollback() {
        let mut grid = make_grid(
            6,
            &[
                ("abcdef", false), // scrollback
                ("ghijkl", false), // visible
            ],
        );
        grid.reflow(3);
        // Both rows should split.
        assert_eq!(grid.rows.len(), 4);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
    }

    #[test]
    fn reflow_mixed_wrapping_shrink_then_grow() {
        // Three logical lines at width 8:
        //   "Hi"                 — short unwrapped
        //   "ABCDEFGHIJKLMNOP"   — 16-char wrapped across two rows
        //   "Bye"                — short unwrapped
        let mut grid = make_grid(
            8,
            &[
                ("Hi      ", false),
                ("ABCDEFGH", true),
                ("IJKLMNOP", false),
                ("Bye     ", false),
            ],
        );

        // Shrink to width 4: "Hi" fits, "ABCD"/"EFGH"/"IJKL"/"MNOP", "Bye" fits.
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "Hi  ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ABCD");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "EFGH");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "IJKL");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "MNOP");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(row_chars(&grid.rows[5]), "Bye ");
        assert!(!grid.rows[5].wrapped);
        assert_eq!(grid.rows.len(), 6);

        // Grow 4 → 6: wrapped chains partially re-merge.
        // 16 chars at width 6 = three rows: 6 + 6 + 4.
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "Hi    ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ABCDEF");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "GHIJKL");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "MNOP  ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "Bye   ");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);
    }

    #[test]
    fn reflow_multiple_wrapped_shrink_then_grow() {
        // Two logical lines, each wrapped across two rows at width 6.
        let mut grid = make_grid(
            6,
            &[
                ("abcdef", true),
                ("ghijkl", true),
                ("mnopqr", false),
                ("stuvwx", true),
                ("yz0123", false),
                ("      ", false),
            ],
        );

        // Shrink to width 3: each wrapped line splits into two.
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "mno");
        assert!(grid.rows[4].wrapped);
        assert_eq!(row_chars(&grid.rows[5]), "pqr");
        assert!(!grid.rows[5].wrapped);
        assert_eq!(row_chars(&grid.rows[6]), "stu");
        assert!(grid.rows[6].wrapped);
        assert_eq!(row_chars(&grid.rows[7]), "vwx");
        assert!(grid.rows[7].wrapped);
        assert_eq!(row_chars(&grid.rows[8]), "yz0");
        assert!(grid.rows[8].wrapped);
        assert_eq!(row_chars(&grid.rows[9]), "123");
        assert!(!grid.rows[9].wrapped);
        assert_eq!(row_chars(&grid.rows[10]), "   ");
        assert!(!grid.rows[10].wrapped);
        assert_eq!(grid.rows.len(), 11);

        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ghijkl");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "mnopqr");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "stuvwx");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "yz0123");
        assert!(!grid.rows[4].wrapped);
    }

    #[test]
    fn reflow_mixed_wrapping_roundtrip() {
        // Shrink then grow back to original width with mixed lines.
        //   "Hi"             — short unwrapped
        //   "abcdefghijkl"   — 12-char wrapped across two rows
        //   "Lo"             — short unwrapped

        let mut grid = make_grid(
            6,
            &[
                ("Hi    ", false),
                ("abcdef", true),
                ("ghijkl", false),
                ("Lo    ", false),
            ],
        );

        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "Hi ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "abc");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "def");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "ghi");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "jkl");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(row_chars(&grid.rows[5]), "Lo ");
        assert!(!grid.rows[5].wrapped);
        assert_eq!(grid.rows.len(), 6);

        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "Hi    ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "abcdef");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghijkl");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "Lo    ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    // ── 3. Reflow edge cases ────────────────────────────────────────

    #[test]
    fn reflow_empty_grid() {
        let mut grid = Grid {
            rows: VecDeque::new(),
            scrollback_limit: 1000,
            total_popped: 0,
            default_fg: default_fg(),
            default_bg: default_bg(),
        };
        grid.reflow(10); // should not panic
        assert_eq!(grid.rows.len(), 0);
    }

    #[test]
    fn reflow_single_row_shrink() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.len(), 2);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[1].wrapped);
    }

    #[test]
    fn reflow_shrink_exact_fit_no_overflow() {
        // Content exactly fills the new width — no split needed.
        let mut grid = make_grid(6, &[("abc   ", false)]);
        grid.reflow(3);
        // "abc" fits in 3 cols, trailing spaces are not content.
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
    }

    #[test]
    fn reflow_shrink_preserves_colors() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[0].fg[3] = red; // 'd' is red
        grid.reflow(3);
        // 'd' is now at row 1, col 0.
        assert_eq!(grid.rows[1].fg[0], red);
    }

    #[test]
    fn reflow_grow_preserves_colors() {
        let mut grid = make_grid(3, &[("abc", true), ("def", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // 'd' is red
        grid.reflow(6);
        // Merged into one row: "abcdef". 'd' is at col 3.
        assert_eq!(grid.rows[0].fg[3], red);
    }

    // ── 4. push_visible_row ─────────────────────────────────────────

    #[test]
    fn push_visible_row_adds_blank() {
        let vp = make_viewport(3, 4);
        let (mut grid, _) = make_grid_with_scrollback(4, 3, &['A', 'B', 'C']);
        grid.push_visible_row(&vp);
        assert_eq!(grid.rows.len(), 4);
        assert_eq!(row_chars(grid.rows.back().unwrap()), "    ");
    }

    #[test]
    fn push_visible_row_trims_scrollback() {
        let vp = make_viewport(3, 4);
        let mut grid = Grid {
            rows: VecDeque::new(),
            scrollback_limit: 2,
            total_popped: 0,
            default_fg: default_fg(),
            default_bg: default_bg(),
        };
        // Fill 3 visible + 2 scrollback = 5 rows (at the limit).
        for ch in ['S', 'T', 'A', 'B', 'C'] {
            let mut row = Row::new(4, default_fg(), default_bg());
            row.cells.fill(char_cell(ch));
            grid.rows.push_back(row);
        }
        assert_eq!(grid.rows.len(), 5); // at limit
        grid.push_visible_row(&vp);
        // Should have trimmed the oldest scrollback row.
        assert_eq!(grid.rows.len(), 5);
        assert_eq!(grid.total_popped, 1);
        assert_eq!(row_chars(&grid.rows[0]), "TTTT"); // 'S' row was removed
    }

    #[test]
    fn push_visible_row_total_popped_accumulates() {
        let vp = make_viewport(2, 3);
        let mut grid = Grid {
            rows: VecDeque::new(),
            scrollback_limit: 0,
            total_popped: 0,
            default_fg: default_fg(),
            default_bg: default_bg(),
        };
        // Start with 2 visible rows.
        for ch in ['A', 'B'] {
            let mut row = Row::new(3, default_fg(), default_bg());
            row.cells.fill(char_cell(ch));
            grid.rows.push_back(row);
        }
        // Push 3 more rows — each should pop one.
        grid.push_visible_row(&vp);
        grid.push_visible_row(&vp);
        grid.push_visible_row(&vp);
        assert_eq!(grid.total_popped, 3);
        assert_eq!(grid.rows.len(), 2);
    }

    // ── 5. reflow_soft_grow across VecDeque split ───────────────────

    #[test]
    fn reflow_grow_across_deque_boundary() {
        // Force wrapped rows to straddle the VecDeque's internal ring buffer
        // boundary. Rotating by exactly `len` preserves logical order while
        // advancing the internal head pointer. With 3 rows and typical
        // capacity 4, head lands at position 3 and elements wrap around.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        let n = grid.rows.len();
        let cap = grid.rows.capacity();
        if cap > n {
            // Rotate by len to preserve order but shift the head pointer.
            for _ in 0..n {
                let row = grid.rows.pop_front().unwrap();
                grid.rows.push_back(row);
            }
        }
        grid.reflow(9);
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghi");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_across_deque_boundary_partial_merge() {
        // 4 rows where only the first 2 are wrapped — merge should stop at
        // the unwrapped boundary. Rotation forces ring buffer wrap-around.
        let mut grid = make_grid(
            3,
            &[("abc", true), ("def", false), ("ghi", true), ("jkl", false)],
        );
        let n = grid.rows.len();
        let cap = grid.rows.capacity();
        if cap > n {
            for _ in 0..n {
                let row = grid.rows.pop_front().unwrap();
                grid.rows.push_back(row);
            }
        }
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ghijkl");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    // ── Reflow: shrink-then-grow with long lines ───────────────────
    //
    // These tests exercise the merge path where the grow width is more
    // than double the shrunk width, requiring multiple source rows to
    // be pulled into a single destination row. This is the common case
    // for long log lines: a wide terminal shrinks narrow, creating many
    // wrapped rows, then grows back.

    #[test]
    fn reflow_shrink_grow_roundtrip_long_line() {
        // "abcdefghij" at width 10, shrink to 3 then grow back to 10.
        // Ratio 10:3 means each destination row consumes ~3 source rows.
        let mut grid = make_grid(10, &[("abcdefghij", false)]);

        grid.reflow(3);
        // "abc"W "def"W "ghi"W "j  "U
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "j  ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);

        grid.reflow(10);
        // Should recover the original single row.
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghij");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_shrink_grow_long_line_partial_grow() {
        // 20-char line shrunk to 4, then grown to 10 (not back to original).
        // Content should reflow into two correctly packed rows.
        let mut grid = make_grid(20, &[("abcdefghijklmnopqrst", false)]);

        grid.reflow(4);
        // "abcd"W "efgh"W "ijkl"W "mnop"W "qrst"U
        assert_eq!(grid.rows.len(), 5);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ijkl");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "mnop");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "qrst");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);

        grid.reflow(10);
        // 20 chars at width 10 = two rows.
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghij");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "klmnopqrst");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_grow_long_line_colors_roundtrip() {
        // Per-cell colors must survive a shrink-then-grow roundtrip even
        // when the grow width is more than double the shrunk width.
        let mut grid = make_grid(10, &[("abcdefghij", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[0].fg[6] = red; // 'g' is red

        grid.reflow(3);
        // After shrink: "abc"W "def"W "ghi"W "j  "U — 'g' at row 2 col 0.
        assert_eq!(grid.rows[2].cells[0], "g");
        assert_eq!(grid.rows[2].fg[0], red);

        grid.reflow(10);
        // After roundtrip: 'g' should be back at col 6 with its red color.
        assert_eq!(grid.rows[0].cells[6], "g");
        assert_eq!(grid.rows[0].fg[6], red);
    }
}
