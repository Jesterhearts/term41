/// Number of cell-widths reserved for each window control button.
pub(crate) const BUTTON_CELLS: f32 = 3.0;

/// Total width of the window-control button region in cell-width units.
pub(crate) const BUTTONS_REGION_CELLS: f32 = BUTTON_CELLS * 3.0;

/// Minimum draggable space between the new-tab button and window controls.
pub(crate) const MIN_TITLEBAR_DRAG_CELLS: f32 = BUTTON_CELLS;

pub(crate) fn move_tab<T>(
    tabs: &mut Vec<T>,
    from_idx: usize,
    to_idx: usize,
) -> bool {
    let Some(last_idx) = tabs.len().checked_sub(1) else {
        return false;
    };
    let to_idx = to_idx.min(last_idx);
    if from_idx >= tabs.len() || from_idx == to_idx {
        return false;
    }

    let tab = tabs.remove(from_idx);
    tabs.insert(to_idx, tab);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabBarHover {
    NewTab,
    Minimize,
    Maximize,
    Close,
}

pub(crate) struct TabMenuItem {
    pub label: &'static str,
}

pub(crate) const TAB_MENU_ITEMS: &[TabMenuItem] = &[
    TabMenuItem { label: "New tab" },
    TabMenuItem { label: "Close tab" },
    TabMenuItem {
        label: "Close others",
    },
];

pub(crate) const TAB_MENU_WIDTH_CELLS: f32 = 16.0;

/// State of the tab context popup while it is open.
#[derive(Clone)]
pub(crate) struct TabContextMenu {
    pub tab_idx: usize,
    /// Pixel position where the popup was opened (used for placement).
    pub x: f32,
    /// Currently hovered menu-item index.
    pub hovered_item: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_tab_reorders_in_both_directions() {
        let mut tabs = vec!['a', 'b', 'c', 'd'];

        assert!(move_tab(&mut tabs, 0, 2));
        assert_eq!(tabs, ['b', 'c', 'a', 'd']);

        assert!(move_tab(&mut tabs, 3, 1));
        assert_eq!(tabs, ['b', 'd', 'c', 'a']);
    }

    #[test]
    fn move_tab_clamps_the_destination_and_rejects_invalid_sources() {
        let mut tabs = vec!['a', 'b', 'c'];

        assert!(move_tab(&mut tabs, 0, usize::MAX));
        assert_eq!(tabs, ['b', 'c', 'a']);
        assert!(!move_tab(&mut tabs, 3, 0));
        assert!(!move_tab(&mut tabs, 1, 1));
        assert_eq!(tabs, ['b', 'c', 'a']);
    }
}
