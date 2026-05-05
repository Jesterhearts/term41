//! Command editor state and shell-oriented text transforms.
//!
//! The crate deliberately knows nothing about terminals, windows, or PTYs.
//! Callers translate their input events into [`EditorInput`], apply them to a
//! [`CommandEditor`], and render the returned [`CommandLineView`] however their
//! UI needs.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use nucleo_matcher::Config as NucleoConfig;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;
use unicode_segmentation::UnicodeSegmentation;

mod history;
mod syntax;
mod vim;
pub use history::HistoryEntry;
pub use history::HistorySource;
pub use syntax::HighlightKind;
pub use syntax::HighlightSpan;
pub use syntax::highlight_shell;
use syntax::is_command_separator_char;
use syntax::is_operator_char;
pub use vim::VimKey;
pub use vim::VimMode;

const DEFAULT_MAX_HISTORY: usize = 200;
const MAX_COMPLETION_CANDIDATES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSettings {
    pub completion_words: Vec<String>,
    pub command_words: Vec<String>,
    pub command_completions: Vec<CommandCompletion>,
    pub history_entries: Vec<HistoryEntry>,
    pub current_dir: Option<PathBuf>,
    pub max_history: usize,
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

const MAX_UNDO_STEPS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    buffer: String,
    cursor: usize,
    selection: Option<EditorSelection>,
}

#[derive(Debug, Clone, Default)]
struct UndoHistory {
    undo: Vec<EditorSnapshot>,
    redo: Vec<EditorSnapshot>,
}

#[derive(Debug, Clone)]
pub struct CommandEditor {
    buffer: String,
    cursor: usize,
    history: history::EditorHistory,
    kill_buffer: String,
    undo: UndoHistory,
    selection: Option<EditorSelection>,
    vim_mode: VimMode,
    vim_pending: Option<vim::VimPending>,
    path_cycle: Option<PathCompletionCycle>,
    completion_selection: Option<CompletionSelection>,
}

impl Default for CommandEditor {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: history::EditorHistory::default(),
            kill_buffer: String::new(),
            undo: UndoHistory::default(),
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
        let selection = self.valid_completion_selection();
        let completion = if let Some(selection) = selection {
            selection.current_suffix()
        } else {
            self.path_cycle_suffix()
                .or_else(|| completion_suffix(self, settings))
        };
        let (candidates, candidate_index) = completion_candidate_view(self, settings);
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
        self.undo = UndoHistory::default();
        self.selection = None;
        self.vim_mode = VimMode::Normal;
        self.vim_pending = None;
        self.path_cycle = None;
        self.completion_selection = None;
    }

    fn path_cycle_suffix(&self) -> Option<String> {
        let cycle = self.path_cycle.as_ref()?;
        if cycle.cursor != self.cursor || cycle.base != self.buffer {
            return None;
        }
        cycle.current_suffix()
    }

    fn valid_completion_selection(&self) -> Option<&CompletionSelection> {
        let selection = self.completion_selection.as_ref()?;
        if selection.cursor == self.cursor && selection.base == self.buffer {
            Some(selection)
        } else {
            None
        }
    }
}

pub fn set_cursor(
    editor: &mut CommandEditor,
    cursor: usize,
) -> EditOutcome {
    let Some(cursor) = valid_boundary(&editor.buffer, cursor) else {
        return EditOutcome::Ignored;
    };
    clear_completion_state(editor);
    editor.cursor = cursor;
    editor.selection = None;
    EditOutcome::Updated
}

pub fn select_range(
    editor: &mut CommandEditor,
    anchor: usize,
    head: usize,
) -> EditOutcome {
    let (Some(anchor), Some(head)) = (
        valid_boundary(&editor.buffer, anchor),
        valid_boundary(&editor.buffer, head),
    ) else {
        return EditOutcome::Ignored;
    };
    clear_completion_state(editor);
    editor.cursor = head;
    editor.selection = Some(EditorSelection { anchor, head });
    EditOutcome::Updated
}

pub fn selected_text(editor: &CommandEditor) -> Option<String> {
    let selection = editor.selection?;
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.ordered();
    Some(editor.buffer[start..end].to_owned())
}

pub fn clear_selection(editor: &mut CommandEditor) -> EditOutcome {
    if editor.selection.take().is_some() {
        EditOutcome::Updated
    } else {
        EditOutcome::Ignored
    }
}

