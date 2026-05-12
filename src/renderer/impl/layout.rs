use terminal41::RowSnapshot;
use terminal41::TermSnapshot;

pub(super) struct FrameLayout {
    pub(super) cell_w: f32,
    pub(super) cell_h: f32,
    pub(super) baseline: f32,
    pub(super) gutter_px: f32,
    pub(super) tab_bar_h: f32,
    pub(super) terminal_y_offset: f32,
    pub(super) block_y_offset: f32,
}

#[derive(Clone, Copy)]
pub(super) struct ClipRect {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) right: f32,
    pub(super) bottom: f32,
}

pub(super) fn terminal_row_y(
    row: u32,
    layout: &FrameLayout,
) -> f32 {
    row as f32 * layout.cell_h + layout.tab_bar_h + layout.terminal_y_offset + layout.block_y_offset
}

pub(super) fn snapshot_row_y(
    row: u32,
    snap: &TermSnapshot,
    layout: &FrameLayout,
) -> f32 {
    let terminal_offset =
        if snap.status_line_row == Some(row) || sticky_prompt_row_at_top(row, snap) {
            0.0
        } else {
            layout.terminal_y_offset + layout.block_y_offset
        };
    row as f32 * layout.cell_h + layout.tab_bar_h + terminal_offset
}

pub(super) fn sticky_prompt_row_at_top(
    row: u32,
    snap: &TermSnapshot,
) -> bool {
    row == 0
        && snap
            .rows
            .iter()
            .any(|snap_row| snap_row.screen_row == 0 && snap_row.sticky_prompt)
}

pub(super) fn row_hidden_by_sticky_prompt(
    snap_row: &RowSnapshot,
    snap: &TermSnapshot,
    layout: &FrameLayout,
) -> bool {
    if snap_row.sticky_prompt || snap.status_line_row == Some(snap_row.screen_row) {
        return false;
    }
    if !sticky_prompt_row_at_top(0, snap) {
        return false;
    }

    let sticky_top = layout.tab_bar_h;
    let sticky_bottom = sticky_top + layout.cell_h;
    let row_top = snapshot_row_y(snap_row.screen_row, snap, layout);
    let row_bottom = row_top + layout.cell_h;

    row_top < sticky_bottom && row_bottom > sticky_top
}

pub(super) fn row_suspended_by_terminal_area(
    snap_row: &RowSnapshot,
    snap: &TermSnapshot,
    suspend_terminal_area: bool,
) -> bool {
    suspend_terminal_area && snap.status_line_row != Some(snap_row.screen_row)
}

pub(super) fn terminal_block_y_offset_rows(
    rows: &[RowSnapshot],
    snap: &TermSnapshot,
) -> u32 {
    if snap.on_alt_screen || snap.viewport_offset != 0 {
        return 0;
    }
    let terminal_row_count = rows
        .iter()
        .filter(|row| snap.status_line_row != Some(row.screen_row))
        .filter(|row| row.screen_row < snap.viewport_rows)
        .count();
    if terminal_row_count >= snap.viewport_rows as usize {
        return 0;
    }
    let row_content = rows
        .iter()
        .filter(|row| snap.status_line_row != Some(row.screen_row))
        .filter(|row| row.screen_row < snap.viewport_rows)
        .filter(|row| row_has_rendered_content(row))
        .map(|row| row.screen_row + 1)
        .max()
        .unwrap_or(0);
    let cursor_content = snap.cursor.map_or(
        0,
        |(row, _)| {
            if row < snap.viewport_rows { row + 1 } else { 0 }
        },
    );
    let content_rows = row_content.max(cursor_content);
    if content_rows == 0 {
        return 0;
    }
    snap.viewport_rows.saturating_sub(content_rows)
}

pub(super) fn row_has_rendered_content(row: &RowSnapshot) -> bool {
    row.block_separator
        || row.cells.iter().any(|cell| cell != " ")
        || row.has_link.iter().any(|&v| v)
}

pub(super) fn visible_command_editor<'a>(
    command_editor: Option<&'a commands41::CommandLineView>,
    snap: &TermSnapshot,
) -> Option<&'a commands41::CommandLineView> {
    command_editor.filter(|_| {
        !snap.command_editor_hidden
            && !snap.on_alt_screen
            && !snap.search_active
            && snap.viewport_offset == 0
    })
}
