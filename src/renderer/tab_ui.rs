/// Number of cell-widths reserved for each window control button.
pub(crate) const BUTTON_CELLS: f32 = 3.0;

/// Total width of the window-control button region in cell-width units.
pub(crate) const BUTTONS_REGION_CELLS: f32 = BUTTON_CELLS * 3.0;

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
