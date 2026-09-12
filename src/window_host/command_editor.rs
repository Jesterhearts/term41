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
use winit::event::MouseButton;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;

use super::InputState;
use super::TabId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandEditorContext {
    pub(crate) current_dir: Option<PathBuf>,
}

pub(crate) fn command_editor_view_context(terminal: &Terminal) -> Option<CommandEditorContext> {
    if terminal.on_alt_screen
        || !matches!(
            terminal.metadata.shell_integration_phase,
            terminal41::ShellIntegrationPhase::Command | terminal41::ShellIntegrationPhase::Prompt
        )
    {
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

pub(crate) fn command_editor_input_context(terminal: &Terminal) -> Option<CommandEditorContext> {
    command_editor_view_context(terminal)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandEditorPopupSide {
    Above,
    Below,
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
