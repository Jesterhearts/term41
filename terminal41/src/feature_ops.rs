use pty_pipe41::ForegroundProcessSet;

use crate::ColorPalette;
use crate::DrcsStore;
use crate::FeaturePermissions;
use crate::Screen;
use crate::StatusDisplayKind;
use crate::Terminal;
use crate::Viewport;
use crate::dcs;
use crate::dec::r#macro::MAX_MACRO_INVOCATION_DEPTH;
use crate::dec::r#macro::MacroEncoding;
use crate::dec::r#macro::MacroStore;
use crate::screen;

pub(crate) fn log_foreground_process_probe(
    permissions: &FeaturePermissions,
    processes: Option<&ForegroundProcessSet>,
) {
    let macro_state = if permissions.macros.allows_programs(processes) {
        "allow"
    } else {
        "deny"
    };
    match processes {
        Some(processes) if !processes.programs.is_empty() => {
            let programs = processes
                .programs
                .iter()
                .map(|program| program.exe_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            info!("Foreground PTY processes: [{programs}] macros={macro_state}");
        }
        _ => info!("Foreground PTY processes: [unresolved] macros={macro_state}"),
    }
}

pub(crate) fn macro_feature_enabled(
    permissions: &FeaturePermissions,
    foreground_processes: Option<&ForegroundProcessSet>,
) -> bool {
    permissions.macros.allows_programs(foreground_processes)
}

pub(crate) fn define_macro(
    enabled: bool,
    macros: &mut MacroStore,
    params: vtepp::Params,
    payload: &[u8],
) {
    if !enabled {
        return;
    }
    let Some(id) = params
        .iter()
        .next()
        .and_then(|group| group.first().copied())
    else {
        return;
    };
    let delete_existing = matches!(
        params
            .iter()
            .nth(1)
            .and_then(|group| group.first().copied()),
        Some(0 | 1)
    );
    let Some(encoding) = params
        .iter()
        .nth(2)
        .and_then(|group| group.first().copied())
        .and_then(MacroEncoding::from_param)
    else {
        return;
    };
    macros.define(id, delete_existing, encoding, payload);
}

pub(crate) fn invoke_macro(
    enabled: bool,
    macros: &MacroStore,
    macro_invocation_depth: usize,
    id: u16,
) -> Option<Vec<u8>> {
    if !enabled || macro_invocation_depth >= MAX_MACRO_INVOCATION_DEPTH {
        return None;
    }
    macros.get(id).map(ToOwned::to_owned)
}

pub(crate) fn apply_macro_bytes(
    terminal: &mut Terminal,
    bytes: &[u8],
) {
    let mut parser = vtepp::Parser::new();
    let mut hooks: Vec<dcs::HookState> = vec![];

    for action in parser.parse(bytes) {
        match action {
            vtepp::Action::Hook {
                params,
                intermediates,
                action,
            } => dcs::push_hook_state(&mut hooks, params, intermediates, action),
            vtepp::Action::Put(chunk) => dcs::append_hook_bytes(&mut hooks, chunk),
            vtepp::Action::Unhook => {
                let Some(hook) = hooks.pop() else {
                    continue;
                };
                dcs::dispatch_hook(hook, terminal);
            }
            action => terminal.apply(action),
        }
    }
}

pub(crate) fn drcs_render_glyphs(drcs: &DrcsStore) -> font41::DrcsGlyphMap {
    drcs.render_glyphs()
}

pub(crate) fn apply_status_display_mode(
    screen: &mut Screen,
    total_rows: u32,
    cols: u32,
    status_display: StatusDisplayKind,
    palette: &ColorPalette,
) -> u32 {
    let old_rows = total_rows.saturating_sub(screen::status_line_rows(screen));
    screen::set_status_display(
        screen,
        cols,
        status_display,
        palette.status_line_fg,
        palette.status_line_bg,
    );
    let new_rows = total_rows.saturating_sub(screen::status_line_rows(screen));
    if new_rows != old_rows {
        screen::resize_screen(screen, cols, old_rows, cols, new_rows);
        if screen::page_memory_active(screen)
            && let Some(page_rows) = screen::page_rows(screen)
        {
            screen::resize_page_memory(
                screen,
                &Viewport {
                    rows: new_rows,
                    cols,
                    top: 0,
                },
                page_rows,
            );
        }
    }
    new_rows
}

pub(crate) fn alt_scrollback_limit(
    scrollback_limit: u32,
    strict_altscreen_scrollback: bool,
) -> u32 {
    if strict_altscreen_scrollback {
        0
    } else {
        scrollback_limit
    }
}

pub(crate) fn apply_scrollback_limit(
    screen: &mut Screen,
    viewport: &Viewport,
    limit: u32,
) {
    screen.grid.scrollback_limit = limit;

    let max_rows = viewport.rows as usize + limit as usize;
    let grid = &mut screen.grid;
    let popped_before = grid.rows.len();
    while grid.rows.len() > max_rows {
        grid.rows.pop_front();
        grid.total_popped += 1;
    }
    let popped = popped_before - grid.rows.len();
    if popped > 0 {
        screen.images.retain(|_, img| img.row >= popped);
        for img in screen.images.values_mut() {
            img.row -= popped;
        }
    }
}
