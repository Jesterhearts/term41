use super::coords::absolute_row_to_local;
use super::coords::screen_row_to_absolute;
use super::model::Selection;
use super::model::SelectionMode;
use super::model::SelectionPoint;
use super::word::expand_to_line;
use super::word::expand_to_word;
use crate::Screen;
use crate::Viewport;

#[must_use]
/// Start a selection at a viewport cell.
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
        rendered: false,
        origin,
    })
}

#[must_use]
/// Extend an existing selection to a new viewport cell.
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
        rendered: selection.rendered,
        origin: selection.origin,
    })
}

#[must_use]
/// Extend from the current ordered start of a selection to a new viewport
/// cell. This is used for shift-click extension, where the fixed endpoint is
/// the visible selection start rather than the original drag origin.
pub fn extend_selection_from_start(
    selection: &Selection,
    screen: &Screen,
    viewport: &Viewport,
    col: u32,
    screen_row: u32,
) -> Option<Selection> {
    let (start, _) = selection.ordered();
    absolute_row_to_local(screen, start.row)?;
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    absolute_row_to_local(screen, abs_row)?;
    let head = SelectionPoint { row: abs_row, col };

    Some(Selection {
        anchor: start,
        head,
        mode: SelectionMode::Char,
        rendered: selection.rendered,
        origin: start,
    })
}

/// Whether a viewport cell is covered by the current selection.
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
    if selection.rendered {
        return false;
    }
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    selection.contains(SelectionPoint {
        row: abs_row,
        col: screen_col,
    })
}
