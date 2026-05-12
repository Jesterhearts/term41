//! Command editor state and shell-oriented text transforms.
//!
//! The crate deliberately knows nothing about terminals, windows, or PTYs.
//! Callers translate their input events into [`EditorInput`], apply them to a
//! [`CommandEditor`], and render the returned [`CommandLineView`] however their
//! UI needs.

#[cfg(test)]
use std::fs;
use std::path::PathBuf;

mod completion;
mod editing;
mod history;
mod shell_words;
mod syntax;
mod undo;
mod vim;

pub use editing::apply_input;
pub use editing::clear_selection;
pub use editing::select_range;
pub use editing::selected_text;
pub use editing::set_cursor;
pub use history::HistoryEntry;
pub use history::HistorySource;
pub use syntax::HighlightKind;
pub use syntax::HighlightSpan;
pub use syntax::highlight_shell;
pub use vim::VimKey;
pub use vim::VimMode;

const DEFAULT_MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSettings {
    pub completion_words: Vec<String>,
    pub command_words: Vec<String>,
    pub command_completions: Vec<CommandCompletion>,
    pub history_entries: Vec<HistoryEntry>,
    pub current_dir: Option<PathBuf>,
    pub max_history: usize,
    pub escape_character: char,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            completion_words: Vec::new(),
            command_words: Vec::new(),
            command_completions: Vec::new(),
            history_entries: Vec::new(),
            current_dir: None,
            max_history: DEFAULT_MAX_HISTORY,
            escape_character: '\\',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCompletion {
    pub command: String,
    pub subcommands: Vec<SubcommandCompletion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubcommandCompletion {
    pub name: String,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorSelection {
    pub anchor: usize,
    pub head: usize,
}

impl EditorSelection {
    pub fn ordered(self) -> (usize, usize) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLineView {
    pub text: String,
    pub cursor: usize,
    pub cursor_style: CommandEditorCursorStyle,
    pub spans: Vec<HighlightSpan>,
    pub selection: Option<EditorSelection>,
    pub completion: Option<String>,
    pub candidates: Vec<String>,
    pub candidate_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEditorCursorStyle {
    Beam,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorInput {
    Insert(String),
    Vim(VimKey),
    Enter,
    Backspace,
    Delete,
    MoveLeft,
    MoveRight,
    MoveWordLeft,
    MoveWordRight,
    MoveHome,
    MoveEnd,
    DeleteWordLeft,
    DeleteWordRight,
    KillToStart,
    KillToEnd,
    Yank,
    Undo,
    Redo,
    HistoryPrevious,
    HistoryNext,
    Complete,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Ignored,
    Updated,
    Submitted(String),
    Canceled,
}

#[derive(Debug, Clone)]
pub struct CommandEditor {
    buffer: String,
    cursor: usize,
    history: history::EditorHistory,
    kill_buffer: String,
    undo: undo::UndoHistory,
    selection: Option<EditorSelection>,
    vim_mode: VimMode,
    vim_pending: Option<vim::VimPending>,
    path_cycle: Option<completion::path::PathCompletionCycle>,
    completion_selection: Option<completion::CompletionSelection>,
}

impl Default for CommandEditor {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: history::EditorHistory::default(),
            kill_buffer: String::new(),
            undo: undo::UndoHistory::default(),
            selection: None,
            vim_mode: VimMode::Normal,
            vim_pending: None,
            path_cycle: None,
            completion_selection: None,
        }
    }
}

impl CommandEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn view(
        &self,
        settings: &EditorSettings,
    ) -> CommandLineView {
        let completion = completion::completion_preview(self, settings);
        let (candidates, candidate_index) = completion::completion_candidate_view(self, settings);
        CommandLineView {
            text: self.buffer.clone(),
            cursor: self.cursor,
            cursor_style: match self.vim_mode {
                VimMode::Normal => CommandEditorCursorStyle::Block,
                VimMode::Insert => CommandEditorCursorStyle::Beam,
            },
            spans: highlight_shell(&self.buffer),
            selection: self.selection.filter(|selection| !selection.is_empty()),
            completion,
            candidates,
            candidate_index,
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        history::clear(&mut self.history);
        self.undo = undo::UndoHistory::default();
        self.selection = None;
        self.vim_mode = VimMode::Normal;
        self.vim_pending = None;
        self.path_cycle = None;
        self.completion_selection = None;
    }
}

#[cfg(test)]
mod tests;
