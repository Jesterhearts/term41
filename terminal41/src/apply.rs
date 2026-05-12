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
use crate::snapshot_dirty::SnapshotDirtyScope;
use crate::snapshot_dirty::action_clears_history_blocks;

impl Terminal {
    pub(crate) fn restore_dec_color_table(
        &mut self,
        payload: &[u8],
    ) -> bool {
        if !restore_color_table(&mut self.dec_color, payload) {
            return false;
        }
        self.apply_dec_color_defaults();
        true
    }

    fn apply_dec_color_defaults(&mut self) {
        let old_palette = self.palette.clone();
        self.palette = effective_palette(&self.base_palette, &self.dec_color);
        for screen in [&mut self.active, &mut self.stash] {
            apply_screen_palette(screen, &old_palette, &self.palette);
            sync_screen_erase_defaults(screen, &self.dec_color);
        }
    }

    pub(crate) fn define_macro(
        &mut self,
        params: vtepp::Params,
        payload: &[u8],
    ) {
        feature::define_macro(
            self.macro_feature_enabled(),
            &mut self.protocol.macros,
            params,
            payload,
            self.protocol.limits,
        );
    }

    pub(crate) fn define_udk(
        &mut self,
        params: vtepp::Params,
        payload: &[u8],
    ) {
        feature::define_udk(
            self.udk_feature_enabled(),
            &mut self.protocol.udks,
            params,
            payload,
            self.protocol.limits,
        );
    }

