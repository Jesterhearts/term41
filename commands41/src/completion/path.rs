use std::fs;
use std::path::Path;
use std::path::PathBuf;

use super::CompletionCandidate;
use super::CompletionMatches;
use crate::CommandEditor;
use crate::EditorSettings;
use crate::editing::begin_text_edit;
use crate::editing::replace_selection_or_insert;
use crate::shell_words::current_completion_word;
use crate::shell_words::is_command_completion_word;
use crate::shell_words::is_path_separator;

#[derive(Debug)]
struct PathCompletionRequest {
    directory: PathBuf,
    entry_prefix: String,
    completed_prefix: String,
    quote: Option<char>,
    escape_character: char,
}

#[derive(Debug, Clone)]
pub(crate) struct PathCompletionCandidate {
    pub(crate) completed_word: String,
    pub(crate) is_dir: bool,
}

#[derive(Debug)]
struct PathCompletionWord {
    start: usize,
    raw: String,
    decoded: String,
    quote: Option<char>,
    escape_character: char,
}

#[derive(Debug, Clone)]
pub(crate) struct PathCompletionCycle {
    pub(crate) base: String,
    pub(crate) cursor: usize,
    word: String,
    pub(crate) candidates: Vec<PathCompletionCandidate>,
    pub(crate) index: usize,
}

impl PathCompletionCycle {
    fn current_suffix(&self) -> Option<String> {
        let candidate = self.candidates.get(self.index)?;
        Some(candidate.completed_word[self.word.len()..].to_owned())
    }
}

pub(crate) fn path_cycle_suffix(editor: &CommandEditor) -> Option<String> {
    let cycle = editor.path_cycle.as_ref()?;
    if cycle.cursor != editor.cursor || cycle.base != editor.buffer {
        return None;
    }
    cycle.current_suffix()
}

pub(super) fn path_completion_suffix(
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

pub(super) fn path_completion_matches(
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
    let word = current_path_completion_word(buffer, cursor, settings.escape_character)?;
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

fn current_path_completion_word(
    buffer: &str,
    cursor: usize,
    escape_character: char,
) -> Option<PathCompletionWord> {
    let (start, raw) = current_completion_word(buffer, cursor, escape_character)?;
    if let Some(quote) = raw.chars().next().filter(|ch| *ch == '\'' || *ch == '"') {
        let quote_len = quote.len_utf8();
        let inner = &raw[quote_len..];
        return Some(PathCompletionWord {
            start: start + quote_len,
            raw: inner.to_owned(),
            decoded: inner.to_owned(),
            quote: Some(quote),
            escape_character,
        });
    }

    Some(PathCompletionWord {
        start,
        raw: raw.to_owned(),
        decoded: decode_unquoted_path_word(raw, escape_character),
        quote: None,
        escape_character,
    })
}

fn decode_unquoted_path_word(
    raw: &str,
    escape_character: char,
) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != escape_character {
            out.push(ch);
            continue;
        }

        let Some(&next) = chars.peek() else {
            out.push(escape_character);
            break;
        };
        if escape_character_escapes_path_char(escape_character, next) {
            out.push(chars.next().expect("peeked char exists"));
        } else {
            out.push(escape_character);
        }
    }
    out
}

fn escape_character_escapes_path_char(
    escape_character: char,
    ch: char,
) -> bool {
    if escape_character != '\\' {
        return true;
    }
    ch.is_whitespace()
        || matches!(
            ch,
            '\\' | '\'' | '"' | '$' | '`' | '!' | '&' | ';' | '|' | '<' | '>' | '(' | ')'
        )
}

pub(super) fn cycle_path_completion(
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
        super::clear_completion_state(editor);
        return false;
    };
    let candidates = candidates
        .into_iter()
        .filter(|candidate| {
            candidate.completed_word != word && candidate.completed_word.starts_with(&word)
        })
        .collect::<Vec<_>>();

    if candidates.len() <= 1 {
        super::clear_completion_state(editor);
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

pub(super) fn accept_path_cycle(editor: &mut CommandEditor) -> bool {
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

pub(super) fn accept_visible_path_completion(
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
        escape_character: word.escape_character,
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
            encode_path_completion_text(&suffix, request.quote, request.escape_character)
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
    escape_character: char,
) -> String {
    match quote {
        Some('"') => escape_double_quoted_path_text(text, escape_character),
        Some('\'') => text.to_owned(),
        None => escape_unquoted_path_text(text, escape_character),
        Some(_) => text.to_owned(),
    }
}

fn escape_unquoted_path_text(
    text: &str,
    escape_character: char,
) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if unquoted_path_char_needs_escape(ch, escape_character) {
            out.push(escape_character);
        }
        out.push(ch);
    }
    out
}

fn unquoted_path_char_needs_escape(
    ch: char,
    escape_character: char,
) -> bool {
    ch.is_whitespace()
        || ch == escape_character
        || matches!(
            ch,
            '\'' | '"' | '$' | '!' | '&' | ';' | '|' | '<' | '>' | '(' | ')'
        )
}

fn escape_double_quoted_path_text(
    text: &str,
    escape_character: char,
) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if double_quoted_path_char_needs_escape(ch, escape_character) {
            out.push(escape_character);
        }
        out.push(ch);
    }
    out
}

fn double_quoted_path_char_needs_escape(
    ch: char,
    escape_character: char,
) -> bool {
    ch == '"' || ch == '$' || ch == escape_character || (escape_character == '\\' && ch == '`')
}
