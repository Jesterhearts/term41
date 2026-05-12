use std::time::Instant;

use config41::EmojiCompatibilityMode;
use vtepp::Action;

use crate::KittyFileRequest;
use crate::PlacedImage;
use crate::ShellIntegrationPhase;
use crate::Terminal;
use crate::TerminalEffects;
use crate::dec::color::effective_palette;
use crate::dec::color::restore_color_table;
use crate::dispatch;
use crate::feature;
use crate::graphics;
use crate::metadata::shift_terminal_metadata_rows;
use crate::metadata::shift_visible_absolute_rows;
use crate::screen;
use crate::screen::palette_sync::apply_screen_palette;
use crate::screen::palette_sync::sync_screen_erase_defaults;
use crate::snapshot_dirty;
use crate::snapshot_dirty::SnapshotDirtyScope;
use crate::snapshot_dirty::action_clears_history_blocks;

pub(crate) fn restore_dec_color_table(
    terminal: &mut Terminal,
    payload: &[u8],
) -> bool {
    if !restore_color_table(&mut terminal.dec_color, payload) {
        return false;
    }
    apply_dec_color_defaults(terminal);
    true
}

fn apply_dec_color_defaults(terminal: &mut Terminal) {
    let old_palette = terminal.palette.clone();
    terminal.palette = effective_palette(&terminal.base_palette, &terminal.dec_color);
    for screen in [&mut terminal.active, &mut terminal.stash] {
        apply_screen_palette(screen, &old_palette, &terminal.palette);
        sync_screen_erase_defaults(screen, &terminal.dec_color);
    }
}

pub(crate) fn define_macro(
    terminal: &mut Terminal,
    params: vtepp::Params,
    payload: &[u8],
) {
    feature::define_macro(
        terminal.macro_feature_enabled(),
        &mut terminal.protocol.macros,
        params,
        payload,
        terminal.protocol.limits,
    );
}

pub(crate) fn define_udk(
    terminal: &mut Terminal,
    params: vtepp::Params,
    payload: &[u8],
) {
    feature::define_udk(
        terminal.udk_feature_enabled(),
        &mut terminal.protocol.udks,
        params,
        payload,
        terminal.protocol.limits,
    );
}

fn legacy_emoji_compatibility_active(terminal: &Terminal) -> bool {
    match terminal.emoji_compatibility_mode {
        EmojiCompatibilityMode::Off => false,
        EmojiCompatibilityMode::On => true,
        EmojiCompatibilityMode::Auto => {
            terminal.metadata.shell_integration_phase == ShellIntegrationPhase::Command
        }
    }
}

