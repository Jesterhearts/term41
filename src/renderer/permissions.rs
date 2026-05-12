#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionChoice {
    Allow,
    Deny,
}

#[derive(Clone)]
pub(crate) struct PermissionModal {
    pub feature: String,
    pub hovered: Option<PermissionChoice>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PermissionButtonLayout {
    pub yes: (f32, f32, f32, f32),
    pub no: (f32, f32, f32, f32),
}

pub(crate) fn permission_modal_button_at(
    feature: &str,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    tab_bar_h: f32,
) -> Option<PermissionChoice> {
    let layout = permission_button_layout(feature, cell_w, cell_h, surface_w, surface_h, tab_bar_h);
    if point_in_rect(x, y, layout.yes) {
        return Some(PermissionChoice::Allow);
    }
    if point_in_rect(x, y, layout.no) {
        return Some(PermissionChoice::Deny);
    }
    None
}

pub(crate) fn permission_button_layout(
    feature: &str,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    tab_bar_h: f32,
) -> PermissionButtonLayout {
    let panel = permission_panel_rect(feature, cell_w, cell_h, surface_w, surface_h, tab_bar_h);
    let button_y = panel.1 + 4.0 * cell_h;
    let yes_w = 7.0 * cell_w;
    let no_w = 6.0 * cell_w;
    let gap = 2.0 * cell_w;
    let buttons_w = yes_w + gap + no_w;
    let yes_x = panel.0 + (panel.2 - buttons_w) * 0.5;
    let no_x = yes_x + yes_w + gap;
    PermissionButtonLayout {
        yes: (yes_x, button_y, yes_w, cell_h),
        no: (no_x, button_y, no_w, cell_h),
    }
}

pub(crate) fn permission_panel_rect(
    feature: &str,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    tab_bar_h: f32,
) -> (f32, f32, f32, f32) {
    let feature_line = permission_feature_line(feature);
    let max_chars = feature_line
        .chars()
        .count()
        .max("Would you like to allow this?".chars().count())
        .max("[y]es   [n]o".chars().count());
    let panel_w = (max_chars as f32 + 4.0) * cell_w;
    let panel_h = 6.0 * cell_h;
    let panel_x = ((surface_w - panel_w) * 0.5).max(0.0);
    let panel_y = ((surface_h - panel_h + tab_bar_h) * 0.5).max(tab_bar_h);
    (panel_x, panel_y, panel_w, panel_h)
}

pub(crate) fn permission_feature_line(feature: &str) -> String {
    format!(
        "A program would like to use {}.",
        permission_feature_label(feature)
    )
}

fn permission_feature_label(feature: &str) -> String {
    let mut label = String::new();
    for (len, ch) in feature.chars().enumerate() {
        if len >= 32 {
            label.push_str("...");
            break;
        }
        if ch.is_control() {
            label.push(' ');
        } else {
            label.push(ch);
        }
    }
    label
}

fn point_in_rect(
    x: f32,
    y: f32,
    rect: (f32, f32, f32, f32),
) -> bool {
    let (rx, ry, rw, rh) = rect;
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_buttons_are_centered_in_panel() {
        let panel = permission_panel_rect("the clipboard", 10.0, 20.0, 800.0, 600.0, 20.0);
        let buttons = permission_button_layout("the clipboard", 10.0, 20.0, 800.0, 600.0, 20.0);
        let left_gap = buttons.yes.0 - panel.0;
        let right_gap = panel.0 + panel.2 - (buttons.no.0 + buttons.no.2);
        assert!((left_gap - right_gap).abs() < 0.01);
    }

    #[test]
    fn permission_button_hit_testing_distinguishes_yes_and_no() {
        let buttons = permission_button_layout("the clipboard", 10.0, 20.0, 800.0, 600.0, 20.0);
        let yes = permission_modal_button_at(
            "the clipboard",
            buttons.yes.0 + 1.0,
            buttons.yes.1 + 1.0,
            10.0,
            20.0,
            800.0,
            600.0,
            20.0,
        );
        let no = permission_modal_button_at(
            "the clipboard",
            buttons.no.0 + 1.0,
            buttons.no.1 + 1.0,
            10.0,
            20.0,
            800.0,
            600.0,
            20.0,
        );
        assert_eq!(yes, Some(PermissionChoice::Allow));
        assert_eq!(no, Some(PermissionChoice::Deny));
    }

    #[test]
    fn permission_feature_line_sanitizes_untrusted_label() {
        let line = permission_feature_line("clipboard\nread");
        assert_eq!(line, "A program would like to use clipboard read.");

        let long = permission_feature_line("abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(long.contains("abcdefghijklmnopqrstuvwxyz012345..."));
    }
}
