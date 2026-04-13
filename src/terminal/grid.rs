use std::collections::VecDeque;

use crate::terminal::row::Row;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub col: u32,
    pub row: u32,
}

/// Dimensions of the rendered terminal window, shared by both screens.
/// Per-screen state (scroll region, scrollback offset) lives on
/// [`super::Screen`].
#[derive(Debug, Default)]
pub struct Viewport {
    pub rows: u32,
    pub cols: u32,
}

#[derive(Debug)]
pub struct Grid {
    pub rows: VecDeque<Row>,
    pub scrollback_limit: u32,
    /// Running count of rows popped from the front (for image position
    /// tracking).
    pub total_popped: usize,
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
        self.rows.push_back(Row::new(viewport.cols));

        let max_rows = viewport.rows as usize + self.scrollback_limit as usize;
        if self.rows.len() > max_rows {
            self.rows.pop_front();
            self.total_popped += 1;
        }
    }

    pub fn erase_in_display(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let first_visible = self.rows.len() - viewport.rows as usize;
        let col = cursor.col as usize;

        match mode {
            // Erase from cursor to end of screen.
            0 => {
                let cols = self.rows[active].cells.len();
                self.rows[active].clear_range(col..cols);
                for r in (active + 1)..self.rows.len() {
                    self.rows[r].clear();
                }
            }
            // Erase from start of screen to cursor (inclusive).
            1 => {
                for r in first_visible..active {
                    self.rows[r].clear();
                }
                self.rows[active].clear_range(0..col + 1);
            }
            // Erase entire screen.
            2 => {
                for r in first_visible..self.rows.len() {
                    self.rows[r].clear();
                }
            }
            // Erase scrollback buffer.
            3 => {
                self.total_popped += first_visible;
                self.rows.drain(0..first_visible);
            }
            _ => {}
        }
    }

    pub fn erase_in_line(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].cells.len();
        let col = cursor.col as usize;

        match mode {
            // Erase from cursor to end of line.
            0 => self.rows[active].clear_range(col..cols),
            // Erase from start of line to cursor (inclusive).
            1 => self.rows[active].clear_range(0..col + 1),
            // Erase entire line.
            2 => self.rows[active].clear(),
            _ => {}
        }
    }

    pub fn delete_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].cells.len();
        let col = cursor.col as usize;
        let count = (n as usize).min(cols - col);

        self.rows[active].copy_within(col + count..cols, col);
        self.rows[active].clear_range(cols - count..cols);
    }

    pub fn insert_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].cells.len();
        let col = cursor.col as usize;
        let count = (n as usize).min(cols - col);

        self.rows[active].copy_within(col..cols - count, col + count);
        self.rows[active].clear_range(col..col + count);
    }

    pub fn erase_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].cells.len();
        let col = cursor.col as usize;
        let end = (col + n as usize).min(cols);

        self.rows[active].clear_range(col..end);
    }

    /// Scroll content up within a region: remove line at `top`, insert blank
    /// at `bottom`. Both are viewport-relative row indices.
    pub(super) fn scroll_up_in_region(
        &mut self,
        viewport: &Viewport,
        top: u32,
        bottom: u32,
        n: u32,
    ) {
        let first_visible = self.rows.len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        for _ in 0..n {
            self.rows.remove(abs_top);
            self.rows.insert(abs_bottom, Row::new(viewport.cols));
        }
    }

    /// Scroll content down within a region: insert blank at `top`, remove
    /// line at `bottom`. Both are viewport-relative row indices.
    pub(super) fn scroll_down_in_region(
        &mut self,
        viewport: &Viewport,
        top: u32,
        bottom: u32,
        n: u32,
    ) {
        let first_visible = self.rows.len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        for _ in 0..n {
            self.rows.remove(abs_bottom);
            self.rows.insert(abs_top, Row::new(viewport.cols));
        }
    }

    pub fn active_row_index(
        &self,
        cursor: &Cursor,
        viewport: &Viewport,
    ) -> usize {
        self.rows.len() - viewport.rows as usize + cursor.row as usize
    }

    pub(super) fn reflow(
        &mut self,
        new_width: u32,
    ) {
        if self.rows.is_empty() {
            return;
        }

        if self.rows[0].len() == new_width {
            return;
        }

        if new_width > self.rows[0].len() {
            let new_width = new_width as usize;
            let mut dst = 0;
            let mut dst_col = self.rows[0].content_len() as usize;
            let mut src = 1;
            let mut src_col: usize = 0;

            while dst < self.rows.len() && src < self.rows.len() {
                self.rows[dst].resize(new_width as u32);

                if !self.rows[dst].wrapped {
                    dst += 1;
                    dst_col = if dst == src && self.rows[dst].wrapped {
                        self.rows[dst].content_len() as usize
                    } else {
                        0
                    };
                    if dst == src {
                        src += 1;
                    }
                    continue;
                }

                // Pull one chunk from src into dst.
                let (d, s) = self.split_current_next(dst, src);
                let s_content = s.content_len() as usize;
                let n = d.copy_from(s, src_col..s_content, dst_col);
                dst_col += n;
                src_col += n;

                // If src exhausted: inherit its wrap state, clear it, advance.
                if src_col >= s_content {
                    d.wrapped = s.wrapped;
                    s.clear();
                    s.wrapped = true;
                    src += 1;
                    src_col = 0;
                }

                // If dst full: advance to next row.
                if dst_col >= new_width {
                    if src_col > 0 {
                        self.rows[dst].wrapped = true;
                    }
                    dst += 1;
                    dst_col = 0;
                    if dst == src {
                        // Collision: dst caught up to partially-consumed src.
                        // Shift remaining content to front and advance src.
                        self.rows[dst].copy_within(src_col.., 0);
                        let len = self.rows[dst].len() as usize;
                        self.rows[dst].clear_range(len - src_col..len);
                        dst_col = len - src_col;
                        src += 1;
                        src_col = 0;
                    }
                }
            }

            self.rows[dst].resize(new_width as u32);
            self.rows
                .truncate(dst + if self.rows[dst].wrapped { 0 } else { 1 });
        } else {
            let mut row = 0;
            while row < self.rows.len() {
                if self.rows[row].len() > new_width {
                    if self.rows[row].content_len() > new_width {
                        let overflow = Row {
                            cells: self.rows[row].cells.split_off(new_width as usize),
                            fg: self.rows[row].fg.split_off(new_width as usize),
                            bg: self.rows[row].bg.split_off(new_width as usize),
                            links: self.rows[row].links.split_off(new_width as usize),
                            wrapped: self.rows[row].wrapped,
                        };

                        self.rows[row].wrapped = true;
                        self.rows.insert(row + 1, overflow);
                    } else {
                        self.rows[row].wrapped = false;
                        self.rows[row].truncate(new_width);
                    }
                } else {
                    let mut content = self.rows[row].len() as usize;
                    self.rows[row].resize(new_width);

                    // Pull content from continuation rows to fill space left
                    // by a short overflow. This maintains the invariant that
                    // only the last row in a wrapped sequence is partially
                    // filled.
                    while self.rows[row].wrapped && row + 1 < self.rows.len() {
                        let room = new_width as usize - content;
                        if room == 0 {
                            break;
                        }

                        let next = row + 1;
                        let next_content = self.rows[next].content_len() as usize;
                        let to_copy = room.min(next_content);

                        if to_copy > 0 {
                            let (dst, src) = self.split_current_next(row, next);
                            for i in 0..to_copy {
                                dst.cells[content + i] = src.cells[i].clone();
                            }
                            dst.fg[content..content + to_copy].copy_from_slice(&src.fg[..to_copy]);
                            dst.bg[content..content + to_copy].copy_from_slice(&src.bg[..to_copy]);
                        }

                        if to_copy >= next_content {
                            // Fully consumed the next row — inherit its wrap
                            // state and remove it.
                            let next_wrapped = self.rows[next].wrapped;
                            self.rows.remove(next);
                            self.rows[row].wrapped = next_wrapped;
                            content += to_copy;
                        } else {
                            // Partially consumed — shift remaining content left
                            // and trim to its new length so the main loop can
                            // process it correctly.
                            self.rows[next].copy_within(to_copy.., 0);
                            let remaining = self.rows[next].len() as usize - to_copy;
                            self.rows[next].truncate(remaining as u32);
                            break;
                        }
                    }
                }
                row += 1;
            }
        }
    }

    fn split_current_next(
        &mut self,
        row: usize,
        next: usize,
    ) -> (&mut Row, &mut Row) {
        let (front, back) = self.rows.as_mut_slices();

        if row < front.len() && next >= front.len() {
            let next = next - front.len();
            (&mut front[row], &mut back[next])
        } else if next < front.len() && row >= front.len() {
            (&mut back[row - front.len()], &mut front[next])
        } else if next < front.len() {
            let (first, second) = front.split_at_mut(next);
            (&mut first[row], &mut second[0])
        } else {
            let (first, second) = back.split_at_mut(next - front.len());
            (&mut first[row - front.len()], &mut second[0])
        }
    }
}

