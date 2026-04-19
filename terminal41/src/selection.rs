//! Text-selection model for mouse-driven copy.
//!
//! Positions are stored in **absolute** row coordinates — `total_popped +
//! index` into the grid — so selections stay anchored to their content even
//! as scrollback trims the front of the grid or the user scrolls history.

use clip41::Clipboard;
use clip41::ClipboardKind;
use unicode_segmentation::UnicodeSegmentation;

use crate::Row;
use crate::Screen;
use crate::Viewport;
use crate::screen;
use crate::search::MatchSpan;
use crate::search::SearchState;

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

pub(crate) fn active_viewport(
    screen: &Screen,
    viewport: &Viewport,
) -> Viewport {
    let mut view = screen::screen_viewport(screen, viewport);
    if screen.offset > 0 {
        view.top = view
            .top_index(screen.grid.rows.len())
            .saturating_sub(screen.offset as usize);
    }
    view
}

pub(crate) fn screen_row_to_absolute(
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> u64 {
    let base = active_viewport(screen, viewport).top_index(screen.grid.rows.len());
    (screen.grid.total_popped + base + screen_row as usize) as u64
}

pub(crate) fn absolute_row_to_local(
    screen: &Screen,
    abs: u64,
) -> Option<usize> {
    let popped = screen.grid.total_popped as u64;
    if abs < popped {
        return None;
    }
    let local = (abs - popped) as usize;
    if local >= screen.grid.rows.len() {
        return None;
    }
    Some(local)
}

#[must_use]
pub fn start_selection(
    screen: &Screen,
    viewport: &Viewport,
    col: u32,
    screen_row: u32,
    mode: SelectionMode,
) -> Option<Selection> {
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    let local = absolute_row_to_local(screen, abs_row)?;
    let row = &screen.grid.rows[local];
    let origin = SelectionPoint { row: abs_row, col };

    let (anchor, head) = match mode {
        SelectionMode::Char => (origin, origin),
        SelectionMode::Word => {
            let (s, e) = expand_to_word(row, col);
            (
                SelectionPoint {
                    row: abs_row,
                    col: s,
                },
                SelectionPoint {
                    row: abs_row,
                    col: e,
                },
            )
        }
        SelectionMode::Line => {
            let (s, e) = expand_to_line(row);
            (
                SelectionPoint {
                    row: abs_row,
                    col: s,
                },
                SelectionPoint {
                    row: abs_row,
                    col: e,
                },
            )
        }
    };

    Some(Selection {
        anchor,
        head,
        mode,
        origin,
    })
}

#[must_use]
pub fn extend_selection(
    selection: &Selection,
    screen: &Screen,
    viewport: &Viewport,
    col: u32,
    screen_row: u32,
) -> Option<Selection> {
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    let local = absolute_row_to_local(screen, abs_row)?;
    let origin_local = absolute_row_to_local(screen, selection.origin.row)?;

    let head_row = &screen.grid.rows[local];
    let origin_row = &screen.grid.rows[origin_local];

    let new_point = SelectionPoint { row: abs_row, col };
    let forward = (new_point.row, new_point.col) >= (selection.origin.row, selection.origin.col);

    let (anchor, head) = match selection.mode {
        SelectionMode::Char => (selection.origin, new_point),
        SelectionMode::Word => {
            let (o_start, o_end) = expand_to_word(origin_row, selection.origin.col);
            let (h_start, h_end) = expand_to_word(head_row, col);
            if forward {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_start,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_end,
                    },
                )
            } else {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_end,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_start,
                    },
                )
            }
        }
        SelectionMode::Line => {
            let (o_start, o_end) = expand_to_line(origin_row);
            let (h_start, h_end) = expand_to_line(head_row);
            if forward {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_start,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_end,
                    },
                )
            } else {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_end,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_start,
                    },
                )
            }
        }
    };

    Some(Selection {
        anchor,
        head,
        mode: selection.mode,
        origin: selection.origin,
    })
}

pub fn is_cell_selected(
    selection: Option<&Selection>,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    screen_col: u32,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    if selection.is_empty() {
        return false;
    }
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    selection.contains(SelectionPoint {
        row: abs_row,
        col: screen_col,
    })
}

pub fn close_search(
    search: &mut SearchState,
    selection: &mut Option<Selection>,
) {
    if let Some(&active) = search.matches.get(search.active_idx) {
        let anchor = SelectionPoint {
            row: active.row,
            col: active.start_col,
        };
        let head = SelectionPoint {
            row: active.row,
            col: active.end_col,
        };
        *selection = Some(Selection {
            anchor,
            head,
            mode: SelectionMode::Char,
            origin: anchor,
        });
    }
    search.active = false;
    search.query.clear();
    search.matches.clear();
    search.active_idx = 0;
}

pub fn search_state(search: &SearchState) -> Option<&SearchState> {
    if search.active { Some(search) } else { None }
}

#[must_use]
pub fn search_append(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
    s: &str,
) -> u32 {
    if !search.active {
        return screen.offset;
    }
    search.query.push_str(s);
    refresh_search(search, screen, viewport)
}

#[must_use]
pub fn search_backspace(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    if !search.active {
        return screen.offset;
    }
    search.query.pop();
    refresh_search(search, screen, viewport)
}

