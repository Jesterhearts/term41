#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySource {
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub command: String,
    pub source: HistorySource,
}

impl HistoryEntry {
    pub fn external(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            source: HistorySource::External,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct EditorHistory {
    local: Vec<String>,
    pos: Option<usize>,
    draft: String,
}

pub(super) fn clear(history: &mut EditorHistory) {
    history.pos = None;
    history.draft.clear();
}

pub(super) fn replace_edit_with_draft(
    history: &mut EditorHistory,
    current_buffer: &str,
) {
    if history.pos.is_some() {
        history.pos = None;
        history.draft = current_buffer.to_owned();
    }
}

pub(super) fn previous(
    history: &mut EditorHistory,
    current_buffer: &str,
    external: &[HistoryEntry],
) -> Option<String> {
    let commands = commands(history, external);
    if commands.is_empty() {
        return None;
    }
    let pos = match history.pos {
        Some(pos) if pos > 0 => pos - 1,
        Some(_) => return None,
        None => {
            history.draft = current_buffer.to_owned();
            commands.len() - 1
        }
    };
    history.pos = Some(pos);
    Some(commands[pos].clone())
}

pub(super) fn next(
    history: &mut EditorHistory,
    external: &[HistoryEntry],
) -> Option<String> {
    let pos = history.pos?;
    let commands = commands(history, external);
    if pos + 1 < commands.len() {
        history.pos = Some(pos + 1);
        return Some(commands[pos + 1].clone());
    }
    history.pos = None;
    let draft = history.draft.clone();
    history.draft.clear();
    Some(draft)
}

pub(super) fn push(
    history: &mut EditorHistory,
    command: &str,
    max_history: usize,
) {
    let trimmed = command.trim();
    if trimmed.is_empty() || history.local.last().is_some_and(|last| last == command) {
        return;
    }
    history.local.push(command.to_owned());
    let max_history = max_history.max(1);
    let excess = history.local.len().saturating_sub(max_history);
    if excess > 0 {
        history.local.drain(0..excess);
    }
}

pub(super) fn command_candidates(
    history: &EditorHistory,
    external: &[HistoryEntry],
) -> Vec<String> {
    let mut out = Vec::new();
    for command in commands(history, external).iter().rev() {
        push_unique(&mut out, command);
    }
    out
}

pub(super) fn command_word_candidates(
    history: &EditorHistory,
    external: &[HistoryEntry],
) -> Vec<String> {
    let mut out = Vec::new();
    for command in commands(history, external).iter().rev() {
        if let Some(first_word) = command.split_whitespace().next() {
            push_unique(&mut out, first_word);
        }
    }
    out
}

pub(super) fn word_candidates(
    history: &EditorHistory,
    external: &[HistoryEntry],
) -> Vec<String> {
    let mut out = Vec::new();
    for command in commands(history, external).iter().rev() {
        push_unique(&mut out, command);
    }
    out
}

fn commands(
    history: &EditorHistory,
    external: &[HistoryEntry],
) -> Vec<String> {
    let mut out = Vec::new();
    for entry in external {
        push_latest(&mut out, &entry.command);
    }
    for command in &history.local {
        push_latest(&mut out, command);
    }
    out
}

fn push_latest(
    out: &mut Vec<String>,
    command: &str,
) {
    if command.trim().is_empty() {
        return;
    }
    if let Some(idx) = out.iter().position(|existing| existing == command) {
        out.remove(idx);
    }
    out.push(command.to_owned());
}

fn push_unique(
    out: &mut Vec<String>,
    value: &str,
) {
    if !value.is_empty() && !out.iter().any(|existing| existing == value) {
        out.push(value.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_merges_external_and_local_with_local_as_newest() {
        let external = [
            HistoryEntry::external("cargo check"),
            HistoryEntry::external("cargo test"),
        ];
        let mut history = EditorHistory::default();
        push(&mut history, "cargo clippy", 20);

        assert_eq!(
            previous(&mut history, "draft", &external),
            Some("cargo clippy".to_owned())
        );
        assert_eq!(
            previous(&mut history, "cargo clippy", &external),
            Some("cargo test".to_owned())
        );
        assert_eq!(
            next(&mut history, &external),
            Some("cargo clippy".to_owned())
        );
        assert_eq!(next(&mut history, &external), Some("draft".to_owned()));
    }

    #[test]
    fn later_duplicate_commands_win() {
        let external = [
            HistoryEntry::external("cargo check"),
            HistoryEntry::external("cargo test"),
        ];
        let mut history = EditorHistory::default();
        push(&mut history, "cargo check", 20);

        assert_eq!(
            command_candidates(&history, &external),
            ["cargo check", "cargo test"]
        );
    }
}
