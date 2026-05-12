use std::collections::HashMap;
use std::path::PathBuf;

use clip41::ClipboardKind;
use commands41::CommandEditor;
use commands41::CommandEditorCursorStyle;
use commands41::CommandLineView;
use commands41::EditorInput;
use commands41::EditorSettings;
use commands41::VimKey;
use terminal41::Terminal;
use terminal41::host;
use terminal41::selection::active_screen_row_at_viewport_row;
use terminal41::selection::search_active;
use unicode_segmentation::UnicodeSegmentation;
use winit::event::MouseButton;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;

use super::InputState;
use super::TabId;
use crate::COMMAND_EDITOR_BOX_ROWS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandEditorContext {
    pub(crate) current_dir: Option<PathBuf>,
}

pub(crate) fn command_editor_view_context(terminal: &Terminal) -> Option<CommandEditorContext> {
    if terminal.on_alt_screen || command_editor_hidden_by_running_command(terminal) {
        return None;
    }
    Some(CommandEditorContext {
        current_dir: terminal.metadata.current_directory.clone(),
    })
}

pub(crate) fn command_editor_view_for_input_tab(
    input_state: &InputState,
    tab_id: TabId,
) -> Option<&CommandLineView> {
    if !input_state.command_editor_config.enabled {
        return None;
    }
    command_editor_view_for_tab_state(&input_state.command_editor_views, tab_id)
}

pub(crate) fn command_editor_view_for_tab_state(
    view_state: &HashMap<TabId, CommandLineView>,
    tab_id: TabId,
) -> Option<&CommandLineView> {
    view_state.get(&tab_id)
}

pub(crate) fn command_editor_view_open_for_input_tab(
    input_state: &InputState,
    tab_id: Option<TabId>,
) -> bool {
    tab_id
        .and_then(|tab_id| command_editor_view_for_input_tab(input_state, tab_id))
        .is_some()
}

fn command_editor_hidden_by_running_command(terminal: &Terminal) -> bool {
    if terminal.metadata.shell_integration_phase == terminal41::ShellIntegrationPhase::Output {
        return true;
    }
    terminal.metadata.shell_integration_phase != terminal41::ShellIntegrationPhase::Command
        && (host::mouse_tracking_enabled(terminal.modes.mouse_tracking)
            || terminal.active.app_cursor_keys
            || terminal.active.app_keypad)
}

pub(crate) fn command_editor_input_context(
    terminal: &Terminal,
    command_editor_open: bool,
) -> Option<CommandEditorContext> {
    let context = command_editor_view_context(terminal)?;
    if command_editor_open
        || terminal.metadata.shell_integration_phase == terminal41::ShellIntegrationPhase::Command
    {
        Some(context)
    } else {
        None
    }
}

pub(crate) fn command_editor_visible_for_terminal(
    terminal: &Terminal,
    command_editor_open: bool,
) -> bool {
    command_editor_open
        && terminal.active.offset == 0
        && !search_active(&terminal.search)
        && command_editor_view_context(terminal).is_some()
}

pub(crate) fn command_editor_terminal_row_offset(
    terminal: &Terminal,
    command_editor_view_present: bool,
) -> u32 {
    if command_editor_visible_for_terminal(terminal, command_editor_view_present) {
        let cursor_row = command_editor_visual_cursor_row(terminal);
        command_editor_terminal_row_offset_for_cursor(cursor_row, terminal.viewport.rows)
    } else {
        0
    }
}