/// Apply a single parsed VTE action to the terminal state. Called by the
/// terminal thread with the lock held - the parser runs *outside* the lock
/// so the SIMD byte-scanning path never blocks rendering.
///
/// Hook/Put/Unhook (DCS accumulation) are handled by the terminal thread
/// directly and should not be passed here.
#[must_use]
pub(crate) fn apply(
    terminal: &mut Terminal,
    action: Action<'_>,
    effects: &mut TerminalEffects,
) -> dispatch::PendingApplication {
    let action = dispatch::classify_action(
        &terminal.active,
        &terminal.modes,
        &terminal.protocol.drcs,
        &mut terminal.vt52_cursor_addr,
        &action,
    );
    trace!("Classified action: {:?}", action);
    let dirty_before = snapshot_dirty::snapshot_dirty_baseline(terminal);
    let dirty_scope = snapshot_dirty::snapshot_dirty_scope(terminal, &action, dirty_before);
    let input_context_before = snapshot_dirty::input_context_state(terminal);
    if !terminal.on_alt_screen && action_clears_history_blocks(&action) {
        let shifted =
            screen::clear_command_history_blocks(&mut terminal.active, &terminal.viewport);
        shift_visible_absolute_rows(&mut terminal.selection, &mut terminal.search, shifted);
        shift_terminal_metadata_rows(&mut terminal.metadata, shifted);
        terminal.snapshot.mark_all();
    }
    let was_on_alt_screen = terminal.on_alt_screen;
    let pending = match action {
        dispatch::TerminalAction::Ignore => dispatch::PendingApplication::None,
        dispatch::TerminalAction::Basic(action) => {
            let preserve_top_origin_scrollback =
                !terminal.on_alt_screen && !screen::page_memory_active(&terminal.active);
            let legacy_emoji_compatibility = legacy_emoji_compatibility_active(terminal);
            dispatch::apply_basic_action(
                action,
                &mut terminal.active,
                &terminal.viewport,
                terminal.modes.insert_mode,
                terminal.modes.newline_mode,
                &mut effects.bell,
                preserve_top_origin_scrollback,
                legacy_emoji_compatibility,
            );
            dispatch::PendingApplication::None
        }
        dispatch::TerminalAction::Vt52(action) => {
            let preserve_top_origin_scrollback =
                !terminal.on_alt_screen && !screen::page_memory_active(&terminal.active);
            dispatch::apply_vt52_action(
                action,
                &mut terminal.active,
                &terminal.viewport,
                terminal.modes.insert_mode,
                preserve_top_origin_scrollback,
            );
            dispatch::PendingApplication::None
        }
        dispatch::TerminalAction::Csi(action) => dispatch::apply_csi_action(
            action,
            &mut terminal.active,
            &mut terminal.stash,
            &mut terminal.viewport,
            &mut terminal.on_alt_screen,
            &mut terminal.modes,
            &mut terminal.kitty_keyboard,
            &mut effects.host_bytes,
            &mut effects.resize_request,
            terminal.default_cursor_style,
            &mut terminal.cursor_style,
            &mut terminal.saved_alt_cursor_style,
            terminal.cell_width,
            terminal.cell_height,
            &mut terminal.default_status_display,
            &mut terminal.metadata.title_stack,
            &mut terminal.metadata.current_title,
            &mut terminal.saved_private_modes,
            &mut terminal.metadata.current_prompt_row,
            &mut terminal.metadata.shell_integration_phase,
            &mut effects.bell,
            &mut terminal.vt52_cursor_addr,
            &mut terminal.protocol.macros,
            terminal.protocol.macro_invocation_depth,
            &mut terminal.protocol.udks,
            &terminal.protocol.feature_permissions,
            terminal.protocol.limits,
            &mut terminal.protocol.drcs,
            &mut terminal.palette,
            &terminal.base_palette,
            &mut terminal.dec_color,
        ),
        dispatch::TerminalAction::Esc(action) => {
            dispatch::apply_esc_action(
                action,
                &mut terminal.active,
                &mut terminal.stash,
                &mut terminal.viewport,
                &mut terminal.on_alt_screen,
                &mut terminal.modes,
                &mut terminal.kitty_keyboard,
                terminal.default_cursor_style,
                &mut terminal.cursor_style,
                &mut terminal.saved_alt_cursor_style,
                &mut terminal.metadata.current_title,
                &mut terminal.metadata.title_stack,
                &mut terminal.saved_private_modes,
                &mut terminal.metadata.current_prompt_row,
                &mut terminal.metadata.shell_integration_phase,
                &mut effects.bell,
                &mut terminal.palette,
                &terminal.base_palette,
                &mut terminal.dec_color,
                &mut terminal.default_status_display,
                &mut effects.host_bytes,
                &mut terminal.vt52_cursor_addr,
                &mut terminal.protocol.macros,
                &mut terminal.protocol.udks,
                &mut terminal.protocol.drcs,
            );
            dispatch::PendingApplication::None
        }
        dispatch::TerminalAction::Osc(action) => {
            dispatch::apply_osc_action(
                action,
                &mut terminal.clipboard,
                &mut effects.host_bytes,
                &mut effects.clipboard_requests,
                &terminal.protocol.feature_permissions,
                terminal.modes.c1_mode,
                &mut terminal.metadata.current_directory,
                &mut terminal.hyperlinks,
                &mut terminal.active,
                &terminal.viewport,
                terminal.on_alt_screen,
                &mut terminal.metadata.current_title,
                &mut terminal.metadata.current_prompt_row,
                &mut terminal.metadata.shell_integration_phase,
                &mut terminal.metadata.command_metas,
                &terminal.palette,
                terminal.cell_width,
                terminal.cell_height,
                &mut terminal.images.iterm_chunked,
                &mut terminal.images.next_image_id,
            );
            dispatch::PendingApplication::None
        }
        dispatch::TerminalAction::Apc(action) => {
            dispatch::apply_apc_action(
                action,
                &mut terminal.images.kitty_images,
                &mut terminal.images.kitty_chunked,
                &mut effects.kitty_file_requests,
                terminal.protocol.feature_permissions.kitty_graphics_files,
                terminal.protocol.limits,
                &mut terminal.active,
                &terminal.viewport,
                &terminal.palette,
                &mut terminal.images.next_image_id,
                terminal.cell_height,
                terminal.cell_width,
                terminal.modes.c1_mode,
                &mut effects.host_bytes,
            );
            dispatch::PendingApplication::None
        }
    };
    if terminal.on_alt_screen != was_on_alt_screen {
        terminal.selection = None;
    }
    if snapshot_dirty::input_context_state(terminal) != input_context_before {
        effects.input_context_changed = true;
    }
    snapshot_dirty::mark_snapshot_dirty_after(terminal, dirty_before, dirty_scope);
    pending
}