    fn legacy_emoji_compatibility_active(&self) -> bool {
        match self.emoji_compatibility_mode {
            EmojiCompatibilityMode::Off => false,
            EmojiCompatibilityMode::On => true,
            EmojiCompatibilityMode::Auto => {
                self.metadata.shell_integration_phase == ShellIntegrationPhase::Command
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
        &mut self,
        action: Action<'_>,
        effects: &mut TerminalEffects,
    ) -> dispatch::PendingApplication {
        let action = dispatch::classify_action(
            &self.active,
            &self.modes,
            &self.protocol.drcs,
            &mut self.vt52_cursor_addr,
            &action,
        );
        trace!("Classified action: {:?}", action);
        let dirty_before = self.snapshot_dirty_baseline();
        let dirty_scope = self.snapshot_dirty_scope(&action, dirty_before);
        let input_context_before = self.input_context_state();
        if !self.on_alt_screen && action_clears_history_blocks(&action) {
            let shifted = screen::clear_command_history_blocks(&mut self.active, &self.viewport);
            shift_visible_absolute_rows(&mut self.selection, &mut self.search, shifted);
            shift_terminal_metadata_rows(&mut self.metadata, shifted);
            self.snapshot.mark_all();
        }
        let was_on_alt_screen = self.on_alt_screen;
        let pending = match action {
            dispatch::TerminalAction::Ignore => dispatch::PendingApplication::None,
            dispatch::TerminalAction::Basic(action) => {
                let preserve_top_origin_scrollback =
                    !self.on_alt_screen && !screen::page_memory_active(&self.active);
                let legacy_emoji_compatibility = self.legacy_emoji_compatibility_active();
                dispatch::apply_basic_action(
                    action,
                    &mut self.active,
                    &self.viewport,
                    self.modes.insert_mode,
                    self.modes.newline_mode,
                    &mut effects.bell,
                    preserve_top_origin_scrollback,
                    legacy_emoji_compatibility,
                );
                dispatch::PendingApplication::None
            }
            dispatch::TerminalAction::Vt52(action) => {
                let preserve_top_origin_scrollback =
                    !self.on_alt_screen && !screen::page_memory_active(&self.active);
                dispatch::apply_vt52_action(
                    action,
                    &mut self.active,
                    &self.viewport,
                    self.modes.insert_mode,
                    preserve_top_origin_scrollback,
                );
                dispatch::PendingApplication::None
            }
            dispatch::TerminalAction::Csi(action) => dispatch::apply_csi_action(
                action,
                &mut self.active,
                &mut self.stash,
                &mut self.viewport,
                &mut self.on_alt_screen,
                &mut self.modes,
                &mut self.kitty_keyboard,
                &mut effects.host_bytes,
                &mut effects.resize_request,
                self.default_cursor_style,
                &mut self.cursor_style,
                &mut self.saved_alt_cursor_style,
                self.cell_width,
                self.cell_height,
                &mut self.default_status_display,
                &mut self.metadata.title_stack,
                &mut self.metadata.current_title,
                &mut self.saved_private_modes,
                &mut self.metadata.current_prompt_row,
                &mut self.metadata.shell_integration_phase,
                &mut effects.bell,
                &mut self.vt52_cursor_addr,
                &mut self.protocol.macros,
                self.protocol.macro_invocation_depth,
                &mut self.protocol.udks,
                &self.protocol.feature_permissions,
                self.protocol.limits,
                &mut self.protocol.drcs,
                &mut self.palette,
                &self.base_palette,
                &mut self.dec_color,
            ),
            dispatch::TerminalAction::Esc(action) => {
                dispatch::apply_esc_action(
                    action,
                    &mut self.active,
                    &mut self.stash,
                    &mut self.viewport,
                    &mut self.on_alt_screen,
                    &mut self.modes,
                    &mut self.kitty_keyboard,
                    self.default_cursor_style,
                    &mut self.cursor_style,
                    &mut self.saved_alt_cursor_style,
                    &mut self.metadata.current_title,
                    &mut self.metadata.title_stack,
                    &mut self.saved_private_modes,
                    &mut self.metadata.current_prompt_row,
                    &mut self.metadata.shell_integration_phase,
                    &mut effects.bell,
                    &mut self.palette,
                    &self.base_palette,
                    &mut self.dec_color,
                    &mut self.default_status_display,
                    &mut effects.host_bytes,
                    &mut self.vt52_cursor_addr,
                    &mut self.protocol.macros,
                    &mut self.protocol.udks,
                    &mut self.protocol.drcs,
                );
                dispatch::PendingApplication::None
            }
            dispatch::TerminalAction::Osc(action) => {
                dispatch::apply_osc_action(
                    action,
                    &mut self.clipboard,
                    &mut effects.host_bytes,
                    &mut effects.clipboard_requests,
                    &self.protocol.feature_permissions,
                    self.modes.c1_mode,
                    &mut self.metadata.current_directory,
                    &mut self.hyperlinks,
                    &mut self.active,
                    &self.viewport,
                    self.on_alt_screen,
                    &mut self.metadata.current_title,
                    &mut self.metadata.current_prompt_row,
                    &mut self.metadata.shell_integration_phase,
                    &mut self.metadata.command_metas,
                    &self.palette,
                    self.cell_width,
                    self.cell_height,
                    &mut self.images.iterm_chunked,
                    &mut self.images.next_image_id,
                );
                dispatch::PendingApplication::None
            }
            dispatch::TerminalAction::Apc(action) => {
                dispatch::apply_apc_action(
                    action,
                    &mut self.images.kitty_images,
                    &mut self.images.kitty_chunked,
                    &mut effects.kitty_file_requests,
                    self.protocol.feature_permissions.kitty_graphics_files,
                    self.protocol.limits,
                    &mut self.active,
                    &self.viewport,
                    &self.palette,
                    &mut self.images.next_image_id,
                    self.cell_height,
                    self.cell_width,
                    self.modes.c1_mode,
                    &mut effects.host_bytes,
                );
                dispatch::PendingApplication::None
            }
        };
        if self.on_alt_screen != was_on_alt_screen {
            self.selection = None;
        }
        if self.input_context_state() != input_context_before {
            effects.input_context_changed = true;
        }
        self.mark_snapshot_dirty_after(dirty_before, dirty_scope);
        pending
    }

    /// Place a fully-decoded sixel image at the current cursor position.
    /// Called by the terminal thread *after* parsing the sixel data outside
    /// the lock, so the CPU-intensive decode doesn't block rendering.
    pub fn place_sixel_image(
        &mut self,
        image: image41::DecodedImage,
    ) {
        let dirty_before = self.snapshot_dirty_baseline();
        let popped_before: usize = self.active.grid.total_popped;

        let id = self.images.next_image_id;
        self.images.next_image_id += 1;
        let row = screen::active_row_index(&self.active, &self.viewport);
        let image_rows = image.height.div_ceil(self.cell_height);
        crate::image::remove_overlapping(
            &mut self.active.images,
            row,
            image_rows.max(1) as usize,
            self.active.cursor.col,
            self.cell_height,
        );
        let display_width = image.width;
        let display_height = image.height;
        self.active.images.insert(
            id,
            PlacedImage {
                image,
                id,
                kitty_image_id: None,
                kitty_placement_id: None,
                row,
                col: self.active.cursor.col,
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
            self.active.cursor.row += 1;
            if self.active.cursor.row >= self.viewport.rows {
                self.active.grid.push_visible_row(&self.viewport);
                self.active.cursor.row = self.viewport.rows - 1;
            }
        }
        self.active.cursor.col = 0;

        self.track_scroll(popped_before);
        self.mark_snapshot_dirty_after(dirty_before, SnapshotDirtyScope::CursorRows);
    }

    /// Apply one approved kitty graphics file request after the app-level
    /// permission path has allowed reading the local file.
    pub fn apply_kitty_file_request(
        &mut self,
        request: KittyFileRequest,
    ) -> TerminalEffects {
        let dirty_before = self.snapshot_dirty_baseline();
        let popped_before = self.active.grid.total_popped;
        let mut effects = TerminalEffects::default();
        graphics::apply_kitty_file_request(
            request,
            &mut self.images.kitty_images,
            &mut self.active,
            &self.viewport,
            &self.palette,
            &mut self.images.next_image_id,
            self.cell_height,
            self.cell_width,
            &mut effects.host_bytes,
        );
        self.track_scroll(popped_before);
        self.mark_snapshot_dirty_after(dirty_before, SnapshotDirtyScope::All);
        effects
    }

    /// Reject one kitty graphics file request after the app-level permission
    /// path has denied reading the local file.
    pub fn deny_kitty_file_request(
        &mut self,
        request: KittyFileRequest,
    ) -> TerminalEffects {
        let mut effects = TerminalEffects::default();
        graphics::deny_kitty_file_request(request, &mut effects.host_bytes);
        effects
    }
}
