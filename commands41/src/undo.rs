use crate::CommandEditor;
use crate::EditOutcome;
use crate::EditorSelection;
use crate::completion;
use crate::editing;

const MAX_UNDO_STEPS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    buffer: String,
    cursor: usize,
    selection: Option<EditorSelection>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UndoHistory {
    undo: Vec<EditorSnapshot>,
    redo: Vec<EditorSnapshot>,
}

pub(crate) fn push_undo_snapshot(editor: &mut CommandEditor) {
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

pub(crate) fn undo_text_edit(editor: &mut CommandEditor) -> EditOutcome {
    let Some(snapshot) = editor.undo.undo.pop() else {
        return EditOutcome::Ignored;
    };
    editor.undo.redo.push(editor_snapshot(editor));
    trim_snapshot_stack(&mut editor.undo.redo);
    restore_editor_snapshot(editor, snapshot);
    EditOutcome::Updated
}

pub(crate) fn redo_text_edit(editor: &mut CommandEditor) -> EditOutcome {
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
    completion::clear_completion_state(editor);
    editor.buffer = snapshot.buffer;
    editor.cursor = snapshot.cursor;
    editor.selection = snapshot.selection;
    editor.vim_pending = None;
    editing::replace_history_edit_with_draft(editor);
}