/// Place a fully-decoded sixel image at the current cursor position.
/// Called by the terminal thread *after* parsing the sixel data outside
/// the lock, so the CPU-intensive decode doesn't block rendering.
pub fn place_sixel_image(
    terminal: &mut Terminal,
    image: image41::DecodedImage,
) {
    let dirty_before = snapshot_dirty::snapshot_dirty_baseline(terminal);
    let popped_before: usize = terminal.active.grid.total_popped;

    let id = terminal.images.next_image_id;
    terminal.images.next_image_id += 1;
    let row = screen::active_row_index(&terminal.active, &terminal.viewport);
    let image_rows = image.height.div_ceil(terminal.cell_height);
    crate::image::remove_overlapping(
        &mut terminal.active.images,
        row,
        image_rows.max(1) as usize,
        terminal.active.cursor.col,
        terminal.cell_height,
    );
    let display_width = image.width;
    let display_height = image.height;
    terminal.active.images.insert(
        id,
        PlacedImage {
            image,
            id,
            kitty_image_id: None,
            kitty_placement_id: None,
            row,
            col: terminal.active.cursor.col,
            display_width,
            display_height,
            cell_x_offset: 0,
            cell_y_offset: 0,
            z_index: 0,
            placed_at: Instant::now(),
        },
    );

    // Advance cursor past the image, scrolling as needed.
    for _ in 0..image_rows {
        terminal.active.cursor.row += 1;
        if terminal.active.cursor.row >= terminal.viewport.rows {
            terminal.active.grid.push_visible_row(&terminal.viewport);
            terminal.active.cursor.row = terminal.viewport.rows - 1;
        }
    }
    terminal.active.cursor.col = 0;

    terminal.track_scroll(popped_before);
    snapshot_dirty::mark_snapshot_dirty_after(
        terminal,
        dirty_before,
        SnapshotDirtyScope::CursorRows,
    );
}

/// Apply one approved kitty graphics file request after the app-level
/// permission path has allowed reading the local file.
pub fn apply_kitty_file_request(
    terminal: &mut Terminal,
    request: KittyFileRequest,
) -> TerminalEffects {
    let dirty_before = snapshot_dirty::snapshot_dirty_baseline(terminal);
    let popped_before = terminal.active.grid.total_popped;
    let mut effects = TerminalEffects::default();
    graphics::apply_kitty_file_request(
        request,
        &mut terminal.images.kitty_images,
        &mut terminal.active,
        &terminal.viewport,
        &terminal.palette,
        &mut terminal.images.next_image_id,
        terminal.cell_height,
        terminal.cell_width,
        &mut effects.host_bytes,
    );
    terminal.track_scroll(popped_before);
    snapshot_dirty::mark_snapshot_dirty_after(terminal, dirty_before, SnapshotDirtyScope::All);
    effects
}

/// Reject one kitty graphics file request after the app-level permission
/// path has denied reading the local file.
pub fn deny_kitty_file_request(request: KittyFileRequest) -> TerminalEffects {
    let mut effects = TerminalEffects::default();
    graphics::deny_kitty_file_request(request, &mut effects.host_bytes);
    effects
}
