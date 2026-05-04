//! Text-selection model for mouse-driven copy.
//!
//! Positions are stored in **absolute** row coordinates — `total_popped +
//! index` into the grid — so selections stay anchored to their content even
//! as scrollback trims the front of the grid or the user scrolls history.

pub mod search;

use clip41::Clipboard;
use clip41::ClipboardKind;
use unicode_segmentation::UnicodeSegmentation;

use crate::Row;
use crate::Screen;
use crate::Viewport;
use crate::screen;
use crate::selection::search::MatchSpan;
use crate::selection::search::SearchState;

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

/// Map a physical viewport row from mouse input to the terminal row index used
/// by snapshots and selection APIs.
pub fn rendered_screen_row_at_viewport_row(
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    viewport_row: u32,
) -> Option<u32> {
    if viewport_row >= viewport.rows {
        return None;
    }
    if on_alt_screen || screen.offset != 0 {
        return Some(viewport_row);
    }

    let rendered_rows = screen::rendered_rows_len(screen) as u32;
    let visible_rows = rendered_rows.min(viewport.rows).max(1);
    let row_offset = viewport.rows.saturating_sub(visible_rows);
    if viewport_row < row_offset {
        return None;
    }
    Some(viewport_row - row_offset)
}

/// Map a physical viewport row from mouse input to a selectable active-grid
/// screen row. Completed command blocks own separate grids, so they are not
/// addressable by the current selection model.
pub fn active_screen_row_at_viewport_row(
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    viewport_row: u32,
) -> Option<u32> {
    let screen_row =
        rendered_screen_row_at_viewport_row(screen, viewport, on_alt_screen, viewport_row)?;
    if on_alt_screen || screen.scrollback_blocks.is_empty() {
        return Some(screen_row);
    }

    let rendered_len = screen::rendered_rows_len(screen) as u32;
    let max_top = rendered_len.saturating_sub(viewport.rows);
    let top = max_top.saturating_sub(screen.offset);
    let mut idx = top + screen_row;
    for block in &screen.scrollback_blocks {
        let block_rows = screen::command_block_rendered_rows_len(block) as u32;
        if idx < block_rows {
            return None;
        }
        idx -= block_rows;
        if idx == 0 {
            return None;
        }
        idx -= 1;
    }
    Some(idx)
}

fn rendered_document_top(
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    let rendered_len = screen::rendered_rows_len(screen) as u32;
    let visible_rows = rendered_len.min(viewport.rows).max(1);
    let row_offset = viewport.rows.saturating_sub(visible_rows);
    let max_top = rendered_len.saturating_sub(visible_rows);
    max_top
        .saturating_sub(screen.offset)
        .saturating_sub(row_offset)
}

pub fn rendered_document_row_at_viewport_row(
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    viewport_row: u32,
) -> Option<u32> {
    let screen_row =
        rendered_screen_row_at_viewport_row(screen, viewport, on_alt_screen, viewport_row)?;
    if on_alt_screen || screen.scrollback_blocks.is_empty() {
        return Some(screen_row);
    }
    Some(rendered_document_top(screen, viewport) + screen_row)
}

fn rendered_row_ref(
    screen: &Screen,
    rendered_row: u32,
) -> Option<&Row> {
    let mut idx = rendered_row;
    for block in &screen.scrollback_blocks {
        let block_rows = screen::command_block_rendered_rows_len(block) as u32;
        if idx < block_rows {
            return block.grid.rows.get(idx as usize);
        }
        idx -= block_rows;
        if idx == 0 {
            return None;
        }
        idx -= 1;
    }
    screen.grid.rows.get(idx as usize)
}

