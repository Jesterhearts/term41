//! Search-in-scrollback state.
//!
//! Match positions use absolute row indices — `Grid::total_popped +
//! index_in_rows` — so matches stay pinned to their row content even as
//! new PTY output trims the front of scrollback. Matching is exact,
//! case-sensitive, and single-row only in this first cut; a query longer
//! than a row's content simply finds no hits there.

use super::coords::screen_row_to_absolute;
use super::model::Selection;
use super::model::SelectionMode;
use super::model::SelectionPoint;
use crate::Screen;
use crate::Viewport;

/// A single match span, inclusive on both ends. Multi-byte grapheme cells
/// still count as one column each, matching how the row stores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchSpan {
    /// Absolute row containing the match.
    pub row: u64,
    /// Inclusive start column.
    pub start_col: u32,
    /// Inclusive end column.
    pub end_col: u32,
}

impl MatchSpan {
    /// Whether this span covers the given absolute cell.
    pub fn contains(
        &self,
        row: u64,
        col: u32,
    ) -> bool {
        self.row == row && col >= self.start_col && col <= self.end_col
    }
}

/// Live state of the search bar. Held by `Terminal` so both the keyboard
/// router and the renderer can consult it without extra plumbing.
#[derive(Debug, Default)]
pub struct SearchState {
    /// True while the search bar is open and consuming keyboard input.
    pub active: bool,
    /// Typed query. Empty query produces no matches.
    pub query: String,
    /// All matches currently in the grid, in document order.
    pub matches: Vec<MatchSpan>,
    /// Index of the "focused" match — the one `next`/`prev` cycle from,
    /// and the one the viewport scrolls to. Ignored when `matches` is empty.
    pub active_idx: usize,
}

impl SearchState {
    /// Create an inactive empty search state.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Close search and select the active match, if one exists.
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
            rendered: false,
            origin: anchor,
        });
    }
    search.active = false;
    search.query.clear();
    search.matches.clear();
    search.active_idx = 0;
}

/// Open search and reset any prior query/matches.
pub fn open_search(search: &mut SearchState) {
    search.active = true;
    search.query.clear();
    search.matches.clear();
    search.active_idx = 0;
}

/// Whether search is currently active.
pub fn search_active(search: &SearchState) -> bool {
    search.active
}

/// Return search state only while search is active.
pub fn search_state(search: &SearchState) -> Option<&SearchState> {
    if search.active { Some(search) } else { None }
}

#[must_use]
/// Append text to the search query and return the next viewport offset.
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
/// Remove one character from the search query and return the next viewport
/// offset.
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
/// Move to the next match and return the next viewport offset.
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
/// Move to the previous match and return the next viewport offset.
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

/// Whether a viewport cell belongs to any search match.
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

/// Whether a viewport cell belongs to the active search match.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_contains_is_inclusive() {
        let m = MatchSpan {
            row: 3,
            start_col: 4,
            end_col: 6,
        };
        assert!(!m.contains(3, 3));
        assert!(m.contains(3, 4));
        assert!(m.contains(3, 5));
        assert!(m.contains(3, 6));
        assert!(!m.contains(3, 7));
        assert!(!m.contains(2, 5));
    }
}
