use unicode_segmentation::UnicodeSegmentation;

use crate::Row;

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