#[cfg(test)]
mod tests {
    use palette::Srgb;

    use super::*;
    use crate::terminal::color::default_fg;

    /// Build a grid from `(text, wrapped)` pairs. Each row is padded to `width`
    /// with spaces.
    fn make_grid(
        width: u32,
        rows: &[(&str, bool)],
    ) -> Grid {
        let mut grid_rows = VecDeque::new();
        for &(text, wrapped) in rows {
            let mut row = Row::new(width);
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
        Viewport { rows, cols }
    }

    /// Build a grid with `scrollback` history rows + `visible` visible rows.
    /// Each row is labeled with a single char repeated to fill the width.
    fn make_grid_with_scrollback(
        width: u32,
        visible: u32,
        labels: &[char],
    ) -> (Grid, Viewport) {
        let vp = make_viewport(visible, width);
        let mut rows = VecDeque::new();
        for &ch in labels {
            let mut row = Row::new(width);
            for c in row.cells.iter_mut() {
                *c = char_cell(ch);
            }
            rows.push_back(row);
        }
        let grid = Grid {
            rows,
            scrollback_limit: 1000,
            total_popped: 0,
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
        grid.scroll_up_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["BBB", "CCC", "   "]);
    }

    #[test]
    fn scroll_up_region_partial() {
        // Scroll region covers only rows 1-2 of a 4-row viewport.
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_up_in_region(&vp, 1, 2, 1);
        // Row 0 and 3 unchanged; row 1 (B) removed, blank at row 2.
        assert_eq!(all_chars(&grid), vec!["AAA", "CCC", "   ", "DDD"]);
    }

    #[test]
    fn scroll_up_region_n_greater_than_1() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_up_in_region(&vp, 0, 3, 2);
        assert_eq!(all_chars(&grid), vec!["CCC", "DDD", "   ", "   "]);
    }

