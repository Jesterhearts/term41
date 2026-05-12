use crate::syntax::is_command_separator_char;
use crate::syntax::is_operator_char;

pub(crate) fn current_completion_word(
    buffer: &str,
    cursor: usize,
    escape_character: char,
) -> Option<(usize, &str)> {
    if cursor > buffer.len() || !buffer.is_char_boundary(cursor) {
        return None;
    }
    let scan = scan_completion_word(buffer, cursor, escape_character);
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
    escape_character: char,
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
                if ch == escape_character {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None => {
                if ch == escape_character {
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

pub(crate) fn shell_segment_words_before(
    buffer: &str,
    word_start: usize,
    escape_character: char,
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
                if ch == escape_character {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some(_) => {}
            None => {
                if ch == escape_character {
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

pub(crate) fn is_path_separator(ch: char) -> bool {
    matches!(ch, '/' | '\\')
}

pub(crate) fn is_command_completion_word(
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
