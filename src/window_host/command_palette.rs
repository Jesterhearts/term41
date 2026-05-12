use config41::keybindings::Action;
use nucleo_matcher::Config as NucleoConfig;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;

#[derive(Clone)]
pub(crate) struct CommandPaletteItem {
    pub(crate) label: String,
    pub(crate) action: Action,
    argument: Option<CommandPaletteArgumentKind>,
}

struct CommandPaletteMatch {
    item: CommandPaletteItem,
    score: u32,
}

#[derive(Clone)]
pub(crate) struct CommandPaletteView {
    pub(crate) query: String,
    pub(crate) items: Vec<CommandPaletteItem>,
    pub(crate) selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandPaletteArgumentKind {
    WorkingDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandPaletteArgument {
    WorkingDirectory(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandPaletteInvocation {
    pub(crate) action: Action,
    pub(crate) argument: Option<CommandPaletteArgument>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommandPaletteAccept {
    Ready(CommandPaletteInvocation),
    NeedsArgument,
}

#[derive(Debug, PartialEq, Eq)]
struct CommandPaletteInput<'a> {
    command: &'a str,
    argument: Option<&'a str>,
}

pub(crate) fn command_palette_view() -> CommandPaletteView {
    CommandPaletteView {
        query: String::new(),
        items: command_palette_items(""),
        selected: 0,
    }
}

pub(crate) fn command_palette_items(query: &str) -> Vec<CommandPaletteItem> {
    let input = parse_command_palette_input(query);
    let items = Action::command_palette_actions()
        .iter()
        .flat_map(|action| command_palette_items_for_action(*action))
        .filter(|item| input.argument.is_none() || item.argument.is_some());
    if input.command.is_empty() {
        return sorted_command_palette_items(items);
    }

    fuzzy_command_palette_items(input.command, items)
}

fn parse_command_palette_input(query: &str) -> CommandPaletteInput<'_> {
    if let Some((command, argument)) = query.split_once(':') {
        CommandPaletteInput {
            command: command.trim_end(),
            argument: Some(argument.trim_start()),
        }
    } else {
        CommandPaletteInput {
            command: query,
            argument: None,
        }
    }
}

fn command_palette_items_for_action(action: Action) -> Vec<CommandPaletteItem> {
    let mut items = vec![command_palette_item(action, action.palette_label(), None)];
    if action == Action::OpenNewWindow {
        items.push(command_palette_item(
            action,
            "Open new window in dir:",
            Some(CommandPaletteArgumentKind::WorkingDirectory),
        ));
    }
    if action == Action::NewTab {
        items.push(command_palette_item(
            action,
            "Open new tab in dir:",
            Some(CommandPaletteArgumentKind::WorkingDirectory),
        ));
    }
    items
}

fn command_palette_item(
    action: Action,
    label: &str,
    argument: Option<CommandPaletteArgumentKind>,
) -> CommandPaletteItem {
    CommandPaletteItem {
        label: label.to_owned(),
        action,
        argument,
    }
}

fn sorted_command_palette_items(
    items: impl IntoIterator<Item = CommandPaletteItem>
) -> Vec<CommandPaletteItem> {
    let mut items: Vec<_> = items.into_iter().collect();
    items.sort_by_key(|item| item.label.to_ascii_lowercase());
    items
}

fn fuzzy_command_palette_items(
    query: &str,
    items: impl IntoIterator<Item = CommandPaletteItem>,
) -> Vec<CommandPaletteItem> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = command_palette_matcher();
    let mut utf32_buf = Vec::new();
    let mut matches: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            pattern
                .score(
                    Utf32Str::new(item.label.as_str(), &mut utf32_buf),
                    &mut matcher,
                )
                .map(|score| CommandPaletteMatch { item, score })
        })
        .collect();
    matches.sort_by(command_palette_match_order);
    matches.into_iter().map(|matched| matched.item).collect()
}

fn command_palette_matcher() -> Matcher {
    let mut config = NucleoConfig::DEFAULT;
    config.prefer_prefix = true;
    Matcher::new(config)
}

fn command_palette_match_order(
    left: &CommandPaletteMatch,
    right: &CommandPaletteMatch,
) -> std::cmp::Ordering {
    right.score.cmp(&left.score).then_with(|| {
        left.item
            .label
            .to_ascii_lowercase()
            .cmp(&right.item.label.to_ascii_lowercase())
    })
}

pub(crate) fn move_command_palette_selection(
    view: &mut CommandPaletteView,
    delta: isize,
) {
    if view.items.is_empty() {
        view.selected = 0;
        return;
    }
    let len = view.items.len();
    view.selected = if delta < 0 {
        (view.selected + len - 1) % len
    } else {
        (view.selected + 1) % len
    };
}

pub(crate) fn set_command_palette_query(
    view: &mut CommandPaletteView,
    query: String,
) {
    view.query = query;
    view.items = command_palette_items(&view.query);
    view.selected = 0;
}

pub(crate) fn complete_command_palette_selection(view: &mut CommandPaletteView) -> bool {
    let Some(query) = command_palette_completion_text(view) else {
        return false;
    };
    set_command_palette_query(view, query);
    true
}