pub fn apply_input(
    editor: &mut CommandEditor,
    input: EditorInput,
    settings: &EditorSettings,
) -> EditOutcome {
    match input {
        EditorInput::Insert(text) => {
            if text.is_empty() {
                return EditOutcome::Ignored;
            }
            begin_text_edit(editor);
            replace_selection_or_insert(editor, &text);
            EditOutcome::Updated
        }
        EditorInput::Vim(key) => vim::apply_vim_key(editor, key, settings),
        EditorInput::Enter => {
            let command = submitted_command(&editor.buffer);
            history::push(&mut editor.history, &command, settings.max_history);
            editor.clear();
            EditOutcome::Submitted(command)
        }
        EditorInput::Backspace => {
            if editor
                .selection
                .is_some_and(|selection| !selection.is_empty())
            {
                begin_text_edit(editor);
                delete_selection_without_undo(editor);
                return EditOutcome::Updated;
            }
            let Some(prev) = previous_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            begin_text_edit(editor);
            editor.buffer.drain(prev..editor.cursor);
            editor.cursor = prev;
            EditOutcome::Updated
        }
        EditorInput::Delete => {
            if editor
                .selection
                .is_some_and(|selection| !selection.is_empty())
            {
                begin_text_edit(editor);
                delete_selection_without_undo(editor);
                return EditOutcome::Updated;
            }
            let Some(next) = next_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            begin_text_edit(editor);
            editor.buffer.drain(editor.cursor..next);
            EditOutcome::Updated
        }
        EditorInput::MoveLeft => {
            let Some(prev) = previous_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = prev;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveRight => {
            if accept_selected_completion(editor)
                || accept_path_cycle(editor)
                || accept_visible_history_completion(editor, settings)
                || accept_visible_path_completion(editor, settings)
            {
                return EditOutcome::Updated;
            }
            let Some(next) = next_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = next;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveWordLeft => {
            let Some(prev) = previous_word_start(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = prev;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveWordRight => {
            let Some(next) = next_word_end(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = next;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveHome => {
            if editor.cursor == 0 {
                EditOutcome::Ignored
            } else {
                clear_completion_state(editor);
                editor.cursor = 0;
                editor.selection = None;
                EditOutcome::Updated
            }
        }
        EditorInput::MoveEnd => {
            if editor.cursor == editor.buffer.len() {
                EditOutcome::Ignored
            } else {
                clear_completion_state(editor);
                editor.cursor = editor.buffer.len();
                editor.selection = None;
                EditOutcome::Updated
            }
        }
        EditorInput::DeleteWordLeft => {
            if editor
                .selection
                .is_some_and(|selection| !selection.is_empty())
            {
                begin_text_edit(editor);
                delete_selection_without_undo(editor);
                return EditOutcome::Updated;
            }
            let Some(prev) = previous_word_start(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            begin_text_edit(editor);
            editor.kill_buffer = editor.buffer[prev..editor.cursor].to_owned();
            editor.buffer.drain(prev..editor.cursor);
            editor.cursor = prev;
            EditOutcome::Updated
        }
        EditorInput::DeleteWordRight => {
            if editor
                .selection
                .is_some_and(|selection| !selection.is_empty())
            {
                begin_text_edit(editor);
                delete_selection_without_undo(editor);
                return EditOutcome::Updated;
            }
            let Some(next) = next_word_end(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            begin_text_edit(editor);
            editor.kill_buffer = editor.buffer[editor.cursor..next].to_owned();
            editor.buffer.drain(editor.cursor..next);
            EditOutcome::Updated
        }
        EditorInput::KillToStart => {
            if editor.cursor == 0 {
                return EditOutcome::Ignored;
            }
            begin_text_edit(editor);
            editor.kill_buffer = editor.buffer[..editor.cursor].to_owned();
            editor.buffer.drain(..editor.cursor);
            editor.cursor = 0;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::KillToEnd => {
            if editor.cursor == editor.buffer.len() {
                return EditOutcome::Ignored;
            }
            begin_text_edit(editor);
            editor.kill_buffer = editor.buffer[editor.cursor..].to_owned();
            editor.buffer.truncate(editor.cursor);
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::Yank => {
            if editor.kill_buffer.is_empty() {
                return EditOutcome::Ignored;
            }
            begin_text_edit(editor);
            let text = editor.kill_buffer.clone();
            replace_selection_or_insert(editor, &text);
            EditOutcome::Updated
        }
        EditorInput::Undo => undo_text_edit(editor),
        EditorInput::Redo => redo_text_edit(editor),
        EditorInput::HistoryPrevious => {
            if history::is_navigating(&editor.history) {
                return history_previous(editor, settings);
            }
            cycle_completion_selection(editor, settings, CompletionDirection::Previous)
                .or_else(|| move_cursor_line(editor, LineDirection::Previous))
                .unwrap_or_else(|| history_previous(editor, settings))
        }
        EditorInput::HistoryNext => {
            if history::is_navigating(&editor.history) {
                return history_next(editor, settings);
            }
            cycle_completion_selection(editor, settings, CompletionDirection::Next)
                .or_else(|| move_cursor_line(editor, LineDirection::Next))
                .unwrap_or_else(|| history_next(editor, settings))
        }
        EditorInput::Complete => complete_current_prefix(editor, settings),
        EditorInput::Cancel => {
            if editor.buffer.is_empty() {
                EditOutcome::Ignored
            } else {
                editor.clear();
                EditOutcome::Canceled
            }
        }
    }
}

fn previous_grapheme_boundary(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    if cursor == 0 {
        return None;
    }
    text.grapheme_indices(true)
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx < cursor)
        .last()
}

fn next_grapheme_boundary(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    if cursor >= text.len() {
        return None;
    }
    text.grapheme_indices(true)
        .map(|(idx, g)| idx + g.len())
        .find(|idx| *idx > cursor)
}

fn previous_word_start(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    if cursor == 0 || cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }

    let mut boundary = cursor;
    while let Some(prev) = previous_grapheme_boundary(text, boundary) {
        if !is_word_separator(&text[prev..boundary]) {
            break;
        }
        boundary = prev;
    }
    while let Some(prev) = previous_grapheme_boundary(text, boundary) {
        if is_word_separator(&text[prev..boundary]) {
            break;
        }
        boundary = prev;
    }

    (boundary != cursor).then_some(boundary)
}

fn next_word_end(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    if cursor >= text.len() || !text.is_char_boundary(cursor) {
        return None;
    }

    let mut boundary = cursor;
    while let Some(next) = next_grapheme_boundary(text, boundary) {
        if !is_word_separator(&text[boundary..next]) {
            break;
        }
        boundary = next;
    }
    while let Some(next) = next_grapheme_boundary(text, boundary) {
        if is_word_separator(&text[boundary..next]) {
            break;
        }
        boundary = next;
    }

    (boundary != cursor).then_some(boundary)
}

fn is_word_separator(grapheme: &str) -> bool {
    grapheme
        .chars()
        .next()
        .is_some_and(|ch| ch.is_whitespace() || is_operator_char(ch))
}

fn replace_history_edit_with_draft(editor: &mut CommandEditor) {
    history::replace_edit_with_draft(&mut editor.history, &editor.buffer);
}

fn begin_text_edit(editor: &mut CommandEditor) {
    push_undo_snapshot(editor);
    clear_completion_state(editor);
    replace_history_edit_with_draft(editor);
}

fn push_undo_snapshot(editor: &mut CommandEditor) {
    let snapshot = editor_snapshot(editor);
    if editor.undo.undo.last() == Some(&snapshot) {
        editor.undo.redo.clear();
        return;
    }
    editor.undo.undo.push(snapshot);
    trim_snapshot_stack(&mut editor.undo.undo);
    editor.undo.redo.clear();
}

fn editor_snapshot(editor: &CommandEditor) -> EditorSnapshot {
    EditorSnapshot {
        buffer: editor.buffer.clone(),
        cursor: editor.cursor,
        selection: editor.selection,
    }
}

fn trim_snapshot_stack(stack: &mut Vec<EditorSnapshot>) {
    let overflow = stack.len().saturating_sub(MAX_UNDO_STEPS);
    if overflow > 0 {
        stack.drain(..overflow);
    }
}

fn undo_text_edit(editor: &mut CommandEditor) -> EditOutcome {
    let Some(snapshot) = editor.undo.undo.pop() else {
        return EditOutcome::Ignored;
    };
    editor.undo.redo.push(editor_snapshot(editor));
    trim_snapshot_stack(&mut editor.undo.redo);
    restore_editor_snapshot(editor, snapshot);
    EditOutcome::Updated
}

fn redo_text_edit(editor: &mut CommandEditor) -> EditOutcome {
    let Some(snapshot) = editor.undo.redo.pop() else {
        return EditOutcome::Ignored;
    };
    editor.undo.undo.push(editor_snapshot(editor));
    trim_snapshot_stack(&mut editor.undo.undo);
    restore_editor_snapshot(editor, snapshot);
    EditOutcome::Updated
}

fn restore_editor_snapshot(
    editor: &mut CommandEditor,
    snapshot: EditorSnapshot,
) {
    clear_completion_state(editor);
    editor.buffer = snapshot.buffer;
    editor.cursor = snapshot.cursor;
    editor.selection = snapshot.selection;
    editor.vim_pending = None;
    replace_history_edit_with_draft(editor);
}

fn valid_boundary(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    (cursor <= text.len() && text.is_char_boundary(cursor)).then_some(cursor)
}

fn replace_selection_or_insert(
    editor: &mut CommandEditor,
    text: &str,
) {
    if let Some((start, _)) = delete_selection_without_undo(editor) {
        editor.buffer.insert_str(start, text);
        editor.cursor = start + text.len();
    } else {
        editor.buffer.insert_str(editor.cursor, text);
        editor.cursor += text.len();
    }
}

fn delete_selection_without_undo(editor: &mut CommandEditor) -> Option<(usize, usize)> {
    let selection = editor.selection.take()?;
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.ordered();
    editor.buffer.drain(start..end);
    editor.cursor = start;
    Some((start, end))
}

fn clear_completion_state(editor: &mut CommandEditor) {
    editor.path_cycle = None;
    editor.completion_selection = None;
}

fn history_previous(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> EditOutcome {
    let Some(command) = history::previous(
        &mut editor.history,
        &editor.buffer,
        &settings.history_entries,
    ) else {
        return EditOutcome::Ignored;
    };
    clear_completion_state(editor);
    editor.undo = UndoHistory::default();
    editor.buffer = command;
    editor.cursor = editor.buffer.len();
    editor.selection = None;
    EditOutcome::Updated
}

fn history_next(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> EditOutcome {
    let Some(command) = history::next(&mut editor.history, &settings.history_entries) else {
        return EditOutcome::Ignored;
    };
    clear_completion_state(editor);
    editor.undo = UndoHistory::default();
    editor.buffer = command;
    editor.cursor = editor.buffer.len();
    editor.selection = None;
    EditOutcome::Updated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineDirection {
    Previous,
    Next,
}

fn move_cursor_line(
    editor: &mut CommandEditor,
    direction: LineDirection,
) -> Option<EditOutcome> {
    let lines = line_ranges(&editor.buffer);
    if lines.len() <= 1 {
        return None;
    }
    let current = line_index_at_cursor(&lines, editor.cursor);
    let target = match direction {
        LineDirection::Previous => current.checked_sub(1)?,
        LineDirection::Next => {
            let next = current + 1;
            (next < lines.len()).then_some(next)?
        }
    };
    let cursor_col = grapheme_count(&editor.buffer[lines[current].0..editor.cursor]);
    let (target_start, target_end) = lines[target];
    let target_cursor =
        byte_index_at_grapheme_col(&editor.buffer, target_start, target_end, cursor_col);
    clear_completion_state(editor);
    editor.cursor = target_cursor;
    editor.selection = None;
    Some(EditOutcome::Updated)
}

fn line_ranges(text: &str) -> Vec<(usize, usize)> {
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

fn line_index_at_cursor(
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

fn byte_index_at_grapheme_col(
    text: &str,
    start: usize,
    end: usize,
    col: usize,
) -> usize {
    text[start..end]
        .grapheme_indices(true)
        .nth(col)
        .map_or(end, |(idx, _)| start + idx)
}

fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

fn complete_current_prefix(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> EditOutcome {
    if accept_selected_completion_step(editor) {
        return EditOutcome::Updated;
    }

    if cycle_path_completion(editor, settings) {
        replace_history_edit_with_draft(editor);
        return EditOutcome::Updated;
    }

    if let Some(suffix) = history_completion_step_suffix(editor, settings) {
        begin_text_edit(editor);
        replace_selection_or_insert(editor, &suffix);
        return EditOutcome::Updated;
    }

    let Some(suffix) = completion_suffix(editor, settings) else {
        return EditOutcome::Ignored;
    };
    begin_text_edit(editor);
    replace_selection_or_insert(editor, &suffix);
    EditOutcome::Updated
}

fn submitted_command(buffer: &str) -> String {
    buffer
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_history(
    editor: &mut CommandEditor,
    command: &str,
    max_history: usize,
) {
    history::push(&mut editor.history, command, max_history);
}

fn completion_suffix(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> Option<String> {
    let buffer = &editor.buffer;
    let cursor = editor.cursor;
    if let Some(suffix) = history_completion_suffix(editor, settings) {
        return Some(suffix);
    }

    if let Some(suffix) = path_completion_suffix(buffer, cursor, settings) {
        return Some(suffix);
    }

    let (word_start, prefix) = current_completion_word(buffer, cursor)?;
    if prefix.is_empty() {
        return None;
    }
    let candidates = if is_command_completion_word(buffer, word_start) {
        command_completion_candidates(&editor.history, settings)
    } else if let Some(candidates) = structured_completion_candidates(settings, buffer, word_start)
    {
        candidates
    } else {
        word_completion_candidates(&editor.history, settings)
    };
    for candidate in candidates {
        if candidate != prefix && candidate.starts_with(prefix) {
            return Some(candidate[prefix.len()..].to_owned());
        }
    }
    None
}

fn history_completion_suffix(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> Option<String> {
    let buffer = &editor.buffer;
    let cursor = editor.cursor;
    if cursor != buffer.len() || buffer.is_empty() {
        return None;
    }
    history::command_candidates(&editor.history, &settings.history_entries)
        .into_iter()
        .find(|candidate| candidate != buffer && candidate.starts_with(buffer))
        .map(|candidate| candidate[buffer.len()..].to_owned())
}

fn history_completion_step_suffix(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> Option<String> {
    let suffix = history_completion_suffix(editor, settings)?;
    let end = next_completion_step_end(&suffix)?;
    Some(suffix[..end].to_owned())
}

fn next_completion_step_end(text: &str) -> Option<usize> {
    let mut saw_segment_text = false;
    let mut end = 0;
    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if saw_segment_text {
                break;
            }
        } else if is_path_separator(ch) {
            end = idx + ch.len_utf8();
            if saw_segment_text {
                return Some(end);
            }
            continue;
        } else {
            saw_segment_text = true;
        }
        end = idx + ch.len_utf8();
    }
    saw_segment_text.then_some(end)
}

fn is_path_separator(ch: char) -> bool {
    matches!(ch, '/' | '\\')
}

fn completion_candidate_view(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> (Vec<String>, usize) {
    if let Some(selection) = editor.valid_completion_selection() {
        let candidates = selection
            .candidates
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect::<Vec<_>>();
        let index = selection.index.min(candidates.len().saturating_sub(1));
        return (candidates, index);
    }

    if let Some(cycle) = editor.path_cycle.as_ref()
        && cycle.cursor == editor.cursor
        && cycle.base == editor.buffer
    {
        let candidates = top_ambiguous_candidates(
            cycle
                .candidates
                .iter()
                .map(|candidate| candidate.completed_word.clone())
                .collect(),
        );
        let index = cycle.index.min(candidates.len().saturating_sub(1));
        return (candidates, index);
    }

    let matches = completion_matches(editor, settings);
    let candidates = matches
        .map(|matches| {
            visible_completion_candidates(matches)
                .into_iter()
                .map(|candidate| candidate.text)
                .collect()
        })
        .unwrap_or_default();
    (candidates, 0)
}

fn completion_matches(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> Option<CompletionMatches> {
    let buffer = &editor.buffer;
    let cursor = editor.cursor;
    if cursor == buffer.len() && !buffer.is_empty() {
        let candidates = command_history_completion_candidates(editor, settings, buffer);
        if !candidates.is_empty() {
            return Some(CompletionMatches {
                replacement_start: 0,
                query: buffer.to_owned(),
                candidates,
            });
        }
    }

    if let Some(matches) = path_completion_matches(buffer, cursor, settings)
        && !matches.candidates.is_empty()
    {
        return Some(matches);
    }

    let (word_start, prefix) = current_completion_word(buffer, cursor)?;
    if prefix.is_empty() {
        return None;
    }
    let candidates = if is_command_completion_word(buffer, word_start) {
        command_word_completion_matches(&editor.history, settings, prefix)
    } else if let Some(candidates) = structured_completion_candidates(settings, buffer, word_start)
    {
        prefix_completion_candidates(prefix, candidates)
    } else {
        word_completion_matches(&editor.history, settings, prefix)
    };
    Some(CompletionMatches {
        replacement_start: word_start,
        query: prefix.to_owned(),
        candidates,
    })
}

fn command_history_completion_candidates(
    editor: &CommandEditor,
    settings: &EditorSettings,
    query: &str,
) -> Vec<CompletionCandidate> {
    prefix_then_fuzzy_completion_candidates(
        query,
        history::command_candidates(&editor.history, &settings.history_entries),
    )
}

fn command_word_completion_matches(
    history: &history::EditorHistory,
    settings: &EditorSettings,
    query: &str,
) -> Vec<CompletionCandidate> {
    prefix_then_fuzzy_completion_candidates(query, command_completion_candidates(history, settings))
}

fn word_completion_matches(
    history: &history::EditorHistory,
    settings: &EditorSettings,
    query: &str,
) -> Vec<CompletionCandidate> {
    prefix_completion_candidates(query, word_completion_candidates(history, settings))
}

fn prefix_completion_candidates(
    query: &str,
    candidates: Vec<String>,
) -> Vec<CompletionCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| candidate != query && candidate.starts_with(query))
        .map(CompletionCandidate::prefix)
        .collect()
}

fn prefix_then_fuzzy_completion_candidates(
    query: &str,
    candidates: Vec<String>,
) -> Vec<CompletionCandidate> {
    let mut out = candidates
        .iter()
        .filter(|candidate| candidate.as_str() != query && candidate.starts_with(query))
        .map(|candidate| CompletionCandidate::prefix(candidate.clone()))
        .collect::<Vec<_>>();
    for candidate in fuzzy_completion_candidates(query, candidates) {
        push_unique_completion_candidate(&mut out, candidate);
    }
    out
}

fn fuzzy_completion_candidates(
    query: &str,
    candidates: Vec<String>,
) -> Vec<CompletionCandidate> {
    if query.is_empty() {
        return Vec::new();
    }

    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = fuzzy_completion_matcher();
    let mut utf32_buf = Vec::new();
    let mut matches = candidates
        .into_iter()
        .filter(|candidate| candidate != query && !candidate.starts_with(query))
        .filter_map(|candidate| {
            pattern
                .score(
                    Utf32Str::new(candidate.as_str(), &mut utf32_buf),
                    &mut matcher,
                )
                .map(|score| ScoredCompletionCandidate { candidate, score })
        })
        .collect::<Vec<_>>();
    matches.sort_by(fuzzy_completion_match_order);
    matches
        .into_iter()
        .map(|matched| CompletionCandidate::fuzzy(matched.candidate))
        .collect()
}

fn fuzzy_completion_matcher() -> Matcher {
    let mut config = NucleoConfig::DEFAULT;
    config.prefer_prefix = true;
    Matcher::new(config)
}

fn fuzzy_completion_match_order(
    left: &ScoredCompletionCandidate,
    right: &ScoredCompletionCandidate,
) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.candidate.cmp(&right.candidate))
}

fn visible_completion_candidates(matches: CompletionMatches) -> Vec<CompletionCandidate> {
    let candidates = dedupe_completion_candidates(matches.candidates);
    if candidates.len() <= 1 && !candidates.iter().any(CompletionCandidate::is_fuzzy) {
        return Vec::new();
    }
    candidates
        .into_iter()
        .take(MAX_COMPLETION_CANDIDATES)
        .collect()
}

fn top_ambiguous_candidates(candidates: Vec<String>) -> Vec<String> {
    let candidates = dedupe_candidates(candidates);
    if candidates.len() <= 1 {
        return Vec::new();
    }
    candidates
        .into_iter()
        .take(MAX_COMPLETION_CANDIDATES)
        .collect()
}

fn dedupe_candidates(candidates: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for candidate in candidates {
        push_unique(&mut out, &candidate);
    }
    out
}

fn dedupe_completion_candidates(candidates: Vec<CompletionCandidate>) -> Vec<CompletionCandidate> {
    let mut out = Vec::new();
    for candidate in candidates {
        push_unique_completion_candidate(&mut out, candidate);
    }
    out
}

fn push_unique_completion_candidate(
    out: &mut Vec<CompletionCandidate>,
    candidate: CompletionCandidate,
) {
    if !candidate.text.is_empty() && !out.iter().any(|existing| existing.text == candidate.text) {
        out.push(candidate);
    }
}

fn path_completion_suffix(
    buffer: &str,
    cursor: usize,
    settings: &EditorSettings,
) -> Option<String> {
    let (word, candidates) = path_completion_word_and_candidates(buffer, cursor, settings)?;
    let candidate = candidates.into_iter().find(|candidate| {
        candidate.completed_word != word && candidate.completed_word.starts_with(&word)
    })?;
    Some(path_completion_candidate_suffix(&word, &candidate).to_owned())
}

fn path_completion_matches(
    buffer: &str,
    cursor: usize,
    settings: &EditorSettings,
) -> Option<CompletionMatches> {
    let (word, candidates) = path_completion_word_and_candidates(buffer, cursor, settings)?;
    let candidates = candidates
        .into_iter()
        .filter(|candidate| {
            candidate.completed_word != word && candidate.completed_word.starts_with(&word)
        })
        .map(|candidate| candidate.completed_word)
        .collect::<Vec<_>>();
    Some(CompletionMatches {
        replacement_start: cursor.saturating_sub(word.len()),
        query: word,
        candidates: candidates
            .into_iter()
            .map(CompletionCandidate::prefix)
            .collect(),
    })
}

fn path_completion_word_and_candidates(
    buffer: &str,
    cursor: usize,
    settings: &EditorSettings,
) -> Option<(String, Vec<PathCompletionCandidate>)> {
    let current_dir = settings.current_dir.as_deref()?;
    let word = current_path_completion_word(buffer, cursor)?;
    if word.decoded.is_empty()
        || !path_completion_allowed(buffer, word.start, &word.decoded, word.quote)
    {
        return None;
    }

    let request = path_completion_request(current_dir, &word)?;
    let mut candidates = path_completion_candidates(&request)?;
    candidates.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.completed_word.cmp(&b.completed_word))
    });
    Some((word.raw, candidates))
}

fn current_completion_word(
    buffer: &str,
    cursor: usize,
) -> Option<(usize, &str)> {
    if cursor > buffer.len() || !buffer.is_char_boundary(cursor) {
        return None;
    }
    let scan = scan_completion_word(buffer, cursor);
    if !cursor_at_completion_word_end(buffer, cursor, scan) {
        return None;
    }
    let start = scan.start;
    Some((start, &buffer[start..cursor]))
}

#[derive(Debug, Clone, Copy)]
struct CompletionWordScan {
    start: usize,
    quote: Option<char>,
    escaping_next: bool,
}

fn scan_completion_word(
    buffer: &str,
    cursor: usize,
) -> CompletionWordScan {
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;

    for (idx, ch) in buffer[..cursor].char_indices() {
        let next = idx + ch.len_utf8();
        if escaped {
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if ch.is_whitespace() || is_operator_char(ch) {
                    start = next;
                }
            }
        }
    }

    CompletionWordScan {
        start,
        quote,
        escaping_next: escaped,
    }
}

fn cursor_at_completion_word_end(
    buffer: &str,
    cursor: usize,
    scan: CompletionWordScan,
) -> bool {
    let Some(ch) = buffer[cursor..].chars().next() else {
        return true;
    };
    if scan.escaping_next {
        return false;
    }
    match scan.quote {
        Some('\'') => ch == '\'',
        Some('"') => ch == '"',
        Some(_) => false,
        None => ch.is_whitespace() || is_operator_char(ch),
    }
}

fn current_path_completion_word(
    buffer: &str,
    cursor: usize,
) -> Option<PathCompletionWord> {
    let (start, raw) = current_completion_word(buffer, cursor)?;
    if let Some(quote) = raw.chars().next().filter(|ch| *ch == '\'' || *ch == '"') {
        let quote_len = quote.len_utf8();
        let inner = &raw[quote_len..];
        return Some(PathCompletionWord {
            start: start + quote_len,
            raw: inner.to_owned(),
            decoded: inner.to_owned(),
            quote: Some(quote),
        });
    }

    Some(PathCompletionWord {
        start,
        raw: raw.to_owned(),
        decoded: decode_unquoted_path_word(raw),
        quote: None,
    })
}

fn decode_unquoted_path_word(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        let Some(&next) = chars.peek() else {
            out.push('\\');
            break;
        };
        if backslash_escapes_path_char(next) {
            out.push(chars.next().expect("peeked char exists"));
        } else {
            out.push('\\');
        }
    }
    out
}

fn backslash_escapes_path_char(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '\\' | '\'' | '"' | '$' | '`' | '!' | '&' | ';' | '|' | '<' | '>' | '(' | ')'
        )
}

fn command_completion_candidates(
    history: &history::EditorHistory,
    settings: &EditorSettings,
) -> Vec<String> {
    let mut out = history::command_word_candidates(history, &settings.history_entries);
    for word in &settings.completion_words {
        push_unique(&mut out, word);
    }
    for completion in &settings.command_completions {
        push_unique(&mut out, &completion.command);
    }
    for word in shortest_first(&settings.command_words) {
        push_unique(&mut out, word);
    }
    out
}

fn structured_completion_candidates(
    settings: &EditorSettings,
    buffer: &str,
    word_start: usize,
) -> Option<Vec<String>> {
    let words = shell_segment_words_before(buffer, word_start);
    let command = words.first()?;
    let completion = settings
        .command_completions
        .iter()
        .find(|completion| completion.command == *command)?;

    if words.len() == 1 {
        return Some(
            completion
                .subcommands
                .iter()
                .map(|subcommand| subcommand.name.clone())
                .collect(),
        );
    }

    let subcommand = completion
        .subcommands
        .iter()
        .find(|subcommand| subcommand.name == words[1])?;
    Some(subcommand.arguments.clone())
}

fn shell_segment_words_before(
    buffer: &str,
    word_start: usize,
) -> Vec<String> {
    let Some(prefix) = buffer.get(..word_start) else {
        return Vec::new();
    };
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in prefix.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some('"') => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some(_) => {}
            None => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if ch.is_whitespace() {
                    push_shell_word(&mut words, &mut word);
                } else if is_operator_char(ch) {
                    push_shell_word(&mut words, &mut word);
                    words.clear();
                } else {
                    word.push(ch);
                }
            }
        }
    }
    push_shell_word(&mut words, &mut word);
    words
}

fn push_shell_word(
    words: &mut Vec<String>,
    word: &mut String,
) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

fn word_completion_candidates(
    history: &history::EditorHistory,
    settings: &EditorSettings,
) -> Vec<String> {
    let mut out = history::word_candidates(history, &settings.history_entries);
    for word in &settings.completion_words {
        push_unique(&mut out, word);
    }
    out
}

fn push_unique(
    out: &mut Vec<String>,
    value: &str,
) {
    if !value.is_empty() && !out.iter().any(|existing| existing == value) {
        out.push(value.to_owned());
    }
}

fn shortest_first(words: &[String]) -> Vec<&str> {
    let mut sorted = words.iter().map(String::as_str).collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    sorted
}

#[derive(Debug)]
struct PathCompletionRequest {
    directory: PathBuf,
    entry_prefix: String,
    completed_prefix: String,
    quote: Option<char>,
}

#[derive(Debug, Clone)]
struct PathCompletionCandidate {
    completed_word: String,
    is_dir: bool,
}

#[derive(Debug)]
struct PathCompletionWord {
    start: usize,
    raw: String,
    decoded: String,
    quote: Option<char>,
}

#[derive(Debug, Clone)]
struct CompletionMatches {
    replacement_start: usize,
    query: String,
    candidates: Vec<CompletionCandidate>,
}

#[derive(Debug, Clone)]
struct CompletionCandidate {
    text: String,
    kind: CompletionCandidateKind,
}

impl CompletionCandidate {
    fn prefix(text: String) -> Self {
        Self {
            text,
            kind: CompletionCandidateKind::Prefix,
        }
    }

    fn fuzzy(text: String) -> Self {
        Self {
            text,
            kind: CompletionCandidateKind::Fuzzy,
        }
    }

    fn is_fuzzy(&self) -> bool {
        self.kind == CompletionCandidateKind::Fuzzy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionCandidateKind {
    Prefix,
    Fuzzy,
}

#[derive(Debug)]
struct ScoredCompletionCandidate {
    candidate: String,
    score: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone)]
struct CompletionSelection {
    base: String,
    cursor: usize,
    replacement_start: usize,
    query: String,
    candidates: Vec<CompletionCandidate>,
    index: usize,
}

impl CompletionSelection {
    fn current_suffix(&self) -> Option<String> {
        let candidate = self.candidates.get(self.index)?;
        if candidate.is_fuzzy() {
            return None;
        }
        candidate
            .text
            .starts_with(&self.query)
            .then(|| candidate.text[self.query.len()..].to_owned())
    }

    fn current_text(&self) -> Option<&str> {
        self.candidates
            .get(self.index)
            .map(|candidate| candidate.text.as_str())
    }
}

#[derive(Debug, Clone)]
struct PathCompletionCycle {
    base: String,
    cursor: usize,
    word: String,
    candidates: Vec<PathCompletionCandidate>,
    index: usize,
}

impl PathCompletionCycle {
    fn current_suffix(&self) -> Option<String> {
        let candidate = self.candidates.get(self.index)?;
        Some(candidate.completed_word[self.word.len()..].to_owned())
    }
}

fn cycle_path_completion(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> bool {
    editor.completion_selection = None;
    if let Some(cycle) = editor.path_cycle.as_mut()
        && cycle.cursor == editor.cursor
        && cycle.base == editor.buffer
        && !cycle.candidates.is_empty()
    {
        cycle.index = (cycle.index + 1) % cycle.candidates.len();
        return true;
    }

    let Some((word, candidates)) =
        path_completion_word_and_candidates(&editor.buffer, editor.cursor, settings)
    else {
        clear_completion_state(editor);
        return false;
    };
    let candidates = candidates
        .into_iter()
        .filter(|candidate| {
            candidate.completed_word != word && candidate.completed_word.starts_with(&word)
        })
        .collect::<Vec<_>>();

    if candidates.len() <= 1 {
        clear_completion_state(editor);
        return false;
    }

    let active_suffix = path_completion_suffix(&editor.buffer, editor.cursor, settings);
    let first_visible = candidates
        .iter()
        .position(|candidate| {
            Some(path_completion_candidate_suffix(&word, candidate)) == active_suffix.as_deref()
        })
        .unwrap_or(0);
    let index = (first_visible + 1) % candidates.len();
    editor.path_cycle = Some(PathCompletionCycle {
        base: editor.buffer.clone(),
        cursor: editor.cursor,
        word,
        candidates,
        index,
    });
    true
}

fn cycle_completion_selection(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
    direction: CompletionDirection,
) -> Option<EditOutcome> {
    let selection = if let Some(selection) = editor.completion_selection.as_mut()
        && selection.cursor == editor.cursor
        && selection.base == editor.buffer
    {
        selection
    } else {
        let matches = completion_matches(editor, settings)?;
        let candidates = visible_completion_candidates(matches.clone());
        if candidates.is_empty() {
            return None;
        }
        editor.path_cycle = None;
        editor.completion_selection = Some(CompletionSelection {
            base: editor.buffer.clone(),
            cursor: editor.cursor,
            replacement_start: matches.replacement_start,
            query: matches.query,
            candidates,
            index: 0,
        });
        editor.completion_selection.as_mut().expect("selection set")
    };

    selection.index = match direction {
        CompletionDirection::Previous => {
            (selection.index + selection.candidates.len() - 1) % selection.candidates.len()
        }
        CompletionDirection::Next => (selection.index + 1) % selection.candidates.len(),
    };
    Some(EditOutcome::Updated)
}

fn accept_selected_completion(editor: &mut CommandEditor) -> bool {
    let Some(selection) = editor.completion_selection.take() else {
        return false;
    };
    if selection.cursor != editor.cursor || selection.base != editor.buffer {
        return false;
    }
    let Some(text) = selection.current_text() else {
        return false;
    };
    begin_text_edit(editor);
    replace_completion_selection(editor, &selection, text);
    true
}

fn accept_selected_completion_step(editor: &mut CommandEditor) -> bool {
    let Some(selection) = editor.completion_selection.take() else {
        return false;
    };
    if selection.cursor != editor.cursor || selection.base != editor.buffer {
        return false;
    }
    if let Some(suffix) = selection.current_suffix()
        && let Some(end) = next_completion_step_end(&suffix)
    {
        begin_text_edit(editor);
        replace_selection_or_insert(editor, &suffix[..end]);
        return true;
    }
    let Some(text) = selection.current_text() else {
        return false;
    };
    begin_text_edit(editor);
    replace_completion_selection(editor, &selection, text);
    true
}

fn replace_completion_selection(
    editor: &mut CommandEditor,
    selection: &CompletionSelection,
    text: &str,
) {
    editor.selection = None;
    editor
        .buffer
        .replace_range(selection.replacement_start..selection.cursor, text);
    editor.cursor = selection.replacement_start + text.len();
}

fn accept_path_cycle(editor: &mut CommandEditor) -> bool {
    let Some(cycle) = editor.path_cycle.take() else {
        return false;
    };
    editor.completion_selection = None;
    if cycle.cursor != editor.cursor || cycle.base != editor.buffer {
        return false;
    }
    let Some(suffix) = cycle.current_suffix() else {
        return false;
    };
    begin_text_edit(editor);
    replace_selection_or_insert(editor, &suffix);
    true
}

fn accept_visible_history_completion(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> bool {
    let Some(suffix) = history_completion_suffix(editor, settings) else {
        return false;
    };
    begin_text_edit(editor);
    replace_selection_or_insert(editor, &suffix);
    true
}

fn accept_visible_path_completion(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> bool {
    let Some(suffix) = path_completion_suffix(&editor.buffer, editor.cursor, settings) else {
        return false;
    };
    begin_text_edit(editor);
    replace_selection_or_insert(editor, &suffix);
    true
}

fn path_completion_candidate_suffix<'a>(
    word: &str,
    candidate: &'a PathCompletionCandidate,
) -> &'a str {
    &candidate.completed_word[word.len()..]
}

fn path_completion_allowed(
    buffer: &str,
    word_start: usize,
    word: &str,
    quote: Option<char>,
) -> bool {
    quote.is_some() || is_explicit_path(word) || !is_command_completion_word(buffer, word_start)
}

fn is_explicit_path(word: &str) -> bool {
    word.contains('/')
        || word.contains('\\')
        || word.starts_with('.')
        || word.starts_with('~')
        || word.starts_with(std::path::MAIN_SEPARATOR)
}

fn is_command_completion_word(
    buffer: &str,
    word_start: usize,
) -> bool {
    let Some(prefix) = buffer.get(..word_start) else {
        return false;
    };
    let prefix = prefix.trim_end();
    if prefix.is_empty() {
        return true;
    }
    prefix
        .chars()
        .next_back()
        .is_some_and(is_command_separator_char)
}

fn path_completion_request(
    current_dir: &Path,
    word: &PathCompletionWord,
) -> Option<PathCompletionRequest> {
    let (decoded_dir, entry_prefix) = split_path_completion_word(&word.decoded);
    let raw_dir = if decoded_dir.is_empty() {
        ""
    } else {
        split_path_completion_word(&word.raw).0
    };
    let directory = path_completion_directory(current_dir, decoded_dir)?;
    Some(PathCompletionRequest {
        directory,
        entry_prefix: entry_prefix.to_owned(),
        completed_prefix: raw_dir.to_owned(),
        quote: word.quote,
    })
}

fn split_path_completion_word(word: &str) -> (&str, &str) {
    word.rfind(is_path_separator)
        .map(|idx| {
            let end = idx + word[idx..].chars().next().map_or(0, char::len_utf8);
            (&word[..end], &word[end..])
        })
        .unwrap_or(("", word))
}

fn path_completion_directory(
    current_dir: &Path,
    typed_dir: &str,
) -> Option<PathBuf> {
    if typed_dir.is_empty() {
        return Some(current_dir.to_owned());
    }
    if typed_dir == "~/" {
        return home_dir();
    }
    if let Some(rest) = typed_dir.strip_prefix("~/") {
        return home_dir().map(|home| home.join(rest));
    }
    let normalized_dir = typed_dir.replace('\\', "/");
    let typed_path = Path::new(&normalized_dir);
    if typed_path.is_absolute() {
        Some(typed_path.to_owned())
    } else {
        Some(current_dir.join(typed_path))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn path_completion_candidates(
    request: &PathCompletionRequest
) -> Option<Vec<PathCompletionCandidate>> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&request.directory).ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with(&request.entry_prefix) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = file_type.is_dir();
        let suffix = if is_dir { format!("{name}/") } else { name };
        let completed_word = format!(
            "{}{}",
            request.completed_prefix,
            encode_path_completion_text(&suffix, request.quote)
        );
        candidates.push(PathCompletionCandidate {
            completed_word,
            is_dir,
        });
    }
    Some(candidates)
}

fn encode_path_completion_text(
    text: &str,
    quote: Option<char>,
) -> String {
    match quote {
        Some('"') => escape_double_quoted_path_text(text),
        Some('\'') => text.to_owned(),
        None => escape_unquoted_path_text(text),
        Some(_) => text.to_owned(),
    }
}

fn escape_unquoted_path_text(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_whitespace()
            || matches!(
                ch,
                '\\' | '\'' | '"' | '$' | '`' | '!' | '&' | ';' | '|' | '<' | '>' | '(' | ')'
            )
        {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn escape_double_quoted_path_text(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests;
