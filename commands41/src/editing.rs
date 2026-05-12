use unicode_segmentation::UnicodeSegmentation;

use crate::CommandEditor;
use crate::EditOutcome;
use crate::EditorInput;
use crate::EditorSelection;
use crate::EditorSettings;
use crate::completion;
use crate::completion::CompletionDirection;
use crate::history;
use crate::syntax::is_operator_char;
use crate::undo;

pub fn set_cursor(
    editor: &mut CommandEditor,
    cursor: usize,
) -> EditOutcome {
    let Some(cursor) = valid_boundary(&editor.buffer, cursor) else {
        return EditOutcome::Ignored;
    };
    completion::clear_completion_state(editor);
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
    completion::clear_completion_state(editor);
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
        EditorInput::Vim(key) => crate::vim::apply_vim_key(editor, key, settings),
        EditorInput::Enter => {
            let command = submitted_command(&editor.buffer, settings.escape_character);
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
            completion::clear_completion_state(editor);
            editor.cursor = prev;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveRight => {
            if completion::accept_selected_completion(editor)
                || completion::accept_path_cycle(editor)
                || completion::accept_visible_history_completion(editor, settings)
                || completion::accept_visible_path_completion(editor, settings)
            {
                return EditOutcome::Updated;
            }
            let Some(next) = next_grapheme_boundary(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            completion::clear_completion_state(editor);
            editor.cursor = next;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveWordLeft => {
            let Some(prev) = previous_word_start(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            completion::clear_completion_state(editor);
            editor.cursor = prev;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveWordRight => {
            let Some(next) = next_word_end(&editor.buffer, editor.cursor) else {
                return EditOutcome::Ignored;
            };
            completion::clear_completion_state(editor);
            editor.cursor = next;
            editor.selection = None;
            EditOutcome::Updated
        }
        EditorInput::MoveHome => {
            if editor.cursor == 0 {
                EditOutcome::Ignored
            } else {
                completion::clear_completion_state(editor);
                editor.cursor = 0;
                editor.selection = None;
                EditOutcome::Updated
            }
        }
        EditorInput::MoveEnd => {
            if editor.cursor == editor.buffer.len() {
                EditOutcome::Ignored
            } else {
                completion::clear_completion_state(editor);
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
        EditorInput::Undo => undo::undo_text_edit(editor),
        EditorInput::Redo => undo::redo_text_edit(editor),
        EditorInput::HistoryPrevious => {
            if history::is_navigating(&editor.history) {
                return history_previous(editor, settings);
            }
            completion::cycle_completion_selection(editor, settings, CompletionDirection::Previous)
                .or_else(|| move_cursor_line(editor, LineDirection::Previous))
                .unwrap_or_else(|| history_previous(editor, settings))
        }
        EditorInput::HistoryNext => {
            if history::is_navigating(&editor.history) {
                return history_next(editor, settings);
            }
            completion::cycle_completion_selection(editor, settings, CompletionDirection::Next)
                .or_else(|| move_cursor_line(editor, LineDirection::Next))
                .unwrap_or_else(|| history_next(editor, settings))
        }
        EditorInput::Complete => completion::complete_current_prefix(editor, settings),
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

pub(crate) fn previous_grapheme_boundary(
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

pub(crate) fn next_grapheme_boundary(
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

pub(crate) fn replace_history_edit_with_draft(editor: &mut CommandEditor) {
    history::replace_edit_with_draft(&mut editor.history, &editor.buffer);
}

pub(crate) fn begin_text_edit(editor: &mut CommandEditor) {
    undo::push_undo_snapshot(editor);
    completion::clear_completion_state(editor);
    replace_history_edit_with_draft(editor);
}

fn valid_boundary(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    (cursor <= text.len() && text.is_char_boundary(cursor)).then_some(cursor)
}

pub(crate) fn replace_selection_or_insert(
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
    completion::clear_completion_state(editor);
    editor.undo = undo::UndoHistory::default();
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
    completion::clear_completion_state(editor);
    editor.undo = undo::UndoHistory::default();
    editor.buffer = command;
    editor.cursor = editor.buffer.len();
    editor.selection = None;
    EditOutcome::Updated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineDirection {
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
    completion::clear_completion_state(editor);
    editor.cursor = target_cursor;
    editor.selection = None;
    Some(EditOutcome::Updated)
}

pub(crate) fn line_ranges(text: &str) -> Vec<(usize, usize)> {
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

pub(crate) fn line_index_at_cursor(
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

pub(crate) fn byte_index_at_grapheme_col(
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

pub(crate) fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

pub(crate) fn submitted_command(
    buffer: &str,
    escape_character: char,
) -> String {
    let mut lines = buffer
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    append_line_continuations(&mut lines, escape_character);
    lines.join("\n")
}

fn append_line_continuations(
    lines: &mut [String],
    escape_character: char,
) {
    let continuation_count = lines.len().saturating_sub(1);
    for line in lines.iter_mut().take(continuation_count) {
        if line_ends_with_continuation(line, escape_character) {
            continue;
        }
        line.push(' ');
        line.push(escape_character);
    }
}

fn line_ends_with_continuation(
    line: &str,
    escape_character: char,
) -> bool {
    line.ends_with(escape_character)
}

pub(crate) fn push_history(
    editor: &mut CommandEditor,
    command: &str,
    max_history: usize,
) {
    history::push(&mut editor.history, command, max_history);
}
