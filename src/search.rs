//! Search-in-scrollback state.
//!
//! Match positions use absolute row indices — `Grid::total_popped +
//! index_in_rows` — so matches stay pinned to their row content even as
//! new PTY output trims the front of scrollback. Matching is exact,
//! case-sensitive, and single-row only in this first cut; a query longer
//! than a row's content simply finds no hits there.

/// A single match span, inclusive on both ends. Multi-byte grapheme cells
/// still count as one column each, matching how the row stores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchSpan {
    pub row: u64,
    pub start_col: u32,
    pub end_col: u32,
}

impl MatchSpan {
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
    pub fn new() -> Self {
        Self::default()
    }
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
