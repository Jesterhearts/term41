#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;

use clip41::Clipboard;
use config41::ColorPalette;
use config41::CursorStyle;
use config41::EmojiCompatibilityMode;
use config41::FeaturePermissions;
use config41::StatusLineMode;
use config41::TerminalLimits;

use crate::DecColorState;
use crate::DecModifierKey;
use crate::LocalFunctionKeyControl;
use crate::ModifierKeyControl;
use crate::dec::color::effective_palette;
use crate::dec::color::state_from_palette as dec_color_state_from_palette;
use crate::feature;
use crate::io::keyboard::KittyKeyboardState;
use crate::lifecycle_ops;
use crate::metadata::TerminalMetadata;
use crate::metadata::shift_terminal_metadata_rows;
use crate::metadata::shift_visible_absolute_rows;
use crate::mode;
use crate::protocol_state::TerminalImageState;
use crate::protocol_state::TerminalModes;
use crate::protocol_state::TerminalProtocolState;
use crate::protocol_state::Vt52CursorAddr;
use crate::screen::Screen;
use crate::screen::StatusDisplayKind;
use crate::screen::grid::Viewport;
use crate::screen::hyperlink::HyperlinkRegistry;
use crate::selection::Selection;
use crate::selection::search::SearchState;
use crate::settings;
use crate::snapshot::SnapshotState;

/// Complete mutable terminal state for one tab.
#[derive(Debug)]
pub struct Terminal {
    /// Currently visible screen buffer.
    pub active: Screen,
    /// Inactive screen buffer used for primary/alternate-screen swapping.
    pub stash: Screen,
    /// Window-sized viewport shared by the active and stashed screens.
    pub viewport: Viewport,

    /// `true` when the alt screen is active, `false` when the primary
    /// screen is active. Initialized to `false`; `stash` starts as the alt
    /// screen.
    pub on_alt_screen: bool,

    /// Cell height in pixels, used to convert sixel image pixel height to rows.
    pub cell_height: u32,
    /// Cell width in pixels. Stored for kitty display-sizing (`c=`/`r=` keys)
    /// once that path is wired up.
    pub cell_width: u32,

    /// System clipboard gateway. Shared between OSC 52 and mouse-driven
    /// copy/paste paths.
    pub clipboard: Clipboard,

    /// Terminal-level modes toggled by escape sequences (DECSET/DECRST,
    /// mode 2004, mode 2026, etc.) and reset together by RIS.
    pub modes: TerminalModes,

    /// Active text selection, if any. Positions use absolute row indices so
    /// the selection stays locked to content across scrollback trimming.
    pub selection: Option<Selection>,

    /// Search-in-scrollback state: open/closed, query text, match cache.
    /// When `active`, the host reroutes keyboard events into this struct
    /// instead of writing them to the PTY. Lives on the terminal so both
    /// the match renderer and the scroll-to-match navigator can touch it.
    pub search: SearchState,

    /// Interns OSC 8 hyperlink targets so each cell only has to carry a
    /// 4-byte id. Lives on the terminal (not per-screen) so a link active
    /// when the alt screen is entered keeps resolving on return.
    pub hyperlinks: HyperlinkRegistry,

    /// Kitty keyboard protocol mode stack. Apps push richer key encodings
    /// here when they want unambiguous Ctrl+letter, Shift+Enter, etc. The
    /// effective flags drive the input encoder in `main.rs`.
    pub kitty_keyboard: KittyKeyboardState,

    /// Configured cursor shape and blink used when an app asks for the
    /// default cursor style.
    pub default_cursor_style: CursorStyle,

    /// Runtime cursor shape and blink, settable via DECSCUSR (`CSI Ps SP q`)
    /// and cursor-blink private mode 12. The renderer reads this each frame;
    /// the blink phase itself is owned by the renderer.
    pub cursor_style: CursorStyle,

    /// Cursor style saved while an application owns the 1049 alternate screen.
    pub(crate) saved_alt_cursor_style: Option<CursorStyle>,

    /// Saved private mode states for XTSAVE/XTRESTORE (CSI ? Ps s / r).
    pub(crate) saved_private_modes: HashMap<mode::PrivateMode, bool>,