    #[test]
    fn scroll_up_region_n_clamped_to_region_size() {
        // n=100 but region is only 3 rows, should clamp.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 0, 2, 100);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "   "]);
    }

    #[test]
    fn scroll_down_region_full_viewport() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_down_region_partial() {
        // Scroll region covers only rows 1-2 of a 4-row viewport.
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_down_in_region(&vp, 1, 2, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "BBB", "DDD"]);
    }

    #[test]
    fn scroll_down_region_n_greater_than_1() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_down_in_region(&vp, 0, 3, 2);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_down_region_n_clamped() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 0, 2, 100);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "   "]);
    }

    #[test]
    fn scroll_up_region_with_scrollback() {
        // 2 scrollback rows + 3 visible. Scroll region is rows 0-2 of the
        // viewport. Scrollback should be untouched.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['S', 'T', 'A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["SSS", "TTT", "BBB", "CCC", "   "]);
    }

    #[test]
    fn scroll_down_region_with_scrollback() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['S', 'T', 'A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["SSS", "TTT", "   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_up_preserves_colors() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // row B, first cell
        grid.scroll_up_in_region(&vp, 0, 2, 1);
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
        grid.scroll_down_in_region(&vp, 0, 2, 1);
        // B moved from row 1 to row 2.
        assert_eq!(grid.rows[2].fg[0], blue);
        // New blank row at top should have default colors.
        assert_eq!(grid.rows[0].fg[0], default_fg());
    }

    #[test]
    fn scroll_up_single_row_region() {
        // A 1-row region: scrolling should just blank it.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 1, 1, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "CCC"]);
    }

    #[test]
    fn scroll_down_single_row_region() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 1, 1, 1);
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
        };
        // Fill 3 visible + 2 scrollback = 5 rows (at the limit).
        for ch in ['S', 'T', 'A', 'B', 'C'] {
            let mut row = Row::new(4);
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
        };
        // Start with 2 visible rows.
        for ch in ['A', 'B'] {
            let mut row = Row::new(3);
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
