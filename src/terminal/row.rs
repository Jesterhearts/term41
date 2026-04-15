use std::ops::RangeBounds;

use palette::Srgb;
use smol_str::SmolStr;

use crate::terminal::attrs::CellAttrs;
use crate::terminal::color::default_bg;
use crate::terminal::color::default_fg;
use crate::terminal::hyperlink::HyperlinkId;

/// Inline SmolStr for the default blank cell. Cheap to clone.
pub(super) const fn blank_cell() -> SmolStr {
    SmolStr::new_inline(" ")
}

/// A terminal row stored as struct-of-arrays for cache-friendly access.
/// Each cell holds one grapheme cluster as a [`SmolStr`] (inline up to
/// 23 bytes), so combining marks are stored alongside their base character.
#[derive(Debug, Default)]
pub struct Row {
    pub cells: Vec<SmolStr>,
    pub fg: Vec<Srgb<u8>>,
    pub bg: Vec<Srgb<u8>>,
    /// Per-cell text attributes (bold/italic/underline). Set from
    /// `screen.attrs` at write time alongside `fg`/`bg`.
    pub attrs: Vec<CellAttrs>,
    /// Hyperlink id per cell, set from the screen's current OSC 8 span at
    /// write time. `None` for plain cells; reused ids share the same target
    /// in [`HyperlinkRegistry`](super::HyperlinkRegistry) so adjacent cells
    /// of one link render as one underlined region.
    pub links: Vec<Option<HyperlinkId>>,
    /// True if this row is a continuation of the previous row (soft wrap).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: u32) -> Self {
        let n = cols as usize;
        Self {
            cells: vec![blank_cell(); n],
            fg: vec![default_fg(); n],
            bg: vec![default_bg(); n],
            attrs: vec![CellAttrs::default(); n],
            links: vec![None; n],
            wrapped: false,
        }
    }

    pub(super) fn len(&self) -> u32 {
        self.cells.len() as u32
    }

    pub(super) fn content_len(&self) -> u32 {
        if self.wrapped {
            self.len()
        } else {
            self.cells
                .iter()
                .rposition(|c| c != " ")
                .map_or(0, |p| p + 1) as u32
        }
    }

    pub(super) fn resize(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.cells.resize(new_len, blank_cell());
        self.fg.resize(new_len, default_fg());
        self.bg.resize(new_len, default_bg());
        self.attrs.resize(new_len, CellAttrs::default());
        self.links.resize(new_len, None);
    }

    pub(super) fn truncate(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.cells.truncate(new_len);
        self.fg.truncate(new_len);
        self.bg.truncate(new_len);
        self.attrs.truncate(new_len);
        self.links.truncate(new_len);
    }

    pub(super) fn clear(&mut self) {
        self.clear_range(0..self.cells.len())
    }

    /// Reset this row for reuse at the bottom of the grid — used when the
    /// scrollback limit is hit and we'd otherwise drop+reallocate the
    /// row's four backing vectors. Resizes to `cols` if the viewport width
    /// changed, blanks every cell, and clears the soft-wrap marker.
    pub(super) fn reset_for_reuse(
        &mut self,
        cols: u32,
    ) {
        let n = cols as usize;
        if self.cells.len() != n {
            self.resize(cols);
        }
        self.clear();
        self.wrapped = false;
    }

    pub(super) fn clear_range(
        &mut self,
        range: std::ops::Range<usize>,
    ) {
        self.cells[range.clone()].fill(blank_cell());
        self.fg[range.clone()].fill(default_fg());
        self.bg[range.clone()].fill(default_bg());
        self.attrs[range.clone()].fill(CellAttrs::default());
        self.links[range].fill(None);
    }

    pub(super) fn copy_within<R>(
        &mut self,
        src: R,
        dest: usize,
    ) where
        R: RangeBounds<usize> + Clone,
    {
        // SmolStr isn't Copy, so copy_within isn't available — use a manual
        // forward/backward clone loop to handle overlapping ranges.
        let (start, end) = range_bounds(src.clone(), self.cells.len());
        let count = end - start;
        if dest <= start {
            for i in 0..count {
                self.cells[dest + i] = self.cells[start + i].clone();
            }
        } else {
            for i in (0..count).rev() {
                self.cells[dest + i] = self.cells[start + i].clone();
            }
        }
        self.fg.copy_within(src.clone(), dest);
        self.bg.copy_within(src.clone(), dest);
        self.attrs.copy_within(src.clone(), dest);
        self.links.copy_within(src, dest);
    }

    pub(super) fn copy_from(
        &mut self,
        other: &Self,
        src: std::ops::Range<usize>,
        dest_offset: usize,
    ) -> usize {
        let copy_len = ((other.content_len() as usize).saturating_sub(src.start))
            .min((self.len() as usize).saturating_sub(dest_offset))
            .min(src.len());
        self.cells[dest_offset..dest_offset + copy_len]
            .clone_from_slice(&other.cells[src.start..src.start + copy_len]);
        self.fg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.fg[src.start..src.start + copy_len]);
        self.bg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.bg[src.start..src.start + copy_len]);
        self.attrs[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.attrs[src.start..src.start + copy_len]);
        self.links[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.links[src.start..src.start + copy_len]);

        copy_len
    }
}