fn command_palette_completion_text(view: &CommandPaletteView) -> Option<String> {
    let item = view.items.get(view.selected)?;
    let input = parse_command_palette_input(&view.query);
    if item.argument.is_none() {
        return Some(item.label.clone());
    }
    match input.argument {
        Some(argument) if !argument.is_empty() => Some(format!("{} {}", item.label, argument)),
        _ => Some(format!("{} ", item.label)),
    }
}

pub(crate) fn command_palette_selected_invocation(
    view: &CommandPaletteView
) -> Option<CommandPaletteAccept> {
    let item = view.items.get(view.selected)?;
    let input = parse_command_palette_input(&view.query);
    let argument = match item.argument {
        Some(CommandPaletteArgumentKind::WorkingDirectory) => {
            let Some(argument) = input.argument else {
                return Some(CommandPaletteAccept::NeedsArgument);
            };
            let argument = argument.trim();
            if argument.is_empty() {
                return Some(CommandPaletteAccept::NeedsArgument);
            }
            Some(CommandPaletteArgument::WorkingDirectory(
                argument.to_owned(),
            ))
        }
        None => None,
    };
    Some(CommandPaletteAccept::Ready(CommandPaletteInvocation {
        action: item.action,
        argument,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_palette_items_are_sorted_by_label() {
        let items = command_palette_items("");
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort_by_key(|label| label.to_ascii_lowercase());
        assert_eq!(labels, sorted);
    }

    #[test]
    fn command_palette_query_matches_labels_by_prefix() {
        let items = command_palette_items("close");
        assert_eq!(
            items
                .iter()
                .map(|item| item.action)
                .collect::<Vec<Action>>(),
            vec![Action::CloseActiveTab, Action::CloseWindow]
        );
    }

    #[test]
    fn command_palette_query_fuzzy_matches_labels() {
        let items = command_palette_items("cat");
        assert_eq!(
            items.first().map(|item| item.action),
            Some(Action::CloseActiveTab)
        );
    }

    #[test]
    fn command_palette_query_normalizes_unicode() {
        let items = fuzzy_command_palette_items(
            "resume",
            [
                CommandPaletteItem {
                    label: "Copy".to_owned(),
                    action: Action::Copy,
                    argument: None,
                },
                CommandPaletteItem {
                    label: "Résumé session".to_owned(),
                    action: Action::Paste,
                    argument: None,
                },
            ],
        );
        assert_eq!(items.first().map(|item| item.action), Some(Action::Paste));
    }

    #[test]
    fn command_palette_query_resets_selection() {
        let mut view = command_palette_view();
        move_command_palette_selection(&mut view, 1);
        set_command_palette_query(&mut view, "toggle".to_owned());
        assert_eq!(view.selected, 0);
        assert!(
            view.items
                .iter()
                .all(|item| item.label.to_ascii_lowercase().contains("toggle"))
        );
    }

    #[test]
    fn command_palette_includes_argument_command_with_colon() {
        let items = command_palette_items("open new window in dir");
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("Open new window in dir:")
        );
    }

    #[test]
    fn command_palette_includes_new_tab_argument_command_with_colon() {
        let items = command_palette_items("open new tab in dir");
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("Open new tab in dir:")
        );
    }

    #[test]
    fn command_palette_argument_text_does_not_affect_matching() {
        let items = command_palette_items("open new window in dir: Documents");
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("Open new window in dir:")
        );
    }

    #[test]
    fn command_palette_tab_completes_selected_label() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir".to_owned());
        assert!(complete_command_palette_selection(&mut view));
        assert_eq!(view.query, "Open new window in dir: ");
    }

    #[test]
    fn command_palette_tab_preserves_argument_text() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir: Documents".to_owned());
        assert!(complete_command_palette_selection(&mut view));
        assert_eq!(view.query, "Open new window in dir: Documents");
    }

    #[test]
    fn command_palette_enter_requires_argument_for_argument_commands() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir".to_owned());
        assert_eq!(
            command_palette_selected_invocation(&view),
            Some(CommandPaletteAccept::NeedsArgument)
        );
    }

    #[test]
    fn command_palette_enter_returns_argument_invocation() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir: Documents".to_owned());
        assert_eq!(
            command_palette_selected_invocation(&view),
            Some(CommandPaletteAccept::Ready(CommandPaletteInvocation {
                action: Action::OpenNewWindow,
                argument: Some(CommandPaletteArgument::WorkingDirectory(
                    "Documents".to_owned()
                )),
            }))
        );
    }

    #[test]
    fn command_palette_enter_returns_new_tab_argument_invocation() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new tab in dir: Documents".to_owned());
        assert_eq!(
            command_palette_selected_invocation(&view),
            Some(CommandPaletteAccept::Ready(CommandPaletteInvocation {
                action: Action::NewTab,
                argument: Some(CommandPaletteArgument::WorkingDirectory(
                    "Documents".to_owned()
                )),
            }))
        );
    }
}
