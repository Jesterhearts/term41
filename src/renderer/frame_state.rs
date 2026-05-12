use crate::window_host::TabId;

pub(super) fn should_suspend_terminal_area(
    active_tab_id: TabId,
    last_rendered_tab_id: Option<TabId>,
    synchronized_update_active: bool,
    reset_cached_rows: bool,
) -> bool {
    synchronized_update_active && !reset_cached_rows && last_rendered_tab_id == Some(active_tab_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronized_update_suspends_previously_rendered_terminal_area() {
        assert!(should_suspend_terminal_area(
            TabId(7),
            Some(TabId(7)),
            true,
            false
        ));
    }

    #[test]
    fn synchronized_update_does_not_reuse_another_tabs_terminal_area() {
        assert!(!should_suspend_terminal_area(
            TabId(7),
            Some(TabId(6)),
            true,
            false
        ));
    }

    #[test]
    fn terminal_area_not_suspended_when_synchronized_update_is_inactive() {
        assert!(!should_suspend_terminal_area(
            TabId(7),
            Some(TabId(7)),
            false,
            false
        ));
    }

    #[test]
    fn terminal_area_not_suspended_when_cached_rows_must_reset() {
        assert!(!should_suspend_terminal_area(
            TabId(7),
            Some(TabId(7)),
            true,
            true
        ));
    }
}