pub(crate) fn command_editor_visual_cursor_row(terminal: &Terminal) -> u32 {
    (0..terminal.viewport.rows.max(1))
        .find(|&viewport_row| {
            active_screen_row_at_viewport_row(
                &terminal.active,
                &terminal.viewport,
                terminal.on_alt_screen,
                viewport_row,
            ) == Some(terminal.active.cursor.row)
        })
        .unwrap_or(terminal.active.cursor.row)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandEditorPopupSide {
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandEditorPlacement {
    pub(crate) top_row: u32,
    pub(crate) rows: u32,
    pub(crate) terminal_row_offset: u32,
}

pub(crate) fn command_editor_placement_for_cursor(
    cursor_row: u32,
    viewport_rows: u32,
) -> CommandEditorPlacement {
    let viewport_rows = viewport_rows.max(1);
    let cursor_row = cursor_row.min(viewport_rows - 1);
    let terminal_row_offset =
        command_editor_terminal_row_offset_for_cursor(cursor_row, viewport_rows);
    let screen_cursor_row = cursor_row.saturating_sub(terminal_row_offset);
    let top_row = screen_cursor_row.saturating_add(1).min(viewport_rows - 1);
    CommandEditorPlacement {
        top_row,
        rows: viewport_rows.saturating_sub(top_row).max(1),
        terminal_row_offset,
    }
}

pub(crate) fn command_editor_popup_side_for_row(
    screen_row: u32,
    viewport_rows: u32,
) -> CommandEditorPopupSide {
    if screen_row < viewport_rows.max(1) / 2 {
        CommandEditorPopupSide::Below
    } else {
        CommandEditorPopupSide::Above
    }
}

fn command_editor_terminal_row_offset_for_cursor(
    cursor_row: u32,
    viewport_rows: u32,
) -> u32 {
    let viewport_rows = viewport_rows.max(1);
    let cursor_row = cursor_row.min(viewport_rows - 1);
    let desired_rows = COMMAND_EDITOR_BOX_ROWS.min(viewport_rows.saturating_sub(1));
    cursor_row
        .saturating_add(1)
        .saturating_add(desired_rows)
        .saturating_sub(viewport_rows)
        .min(desired_rows)
}

pub(crate) fn command_editor_mouse_paste_kind(
    command_editor_open: bool,
    pressed: bool,
    button: MouseButton,
) -> Option<ClipboardKind> {
    if !command_editor_open || !pressed {
        return None;
    }
    match button {
        MouseButton::Right => Some(ClipboardKind::Clipboard),
        MouseButton::Middle => Some(ClipboardKind::Primary),
        _ => None,
    }
}

pub(crate) fn dec_udk_selector(
    key: &Key,
    mods: ModifiersState,
) -> Option<u16> {
    if !mods.shift_key() {
        return None;
    }
    match key {
        Key::Named(named) => dec_function_key_selector(*named),
        _ => None,
    }
}

pub(crate) fn command_editor_input(
    key: &Key,
    mods: ModifiersState,
    vim_mode: bool,
) -> Option<EditorInput> {
    if vim_mode {
        return vim_command_editor_input(key, mods);
    }
    if mods.super_key() {
        return None;
    }
    if let Some(input) = modified_command_editor_input(key, mods) {
        return Some(input);
    }
    if mods.control_key() || mods.alt_key() {
        return None;
    }
    match key {
        Key::Character(text) => Some(EditorInput::Insert(text.to_string())),
        Key::Named(NamedKey::Space) => Some(EditorInput::Insert(" ".to_owned())),
        Key::Named(NamedKey::Enter) if mods.shift_key() => Some(EditorInput::Insert("\n".into())),
        Key::Named(NamedKey::Enter) if !mods.shift_key() => Some(EditorInput::Enter),
        Key::Named(NamedKey::Backspace) if !mods.shift_key() => Some(EditorInput::Backspace),
        Key::Named(NamedKey::Delete) if !mods.shift_key() => Some(EditorInput::Delete),
        Key::Named(NamedKey::ArrowLeft) if !mods.shift_key() => Some(EditorInput::MoveLeft),
        Key::Named(NamedKey::ArrowRight) if !mods.shift_key() => Some(EditorInput::MoveRight),
        Key::Named(NamedKey::Home) if !mods.shift_key() => Some(EditorInput::MoveHome),
        Key::Named(NamedKey::End) if !mods.shift_key() => Some(EditorInput::MoveEnd),
        Key::Named(NamedKey::ArrowUp) if !mods.shift_key() => Some(EditorInput::HistoryPrevious),
        Key::Named(NamedKey::ArrowDown) if !mods.shift_key() => Some(EditorInput::HistoryNext),
        Key::Named(NamedKey::Tab) if !mods.shift_key() => Some(EditorInput::Complete),
        Key::Named(NamedKey::Escape) if !mods.shift_key() => Some(EditorInput::Cancel),
        _ => None,
    }
}

fn vim_command_editor_input(
    key: &Key,
    mods: ModifiersState,
) -> Option<EditorInput> {
    if !mods.shift_key()
        && mods.control_key()
        && !mods.alt_key()
        && !mods.super_key()
        && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("r"))
    {
        return Some(EditorInput::Redo);
    }
    if plain_control_character_key(key, mods, "c") {
        return Some(EditorInput::Cancel);
    }
    if mods.super_key() || mods.control_key() || mods.alt_key() {
        return None;
    }
    let key = match key {
        Key::Character(text) if !mods.shift_key() || text.chars().count() == 1 => {
            VimKey::Text(text.to_string())
        }
        Key::Named(NamedKey::Space) => VimKey::Text(" ".to_owned()),
        Key::Named(NamedKey::Escape) => VimKey::Escape,
        Key::Named(NamedKey::Enter) if mods.shift_key() => VimKey::ShiftEnter,
        Key::Named(NamedKey::Enter) if !mods.shift_key() => VimKey::Enter,
        Key::Named(NamedKey::Backspace) if !mods.shift_key() => VimKey::Backspace,
        Key::Named(NamedKey::Delete) if !mods.shift_key() => VimKey::Delete,
        Key::Named(NamedKey::ArrowLeft) if !mods.shift_key() => VimKey::ArrowLeft,
        Key::Named(NamedKey::ArrowRight) if !mods.shift_key() => VimKey::ArrowRight,
        Key::Named(NamedKey::ArrowUp) if !mods.shift_key() => VimKey::ArrowUp,
        Key::Named(NamedKey::ArrowDown) if !mods.shift_key() => VimKey::ArrowDown,
        Key::Named(NamedKey::Home) if !mods.shift_key() => VimKey::Home,
        Key::Named(NamedKey::End) if !mods.shift_key() => VimKey::End,
        Key::Named(NamedKey::Tab) if !mods.shift_key() => VimKey::Tab,
        _ => return None,
    };
    Some(EditorInput::Vim(key))
}

fn modified_command_editor_input(
    key: &Key,
    mods: ModifiersState,
) -> Option<EditorInput> {
    if mods.shift_key() {
        return None;
    }
    match key {
        Key::Character(text) if mods.control_key() && !mods.alt_key() => {
            control_command_editor_input(text)
        }
        Key::Character(text) if mods.alt_key() && !mods.control_key() => {
            alt_command_editor_input(text)
        }
        Key::Named(NamedKey::ArrowLeft) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::MoveWordLeft)
        }
        Key::Named(NamedKey::ArrowRight) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::MoveWordRight)
        }
        Key::Named(NamedKey::Backspace) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::DeleteWordLeft)
        }
        Key::Named(NamedKey::Delete) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::DeleteWordRight)
        }
        Key::Named(NamedKey::ArrowLeft) if mods.alt_key() && !mods.control_key() => {
            Some(EditorInput::MoveWordLeft)
        }
        Key::Named(NamedKey::ArrowRight) if mods.alt_key() && !mods.control_key() => {
            Some(EditorInput::MoveWordRight)
        }
        Key::Named(NamedKey::Backspace) if mods.alt_key() && !mods.control_key() => {
            Some(EditorInput::DeleteWordLeft)
        }
        _ => None,
    }
}

