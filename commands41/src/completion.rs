pub(crate) mod path;

use nucleo_matcher::Config as NucleoConfig;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;

use crate::CommandEditor;
use crate::EditOutcome;
use crate::EditorSettings;
use crate::editing::begin_text_edit;
use crate::editing::replace_selection_or_insert;
use crate::history;
use crate::shell_words::current_completion_word;
use crate::shell_words::is_command_completion_word;
use crate::shell_words::is_path_separator;
use crate::shell_words::shell_segment_words_before;

const MAX_COMPLETION_CANDIDATES: usize = 5;

pub(crate) fn completion_preview(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> Option<String> {
    if let Some(selection) = valid_completion_selection(editor) {
        selection.current_suffix()
    } else {
        path::path_cycle_suffix(editor).or_else(|| completion_suffix(editor, settings))
    }
}

pub(crate) fn completion_candidate_view(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> (Vec<String>, usize) {
    if let Some(selection) = valid_completion_selection(editor) {
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

pub(crate) fn clear_completion_state(editor: &mut CommandEditor) {
    editor.path_cycle = None;
    editor.completion_selection = None;
}

pub(crate) fn complete_current_prefix(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> EditOutcome {
    if accept_selected_completion_step(editor) {
        return EditOutcome::Updated;
    }

    if path::cycle_path_completion(editor, settings) {
        crate::editing::replace_history_edit_with_draft(editor);
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

fn completion_suffix(
    editor: &CommandEditor,
    settings: &EditorSettings,
) -> Option<String> {
    let buffer = &editor.buffer;
    let cursor = editor.cursor;
    if let Some(suffix) = history_completion_suffix(editor, settings) {
        return Some(suffix);
    }

    if let Some(suffix) = path::path_completion_suffix(buffer, cursor, settings) {
        return Some(suffix);
    }

    let (word_start, prefix) = current_completion_word(buffer, cursor, settings.escape_character)?;
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

    if let Some(matches) = path::path_completion_matches(buffer, cursor, settings)
        && !matches.candidates.is_empty()
    {
        return Some(matches);
    }

    let (word_start, prefix) = current_completion_word(buffer, cursor, settings.escape_character)?;
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
    let words = shell_segment_words_before(buffer, word_start, settings.escape_character);
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
pub(crate) enum CompletionDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionSelection {
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

fn valid_completion_selection(editor: &CommandEditor) -> Option<&CompletionSelection> {
    let selection = editor.completion_selection.as_ref()?;
    if selection.cursor == editor.cursor && selection.base == editor.buffer {
        Some(selection)
    } else {
        None
    }
}

pub(crate) fn cycle_completion_selection(
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

pub(crate) fn accept_selected_completion(editor: &mut CommandEditor) -> bool {
    let Some(selection) = editor.completion_selection.take() else {
        return false;
    };
    if selection.cursor != editor.cursor || selection.base != editor.buffer {
        return false;
    }
    let Some(text) = selection.current_text() else {
        return false;
    };
    let text = text.to_owned();
    begin_text_edit(editor);
    replace_completion_selection(editor, &selection, &text);
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
    let text = text.to_owned();
    begin_text_edit(editor);
    replace_completion_selection(editor, &selection, &text);
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

pub(crate) fn accept_path_cycle(editor: &mut CommandEditor) -> bool {
    path::accept_path_cycle(editor)
}

pub(crate) fn accept_visible_history_completion(
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

pub(crate) fn accept_visible_path_completion(
    editor: &mut CommandEditor,
    settings: &EditorSettings,
) -> bool {
    path::accept_visible_path_completion(editor, settings)
}
