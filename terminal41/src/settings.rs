use super::*;

pub fn set_default_cursor_style(
    cursor_style: &mut CursorStyle,
    style: CursorStyle,
) {
    *cursor_style = style;
}

pub fn set_palette(
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &mut ColorPalette,
    dec_color: &mut DecColorState,
    new_palette: ColorPalette,
) {
    let old_palette = palette.clone();
    rebase_theme_entries(dec_color, base_palette, &new_palette);
    *base_palette = new_palette;
    *palette = effective_palette(base_palette, dec_color);
    for screen in [active, stash] {
        apply_screen_palette(screen, &old_palette, palette);
        sync_screen_erase_defaults(screen, dec_color);
    }
}

pub fn set_feature_permissions(
    protocol: &mut TerminalProtocolState,
    permissions: FeaturePermissions,
) {
    protocol.feature_permissions = permissions;
}

pub fn set_foreground_processes(
    protocol: &mut TerminalProtocolState,
    processes: Option<ForegroundProcessSet>,
) {
    if !protocol.foreground_processes_logged || protocol.foreground_processes != processes {
        feature::log_foreground_process_probe(&protocol.feature_permissions, processes.as_ref());
        protocol.foreground_processes_logged = true;
    }
    protocol.foreground_processes = processes;
}

pub fn set_cell_dimensions(
    cell_width: &mut u32,
    cell_height: &mut u32,
    new_cell_width: u32,
    new_cell_height: u32,
) {
    *cell_width = new_cell_width;
    *cell_height = new_cell_height;
}

pub fn set_scrollback_policy(
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &Viewport,
    strict_altscreen_scrollback: &mut bool,
    limit: u32,
    strict: bool,
) {
    *strict_altscreen_scrollback = strict;
    feature::apply_scrollback_limit(active, viewport, limit);
    let alt_limit = feature::alt_scrollback_limit(limit, *strict_altscreen_scrollback);
    feature::apply_scrollback_limit(stash, viewport, alt_limit);
}

pub fn set_default_status_display(
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    palette: &ColorPalette,
    default_status_display: &mut StatusDisplayKind,
    status_display: StatusDisplayKind,
) {
    lifecycle_ops::set_default_status_display(
        active,
        stash,
        viewport,
        palette,
        default_status_display,
        status_display,
    );
}
