use crate::CsiAction;
use crate::MainCsiAction;
use crate::MouseTracking;
use crate::ParsedCsiAction;
use crate::ShellIntegrationPhase;
use crate::StatusDisplayKind;
use crate::Terminal;
use crate::dispatch;
use crate::screen;
use crate::screen::grid::Viewport;
use crate::snapshot::SnapshotState;
use crate::view;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotDirtyScope {
    None,
    CursorRows,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotDirtyBaseline {
    active_display: screen::ActiveDisplay,
    cursor_row: u32,
    cursor_col: u32,
    cursor_snapshot_row: Option<u32>,
    scroll_bottom: u32,
    grid_rows_len: usize,
    total_popped: usize,
    viewport_top: usize,
    viewport_rows: u32,
    viewport_cols: u32,
    offset: u32,
    total_rows: u32,
    status_line_row: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputContextState {
    on_alt_screen: bool,
    app_cursor_keys: bool,
    app_keypad: bool,
    mouse_tracking: MouseTracking,
    shell_integration_phase: ShellIntegrationPhase,
}

pub(crate) fn action_clears_history_blocks(action: &dispatch::TerminalAction<'_>) -> bool {
    matches!(
        action,
        dispatch::TerminalAction::Csi(CsiAction::Parsed(ParsedCsiAction::Main(
            MainCsiAction::EraseInDisplay { mode: 3 }
        )))
    )
}

fn active_cursor_snapshot_row(
    active: &screen::Screen,
    viewport: &Viewport,
) -> Option<u32> {
    rendered_active_row_to_snapshot_row(active, viewport, active.cursor.row)
}

fn rendered_active_row_to_snapshot_row(
    active: &screen::Screen,
    viewport: &Viewport,
    active_row: u32,
) -> Option<u32> {
    let rendered_row = rendered_active_block_top(active).saturating_add(active_row);
    let rendered_top = rendered_view_top_for_snapshot_dirty(active, viewport);
    let rendered_bottom = rendered_top.saturating_add(viewport.rows);
    (rendered_row >= rendered_top && rendered_row < rendered_bottom)
        .then(|| rendered_row - rendered_top)
}

fn rendered_active_block_top(active: &screen::Screen) -> u32 {
    active
        .scrollback_blocks
        .iter()
        .map(|block| screen::command_block_rendered_rows_len(block) as u32)
        .fold(0_u32, |top, rows| {
            top.saturating_add(rows.saturating_add(1))
        })
}

fn rendered_view_top_for_snapshot_dirty(
    active: &screen::Screen,
    viewport: &Viewport,
) -> u32 {
    let rendered_len = screen::rendered_rows_len(active) as u32;
    let max_top = rendered_len.saturating_sub(viewport.rows);
    max_top.saturating_sub(active.offset)
}

fn mark_snapshot_cursor_rows(
    snapshot: &mut SnapshotState,
    before: SnapshotDirtyBaseline,
    after: SnapshotDirtyBaseline,
) {
    match (before.cursor_snapshot_row, after.cursor_snapshot_row) {
        (Some(before_row), Some(after_row)) => snapshot.mark_rows(before_row, after_row),
        (Some(row), None) | (None, Some(row)) => snapshot.mark_row(row),
        (None, None) => {}
    }
}

fn mark_indicator_status_row(
    terminal: &mut Terminal,
    after: SnapshotDirtyBaseline,
) {
    if view::status_display_kind(&terminal.active) != StatusDisplayKind::Indicator {
        return;
    }
    if let Some(row) = after.status_line_row {
        terminal.snapshot.mark_row(row);
    }
}

pub(crate) fn snapshot_dirty_baseline(terminal: &Terminal) -> SnapshotDirtyBaseline {
    let status_line_row = view::status_line_row(&terminal.active).map(|_| terminal.viewport.rows);
    SnapshotDirtyBaseline {
        active_display: terminal.active.active_display,
        cursor_row: terminal.active.cursor.row,
        cursor_col: terminal.active.cursor.col,
        cursor_snapshot_row: active_cursor_snapshot_row(&terminal.active, &terminal.viewport),
        scroll_bottom: terminal.active.scroll_bottom,
        grid_rows_len: terminal.active.grid.rows.len(),
        total_popped: terminal.active.grid.total_popped,
        viewport_top: terminal.viewport.top_index(terminal.active.grid.rows.len()),
        viewport_rows: terminal.viewport.rows,
        viewport_cols: terminal.viewport.cols,
        offset: terminal.active.offset,
        total_rows: terminal.viewport.rows + u32::from(status_line_row.is_some()),
        status_line_row,
    }
}

pub(crate) fn input_context_state(terminal: &Terminal) -> InputContextState {
    InputContextState {
        on_alt_screen: terminal.on_alt_screen,
        app_cursor_keys: terminal.active.app_cursor_keys,
        app_keypad: terminal.active.app_keypad,
        mouse_tracking: terminal.modes.mouse_tracking,
        shell_integration_phase: terminal.metadata.shell_integration_phase,
    }
}

pub(crate) fn snapshot_dirty_scope(
    terminal: &Terminal,
    action: &dispatch::TerminalAction<'_>,
    before: SnapshotDirtyBaseline,
) -> SnapshotDirtyScope {
    match action {
        dispatch::TerminalAction::Ignore => SnapshotDirtyScope::None,
        dispatch::TerminalAction::Basic(action) => {
            basic_action_dirty_scope(terminal, action, before)
        }
        dispatch::TerminalAction::Vt52(action) => match action {
            dispatch::Vt52Action::AwaitCursorColumn => SnapshotDirtyScope::None,
            dispatch::Vt52Action::CursorPosition { trailing_ascii, .. } => {
                if trailing_ascii.is_empty() {
                    SnapshotDirtyScope::CursorRows
                } else {
                    SnapshotDirtyScope::All
                }
            }
        },
        dispatch::TerminalAction::Csi(_)
        | dispatch::TerminalAction::Esc(_)
        | dispatch::TerminalAction::Osc(_)
        | dispatch::TerminalAction::Apc(_) => SnapshotDirtyScope::All,
    }
}

fn basic_action_dirty_scope(
    terminal: &Terminal,
    action: &dispatch::BasicAction<'_>,
    before: SnapshotDirtyBaseline,
) -> SnapshotDirtyScope {
    if before.active_display == screen::ActiveDisplay::Status {
        return SnapshotDirtyScope::CursorRows;
    }
    if before.cursor_row != before.scroll_bottom {
        return SnapshotDirtyScope::CursorRows;
    }

    match action {
        dispatch::BasicAction::Execute(b'\n' | b'\x0b' | b'\x0c') => SnapshotDirtyScope::All,
        dispatch::BasicAction::PrintAscii(run) => {
            let cols = terminal.viewport.cols.max(1);
            if before.cursor_col.saturating_add(run.len() as u32) > cols {
                SnapshotDirtyScope::All
            } else {
                SnapshotDirtyScope::CursorRows
            }
        }
        dispatch::BasicAction::PrintText(run) => {
            // UTF-8 byte length is a cheap conservative upper bound for
            // terminal column width, so it can detect possible wrapping
            // without recounting chars on every mixed text run.
            if before.cursor_col.saturating_add(run.len() as u32) > terminal.viewport.cols.max(1) {
                SnapshotDirtyScope::All
            } else {
                SnapshotDirtyScope::CursorRows
            }
        }
        dispatch::BasicAction::Print(_) | dispatch::BasicAction::Print8Bit(_) => {
            if before.cursor_col.saturating_add(1) > terminal.viewport.cols.max(1) {
                SnapshotDirtyScope::All
            } else {
                SnapshotDirtyScope::CursorRows
            }
        }
        dispatch::BasicAction::Execute(_) => SnapshotDirtyScope::CursorRows,
    }
}

pub(crate) fn mark_snapshot_dirty_after(
    terminal: &mut Terminal,
    before: SnapshotDirtyBaseline,
    scope: SnapshotDirtyScope,
) {
    if scope == SnapshotDirtyScope::None {
        return;
    }

    let after = snapshot_dirty_baseline(terminal);
    if scope == SnapshotDirtyScope::All
        || before.grid_rows_len != after.grid_rows_len
        || before.total_popped != after.total_popped
        || before.viewport_top != after.viewport_top
        || before.viewport_rows != after.viewport_rows
        || before.viewport_cols != after.viewport_cols
        || before.offset != after.offset
        || before.total_rows != after.total_rows
        || before.status_line_row != after.status_line_row
    {
        terminal.snapshot.mark_all();
        return;
    }

    match before.active_display {
        screen::ActiveDisplay::Main => {
            mark_snapshot_cursor_rows(&mut terminal.snapshot, before, after);
        }
        screen::ActiveDisplay::Status => {
            if let Some(row) = before.status_line_row.or(after.status_line_row) {
                terminal.snapshot.mark_row(row);
            }
        }
    }
    mark_indicator_status_row(terminal, after);

    if after.active_display != before.active_display {
        terminal.snapshot.mark_all();
    }
}
