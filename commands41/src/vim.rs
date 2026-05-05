use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimKey {
    Text(String),
    Escape,
    Enter,
    ShiftEnter,
    Backspace,
    Delete,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    Tab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimOperator {
    Delete,
    Yank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimPending {
    Operator(VimOperator),
    OperatorG(VimOperator),
    G,
}

pub(super) fn apply_vim_key(
    editor: &mut CommandEditor,
    key: VimKey,
    settings: &EditorSettings,
) -> EditOutcome {
    match editor.vim_mode {
        VimMode::Insert => apply_vim_insert_key(editor, key, settings),
        VimMode::Normal => apply_vim_normal_key(editor, key, settings),
    }
}

fn apply_vim_insert_key(
    editor: &mut CommandEditor,
    key: VimKey,
    settings: &EditorSettings,
) -> EditOutcome {
    editor.vim_pending = None;
    match key {
        VimKey::Text(text) => apply_input(editor, EditorInput::Insert(text), settings),
        VimKey::Escape => {
            editor.vim_mode = VimMode::Normal;
            normalize_vim_normal_cursor(editor);
            EditOutcome::Updated
        }
        VimKey::Enter => apply_input(editor, EditorInput::Enter, settings),
        VimKey::ShiftEnter => apply_input(editor, EditorInput::Insert("\n".to_owned()), settings),
        VimKey::Backspace => apply_input(editor, EditorInput::Backspace, settings),
        VimKey::Delete => apply_input(editor, EditorInput::Delete, settings),
        VimKey::ArrowLeft => apply_input(editor, EditorInput::MoveLeft, settings),
        VimKey::ArrowRight => apply_input(editor, EditorInput::MoveRight, settings),
        VimKey::ArrowUp => apply_input(editor, EditorInput::HistoryPrevious, settings),
        VimKey::ArrowDown => apply_input(editor, EditorInput::HistoryNext, settings),
        VimKey::Home => apply_input(editor, EditorInput::MoveHome, settings),
        VimKey::End => apply_input(editor, EditorInput::MoveEnd, settings),
        VimKey::Tab => apply_input(editor, EditorInput::Complete, settings),
    }
}

fn apply_vim_normal_key(
    editor: &mut CommandEditor,
    key: VimKey,
    settings: &EditorSettings,
) -> EditOutcome {
    match key {
        VimKey::Text(text) => apply_vim_normal_text(editor, &text),
        VimKey::Escape => {
            editor.vim_pending = None;
            EditOutcome::Updated
        }
        VimKey::Enter => {
            editor.vim_pending = None;
            let command = submitted_command(&editor.buffer, settings.escape_character);
            push_history(editor, &command, settings.max_history);
            editor.clear();
            EditOutcome::Submitted(command)
        }
        VimKey::ArrowLeft => apply_vim_motion(editor, VimMotion::Left),
        VimKey::ArrowRight => apply_vim_motion(editor, VimMotion::Right),
        VimKey::ArrowUp => apply_vim_motion(editor, VimMotion::LineUp),
        VimKey::ArrowDown => apply_vim_motion(editor, VimMotion::LineDown),
        VimKey::Home => apply_vim_motion(editor, VimMotion::Start),
        VimKey::End => apply_vim_motion(editor, VimMotion::End),
        VimKey::ShiftEnter | VimKey::Backspace | VimKey::Delete | VimKey::Tab => {
            editor.vim_pending = None;
            EditOutcome::Ignored
        }
    }
}

fn apply_vim_normal_text(
    editor: &mut CommandEditor,
    text: &str,
) -> EditOutcome {
    let mut chars = text.chars();
    let Some(ch) = chars.next() else {
        return EditOutcome::Ignored;
    };
    if chars.next().is_some() {
        editor.vim_pending = None;
        return EditOutcome::Ignored;
    }

    if let Some(pending) = editor.vim_pending.take() {
        return apply_vim_pending(editor, pending, ch);
    }

    match ch {
        'i' => {
            editor.vim_mode = VimMode::Insert;
            EditOutcome::Updated
        }
        'a' => {
            move_vim_cursor(editor, vim_motion_target(editor, VimMotion::Right));
            editor.vim_mode = VimMode::Insert;
            EditOutcome::Updated
        }
        'A' => {
            move_vim_cursor(editor, vim_motion_target(editor, VimMotion::LineEnd));
            editor.vim_mode = VimMode::Insert;
            EditOutcome::Updated
        }
        'o' => vim_open_line(editor, OpenLinePlacement::Below),
        'O' => vim_open_line(editor, OpenLinePlacement::Above),
        'h' => apply_vim_motion(editor, VimMotion::Left),
        'j' => apply_vim_motion(editor, VimMotion::LineDown),
        'k' => apply_vim_motion(editor, VimMotion::LineUp),
        'l' => apply_vim_motion(editor, VimMotion::Right),
        '{' => apply_vim_motion(editor, VimMotion::ParagraphPrevious),
        '}' => apply_vim_motion(editor, VimMotion::ParagraphNext),
        'w' => apply_vim_motion(editor, VimMotion::WordStart),
        'b' => apply_vim_motion(editor, VimMotion::WordBack),
        'e' => apply_vim_motion(editor, VimMotion::WordEnd),
        'W' => apply_vim_motion(editor, VimMotion::WhitespaceWordStart),
        'B' => apply_vim_motion(editor, VimMotion::WhitespaceWordBack),
        'E' => apply_vim_motion(editor, VimMotion::WhitespaceWordEnd),
        '0' => apply_vim_motion(editor, VimMotion::LineStart),
        '^' => apply_vim_motion(editor, VimMotion::LineFirstNonBlank),
        '$' => apply_vim_motion(editor, VimMotion::LineEnd),
        'u' => undo_text_edit(editor),
        'x' => vim_delete_under_cursor(editor),
        'D' => vim_delete_current_line(editor),
        'd' => {
            editor.vim_pending = Some(VimPending::Operator(VimOperator::Delete));
            EditOutcome::Updated
        }
        'y' => {
            editor.vim_pending = Some(VimPending::Operator(VimOperator::Yank));
            EditOutcome::Updated
        }
        'p' => vim_paste(editor, PastePlacement::After),
        'P' => vim_paste(editor, PastePlacement::Before),
        'g' => {
            editor.vim_pending = Some(VimPending::G);
            EditOutcome::Updated
        }
        'G' => apply_vim_motion(editor, VimMotion::End),
        _ => EditOutcome::Ignored,
    }
}

fn apply_vim_pending(
    editor: &mut CommandEditor,
    pending: VimPending,
    ch: char,
) -> EditOutcome {
    match pending {
        VimPending::G => {
            if ch == 'g' {
                apply_vim_motion(editor, VimMotion::Start)
            } else {
                EditOutcome::Ignored
            }
        }
        VimPending::Operator(operator) => {
            if ch == 'd' && operator == VimOperator::Delete {
                return vim_delete_current_line(editor);
            }
            if ch == 'y' && operator == VimOperator::Yank {
                return vim_yank_current_line(editor);
            }
            if ch == 'g' {
                editor.vim_pending = Some(VimPending::OperatorG(operator));
                return EditOutcome::Updated;
            }
            let Some(motion) = vim_motion_from_char(ch) else {
                return EditOutcome::Ignored;
            };
            apply_vim_operator_motion(editor, operator, motion)
        }
        VimPending::OperatorG(operator) => {
            if ch == 'g' {
                apply_vim_operator_motion(editor, operator, VimMotion::Start)
            } else {
                EditOutcome::Ignored
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VimMotion {
    Left,
    Right,
    LineUp,
    LineDown,
    ParagraphPrevious,
    ParagraphNext,
    WordStart,
    WordBack,
    WordEnd,
    WhitespaceWordStart,
    WhitespaceWordBack,
    WhitespaceWordEnd,
    LineStart,
    LineFirstNonBlank,
    LineEnd,
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PastePlacement {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenLinePlacement {
    Above,
    Below,
}

fn vim_motion_from_char(ch: char) -> Option<VimMotion> {
    match ch {
        'h' => Some(VimMotion::Left),
        'j' => Some(VimMotion::LineDown),
        'k' => Some(VimMotion::LineUp),
        'l' => Some(VimMotion::Right),
        '{' => Some(VimMotion::ParagraphPrevious),
        '}' => Some(VimMotion::ParagraphNext),
        'w' => Some(VimMotion::WordStart),
        'b' => Some(VimMotion::WordBack),
        'e' => Some(VimMotion::WordEnd),
        'W' => Some(VimMotion::WhitespaceWordStart),
        'B' => Some(VimMotion::WhitespaceWordBack),
        'E' => Some(VimMotion::WhitespaceWordEnd),
        '0' => Some(VimMotion::LineStart),
        '^' => Some(VimMotion::LineFirstNonBlank),
        '$' => Some(VimMotion::LineEnd),
        'G' => Some(VimMotion::End),
        _ => None,
    }
}

fn apply_vim_motion(
    editor: &mut CommandEditor,
    motion: VimMotion,
) -> EditOutcome {
    let target = vim_motion_target(editor, motion);
    move_vim_cursor(editor, target)
}

fn apply_vim_operator_motion(
    editor: &mut CommandEditor,
    operator: VimOperator,
    motion: VimMotion,
) -> EditOutcome {
    let Some(target) = vim_motion_target(editor, motion) else {
        return EditOutcome::Ignored;
    };
    let (start, end) = ordered_range(editor.cursor, target);
    if start == end {
        return EditOutcome::Ignored;
    }
    editor.kill_buffer = editor.buffer[start..end].to_owned();
    match operator {
        VimOperator::Yank => EditOutcome::Updated,
        VimOperator::Delete => {
            begin_text_edit(editor);
            editor.buffer.drain(start..end);
            editor.cursor = start.min(editor.buffer.len());
            normalize_vim_normal_cursor(editor);
            EditOutcome::Updated
        }
    }
}

fn move_vim_cursor(
    editor: &mut CommandEditor,
    target: Option<usize>,
) -> EditOutcome {
    let Some(target) = target else {
        return EditOutcome::Ignored;
    };
    if target == editor.cursor {
        return EditOutcome::Ignored;
    }
    clear_completion_state(editor);
    editor.cursor = target;
    editor.selection = None;
    EditOutcome::Updated
}

fn vim_motion_target(
    editor: &CommandEditor,
    motion: VimMotion,
) -> Option<usize> {
    match motion {
        VimMotion::Left => previous_grapheme_boundary(&editor.buffer, editor.cursor),
        VimMotion::Right => next_grapheme_boundary(&editor.buffer, editor.cursor),
        VimMotion::LineUp => line_motion_target(editor, LineDirection::Previous),
        VimMotion::LineDown => line_motion_target(editor, LineDirection::Next),
        VimMotion::ParagraphPrevious => previous_paragraph_start(&editor.buffer, editor.cursor),
        VimMotion::ParagraphNext => next_paragraph_start(&editor.buffer, editor.cursor),
        VimMotion::WordStart => next_vim_word_start(&editor.buffer, editor.cursor, false),
        VimMotion::WordBack => previous_vim_word_start(&editor.buffer, editor.cursor, false),
        VimMotion::WordEnd => next_vim_word_end(&editor.buffer, editor.cursor, false),
        VimMotion::WhitespaceWordStart => next_vim_word_start(&editor.buffer, editor.cursor, true),
        VimMotion::WhitespaceWordBack => {
            previous_vim_word_start(&editor.buffer, editor.cursor, true)
        }
        VimMotion::WhitespaceWordEnd => next_vim_word_end(&editor.buffer, editor.cursor, true),
        VimMotion::LineStart => Some(current_line_range(&editor.buffer, editor.cursor).0),
        VimMotion::LineFirstNonBlank => {
            Some(current_line_first_nonblank(&editor.buffer, editor.cursor))
        }
        VimMotion::LineEnd => Some(current_line_range(&editor.buffer, editor.cursor).1),
        VimMotion::Start => Some(0),
        VimMotion::End => Some(editor.buffer.len()),
    }
}

fn line_motion_target(
    editor: &CommandEditor,
    direction: LineDirection,
) -> Option<usize> {
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
    Some(byte_index_at_grapheme_col(
        &editor.buffer,
        target_start,
        target_end,
        cursor_col,
    ))
}

fn vim_delete_current_line(editor: &mut CommandEditor) -> EditOutcome {
    let (start, end) = current_line_delete_range(&editor.buffer, editor.cursor);
    vim_delete_range(editor, start, end)
}

fn vim_delete_under_cursor(editor: &mut CommandEditor) -> EditOutcome {
    let Some(end) = next_grapheme_boundary(&editor.buffer, editor.cursor) else {
        return EditOutcome::Ignored;
    };
    vim_delete_range(editor, editor.cursor, end)
}

fn vim_yank_current_line(editor: &mut CommandEditor) -> EditOutcome {
    let (start, end) = current_line_range(&editor.buffer, editor.cursor);
    if start == end {
        return EditOutcome::Ignored;
    }
    editor.kill_buffer = editor.buffer[start..end].to_owned();
    EditOutcome::Updated
}

fn vim_open_line(
    editor: &mut CommandEditor,
    placement: OpenLinePlacement,
) -> EditOutcome {
    let (line_start, line_end) = current_line_range(&editor.buffer, editor.cursor);
    let insert_at = match placement {
        OpenLinePlacement::Above => line_start,
        OpenLinePlacement::Below if line_end < editor.buffer.len() => line_end + '\n'.len_utf8(),
        OpenLinePlacement::Below => line_end,
    };
    let cursor = match placement {
        OpenLinePlacement::Above => insert_at,
        OpenLinePlacement::Below if line_end < editor.buffer.len() => insert_at,
        OpenLinePlacement::Below => insert_at + '\n'.len_utf8(),
    };

    begin_text_edit(editor);
    editor.selection = None;
    editor.buffer.insert(insert_at, '\n');
    editor.cursor = cursor;
    editor.vim_mode = VimMode::Insert;
    EditOutcome::Updated
}

fn vim_delete_range(
    editor: &mut CommandEditor,
    start: usize,
    end: usize,
) -> EditOutcome {
    if start == end {
        return EditOutcome::Ignored;
    }
    begin_text_edit(editor);
    editor.kill_buffer = editor.buffer[start..end].to_owned();
    editor.buffer.drain(start..end);
    editor.cursor = start.min(editor.buffer.len());
    normalize_vim_normal_cursor(editor);
    EditOutcome::Updated
}

fn vim_paste(
    editor: &mut CommandEditor,
    placement: PastePlacement,
) -> EditOutcome {
    if editor.kill_buffer.is_empty() {
        return EditOutcome::Ignored;
    }
    let text = editor.kill_buffer.clone();
    let cursor = match placement {
        PastePlacement::Before => editor.cursor,
        PastePlacement::After => {
            next_grapheme_boundary(&editor.buffer, editor.cursor).unwrap_or(editor.buffer.len())
        }
    };
    begin_text_edit(editor);
    editor.cursor = cursor;
    replace_selection_or_insert(editor, &text);
    normalize_vim_normal_cursor(editor);
    EditOutcome::Updated
}

fn normalize_vim_normal_cursor(editor: &mut CommandEditor) {
    if editor.buffer.is_empty() {
        editor.cursor = 0;
    } else if editor.cursor >= editor.buffer.len() {
        editor.cursor =
            previous_grapheme_boundary(&editor.buffer, editor.buffer.len()).unwrap_or(0);
    }
}

fn ordered_range(
    first: usize,
    second: usize,
) -> (usize, usize) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn current_line_range(
    text: &str,
    cursor: usize,
) -> (usize, usize) {
    let lines = line_ranges(text);
    lines[line_index_at_cursor(&lines, cursor)]
}

fn current_line_first_nonblank(
    text: &str,
    cursor: usize,
) -> usize {
    let (start, end) = current_line_range(text, cursor);
    text[start..end]
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(idx, _)| start + idx)
        .unwrap_or(start)
}

fn current_line_delete_range(
    text: &str,
    cursor: usize,
) -> (usize, usize) {
    let lines = line_ranges(text);
    let line_idx = line_index_at_cursor(&lines, cursor);
    let (mut start, line_end) = lines[line_idx];
    let mut end = line_end;
    if end < text.len() {
        end += '\n'.len_utf8();
    } else if start > 0 {
        start -= '\n'.len_utf8();
    }
    (start, end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VimWordClass {
    Word,
    Punctuation,
}

fn next_vim_word_start(
    text: &str,
    cursor: usize,
    whitespace_delimited: bool,
) -> Option<usize> {
    vim_word_spans(text, whitespace_delimited)
        .into_iter()
        .map(|(start, _)| start)
        .find(|start| *start > cursor)
}

fn next_vim_word_end(
    text: &str,
    cursor: usize,
    whitespace_delimited: bool,
) -> Option<usize> {
    vim_word_spans(text, whitespace_delimited)
        .into_iter()
        .map(|(_, end)| end)
        .find(|end| *end > cursor)
}

fn previous_vim_word_start(
    text: &str,
    cursor: usize,
    whitespace_delimited: bool,
) -> Option<usize> {
    vim_word_spans(text, whitespace_delimited)
        .into_iter()
        .map(|(start, _)| start)
        .take_while(|start| *start < cursor)
        .last()
}

fn vim_word_spans(
    text: &str,
    whitespace_delimited: bool,
) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut current: Option<(usize, usize, VimWordClass)> = None;

    for (idx, grapheme) in text.grapheme_indices(true) {
        let end = idx + grapheme.len();
        let Some(class) = vim_word_class(grapheme, whitespace_delimited) else {
            if let Some((start, span_end, _)) = current.take() {
                spans.push((start, span_end));
            }
            continue;
        };

        match current {
            Some((start, _, existing)) if existing == class => {
                current = Some((start, end, existing));
            }
            Some((start, span_end, _)) => {
                spans.push((start, span_end));
                current = Some((idx, end, class));
            }
            None => current = Some((idx, end, class)),
        }
    }

    if let Some((start, end, _)) = current {
        spans.push((start, end));
    }
    spans
}

fn vim_word_class(
    grapheme: &str,
    whitespace_delimited: bool,
) -> Option<VimWordClass> {
    let ch = grapheme.chars().next()?;
    if ch.is_whitespace() {
        return None;
    }
    if whitespace_delimited || ch.is_alphanumeric() || ch == '_' {
        Some(VimWordClass::Word)
    } else {
        Some(VimWordClass::Punctuation)
    }
}

fn previous_paragraph_start(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    paragraph_starts(text)
        .into_iter()
        .take_while(|start| *start < cursor)
        .last()
        .or(Some(0))
}

fn next_paragraph_start(
    text: &str,
    cursor: usize,
) -> Option<usize> {
    paragraph_starts(text)
        .into_iter()
        .find(|start| *start > cursor)
        .or(Some(text.len()))
}

fn paragraph_starts(text: &str) -> Vec<usize> {
    let lines = line_ranges(text);
    let mut starts = vec![0];
    for pair in lines.windows(2) {
        let (line_start, line_end) = pair[0];
        if text[line_start..line_end].trim().is_empty() {
            starts.push(pair[1].0);
        }
    }
    starts.dedup();
    starts
}
