//! Which thread runs which [`Action`].
//!
//! Every action is handled on exactly one thread, and the two dispatch sites
//! used to encode that split implicitly: each carried its own list of "not
//! mine" variants whose body did nothing. Both matches were exhaustive, so a
//! new variant forced an edit -- but adding it to the do-nothing list on both
//! sides compiled cleanly and silently did nothing at runtime, which reads to
//! the user like a broken keybinding rather than a missing handler.
//!
//! Modelling ownership as data makes that unrepresentable: [`action_owner`] is
//! the single exhaustive match, so the compiler assigns every action an owner,
//! and each dispatch site checks that owner instead of carrying a list.

use config41::keybindings::Action;

/// The thread that runs an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionOwner {
    /// The window thread, which owns terminal input, selection, modal UI, and
    /// per-tab command-editor state.
    Window,
    /// The render thread, which owns the tab list and the background image.
    Render,
}

/// The thread responsible for running `action`.
pub(crate) fn action_owner(action: Action) -> ActionOwner {
    match action {
        Action::ScrollPageUp
        | Action::ScrollPageDown
        | Action::Copy
        | Action::Paste
        | Action::OpenSearch
        | Action::ScrollPrevPrompt
        | Action::ScrollNextPrompt
        | Action::JumpToPreviousFailed
        | Action::JumpToPreviousCommand
        | Action::JumpToPreviousSuccessful
        | Action::OpenNewWindow
        | Action::ToggleOutputRecording
        | Action::CycleEmojiCompatibility
        | Action::ToggleCommandEditor
        | Action::OpenCommandPalette
        | Action::ClearAllHistory
        | Action::ClearDirectoryHistory
        | Action::ClearHistoryEntries => ActionOwner::Window,

        Action::NewTab
        | Action::CloseActiveTab
        | Action::CloseWindow
        | Action::NextTab
        | Action::PrevTab
        | Action::PasteAsBackground
        | Action::ClearPastedBackground => ActionOwner::Render,
    }
}

/// Report an action that reached its owning thread with no handler.
///
/// Panics in debug builds so it surfaces during development, and warns in
/// release so a shipped build leaves a trace instead of doing nothing.
pub(crate) fn report_unhandled_action(
    action: Action,
    owner: ActionOwner,
) {
    debug_assert!(
        false,
        "{owner:?} thread owns {action:?} but has no handler for it"
    );
    warn!("action {action:?} is owned by the {owner:?} thread but has no handler");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_open_is_window_owned() {
        // The palette runs on the window thread; if this ever moves, the
        // early-return in `run_local_action` has to move with it.
        assert_eq!(
            action_owner(Action::OpenCommandPalette),
            ActionOwner::Window
        );
    }

    #[test]
    fn tab_lifecycle_is_render_owned() {
        for action in [
            Action::NewTab,
            Action::CloseActiveTab,
            Action::CloseWindow,
            Action::NextTab,
            Action::PrevTab,
        ] {
            assert_eq!(action_owner(action), ActionOwner::Render, "{action:?}");
        }
    }
}
