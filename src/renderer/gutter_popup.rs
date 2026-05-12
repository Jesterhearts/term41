pub(crate) struct GutterMenuItem {
    pub label: &'static str,
}

pub(crate) const GUTTER_MENU_ITEMS: &[GutterMenuItem] = &[
    GutterMenuItem { label: "Rerun" },
    GutterMenuItem {
        label: "Copy command",
    },
    GutterMenuItem {
        label: "Copy cmd+output",
    },
    GutterMenuItem {
        label: "Copy output",
    },
];

pub(crate) const POPUP_WIDTH_CELLS: f32 = 20.0;

/// State of the gutter popup while it is open.
#[derive(Clone)]
pub(crate) struct GutterPopup {
    /// Prompt whose marker was clicked.
    pub prompt: terminal41::prompt::PromptRef,
    /// Mouse X position where the popup was opened, in window pixels.
    pub anchor_x: f32,
    /// Mouse Y position where the popup was opened, in window pixels.
    pub anchor_y: f32,
    /// Duration formatted as a human-readable string, if available.
    pub duration_text: Option<String>,
    /// Currently hovered menu-item index (0..GUTTER_MENU_ITEMS.len()).
    pub hovered_item: Option<usize>,
}

pub(crate) fn gutter_popup_origin(
    popup: &GutterPopup,
    popup_w: f32,
    popup_h: f32,
    cell_w: f32,
    cell_h: f32,
    gutter_w: f32,
    window_w: f32,
    window_h: f32,
) -> (f32, f32) {
    let max_x = (window_w - popup_w).max(gutter_w);
    let x = (popup.anchor_x + cell_w * 0.5).max(gutter_w).min(max_x);
    let max_y = (window_h - popup_h).max(cell_h);
    let y = popup.anchor_y.min(max_y).max(cell_h);
    (x, y)
}