    /// Shell/app metadata surfaced to the host and prompt-selection tools.
    pub metadata: TerminalMetadata,

    /// Image-protocol transmission/storage state plus image-id allocation.
    pub(crate) images: TerminalImageState,

    /// Runtime color palette. Stored here so SGR resets, OSC color queries,
    /// and the renderer can all resolve themed colors.
    pub palette: ColorPalette,
    /// User/theme palette before DEC color-table overrides are applied.
    pub base_palette: ColorPalette,
    /// DEC color-table and lookup-mode state.
    pub dec_color: DecColorState,

    /// State machine for the VT52 `ESC Y Pr Pc` direct cursor address. After
    /// `ESC Y` is dispatched, the next 1-2 byte actions carry the row and
    /// column values. This field persists across `apply` calls so the state
    /// survives the per-action dispatch boundary.
    pub(crate) vt52_cursor_addr: Vt52CursorAddr,
    /// Configured status-line mode used when resetting screens.
    pub default_status_display: StatusDisplayKind,
    /// User-selected legacy emoji compatibility mode.
    pub emoji_compatibility_mode: EmojiCompatibilityMode,
    /// Security-sensitive optional protocol state and feature storage.
    pub protocol: TerminalProtocolState,
    /// Row-level snapshot invalidation state. The dirty rows live in one
    /// sidecar vector instead of on individual [`crate::Row`] values.
    pub(crate) snapshot: SnapshotState,
}

