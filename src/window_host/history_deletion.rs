use nucleo_matcher::Config as NucleoConfig;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;

struct HistoryDeletionMatch {
    idx: usize,
    score: u32,
}

#[derive(Clone)]
pub(crate) struct HistoryDeletionEntry {
    pub(crate) command: String,
    pub(crate) cwd: String,
    pub(crate) key: history41::HistoryEntryKey,
}

#[derive(Clone)]
pub(crate) struct HistoryDeletionView {
    pub(crate) query: String,
    pub(crate) entries: Vec<HistoryDeletionEntry>,
    pub(crate) displayed: Vec<usize>,
    pub(crate) scroll: usize,
}

pub(crate) fn history_deletion_view(
    entries: Vec<history41::StoredHistoryEntry>
) -> HistoryDeletionView {
    let entries = entries
        .into_iter()
        .map(|entry| HistoryDeletionEntry {
            command: entry.command,
            cwd: entry.cwd.to_string_lossy().into_owned(),
            key: entry.key,
        })
        .collect();
    let mut view = HistoryDeletionView {
        query: String::new(),
        entries,
        displayed: Vec::new(),
        scroll: 0,
    };
    set_history_deletion_query(&mut view, String::new());
    view
}

pub(crate) fn set_history_deletion_query(
    view: &mut HistoryDeletionView,
    query: String,
) {
    view.query = query;
    view.displayed = history_deletion_displayed_entries(&view.query, &view.entries);
    view.scroll = 0;
}

fn history_deletion_displayed_entries(
    query: &str,
    entries: &[HistoryDeletionEntry],
) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    fuzzy_history_deletion_entries(query, entries)
}

fn fuzzy_history_deletion_entries(
    query: &str,
    entries: &[HistoryDeletionEntry],
) -> Vec<usize> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = history_deletion_matcher();
    let mut utf32_buf = Vec::new();
    let mut matches: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let text = history_deletion_match_text(entry);
            pattern
                .score(Utf32Str::new(text.as_str(), &mut utf32_buf), &mut matcher)
                .map(|score| HistoryDeletionMatch { idx, score })
        })
        .collect();
    matches.sort_by(|left, right| history_deletion_match_order(left, right, entries));
    matches.into_iter().map(|matched| matched.idx).collect()
}

fn history_deletion_matcher() -> Matcher {
    let mut config = NucleoConfig::DEFAULT;
    config.prefer_prefix = true;
    Matcher::new(config)
}

fn history_deletion_match_text(entry: &HistoryDeletionEntry) -> String {
    format!("{} {}", entry.command, entry.cwd)
}

fn history_deletion_match_order(
    left: &HistoryDeletionMatch,
    right: &HistoryDeletionMatch,
    entries: &[HistoryDeletionEntry],
) -> std::cmp::Ordering {
    right.score.cmp(&left.score).then_with(|| {
        entries[left.idx]
            .command
            .to_ascii_lowercase()
            .cmp(&entries[right.idx].command.to_ascii_lowercase())
    })
}

pub(crate) fn scroll_history_deletion_view(
    view: &mut HistoryDeletionView,
    delta: isize,
    visible_rows: usize,
) {
    let max_scroll = view.displayed.len().saturating_sub(visible_rows.max(1));
    if delta < 0 {
        view.scroll = view.scroll.saturating_sub(delta.unsigned_abs());
    } else {
        view.scroll = view.scroll.saturating_add(delta as usize).min(max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn history_deletion_empty_query_displays_entries_without_deleting_by_selection() {
        let view = history_deletion_test_view();

        assert!(view.query.is_empty());
        assert_eq!(view.displayed.len(), 2);
    }

    #[test]
    fn history_deletion_query_fuzzy_filters_entries() {
        let mut view = history_deletion_test_view();

        set_history_deletion_query(&mut view, "bld".to_owned());

        let labels: Vec<_> = view
            .displayed
            .iter()
            .map(|idx| view.entries[*idx].command.as_str())
            .collect();
        assert_eq!(labels, ["cargo build"]);
    }

    #[test]
    fn history_deletion_scroll_clamps_to_visible_entries() {
        let mut view = history_deletion_test_view();

        scroll_history_deletion_view(&mut view, 100, 1);
        assert_eq!(view.scroll, 1);
        scroll_history_deletion_view(&mut view, -100, 1);
        assert_eq!(view.scroll, 0);
    }

    fn history_deletion_test_view() -> HistoryDeletionView {
        let root = std::env::temp_dir().join(format!(
            "term41-history-deletion-view-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        let store = history41::open(&root.join("history.sqlite3")).unwrap();
        for (idx, command) in ["cargo test", "cargo build"].into_iter().enumerate() {
            history41::store_command(
                &store,
                history41::StoreCommandRequest {
                    command: command.to_owned(),
                    cwd: root.join("a"),
                    submitted_at: std::time::UNIX_EPOCH + Duration::from_secs(idx as u64 + 1),
                    retention: history41::HistoryRetention::default(),
                    ignore_leading_space: true,
                },
            )
            .unwrap();
        }
        let entries = history41::all_commands(&store).unwrap();
        let _ = std::fs::remove_dir_all(root);
        history_deletion_view(entries)
    }
}