#[must_use]
pub fn search_step_next(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    if !search.active || search.matches.is_empty() {
        return screen.offset;
    }
    search.active_idx = (search.active_idx + 1) % search.matches.len();
    scroll_to_active_match(search, screen, viewport)
}

#[must_use]
pub fn search_step_prev(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    if !search.active || search.matches.is_empty() {
        return screen.offset;
    }
    let n = search.matches.len();
    search.active_idx = (search.active_idx + n - 1) % n;
    scroll_to_active_match(search, screen, viewport)
}

pub fn is_cell_match(
    search: &SearchState,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    screen_col: u32,
) -> bool {
    if !search.active || search.matches.is_empty() {
        return false;
    }
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    search
        .matches
        .iter()
        .any(|m| m.contains(abs_row, screen_col))
}

pub fn is_cell_active_match(
    search: &SearchState,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    screen_col: u32,
) -> bool {
    if !search.active {
        return false;
    }
    let Some(active) = search.matches.get(search.active_idx) else {
        return false;
    };
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    active.contains(abs_row, screen_col)
}

#[must_use]
fn refresh_search(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    recompute_matches(search, screen);
    if search.matches.is_empty() {
        search.active_idx = 0;
        return screen.offset;
    }
    let viewport_top = screen_row_to_absolute(screen, viewport, 0);
    search.active_idx = search
        .matches
        .iter()
        .position(|m| m.row >= viewport_top)
        .unwrap_or(0);
    scroll_to_active_match(search, screen, viewport)
}

fn recompute_matches(
    search: &mut SearchState,
    screen: &Screen,
) {
    search.matches.clear();
    if search.query.is_empty() {
        return;
    }
    let q = search.query.as_str();
    let popped = screen.grid.total_popped as u64;
    let mut text = String::new();
    let mut cell_byte_starts: Vec<usize> = Vec::new();
    for (local, row) in screen.grid.rows.iter().enumerate() {
        text.clear();
        cell_byte_starts.clear();
        cell_byte_starts.reserve(row.cells.len());
        for cell in &row.cells {
            cell_byte_starts.push(text.len());
            text.push_str(cell);
        }
        if text.len() < q.len() {
            continue;
        }
        let abs_row = popped + local as u64;
        for (byte, _) in text.match_indices(q) {
            let start_col = cell_byte_starts
                .partition_point(|&s| s <= byte)
                .saturating_sub(1) as u32;
            let end_byte = byte + q.len();
            let end_col = cell_byte_starts
                .partition_point(|&s| s < end_byte)
                .saturating_sub(1) as u32;
            search.matches.push(MatchSpan {
                row: abs_row,
                start_col,
                end_col,
            });
        }
    }
}

#[must_use]
fn scroll_to_active_match(
    search: &SearchState,
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    let Some(m) = search.matches.get(search.active_idx).copied() else {
        return screen.offset;
    };
    let popped = screen.grid.total_popped as u64;
    let Some(local) = m.row.checked_sub(popped) else {
        return screen.offset;
    };
    let local = local as usize;
    let grid_len = screen.grid.rows.len();
    let rows = viewport.rows as usize;
    if local >= grid_len {
        return screen.offset;
    }
    if grid_len <= rows {
        return 0;
    }
    let ideal_top = local.saturating_sub(rows / 2);
    let max_top = grid_len - rows;
    let top = ideal_top.min(max_top);
    let next_offset = (grid_len - rows - top) as u32;
    let max_offset = screen.grid.scrollback_len(viewport);
    next_offset.min(max_offset)
}

pub fn selection_text(
    selection: Option<&Selection>,
    screen: &Screen,
) -> Option<String> {
    let selection = selection?;
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.ordered();
    let popped = screen.grid.total_popped as u64;
    let last_idx = screen.grid.rows.len().saturating_sub(1);

    let mut out = String::new();
    for abs_row in start.row..=end.row {
        let local = abs_row.checked_sub(popped)? as usize;
        if local > last_idx {
            break;
        }
        let row = &screen.grid.rows[local];
        let row_len_cols = row.cells.len() as u32;
        if row_len_cols == 0 {
            if abs_row < end.row && !row.wrapped {
                out.push('\n');
            }
            continue;
        }

        let (col_start, col_end, trim) = match selection.mode {
            SelectionMode::Line => (0, row_len_cols - 1, true),
            _ => {
                let is_first = abs_row == start.row;
                let is_last = abs_row == end.row;
                let cs = if is_first { start.col } else { 0 };
                let ce = if is_last { end.col } else { row_len_cols - 1 };
                let trim = !is_last;
                (cs, ce, trim)
            }
        };
        let col_end = col_end.min(row_len_cols - 1);
        if col_start > col_end {
            if abs_row < end.row && !row.wrapped {
                out.push('\n');
            }
            continue;
        }

        let mut segment = String::new();
        for cell in &row.cells[col_start as usize..=col_end as usize] {
            segment.push_str(cell);
        }
        if trim {
            out.push_str(segment.trim_end_matches(' '));
        } else {
            out.push_str(&segment);
        }

        if abs_row < end.row && !row.wrapped {
            out.push('\n');
        }
    }

    Some(out)
}

pub fn copy_selection(
    clipboard: &mut Clipboard,
    selection: Option<&Selection>,
    screen: &Screen,
    kind: ClipboardKind,
) {
    if let Some(text) = selection_text(selection, screen) {
        clipboard.set(kind, &text);
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