fn control_command_editor_input(text: &str) -> Option<EditorInput> {
    match text {
        "a" | "A" => Some(EditorInput::MoveHome),
        "c" | "C" => Some(EditorInput::Cancel),
        "d" | "D" => Some(EditorInput::Delete),
        "e" | "E" => Some(EditorInput::MoveEnd),
        "k" | "K" => Some(EditorInput::KillToEnd),
        "u" | "U" => Some(EditorInput::KillToStart),
        "w" | "W" => Some(EditorInput::DeleteWordLeft),
        "y" | "Y" => Some(EditorInput::Yank),
        "r" | "R" => Some(EditorInput::Redo),
        _ => None,
    }
}

pub(crate) fn ignored_command_editor_input_falls_through(
    input: &EditorInput,
    key: &Key,
    mods: ModifiersState,
    editor_was_empty: bool,
) -> bool {
    *input == EditorInput::Cancel
        || (editor_was_empty
            && *input == EditorInput::Delete
            && plain_control_character_key(key, mods, "d"))
}

pub(crate) fn plain_control_character_key(
    key: &Key,
    mods: ModifiersState,
    text: &str,
) -> bool {
    !mods.shift_key()
        && mods.control_key()
        && !mods.alt_key()
        && !mods.super_key()
        && matches!(key, Key::Character(actual) if actual.eq_ignore_ascii_case(text))
}