impl Terminal {
    /// Create a terminal with primary and alternate screen buffers.
    pub fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        default_status_display: StatusLineMode,
        feature_permissions: FeaturePermissions,
        limits: TerminalLimits,
        cell_height: u32,
        cell_width: u32,
        palette: ColorPalette,
    ) -> Self {
        let default_status_display = match default_status_display {
            StatusLineMode::Off => StatusDisplayKind::None,
            StatusLineMode::Indicator => StatusDisplayKind::Indicator,
        };
        let base_palette = palette;
        let dec_color = dec_color_state_from_palette(&base_palette);
        let palette = effective_palette(&base_palette, &dec_color);
        let mut terminal = Self {
            active: Screen::new(
                cols,
                rows,
                scrollback_limit,
                palette.fg,
                palette.bg,
                palette.status_line_fg,
                palette.status_line_bg,
            ),
            // Stash starts as a blank alt screen. By default it inherits the
            // normal scrollback budget; `strict_altscreen_scrollback`
            // forces the legacy zero-scrollback xterm-style policy.
            stash: Screen::new(
                cols,
                rows,
                0,
                palette.fg,
                palette.bg,
                palette.status_line_fg,
                palette.status_line_bg,
            ),
            viewport: Viewport { rows, cols, top: 0 },
            on_alt_screen: false,
            cell_height,
            clipboard: Clipboard::new(),
            modes: TerminalModes::new(),
            selection: None,
            search: SearchState::new(),
            hyperlinks: HyperlinkRegistry::new(),
            kitty_keyboard: KittyKeyboardState::new(),
            default_cursor_style: CursorStyle::default(),
            cursor_style: CursorStyle::default(),
            saved_alt_cursor_style: None,
            saved_private_modes: HashMap::new(),
            metadata: TerminalMetadata::default(),
            images: TerminalImageState::default(),
            cell_width,
            palette,
            base_palette,
            dec_color,
            vt52_cursor_addr: Vt52CursorAddr::Idle,
            default_status_display,
            emoji_compatibility_mode: EmojiCompatibilityMode::Auto,
            protocol: TerminalProtocolState {
                feature_permissions,
                limits,
                ..TerminalProtocolState::default()
            },
            snapshot: SnapshotState::default(),
        };
        let Terminal {
            active,
            stash,
            viewport,
            palette,
            default_status_display: current_default_status_display,
            ..
        } = &mut terminal;
        settings::set_default_status_display(
            active,
            stash,
            viewport,
            palette,
            current_default_status_display,
            default_status_display,
        );
        terminal
    }

    /// Borrow the DEC color state currently affecting rendering.
    pub fn dec_color_state(&self) -> &DecColorState {
        &self.dec_color
    }

    /// Return DRCS glyphs in the format expected by the font rasterizer.
    pub fn drcs_render_glyphs(&self) -> font41::DrcsGlyphMap {
        feature::drcs_render_glyphs(&self.protocol.drcs)
    }

    /// Whether VT macro definition/invocation is allowed for this terminal.
    pub fn macro_feature_enabled(&self) -> bool {
        feature::macro_feature_enabled(&self.protocol.feature_permissions)
    }

    /// Whether DEC user-defined keys and related keyboard controls are allowed.
    pub fn udk_feature_enabled(&self) -> bool {
        feature::udk_feature_enabled(&self.protocol.feature_permissions)
    }

    pub fn user_defined_key(
        &self,
        selector: u16,
    ) -> Option<Vec<u8>> {
        feature::lookup_udk(self.udk_feature_enabled(), &self.protocol.udks, selector)
    }

    pub fn programmed_udk_selectors(&self) -> Vec<u16> {
        if self.udk_feature_enabled() {
            self.protocol.udks.programmed_selectors()
        } else {
            Vec::new()
        }
    }

    pub fn udks_locked(&self) -> bool {
        self.udk_feature_enabled() && self.protocol.udks.locked()
    }

    pub fn local_function_key_control(
        &self,
        selector: u16,
    ) -> Option<LocalFunctionKeyControl> {
        feature::local_function_key_control(
            self.udk_feature_enabled(),
            &self.protocol.udks,
            selector,
        )
    }

    pub fn modifier_key_control(
        &self,
        key: DecModifierKey,
    ) -> ModifierKeyControl {
        feature::modifier_key_control(self.udk_feature_enabled(), &self.protocol.udks, key)
    }

    pub fn dec_modifier_key_report(
        &self,
        key: DecModifierKey,
        pressed: bool,
    ) -> Option<Vec<u8>> {
        (self.modifier_key_control(key) == ModifierKeyControl::Report).then(|| {
            let mut out = Vec::new();
            crate::dec::udk::write_modifier_report(&mut out, self.modes.c1_mode, key, pressed);
            out
        })
    }

    /// Current cell width in pixels.
    pub fn cell_width(&self) -> u32 {
        self.cell_width
    }

    /// Current cell height in pixels.
    pub fn cell_height(&self) -> u32 {
        self.cell_height
    }

    pub fn kitty_images(&self) -> &image41::kitty::KittyImageStore {
        &self.images.kitty_images
    }

    /// Whether a non-empty selection is active.
    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Cycle the runtime emoji compatibility mode and return the new mode.
    pub fn cycle_emoji_compatibility_mode(&mut self) -> EmojiCompatibilityMode {
        self.emoji_compatibility_mode = self.emoji_compatibility_mode.next();
        self.emoji_compatibility_mode
    }

    /// Resize the active/stashed screen buffers and viewport.
    pub fn resize(
        &mut self,
        cols: u32,
        rows: u32,
    ) {
        let outcome = lifecycle_ops::resize(
            &mut self.active,
            &mut self.stash,
            &mut self.viewport,
            cols,
            rows,
        );
        shift_visible_absolute_rows(
            &mut self.selection,
            &mut self.search,
            outcome.active.prepended_rows,
        );
        let primary_prepend = if self.on_alt_screen {
            outcome.stash.prepended_rows
        } else {
            outcome.active.prepended_rows
        };
        shift_terminal_metadata_rows(&mut self.metadata, primary_prepend);
        self.snapshot.mark_all();
    }

    /// Mark every cached terminal row dirty. UI code should call this after
    /// mutating renderer-visible state such as selection or search matches.
    pub fn invalidate_snapshot_rows(&mut self) {
        self.snapshot.mark_all();
    }

    /// Adjust image positions and prune stale command metadata after rows
    /// have been scrolled off the top of the grid.
    pub(crate) fn track_scroll(
        &mut self,
        popped_before: usize,
    ) {
        lifecycle_ops::track_scroll(
            &mut self.active,
            &mut self.metadata.command_metas,
            popped_before,
        );

        let _ = popped_before;
    }
}
