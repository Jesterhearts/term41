//! Command editor state and shell-oriented text transforms.
//!
//! The crate deliberately knows nothing about terminals, windows, or PTYs.
//! Callers translate their input events into [`EditorInput`], apply them to a
//! [`CommandEditor`], and render the returned [`CommandLineView`] however their
//! UI needs.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use unicode_segmentation::UnicodeSegmentation;

const DEFAULT_MAX_HISTORY: usize = 200;
const MAX_COMPLETION_CANDIDATES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSettings {
    pub completion_words: Vec<String>,
    pub command_words: Vec<String>,
    pub current_dir: Option<PathBuf>,
    pub max_history: usize,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            completion_words: Vec::new(),
            command_words: Vec::new(),
            current_dir: None,
            max_history: DEFAULT_MAX_HISTORY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Plain,
    Command,
    Keyword,
    Builtin,
    String,
    Variable,
    Operator,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub kind: HighlightKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLineView {
    pub text: String,
    pub cursor: usize,
    pub spans: Vec<HighlightSpan>,
    pub completion: Option<String>,
    pub candidates: Vec<String>,
    pub candidate_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorInput {
    Insert(String),
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

#[derive(Debug, Clone, Default)]
pub struct CommandEditor {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
    kill_buffer: String,
    path_cycle: Option<PathCompletionCycle>,
    completion_selection: Option<CompletionSelection>,
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
        let completion = selection
            .and_then(CompletionSelection::current_suffix)
            .or_else(|| self.path_cycle_suffix())
            .or_else(|| completion_suffix(&self.buffer, self.cursor, settings, &self.history));
        let (candidates, candidate_index) = completion_candidate_view(self, settings);
        CommandLineView {
            text: self.buffer.clone(),
            cursor: self.cursor,
            spans: highlight_shell(&self.buffer),
            completion,
            candidates,
            candidate_index,
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_pos = None;
        self.draft.clear();
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
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.buffer.insert_str(editor.cursor, &text);
            editor.cursor += text.len();
            EditOutcome::Updated
        }
        EditorInput::Enter => {
            let command = editor.buffer.clone();
            push_history(editor, &command, settings.max_history);
            editor.clear();
            EditOutcome::Submitted(command)
        }
        EditorInput::Backspace => {
            let Some(prev) = previous_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.buffer.drain(prev..editor.cursor);
            editor.cursor = prev;
            EditOutcome::Updated
        }
        EditorInput::Delete => {
            let Some(next) = next_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.buffer.drain(editor.cursor..next);
            EditOutcome::Updated
        }
        EditorInput::MoveLeft => {
            let Some(prev) = previous_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = prev;
            EditOutcome::Updated
        }
        EditorInput::MoveRight => {
            if accept_selected_completion(editor)
                || accept_path_cycle(editor)
                || accept_visible_path_completion(editor, settings)
            {
                return EditOutcome::Updated;
            }
            let Some(next) = next_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = next;
            EditOutcome::Updated
        }
        EditorInput::MoveWordLeft => {
            let Some(prev) = previous_word_start(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = prev;
            EditOutcome::Updated
        }
        EditorInput::MoveWordRight => {
            let Some(next) = next_word_end(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            editor.cursor = next;
            EditOutcome::Updated
        }
        EditorInput::MoveHome => {
            if editor.cursor == 0 {
                EditOutcome::Ignored
            } else {
                clear_completion_state(editor);
                editor.cursor = 0;
                EditOutcome::Updated
            }
        }
        EditorInput::MoveEnd => {
            if editor.cursor == editor.buffer.len() {
                EditOutcome::Ignored
            } else {
                clear_completion_state(editor);
                editor.cursor = editor.buffer.len();
                EditOutcome::Updated
            }
        }
        EditorInput::DeleteWordLeft => {
            let Some(prev) = previous_word_start(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.kill_buffer = editor.buffer[prev..editor.cursor].to_owned();
            editor.buffer.drain(prev..editor.cursor);
            editor.cursor = prev;
            EditOutcome::Updated
        }
        EditorInput::DeleteWordRight => {
            let Some(next) = next_word_end(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.kill_buffer = editor.buffer[editor.cursor..next].to_owned();
            editor.buffer.drain(editor.cursor..next);
            EditOutcome::Updated
        }
        EditorInput::KillToStart => {
            if editor.cursor == 0 {
                return EditOutcome::Ignored;
            }
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.kill_buffer = editor.buffer[..editor.cursor].to_owned();
            editor.buffer.drain(..editor.cursor);
            editor.cursor = 0;
            EditOutcome::Updated
        }
        EditorInput::KillToEnd => {
            if editor.cursor == editor.buffer.len() {
                return EditOutcome::Ignored;
            }
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.kill_buffer = editor.buffer[editor.cursor..].to_owned();
            editor.buffer.truncate(editor.cursor);
            EditOutcome::Updated
        }
        EditorInput::Yank => {
            if editor.kill_buffer.is_empty() {
                return EditOutcome::Ignored;
            }
            clear_completion_state(editor);
            replace_history_edit_with_draft(editor);
            editor.buffer.insert_str(editor.cursor, &editor.kill_buffer);
            editor.cursor += editor.kill_buffer.len();
            EditOutcome::Updated
        }
        EditorInput::HistoryPrevious => {
            cycle_completion_selection(editor, settings, CompletionDirection::Previous)
                .unwrap_or_else(|| history_previous(editor))
        }
        EditorInput::HistoryNext => {
            cycle_completion_selection(editor, settings, CompletionDirection::Next)
                .unwrap_or_else(|| history_next(editor))
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

pub fn highlight_shell(text: &str) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut command_position = true;
    let mut i = 0;
    while i < text.len() {
        let ch = text[i..].chars().next().expect("valid char boundary");
        if ch.is_whitespace() {
            let start = i;
            i += ch.len_utf8();
            while i < text.len() {
                let next = text[i..].chars().next().expect("valid char boundary");
                if !next.is_whitespace() {
                    break;
                }
                i += next.len_utf8();
            }
            spans.push(span(start, i, HighlightKind::Plain));
            continue;
        }

        if ch == '#' && starts_shell_comment(text, i) {
            spans.push(span(i, text.len(), HighlightKind::Comment));
            break;
        }

        if is_operator_char(ch) {
            let start = i;
            i += ch.len_utf8();
            while i < text.len() {
                let next = text[i..].chars().next().expect("valid char boundary");
                if !is_operator_char(next) {
                    break;
                }
                i += next.len_utf8();
            }
            spans.push(span(start, i, HighlightKind::Operator));
            command_position = true;
            continue;
        }

        if ch == '\'' || ch == '"' {
            let (end, kind) = quoted_span_end(text, i, ch);
            spans.push(span(i, end, kind));
            i = end;
            command_position = false;
            continue;
        }

        if ch == '$' {
            let end = variable_span_end(text, i);
            spans.push(span(i, end, HighlightKind::Variable));
            i = end;
            command_position = false;
            continue;
        }

        let start = i;
        i += ch.len_utf8();
        while i < text.len() {
            let next = text[i..].chars().next().expect("valid char boundary");
            if next.is_whitespace()
                || is_operator_char(next)
                || next == '\''
                || next == '"'
                || next == '$'
                || (next == '#' && command_position)
            {
                break;
            }
            i += next.len_utf8();
        }
        let word = &text[start..i];
        let kind = if is_shell_keyword(word) {
            HighlightKind::Keyword
        } else if is_shell_builtin(word) {
            HighlightKind::Builtin
        } else if command_position {
            HighlightKind::Command
        } else {
            HighlightKind::Plain
        };
        spans.push(span(start, i, kind));
        command_position = false;
    }
    spans
}

fn span(
    start: usize,
    end: usize,
    kind: HighlightKind,
) -> HighlightSpan {
    HighlightSpan { start, end, kind }
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
    if editor.history_pos.is_some() {
        editor.history_pos = None;
        editor.draft = editor.buffer.clone();
    }
}

fn clear_completion_state(editor: &mut CommandEditor) {
    editor.path_cycle = None;
    editor.completion_selection = None;
}

fn history_previous(editor: &mut CommandEditor) -> EditOutcome {
    if editor.history.is_empty() {
        return EditOutcome::Ignored;
    }
    clear_completion_state(editor);
    let pos = match editor.history_pos {
        Some(pos) if pos > 0 => pos - 1,
        Some(_) => return EditOutcome::Ignored,
        None => {
            editor.draft = editor.buffer.clone();
            editor.history.len() - 1
        }
    };
    editor.history_pos = Some(pos);
    editor.buffer = editor.history[pos].clone();
    editor.cursor = editor.buffer.len();
    EditOutcome::Updated
}

fn history_next(editor: &mut CommandEditor) -> EditOutcome {
    let Some(pos) = editor.history_pos else {
        return EditOutcome::Ignored;
    };
    clear_completion_state(editor);
    if pos + 1 < editor.history.len() {
        editor.history_pos = Some(pos + 1);
        editor.buffer = editor.history[pos + 1].clone();
    } else {
        editor.history_pos = None;
        editor.buffer = editor.draft.clone();
        editor.draft.clear();
    }
    editor.cursor = editor.buffer.len();
    EditOutcome::Updated
}

fn complete_current_prefix(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> EditOutcome {
    if accept_selected_completion(editor) {
        return EditOutcome::Updated;
    }

    if cycle_path_completion(editor, settings) {
        return EditOutcome::Updated;
    }

    let Some(suffix) = completion_suffix(&editor.buffer, editor.cursor, settings, &editor.history)
    else {
        return EditOutcome::Ignored;
    };
    clear_completion_state(editor);
    replace_history_edit_with_draft(editor);
    editor.buffer.insert_str(editor.cursor, &suffix);
    editor.cursor += suffix.len();
    EditOutcome::Updated
}

fn push_history(
    editor: &mut CommandEditor,
    command: &str,
    max_history: usize,
) {
    let trimmed = command.trim();
    if trimmed.is_empty() || editor.history.last().is_some_and(|last| last == command) {
        return;
    }
    editor.history.push(command.to_owned());
    let max_history = max_history.max(1);
    let excess = editor.history.len().saturating_sub(max_history);
    if excess > 0 {
        editor.history.drain(0..excess);
    }
}

fn completion_suffix(
    buffer: &str,
    cursor: usize,
    settings: &EditorSettings,
    history: &[String],
) -> Option<String> {
    if cursor == buffer.len() && !buffer.is_empty() {
        for candidate in history_command_candidates(history) {
            if candidate != buffer && candidate.starts_with(buffer) {
                return Some(candidate[buffer.len()..].to_owned());
            }
        }
    }

    if let Some(suffix) = path_completion_suffix(buffer, cursor, settings) {
        return Some(suffix);
    }

    let (word_start, prefix) = current_completion_word(buffer, cursor)?;
    if prefix.is_empty() {
        return None;
    }
    let candidates = if is_command_completion_word(buffer, word_start) {
        command_completion_candidates(settings, history)
    } else {
        word_completion_candidates(settings, history)
    };
    for candidate in candidates {
        if candidate != prefix && candidate.starts_with(prefix) {
            return Some(candidate[prefix.len()..].to_owned());
        }
    }
    None
}

fn completion_candidate_view(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> (Vec<String>, usize) {
    if let Some(selection) = editor.valid_completion_selection() {
        let candidates = top_ambiguous_candidates(selection.candidates.clone());
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

    let matches = completion_matches(&editor.buffer, editor.cursor, settings, &editor.history);
    let candidates = matches
        .map(|matches| top_ambiguous_candidates(matches.candidates))
        .unwrap_or_default();
    (candidates, 0)
}

fn completion_matches(
    buffer: &str,
    cursor: usize,
    settings: &EditorSettings,
    history: &[String],
) -> Option<CompletionMatches> {
    if cursor == buffer.len() && !buffer.is_empty() {
        let candidates = history_command_candidates(history)
            .into_iter()
            .filter(|candidate| candidate != buffer && candidate.starts_with(buffer))
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            return Some(CompletionMatches {
                prefix: buffer.to_owned(),
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
        command_completion_candidates(settings, history)
    } else {
        word_completion_candidates(settings, history)
    };
    let candidates = candidates
        .into_iter()
        .filter(|candidate| candidate != prefix && candidate.starts_with(prefix))
        .collect::<Vec<_>>();
    Some(CompletionMatches {
        prefix: prefix.to_owned(),
        candidates,
    })
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
        .collect();
    Some(CompletionMatches {
        prefix: word,
        candidates,
    })
}

fn path_completion_word_and_candidates(
    buffer: &str,
    cursor: usize,
    settings: &EditorSettings,
) -> Option<(String, Vec<PathCompletionCandidate>)> {
    let current_dir = settings.current_dir.as_deref()?;
    let (word_start, word) = current_completion_word(buffer, cursor)?;
    if word.is_empty() || !path_completion_allowed(buffer, word_start, word) {
        return None;
    }

    let request = path_completion_request(current_dir, word)?;
    let mut candidates = path_completion_candidates(&request)?;
    candidates.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.completed_word.cmp(&b.completed_word))
    });
    Some((word.to_owned(), candidates))
}

fn current_completion_word(
    buffer: &str,
    cursor: usize,
) -> Option<(usize, &str)> {
    if cursor > buffer.len() || !buffer.is_char_boundary(cursor) {
        return None;
    }
    let start = buffer[..cursor]
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace() || is_operator_char(*ch))
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    Some((start, &buffer[start..cursor]))
}

fn history_command_candidates(history: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for command in history.iter().rev() {
        push_unique(&mut out, command);
    }
    out
}

fn command_completion_candidates(
    settings: &EditorSettings,
    history: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for command in history.iter().rev() {
        if let Some(first_word) = command.split_whitespace().next() {
            push_unique(&mut out, first_word);
        }
    }
    for word in &settings.completion_words {
        push_unique(&mut out, word);
    }
    for word in shortest_first(&settings.command_words) {
        push_unique(&mut out, word);
    }
    out
}

fn word_completion_candidates(
    settings: &EditorSettings,
    history: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for command in history.iter().rev() {
        push_unique(&mut out, command);
    }
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
}

#[derive(Debug, Clone)]
struct PathCompletionCandidate {
    completed_word: String,
    is_dir: bool,
}

#[derive(Debug, Clone)]
struct CompletionMatches {
    prefix: String,
    candidates: Vec<String>,
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
    prefix: String,
    candidates: Vec<String>,
    index: usize,
}

impl CompletionSelection {
    fn current_suffix(&self) -> Option<String> {
        let candidate = self.candidates.get(self.index)?;
        candidate
            .starts_with(&self.prefix)
            .then(|| candidate[self.prefix.len()..].to_owned())
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
        let matches = completion_matches(&editor.buffer, editor.cursor, settings, &editor.history)?;
        let candidates = top_ambiguous_candidates(matches.candidates);
        if candidates.len() <= 1 {
            return None;
        }
        editor.path_cycle = None;
        editor.completion_selection = Some(CompletionSelection {
            base: editor.buffer.clone(),
            cursor: editor.cursor,
            prefix: matches.prefix,
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
    let Some(suffix) = selection.current_suffix() else {
        return false;
    };
    editor.path_cycle = None;
    replace_history_edit_with_draft(editor);
    editor.buffer.insert_str(editor.cursor, &suffix);
    editor.cursor += suffix.len();
    true
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
    replace_history_edit_with_draft(editor);
    editor.buffer.insert_str(editor.cursor, &suffix);
    editor.cursor += suffix.len();
    true
}

fn accept_visible_path_completion(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> bool {
    let Some(suffix) = path_completion_suffix(&editor.buffer, editor.cursor, settings) else {
        return false;
    };
    clear_completion_state(editor);
    replace_history_edit_with_draft(editor);
    editor.buffer.insert_str(editor.cursor, &suffix);
    editor.cursor += suffix.len();
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
) -> bool {
    is_explicit_path(word) || !is_command_completion_word(buffer, word_start)
}

fn is_explicit_path(word: &str) -> bool {
    word.contains('/')
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
    word: &str,
) -> Option<PathCompletionRequest> {
    let (typed_dir, entry_prefix) = split_path_completion_word(word);
    let directory = path_completion_directory(current_dir, typed_dir)?;
    Some(PathCompletionRequest {
        directory,
        entry_prefix: entry_prefix.to_owned(),
        completed_prefix: typed_dir.to_owned(),
    })
}

fn split_path_completion_word(word: &str) -> (&str, &str) {
    word.rsplit_once('/')
        .map(|(dir, entry)| (&word[..dir.len() + 1], entry))
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
    let typed_path = Path::new(typed_dir);
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
        let mut completed_word = format!("{}{}", request.completed_prefix, name);
        if is_dir {
            completed_word.push('/');
        }
        candidates.push(PathCompletionCandidate {
            completed_word,
            is_dir,
        });
    }
    Some(candidates)
}

fn quoted_span_end(
    text: &str,
    start: usize,
    quote: char,
) -> (usize, HighlightKind) {
    let mut escaped = false;
    let mut i = start + quote.len_utf8();
    while i < text.len() {
        let ch = text[i..].chars().next().expect("valid char boundary");
        i += ch.len_utf8();
        if quote == '"' && ch == '\\' && !escaped {
            escaped = true;
            continue;
        }
        if ch == quote && !escaped {
            break;
        }
        escaped = false;
    }
    (i, HighlightKind::String)
}

fn variable_span_end(
    text: &str,
    start: usize,
) -> usize {
    let mut i = start + 1;
    if text[i..].starts_with('{') {
        i += 1;
        while i < text.len() {
            let ch = text[i..].chars().next().expect("valid char boundary");
            i += ch.len_utf8();
            if ch == '}' {
                break;
            }
        }
        return i;
    }
    while i < text.len() {
        let ch = text[i..].chars().next().expect("valid char boundary");
        if !(ch == '_' || ch.is_ascii_alphanumeric()) {
            break;
        }
        i += ch.len_utf8();
    }
    i.max(start + 1)
}

fn is_operator_char(ch: char) -> bool {
    matches!(ch, '|' | '&' | ';' | '<' | '>' | '(' | ')')
}

fn is_command_separator_char(ch: char) -> bool {
    matches!(ch, '|' | '&' | ';' | '(')
}

fn starts_shell_comment(
    text: &str,
    idx: usize,
) -> bool {
    if idx == 0 {
        return true;
    }
    text[..idx]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace() || is_operator_char(ch))
}

fn is_shell_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "in"
            | "function"
            | "time"
    )
}

fn is_shell_builtin(word: &str) -> bool {
    matches!(
        word,
        "alias"
            | "bg"
            | "cd"
            | "command"
            | "dirs"
            | "echo"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "fg"
            | "jobs"
            | "popd"
            | "pushd"
            | "pwd"
            | "read"
            | "set"
            | "shift"
            | "source"
            | "test"
            | "trap"
            | "type"
            | "ulimit"
            | "umask"
            | "unalias"
            | "unset"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(words: &[&str]) -> EditorSettings {
        EditorSettings {
            completion_words: words.iter().map(|word| (*word).to_owned()).collect(),
            command_words: Vec::new(),
            current_dir: None,
            max_history: 20,
        }
    }

    fn command_settings(words: &[&str]) -> EditorSettings {
        EditorSettings {
            completion_words: Vec::new(),
            command_words: words.iter().map(|word| (*word).to_owned()).collect(),
            current_dir: None,
            max_history: 20,
        }
    }

    fn path_settings(current_dir: PathBuf) -> EditorSettings {
        EditorSettings {
            completion_words: Vec::new(),
            command_words: Vec::new(),
            current_dir: Some(current_dir),
            max_history: 20,
        }
    }

    #[test]
    fn inserts_and_submits_command() {
        let mut editor = CommandEditor::new();
        let outcome = apply_input(
            &mut editor,
            EditorInput::Insert("cargo test".to_owned()),
            &EditorSettings::default(),
        );
        assert_eq!(outcome, EditOutcome::Updated);
        assert_eq!(
            apply_input(&mut editor, EditorInput::Enter, &EditorSettings::default()),
            EditOutcome::Submitted("cargo test".to_owned())
        );
        assert!(editor.is_empty());
    }

    #[test]
    fn history_arrows_restore_draft() {
        let mut editor = CommandEditor::new();
        let settings = EditorSettings::default();
        apply_input(
            &mut editor,
            EditorInput::Insert("one".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Enter, &settings);
        apply_input(
            &mut editor,
            EditorInput::Insert("two".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Enter, &settings);
        apply_input(
            &mut editor,
            EditorInput::Insert("draft".to_owned()),
            &settings,
        );

        assert_eq!(
            apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
            EditOutcome::Updated
        );
        assert_eq!(editor.view(&settings).text, "two");
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);
        assert_eq!(editor.view(&settings).text, "one");
        apply_input(&mut editor, EditorInput::HistoryNext, &settings);
        apply_input(&mut editor, EditorInput::HistoryNext, &settings);
        assert_eq!(editor.view(&settings).text, "draft");
    }

    #[test]
    fn word_motion_skips_shell_words_and_separators() {
        let mut editor = CommandEditor::new();
        let settings = EditorSettings::default();
        apply_input(
            &mut editor,
            EditorInput::Insert("cargo test | rg foo".to_owned()),
            &settings,
        );

        apply_input(&mut editor, EditorInput::MoveWordLeft, &settings);
        assert_eq!(editor.view(&settings).cursor, "cargo test | rg ".len());

        apply_input(&mut editor, EditorInput::MoveWordLeft, &settings);
        assert_eq!(editor.view(&settings).cursor, "cargo test | ".len());

        apply_input(&mut editor, EditorInput::MoveWordRight, &settings);
        assert_eq!(editor.view(&settings).cursor, "cargo test | rg".len());
    }

    #[test]
    fn word_delete_updates_kill_buffer_for_yank() {
        let mut editor = CommandEditor::new();
        let settings = EditorSettings::default();
        apply_input(
            &mut editor,
            EditorInput::Insert("cargo test".to_owned()),
            &settings,
        );

        apply_input(&mut editor, EditorInput::DeleteWordLeft, &settings);
        assert_eq!(editor.view(&settings).text, "cargo ");
        apply_input(&mut editor, EditorInput::Yank, &settings);
        assert_eq!(editor.view(&settings).text, "cargo test");
    }

    #[test]
    fn line_kill_to_start_and_end_can_yank() {
        let mut editor = CommandEditor::new();
        let settings = EditorSettings::default();
        apply_input(
            &mut editor,
            EditorInput::Insert("cargo test --all".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::MoveWordLeft, &settings);

        apply_input(&mut editor, EditorInput::KillToStart, &settings);
        assert_eq!(editor.view(&settings).text, "--all");
        apply_input(&mut editor, EditorInput::Yank, &settings);
        assert_eq!(editor.view(&settings).text, "cargo test --all");

        apply_input(&mut editor, EditorInput::KillToEnd, &settings);
        assert_eq!(editor.view(&settings).text, "cargo test ");
        apply_input(&mut editor, EditorInput::Yank, &settings);
        assert_eq!(editor.view(&settings).text, "cargo test --all");
    }

    #[test]
    fn tab_completion_uses_prefix_match() {
        let mut editor = CommandEditor::new();
        let settings = settings(&["cargo", "cat"]);
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
        apply_input(&mut editor, EditorInput::Complete, &settings);
        assert_eq!(editor.view(&settings).text, "cargo");
    }

    #[test]
    fn completion_matches_whole_command_prefix_from_history() {
        let mut editor = CommandEditor::new();
        let settings = EditorSettings::default();
        apply_input(
            &mut editor,
            EditorInput::Insert("cargo build".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Enter, &settings);
        apply_input(
            &mut editor,
            EditorInput::Insert("cargo b".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("uild"));
    }

    #[test]
    fn command_words_complete_initial_command() {
        let settings = command_settings(&["cargo", "cat"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
    }

    #[test]
    fn command_words_prefer_shortest_matching_command() {
        let settings = command_settings(&["cargo-audit", "cargo"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
    }

    #[test]
    fn command_view_shows_top_five_ambiguous_candidates() {
        let settings = command_settings(&[
            "cargo-audit",
            "cargo",
            "cargo-edit",
            "cargo-nextest",
            "cargo-watch",
            "cargo-zigbuild",
        ]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );

        assert_eq!(
            editor.view(&settings).candidates,
            [
                "cargo",
                "cargo-edit",
                "cargo-audit",
                "cargo-watch",
                "cargo-nextest"
            ]
        );
        assert_eq!(editor.view(&settings).candidate_index, 0);
    }

    #[test]
    fn command_view_hides_single_candidate_list() {
        let settings = command_settings(&["cargo"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );

        assert!(editor.view(&settings).candidates.is_empty());
    }

    #[test]
    fn history_arrows_cycle_ambiguous_completion_selection() {
        let settings = command_settings(&["cargo", "cargo-audit", "cargo-edit"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );

        apply_input(&mut editor, EditorInput::HistoryNext, &settings);
        let view = editor.view(&settings);
        assert_eq!(view.candidate_index, 1);
        assert_eq!(view.completion.as_deref(), Some("go-edit"));

        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);
        let view = editor.view(&settings);
        assert_eq!(view.candidate_index, 0);
        assert_eq!(view.completion.as_deref(), Some("go"));
    }

    #[test]
    fn tab_accepts_selected_completion_candidate() {
        let settings = command_settings(&["cargo", "cargo-audit", "cargo-edit"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("car".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::HistoryNext, &settings);

        apply_input(&mut editor, EditorInput::Complete, &settings);

        assert_eq!(editor.view(&settings).text, "cargo-edit");
        assert!(editor.view(&settings).candidates.is_empty());
    }

    #[test]
    fn history_arrows_fall_back_without_ambiguous_completion() {
        let settings = command_settings(&["cargo"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("git status".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Enter, &settings);

        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);

        assert_eq!(editor.view(&settings).text, "git status");
    }

    #[test]
    fn history_first_words_win_before_command_words() {
        let settings = command_settings(&["cargo"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat README.md".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Enter, &settings);
        apply_input(&mut editor, EditorInput::Insert("ca".to_owned()), &settings);

        assert_eq!(
            editor.view(&settings).completion.as_deref(),
            Some("t README.md")
        );
    }

    #[test]
    fn command_words_do_not_complete_argument_words() {
        let settings = command_settings(&["cargo"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("echo car".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion, None);
    }

    #[test]
    fn command_words_complete_after_shell_separator() {
        let settings = command_settings(&["cargo"]);
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("echo ok | car".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
    }

    #[test]
    fn completion_matches_relative_file_path() {
        let root = unique_test_dir("relative-file");
        fs::create_dir_all(&root).expect("create temp dir");
        fs::write(root.join("README.md"), "").expect("write temp file");
        fs::write(root.join("ROADMAP.md"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat REA".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("DME.md"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_marks_directory_with_trailing_slash() {
        let root = unique_test_dir("directory");
        fs::create_dir_all(root.join("src")).expect("create temp dir");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cd sr".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("c/"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_matches_nested_path_prefix() {
        let root = unique_test_dir("nested");
        fs::create_dir_all(root.join("src")).expect("create temp dir");
        fs::write(root.join("src/main.rs"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("vim src/ma".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("in.rs"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tab_cycles_multiple_path_matches_without_inserting() {
        let root = unique_test_dir("cycle");
        fs::create_dir_all(&root).expect("create temp dir");
        fs::write(root.join("food.txt"), "").expect("write temp file");
        fs::write(root.join("foot.txt"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat foo".to_owned()),
            &settings,
        );
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

        assert_eq!(
            apply_input(&mut editor, EditorInput::Complete, &settings),
            EditOutcome::Updated
        );
        assert_eq!(editor.view(&settings).text, "cat foo");
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("t.txt"));

        apply_input(&mut editor, EditorInput::Complete, &settings);
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn right_arrow_accepts_active_path_cycle() {
        let root = unique_test_dir("cycle-accept");
        fs::create_dir_all(&root).expect("create temp dir");
        fs::write(root.join("food.txt"), "").expect("write temp file");
        fs::write(root.join("foot.txt"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat foo".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Complete, &settings);

        assert_eq!(
            apply_input(&mut editor, EditorInput::MoveRight, &settings),
            EditOutcome::Updated
        );
        assert_eq!(editor.view(&settings).text, "cat foot.txt");
        assert_eq!(editor.view(&settings).completion, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn right_arrow_accepts_visible_path_completion_without_tab() {
        let root = unique_test_dir("visible-accept");
        fs::create_dir_all(&root).expect("create temp dir");
        fs::write(root.join("food.txt"), "").expect("write temp file");
        fs::write(root.join("foot.txt"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat foo".to_owned()),
            &settings,
        );
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

        assert_eq!(
            apply_input(&mut editor, EditorInput::MoveRight, &settings),
            EditOutcome::Updated
        );
        assert_eq!(editor.view(&settings).text, "cat food.txt");
        assert_eq!(editor.view(&settings).completion, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn typing_resets_active_path_cycle() {
        let root = unique_test_dir("cycle-reset");
        fs::create_dir_all(&root).expect("create temp dir");
        fs::write(root.join("food.txt"), "").expect("write temp file");
        fs::write(root.join("foot.txt"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat foo".to_owned()),
            &settings,
        );
        apply_input(&mut editor, EditorInput::Complete, &settings);
        apply_input(&mut editor, EditorInput::Insert("d".to_owned()), &settings);

        assert_eq!(editor.view(&settings).text, "cat food");
        assert_eq!(editor.view(&settings).completion.as_deref(), Some(".txt"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tab_cycle_skips_exact_file_match() {
        let root = unique_test_dir("cycle-exact");
        fs::create_dir_all(&root).expect("create temp dir");
        fs::write(root.join("foo"), "").expect("write temp file");
        fs::write(root.join("food.txt"), "").expect("write temp file");
        fs::write(root.join("foot.txt"), "").expect("write temp file");
        let settings = path_settings(root.clone());
        let mut editor = CommandEditor::new();
        apply_input(
            &mut editor,
            EditorInput::Insert("cat foo".to_owned()),
            &settings,
        );

        assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));
        apply_input(&mut editor, EditorInput::Complete, &settings);
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("t.txt"));
        apply_input(&mut editor, EditorInput::Complete, &settings);
        assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

        let _ = fs::remove_dir_all(root);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("commands41-{label}-{nonce}"))
    }

    #[test]
    fn highlights_shell_constructs() {
        let spans = highlight_shell("if echo \"$HOME\" # comment");
        assert!(spans.iter().any(|span| span.kind == HighlightKind::Keyword));
        assert!(spans.iter().any(|span| span.kind == HighlightKind::Builtin));
        assert!(spans.iter().any(|span| span.kind == HighlightKind::String));
        assert!(spans.iter().any(|span| span.kind == HighlightKind::Comment));
    }
}