fn rendered_selection_point_at_viewport_row<'a>(
    screen: &'a Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    viewport_row: u32,
    col: u32,
) -> Option<(SelectionPoint, &'a Row)> {
    let rendered_row =
        rendered_document_row_at_viewport_row(screen, viewport, on_alt_screen, viewport_row)?;
    let row = rendered_row_ref(screen, rendered_row)?;
    Some((
        SelectionPoint {
            row: rendered_row as u64,
            col,
        },
        row,
    ))
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
pub fn start_rendered_selection(
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    col: u32,
    viewport_row: u32,
    mode: SelectionMode,
) -> Option<Selection> {
    if on_alt_screen {
        return start_selection(screen, viewport, col, viewport_row, mode);
    }
    let (origin, row) = rendered_selection_point_at_viewport_row(
        screen,
        viewport,
        on_alt_screen,
        viewport_row,
        col,
    )?;
    let (anchor, head) = match mode {
        SelectionMode::Char => (origin, origin),
        SelectionMode::Word => {
            let (s, e) = expand_to_word(row, col);
            (
                SelectionPoint {
                    row: origin.row,
                    col: s,
                },
                SelectionPoint {
                    row: origin.row,
                    col: e,
                },
            )
        }
        SelectionMode::Line => {
            let (s, e) = expand_to_line(row);
            (
                SelectionPoint {
                    row: origin.row,
                    col: s,
                },
                SelectionPoint {
                    row: origin.row,
                    col: e,
                },
            )
        }
    };
    Some(Selection {
        anchor,
        head,
        mode,
        rendered: true,
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
pub fn extend_rendered_selection(
    selection: &Selection,
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    col: u32,
    viewport_row: u32,
) -> Option<Selection> {
    if on_alt_screen {
        return extend_selection(selection, screen, viewport, col, viewport_row);
    }
    if !selection.rendered {
        return extend_selection(selection, screen, viewport, col, viewport_row);
    }
    let (new_point, head_row) = rendered_selection_point_at_viewport_row(
        screen,
        viewport,
        on_alt_screen,
        viewport_row,
        col,
    )?;
    let origin_row = rendered_row_ref(screen, selection.origin.row as u32)?;
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
                        row: new_point.row,
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
                        row: new_point.row,
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
                        row: new_point.row,
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
                        row: new_point.row,
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
        rendered: true,
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

pub fn is_rendered_cell_selected(
    selection: Option<&Selection>,
    rendered_row: u32,
    screen_col: u32,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    if !selection.rendered || selection.is_empty() {
        return false;
    }
    selection.contains(SelectionPoint {
        row: rendered_row as u64,
        col: screen_col,
    })
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

/// Extract selected text from the screen.
pub fn selection_text(
    selection: Option<&Selection>,
    screen: &Screen,
) -> Option<String> {
    let selection = selection?;
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.ordered();
    if selection.rendered {
        return rendered_selection_text(selection, screen, start, end);
    }
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

fn rendered_selection_text(
    selection: &Selection,
    screen: &Screen,
    start: SelectionPoint,
    end: SelectionPoint,
) -> Option<String> {
    let mut out = String::new();
    for abs_row in start.row..=end.row {
        let Some(row) = rendered_row_ref(screen, abs_row as u32) else {
            if abs_row < end.row {
                out.push('\n');
            }
            continue;
        };
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
        if col_start <= col_end {
            let mut segment = String::new();
            for cell in &row.cells[col_start as usize..=col_end as usize] {
                segment.push_str(cell);
            }
            if trim {
                out.push_str(segment.trim_end_matches(' '));
            } else {
                out.push_str(&segment);
            }
        }
        if abs_row < end.row && !row.wrapped {
            out.push('\n');
        }
    }
    Some(out)
}

/// Copy selected text into the requested clipboard selection.
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
mod integration_tests {
    use clip41::Clipboard;
    use clip41::ClipboardKind;

    use super::*;
    use crate::test_support::TestTerm;

    fn write_row(
        term: &mut TestTerm,
        screen_row: u32,
        text: &str,
    ) {
        term.process(format!("\x1b[{};1H", screen_row + 1).as_bytes());
        term.process(text.as_bytes());
    }

    #[test]
    fn start_selection_char_mode_is_empty_initially() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            1,
            SelectionMode::Char,
        );
        assert!(term.selection.is_some());
        assert!(!term.has_selection());
    }

    #[test]
    fn char_selection_extend_produces_text() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn viewport_mouse_row_maps_to_bottom_aligned_active_row() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");

        assert_eq!(
            rendered_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                0,
            ),
            None
        );
        assert_eq!(
            rendered_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                2,
            ),
            Some(0)
        );
        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                2,
            ),
            Some(0)
        );

        let row = active_screen_row_at_viewport_row(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            2,
        )
        .unwrap();
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            row,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            row,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn viewport_mouse_row_does_not_select_completed_command_blocks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"old");
        term.process(b"\x1b]133;A\x07new");

        assert_eq!(
            rendered_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                1,
            ),
            Some(0)
        );
        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                1,
            ),
            None
        );
        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                3,
            ),
            Some(0)
        );
    }

    #[test]
    fn viewport_mouse_row_maps_to_active_row_after_multiple_command_blocks() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                4,
            ),
            Some(0)
        );
    }

    #[test]
    fn rendered_mouse_selection_can_copy_completed_command_blocks() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            2,
            2,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("one\n\ntwo")
        );
    }

    #[test]
    fn bottom_aligned_rendered_mouse_selection_can_copy_visible_blocks() {
        let mut term = TestTerm::new(10, 8, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            3,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            2,
            5,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("one\n\ntwo")
        );
    }

    #[test]
    fn rendered_mouse_selection_uses_active_grid_on_alt_screen() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");
        term.process(b"\x1b[?1049h");
        term.process(b"alpha\r\nbeta");

        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            3,
            1,
        );

        assert_eq!(
            term.inner
                .selection
                .as_ref()
                .map(|selection| selection.rendered),
            Some(false)
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("alpha\nbeta")
        );
    }

    #[test]
    fn word_selection_snaps_to_boundaries() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
            SelectionMode::Word,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn line_selection_covers_full_row_through_test_term() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            5,
            0,
            SelectionMode::Line,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn selection_spans_rows_with_newline_separator() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abc");
        write_row(&mut term, 1, "def");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            2,
            1,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("abc\ndef")
        );
    }

    #[test]
    fn selection_can_extend_into_scrolled_history() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        for i in 0..6 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.process(b"tail");
        assert!(term.active.grid.scrollback_len(&term.viewport) > 0);

        let live_bottom = term.viewport.rows - 1;
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            3,
            live_bottom,
            SelectionMode::Char,
        );
        let origin = term.inner.selection.as_ref().unwrap().origin;

        let viewport = term.inner.viewport;
        crate::view::scroll_viewport_up(&mut term.inner.active, &viewport, 1);
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
        );

        let selection = term.inner.selection.as_ref().unwrap();
        let (start, end) = selection.ordered();
        assert!(start.row < origin.row);
        assert_eq!(end, origin);
        assert!(
            selection_text(Some(selection), &term.inner.active)
                .unwrap()
                .contains("tail")
        );
    }

    #[test]
    fn shift_extension_uses_selection_start_after_viewport_scroll() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        for i in 0..6 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.process(b"tail");

        let live_bottom = term.viewport.rows - 1;
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            live_bottom,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            3,
            live_bottom,
        );
        let original_start = term.inner.selection.as_ref().unwrap().ordered().0;

        let viewport = term.inner.viewport;
        crate::view::scroll_viewport_up(&mut term.inner.active, &viewport, 1);
        term.inner.selection = extend_selection_from_start(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );

        let selection = term.inner.selection.as_ref().unwrap();
        assert_eq!(selection.anchor, original_start);
        assert_eq!(selection.origin, original_start);
        assert_eq!(
            selection.head,
            SelectionPoint {
                row: screen_row_to_absolute(&term.inner.active, &term.inner.viewport, 0),
                col: 4,
            }
        );
    }

    #[test]
    fn selection_drags_backwards_flips_anchor_head() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            8,
            0,
            SelectionMode::Word,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn is_cell_selected_matches_contains() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abcdefghij");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            5,
            0,
        );
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            1,
            0
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            2,
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            5,
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            1,
            3,
        ));
    }

    #[test]
    fn search_finds_exact_case_sensitive_matches() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "abc foo xyz FOO bar");
        term.open_search();
        assert!(term.search_active());
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        assert_eq!(term.search.matches.len(), 1);
        let m = term.search.matches[0];
        assert_eq!((m.start_col, m.end_col), (4, 6));
        assert!(is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            4
        ));
        assert!(is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            5
        ));
        assert!(is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            3
        ));
        assert!(!is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            7
        ));
        assert!(!is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            12
        ));
    }

    #[test]
    fn search_close_clears_state() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "hello",
        );
        assert_eq!(term.search.matches.len(), 1);
        close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(!term.search_active());
        assert!(term.search.matches.is_empty());
        assert!(term.search.query.is_empty());
    }

    #[test]
    fn search_close_promotes_active_match_to_selection() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "abc foo def");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            4
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            5
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            3
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            7
        ));
    }

    #[test]
    fn search_close_without_matches_leaves_prior_selection() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        assert!(term.has_selection());
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "nonexistent",
        );
        close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(is_cell_selected(
            term.selection.as_ref(),
            &term.active,
            &term.inner.viewport,
            0,
            0
        ));
        assert!(is_cell_selected(
            term.selection.as_ref(),
            &term.active,
            &term.inner.viewport,
            0,
            4
        ));
    }

    #[test]
    fn search_next_wraps_around() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "foo");
        write_row(&mut term, 1, "foo");
        write_row(&mut term, 2, "foo");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        assert_eq!(term.search.matches.len(), 3);
        let start_idx = term.search.active_idx;
        term.active.offset = search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        term.active.offset = search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        term.active.offset = search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        assert_eq!(term.search.active_idx, start_idx);
    }

    #[test]
    fn search_backspace_trims_query_and_rescans() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "fox foxy fo");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foxy",
        );
        assert_eq!(term.search.matches.len(), 1);
        term.active.offset = search_backspace(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        assert_eq!(term.search.matches.len(), 2);
    }

    #[test]
    fn copy_selection_writes_to_clipboard() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        write_row(&mut term, 0, "copy-me");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            6,
            0,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            6,
            0,
        );
        copy_selection(
            &mut term.inner.clipboard,
            term.inner.selection.as_ref(),
            &term.inner.active,
            ClipboardKind::Clipboard,
        );
        assert_eq!(
            term.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("copy-me")
        );
        assert!(term.has_selection());
    }

    #[test]
    fn clear_selection_drops_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        term.inner.selection = None;
        assert!(term.inner.selection.is_none());
        assert!(selection_text(term.inner.selection.as_ref(), &term.inner.active).is_none());
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
            rendered: false,
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