fn range_bounds<R: RangeBounds<usize>>(
    range: R,
    len: usize,
) -> (usize, usize) {
    use std::ops::Bound;
    let start = match range.start_bound() {
        Bound::Included(&n) => n,
        Bound::Excluded(&n) => n + 1,
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&n) => n + 1,
        Bound::Excluded(&n) => n,
        Bound::Unbounded => len,
    };
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(row: &Row) -> String {
        let mut s = String::new();
        for cell in &row.cells {
            s.push_str(cell);
        }
        s
    }

    fn set_cell(
        row: &mut Row,
        idx: usize,
        ch: char,
    ) {
        let mut buf = [0u8; 4];
        row.cells[idx] = SmolStr::new_inline(ch.encode_utf8(&mut buf));
    }

    #[test]
    fn row_new_filled_with_spaces() {
        let row = Row::new(4);
        assert_eq!(row.cells, vec![blank_cell(); 4]);
        assert_eq!(row.fg, vec![default_fg(); 4]);
        assert_eq!(row.bg, vec![default_bg(); 4]);
        assert!(!row.wrapped);
    }

    #[test]
    fn row_len() {
        let row = Row::new(5);
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_grow() {
        let mut row = Row::new(3);
        set_cell(&mut row, 0, 'a');
        set_cell(&mut row, 1, 'b');
        set_cell(&mut row, 2, 'c');
        row.resize(5);
        assert_eq!(row_text(&row), "abc  ");
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_shrink() {
        let mut row = Row::new(5);
        set_cell(&mut row, 0, 'a');
        set_cell(&mut row, 1, 'b');
        set_cell(&mut row, 2, 'c');
        row.resize(2);
        assert_eq!(row_text(&row), "ab");
    }

    #[test]
    fn row_clear() {
        let mut row = Row::new(3);
        set_cell(&mut row, 0, 'x');
        set_cell(&mut row, 1, 'y');
        row.fg[0] = Srgb::new(255, 0, 0);
        row.clear();
        assert_eq!(row.cells, vec![blank_cell(); 3]);
        assert_eq!(row.fg, vec![default_fg(); 3]);
    }

    #[test]
    fn row_clear_range() {
        let mut row = Row::new(5);
        for (i, ch) in "abcde".chars().enumerate() {
            set_cell(&mut row, i, ch);
        }
        row.clear_range(1..4);
        assert_eq!(row_text(&row), "a   e");
    }

    #[test]
    fn row_copy_within() {
        let mut row = Row::new(6);
        for (i, ch) in "abcdef".chars().enumerate() {
            set_cell(&mut row, i, ch);
        }
        row.copy_within(0..3, 3);
        assert_eq!(row_text(&row), "abcabc");
    }

    #[test]
    fn row_copy_from() {
        let mut dst = Row::new(6);
        let mut src = Row::new(3);
        for (i, ch) in "xyz".chars().enumerate() {
            set_cell(&mut src, i, ch);
        }
        dst.copy_from(&src, 0..3, 2);
        assert_eq!(row_text(&dst), "  xyz ");
    }

    #[test]
    fn row_copy_from_with_offset() {
        let mut dst = Row::new(5);
        let mut src = Row::new(4);
        for (i, ch) in "abcd".chars().enumerate() {
            set_cell(&mut src, i, ch);
        }
        // Copy from src offset 2 to dst offset 0 → copies "cd" (length min(2,5)=2)
        dst.copy_from(&src, 2..4, 0);
        assert_eq!(row_text(&dst), "cd   ");
    }
}
