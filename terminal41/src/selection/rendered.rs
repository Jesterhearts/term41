use super::active::extend_selection;
use super::active::start_selection;
use super::coords::rendered_screen_row_at_viewport_row;
use super::model::Selection;
use super::model::SelectionMode;
use super::model::SelectionPoint;
use super::word::expand_to_line;
use super::word::expand_to_word;
use crate::Row;
use crate::Screen;
use crate::Viewport;
use crate::screen;

fn rendered_view_top(
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

fn completed_rendered_rows_len(screen: &Screen) -> u64 {
    screen
        .scrollback_blocks
        .iter()
        .map(|block| screen::command_block_rendered_rows_len(block) as u64 + 1)
        .sum()
}

fn rendered_local_row_to_document_row(
    screen: &Screen,
    rendered_row: u32,
) -> Option<u64> {
    let mut idx = rendered_row;
    let mut base = 0_u64;
    for block in &screen.scrollback_blocks {
        let block_rows = screen::command_block_rendered_rows_len(block) as u32;
        if idx < block_rows {
            return Some(base + idx as u64);
        }
        idx -= block_rows;
        base += block_rows as u64;
        if idx == 0 {
            return Some(base);
        }
        idx -= 1;
        base += 1;
    }
    let active_rows = screen::active_block_rendered_rows_len(screen) as u32;
    (idx < active_rows)
        .then(|| completed_rendered_rows_len(screen) + screen.grid.total_popped as u64 + idx as u64)
}

pub fn rendered_document_row_at_viewport_row(
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    viewport_row: u32,
) -> Option<u64> {
    let screen_row =
        rendered_screen_row_at_viewport_row(screen, viewport, on_alt_screen, viewport_row)?;
    if on_alt_screen {
        return Some(screen_row as u64);
    }
    let rendered_row = rendered_view_top(screen, viewport) + screen_row;
    rendered_local_row_to_document_row(screen, rendered_row)
}

pub(super) fn rendered_row_ref(
    screen: &Screen,
    rendered_row: u64,
) -> Option<&Row> {
    let idx = rendered_row;
    let mut base = 0_u64;
    for block in &screen.scrollback_blocks {
        let block_rows = screen::command_block_rendered_rows_len(block) as u64;
        if idx < base + block_rows {
            let local = idx - base;
            return block.grid.rows.get(local as usize);
        }
        base += block_rows;
        if idx == base {
            return None;
        }
        base += 1;
    }
    let active_base = base + screen.grid.total_popped as u64;
    let local = idx.checked_sub(active_base)? as usize;
    screen.grid.rows.get(local)
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
            row: rendered_row,
            col,
        },
        row,
    ))
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
    let origin_row = rendered_row_ref(screen, selection.origin.row)?;
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

pub fn is_rendered_cell_selected(
    selection: Option<&Selection>,
    rendered_row: u64,
    screen_col: u32,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    if !selection.rendered || selection.is_empty() {
        return false;
    }
    selection.contains(SelectionPoint {
        row: rendered_row,
        col: screen_col,
    })
}
