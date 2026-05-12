use crate::Screen;
use crate::Viewport;
use crate::screen;

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
