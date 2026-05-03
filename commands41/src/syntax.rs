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

pub(crate) fn is_operator_char(ch: char) -> bool {
    matches!(ch, '|' | '&' | ';' | '<' | '>' | '(' | ')')
}

pub(crate) fn is_command_separator_char(ch: char) -> bool {
    matches!(ch, '|' | '&' | ';' | '(')
}

fn span(
    start: usize,
    end: usize,
    kind: HighlightKind,
) -> HighlightSpan {
    HighlightSpan { start, end, kind }
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
