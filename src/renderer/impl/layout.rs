use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use unicode_segmentation::UnicodeSegmentation;

pub(in crate::renderer) struct FrameLayout {
    pub(in crate::renderer) cell_w: f32,
    pub(in crate::renderer) cell_h: f32,
    pub(in crate::renderer) baseline: f32,
    pub(in crate::renderer) gutter_px: f32,
    pub(in crate::renderer) tab_bar_h: f32,
    pub(in crate::renderer) terminal_y_offset: f32,
    pub(in crate::renderer) block_y_offset: f32,
}

#[derive(Clone, Copy)]
pub(in crate::renderer) struct ClipRect {
    pub(in crate::renderer) left: f32,
    pub(in crate::renderer) top: f32,
    pub(in crate::renderer) right: f32,
    pub(in crate::renderer) bottom: f32,
}

#[derive(Clone, Copy)]
pub(in crate::renderer) struct CommandCompletionLayout {
    pub(in crate::renderer) x: f32,
    pub(in crate::renderer) y: f32,
    pub(in crate::renderer) cursor_row: u32,
    pub(in crate::renderer) cols: usize,
}

pub(in crate::renderer) fn terminal_row_y(
    row: u32,
    layout: &FrameLayout,
) -> f32 {
    row as f32 * layout.cell_h + layout.tab_bar_h + layout.terminal_y_offset + layout.block_y_offset
}

pub(in crate::renderer) fn snapshot_row_y(
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

pub(in crate::renderer) fn sticky_prompt_row_at_top(
    row: u32,
    snap: &TermSnapshot,
) -> bool {
    row == 0
        && snap
            .rows
            .iter()
            .any(|snap_row| snap_row.screen_row == 0 && snap_row.sticky_prompt)
}

pub(in crate::renderer) fn row_hidden_by_sticky_prompt(
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

pub(in crate::renderer) fn row_suspended_by_terminal_area(
    snap_row: &RowSnapshot,
    snap: &TermSnapshot,
    suspend_terminal_area: bool,
) -> bool {
    suspend_terminal_area && snap.status_line_row != Some(snap_row.screen_row)
}

pub(in crate::renderer) fn terminal_block_y_offset_rows(
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

pub(in crate::renderer) fn row_has_rendered_content(row: &RowSnapshot) -> bool {
    row.block_separator
        || row.cells.iter().any(|cell| cell != " ")
        || row.has_link.iter().any(|&v| v)
}

pub(in crate::renderer) fn visible_command_editor<'a>(
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

pub(in crate::renderer) fn apply_terminal_layout_offsets(
    layout: &mut FrameLayout,
    snap: &TermSnapshot,
) -> u32 {
    let block_y_offset_rows = terminal_block_y_offset_rows(&snap.rows, snap);
    layout.block_y_offset = block_y_offset_rows as f32 * layout.cell_h;
    layout.terminal_y_offset = 0.0;
    block_y_offset_rows
}

pub(in crate::renderer) fn command_completion_layout(
    snap: &TermSnapshot,
    layout: &FrameLayout,
) -> Option<CommandCompletionLayout> {
    let (cursor_row, cursor_col) = snap.cursor?;
    let block_offset_rows = (layout.block_y_offset / layout.cell_h).round().max(0.0) as u32;
    Some(CommandCompletionLayout {
        x: layout.gutter_px + cursor_col as f32 * layout.cell_w,
        y: terminal_row_y(cursor_row, layout),
        cursor_row: cursor_row.saturating_add(block_offset_rows),
        cols: snap.viewport_cols.saturating_sub(cursor_col) as usize,
    })
}

pub(in crate::renderer) fn command_completion_list_y(
    completion: &CommandCompletionLayout,
    list_h: f32,
    snap: &TermSnapshot,
    layout: &FrameLayout,
) -> f32 {
    use crate::window_host::CommandEditorPopupSide;
    use crate::window_host::command_editor_popup_side_for_row;
    match command_editor_popup_side_for_row(completion.cursor_row, snap.viewport_rows) {
        CommandEditorPopupSide::Below => (completion.y + layout.cell_h)
            .min(layout.tab_bar_h + snap.viewport_rows as f32 * layout.cell_h - list_h)
            .max(layout.tab_bar_h),
        CommandEditorPopupSide::Above => (completion.y - list_h).max(layout.tab_bar_h),
    }
}

pub(in crate::renderer) fn truncate_command_label(
    text: &str,
    max_cells: usize,
) -> String {
    let mut output = String::new();
    let mut cells = 0;
    for grapheme in text.graphemes(true) {
        let width = unicode_width::UnicodeWidthStr::width(grapheme);
        if grapheme.contains(['\n', '\r']) || cells + width > max_cells {
            break;
        }
        output.push_str(grapheme);
        cells += width;
    }
    output
}

pub(in crate::renderer) fn command_cursor_shape(
    default: config41::CursorShape,
    editor: Option<&commands41::CommandLineView>,
) -> config41::CursorShape {
    editor.map_or(default, |editor| match editor.cursor_style {
        commands41::CommandEditorCursorStyle::Block => config41::CursorShape::Block,
        commands41::CommandEditorCursorStyle::Beam => config41::CursorShape::Beam,
    })
}
