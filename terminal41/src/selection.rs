//! Text-selection model for mouse-driven copy.
//!
//! Positions are stored in **absolute** row coordinates — `total_popped +
//! index` into the grid — so selections stay anchored to their content even
//! as scrollback trims the front of the grid or the user scrolls history.

use unicode_segmentation::UnicodeSegmentation;

use crate::Row;

/// A point in the grid addressable across scrollback lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionPoint {
    /// Absolute row index. `Grid::total_popped + index_in_rows` gives this.
    pub row: u64,
    /// Column (0-based) within the row.
    pub col: u32,
}

impl SelectionPoint {
    fn as_tuple(self) -> (u64, u32) {
        (self.row, self.col)
    }
}

/// How an in-progress selection expands around the anchor/head points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    /// Cell-granular — one cell per pointer move.
    Char,
    /// Expanded to word boundaries at each endpoint (double-click).
    Word,
    /// Whole row, end to end (triple-click).
    Line,
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub anchor: SelectionPoint,
    pub head: SelectionPoint,
    pub mode: SelectionMode,
    /// The cell the user originally clicked. Carried so Word/Line selections
    /// can pick the correct word/line boundary as the head end when the
    /// drag direction flips relative to where the click started.
    pub origin: SelectionPoint,
}

impl Selection {
    /// Normalize to (start, end) with start ≤ end in document order.
    pub fn ordered(&self) -> (SelectionPoint, SelectionPoint) {
        if self.anchor.as_tuple() <= self.head.as_tuple() {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// A Char-mode selection that hasn't been dragged off the anchor is
    /// considered empty — right-click paste treats it that way so a click
    /// followed by a right-click yields a paste rather than a zero-width copy.
    pub fn is_empty(&self) -> bool {
        matches!(self.mode, SelectionMode::Char) && self.anchor == self.head
    }

    /// Returns true if the given absolute cell is covered by this selection.
    /// Both endpoints are inclusive so the cell under the release point is
    /// visually selected, matching xterm/alacritty behavior.
    pub fn contains(
        &self,
        point: SelectionPoint,
    ) -> bool {
        let (start, end) = self.ordered();
        if matches!(self.mode, SelectionMode::Line) {
            return point.row >= start.row && point.row <= end.row;
        }
        if point.row < start.row || point.row > end.row {
            return false;
        }
        if start.row == end.row {
            point.col >= start.col && point.col <= end.col
        } else if point.row == start.row {
            point.col >= start.col
        } else if point.row == end.row {
            point.col <= end.col
        } else {
            true
        }
    }
}

/// Expand a cell to the word boundary containing it.
///
/// Returns the inclusive `(start_col, end_col)` range covered by the
/// Unicode word-bound segment at `col`. If `col` is out of range the cell
/// itself is returned as a degenerate range.
pub fn expand_to_word(
    row: &Row,
    col: u32,
) -> (u32, u32) {
    let col = col as usize;
    if col >= row.cells.len() {
        return (col as u32, col as u32);
    }

    // Build the row text and a per-cell byte offset so grapheme-cluster cells
    // map bidirectionally to column indices.
    let mut text = String::new();
    let mut cell_byte_starts: Vec<usize> = Vec::with_capacity(row.cells.len() + 1);
    for cell in &row.cells {
        cell_byte_starts.push(text.len());
        text.push_str(cell);
    }
    cell_byte_starts.push(text.len());

    let click_byte = cell_byte_starts[col];

    for (start_byte, segment) in text.split_word_bound_indices() {
        let end_byte = start_byte + segment.len();
        if click_byte >= start_byte && click_byte < end_byte {
            let start_col = byte_to_col(&cell_byte_starts, start_byte) as u32;
            let end_col = byte_to_col(&cell_byte_starts, end_byte) as u32;
            return (start_col, end_col.saturating_sub(1));
        }
    }
    (col as u32, col as u32)
}

fn byte_to_col(
    cell_byte_starts: &[usize],
    byte: usize,
) -> usize {
    cell_byte_starts
        .iter()
        .rposition(|&b| b <= byte)
        .unwrap_or(0)
}

/// Expand a point to cover a full row in Line mode. Returns the inclusive
/// column range; the caller pairs this with the row to produce start/end
/// selection points.
pub fn expand_to_line(row: &Row) -> (u32, u32) {
    if row.cells.is_empty() {
        (0, 0)
    } else {
        (0, row.cells.len() as u32 - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Row;

    fn row_from(text: &str) -> Row {
        use crate::ColorPalette;
        let pal = ColorPalette::default();
        let mut r = Row::new(text.chars().count() as u32, pal.fg, pal.bg);
        let mut buf = [0u8; 4];
        for (i, c) in text.chars().enumerate() {
            r.cells[i] = smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf));
        }
        r
    }

    fn pt(
        row: u64,
        col: u32,
    ) -> SelectionPoint {
        SelectionPoint { row, col }
    }

    fn sel(
        anchor: SelectionPoint,
        head: SelectionPoint,
        mode: SelectionMode,
    ) -> Selection {
        Selection {
            anchor,
            head,
            mode,
            origin: anchor,
        }
    }

    #[test]
    fn ordered_swaps_when_anchor_after_head() {
        let s = sel(pt(5, 10), pt(2, 3), SelectionMode::Char);
        assert_eq!(s.ordered(), (pt(2, 3), pt(5, 10)));
    }

    #[test]
    fn empty_char_selection_is_empty() {
        let s = sel(pt(3, 4), pt(3, 4), SelectionMode::Char);
        assert!(s.is_empty());
    }

    #[test]
    fn word_selection_is_never_empty() {
        let s = sel(pt(3, 4), pt(3, 4), SelectionMode::Word);
        assert!(!s.is_empty());
    }

    #[test]
    fn contains_inclusive_on_both_ends_single_row() {
        let s = sel(pt(0, 3), pt(0, 7), SelectionMode::Char);
        assert!(!s.contains(pt(0, 2)));
        assert!(s.contains(pt(0, 3)));
        assert!(s.contains(pt(0, 5)));
        assert!(s.contains(pt(0, 7)));
        assert!(!s.contains(pt(0, 8)));
    }

    #[test]
    fn contains_multi_row_excludes_cells_before_start_col() {
        let s = sel(pt(0, 5), pt(2, 3), SelectionMode::Char);
        assert!(!s.contains(pt(0, 4)));
        assert!(s.contains(pt(0, 5)));
        assert!(s.contains(pt(0, 79))); // anywhere in first row past start
        assert!(s.contains(pt(1, 0))); // middle row — everything
        assert!(s.contains(pt(2, 0))); // last row up to end_col
        assert!(s.contains(pt(2, 3)));
        assert!(!s.contains(pt(2, 4)));
    }

    #[test]
    fn line_mode_covers_full_rows() {
        let s = sel(pt(1, 5), pt(3, 2), SelectionMode::Line);
        assert!(!s.contains(pt(0, 100)));
        assert!(s.contains(pt(1, 0)));
        assert!(s.contains(pt(2, 42))); // middle row
        assert!(s.contains(pt(3, 999)));
        assert!(!s.contains(pt(4, 0)));
    }

    #[test]
    fn expand_to_word_picks_word_around_col() {
        let row = row_from("hello world foo");
        // click on `l` in hello
        assert_eq!(expand_to_word(&row, 3), (0, 4));
        // click on space — the whitespace run is the segment
        assert_eq!(expand_to_word(&row, 5), (5, 5));
        // click on `r` in world
        assert_eq!(expand_to_word(&row, 8), (6, 10));
    }

    #[test]
    fn expand_to_word_handles_punctuation_as_own_segment() {
        let row = row_from("foo=bar");
        assert_eq!(expand_to_word(&row, 0), (0, 2)); // foo
        assert_eq!(expand_to_word(&row, 3), (3, 3)); // =
        assert_eq!(expand_to_word(&row, 4), (4, 6)); // bar
    }

    #[test]
    fn expand_to_line_covers_full_row() {
        let row = row_from("hello");
        assert_eq!(expand_to_line(&row), (0, 4));
    }
}
