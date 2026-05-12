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

/// Active selection state.
#[derive(Clone, Debug)]
pub struct Selection {
    /// Fixed endpoint where selection began.
    pub anchor: SelectionPoint,
    /// Moving endpoint.
    pub head: SelectionPoint,
    /// Expansion mode for this selection.
    pub mode: SelectionMode,
    /// True when row coordinates address the rendered command-block stream
    /// rather than the active grid.
    pub rendered: bool,
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