fn alt_command_editor_input(text: &str) -> Option<EditorInput> {
    match text {
        "b" | "B" => Some(EditorInput::MoveWordLeft),
        "f" | "F" => Some(EditorInput::MoveWordRight),
        "d" | "D" => Some(EditorInput::DeleteWordRight),
        _ => None,
    }
}

pub(crate) fn command_editor_view(
    editor: &CommandEditor,
    settings: &EditorSettings,
    vim_mode: bool,
) -> Option<CommandLineView> {
    let mut view = editor.view(settings);
    if !vim_mode {
        view.cursor_style = CommandEditorCursorStyle::Beam;
    }
    Some(view)
}

fn command_editor_line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            ranges.push((start, idx));
            start = idx + ch.len_utf8();
        }
    }
    ranges.push((start, text.len()));
    ranges
}

fn command_editor_cursor_line(
    lines: &[(usize, usize)],
    cursor: usize,
) -> usize {
    for (idx, &(_, end)) in lines.iter().enumerate() {
        if cursor <= end {
            return idx;
        }
    }
    lines.len().saturating_sub(1)
}

fn command_editor_visible_line_start(
    line_count: usize,
    cursor_line: usize,
    visible_rows: usize,
) -> usize {
    let visible = visible_rows.max(1);
    if line_count <= visible {
        return 0;
    }
    cursor_line.saturating_add(1).saturating_sub(visible)
}

pub(crate) fn command_editor_byte_index_at_cell(
    view: &CommandLineView,
    viewport_cols: u32,
    visible_rows: u32,
    visible_row: u32,
    col: u32,
) -> usize {
    let lines = command_editor_line_ranges(&view.text);
    let cursor = view.cursor.min(view.text.len());
    if !view.text.is_char_boundary(cursor) {
        return view.text.len();
    }
    let cursor_line = command_editor_cursor_line(&lines, cursor);
    let visible_rows = visible_rows.max(1) as usize;
    let visible_start = command_editor_visible_line_start(lines.len(), cursor_line, visible_rows);
    let line_idx = (visible_start
        + visible_row.min(visible_rows.saturating_sub(1) as u32) as usize)
        .min(lines.len().saturating_sub(1));
    let has_overflow = lines.len() > visible_rows;
    let scrollbar_cols = u32::from(has_overflow);
    let content_cols = viewport_cols.saturating_sub(1 + scrollbar_cols).max(1);
    let text_col = col.min(content_cols);
    let (line_start, line_end) = lines[line_idx];
    view.text[line_start..line_end]
        .grapheme_indices(true)
        .nth(text_col as usize)
        .map_or(line_end, |(idx, _)| line_start + idx)
}

pub(crate) fn dec_local_function_key_selector(
    key: &Key,
    mods: ModifiersState,
) -> Option<u16> {
    if mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key() {
        return None;
    }
    match key {
        Key::Named(NamedKey::F1) => Some(1),
        Key::Named(NamedKey::F2) => Some(2),
        Key::Named(NamedKey::F3) => Some(3),
        Key::Named(NamedKey::F4) => Some(4),
        _ => None,
    }
}

fn dec_function_key_selector(named: NamedKey) -> Option<u16> {
    match named {
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        NamedKey::F13 => Some(25),
        NamedKey::F14 => Some(26),
        NamedKey::F15 => Some(28),
        NamedKey::F16 => Some(29),
        NamedKey::F17 => Some(31),
        NamedKey::F18 => Some(32),
        NamedKey::F19 => Some(33),
        NamedKey::F20 => Some(34),
        _ => None,
    }
}
