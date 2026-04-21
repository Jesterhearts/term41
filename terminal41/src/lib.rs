#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

//! Terminal-emulation core for `term41`.
//!
//! This crate owns terminal state, parsing, screen buffers, host I/O protocol
//! helpers, selection/search state, inline image placement, and DEC/xterm
//! compatibility features. The application crate drives it by feeding PTY
//! bytes through [`TerminalProcessor`] and routing host-originated events
//! through [`HostInput`].

#[macro_use]
extern crate log;

mod charset;
mod color;
mod conformance;
mod cursor;
mod dcs;
mod dec;
mod dispatch;
mod drcs;
mod feature;
mod graphics;
/// Host-bound reports and event encoders.
pub mod host;
mod image;
/// Host clipboard helpers plus keyboard/mouse protocol state reexports.
pub mod io;
mod lifecycle_ops;
mod mode;
mod osc;
mod parser;
mod processing;
/// Shell-integration prompt metadata helpers.
pub mod prompt;
mod report;
mod runtime;
mod screen;
pub mod selection;
/// Runtime settings mutation helpers used by config reload and UI actions.
pub mod settings;
#[doc(hidden)]
pub mod test_support;
/// Read-only view/navigation helpers for renderer and UI code.
pub mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use clip41::Clipboard;
use parking_lot::Mutex;
use pty_pipe41::MAX_READ_CHUNK;
use pty_pipe41::PtyReader;
pub use vte_mode41::TextMode;
use vtepp::Action;

pub use self::color::ColorPalette;
pub use self::conformance::C1Mode;
pub use self::conformance::ConformanceLevel;
pub use self::cursor::CursorShape;
pub use self::cursor::CursorStyle;
pub use self::dec::color::ColorSpace as DecColorSpace;
pub use self::dec::color::DecColorState;
pub use self::dec::color::LookupTable as DecColorLookupTable;
pub use self::dec::color::alternate_assignment_for_style as dec_alternate_assignment_for_style;
pub use self::dec::color::assign_alternate_text_color as dec_assign_alternate_text_color;
pub use self::dec::color::select_lookup_table as dec_select_lookup_table;
pub use self::dec::color::state_from_palette as dec_color_state_from_palette;
pub use self::dec::color::table_color as dec_table_color;
use self::dec::r#macro::MacroStore;
use self::dispatch::TerminalAction;
use self::drcs::Store as DrcsStore;
pub use self::feature::FeaturePermissions;
pub use self::feature::ProgramAllowlist;
pub(crate) use self::feature::apply_status_display_mode;
pub use self::image::PlacedImage;
pub use self::image::VisibleImage;
pub use self::io::keyboard::KittyFlags;
pub use self::io::keyboard::KittyKeyboardState;
pub use self::io::keyboard::KittyKeys;
pub use self::io::mouse::MouseButton;
pub use self::io::mouse::MouseEncoding;
pub use self::io::mouse::MouseEventKind;
pub use self::io::mouse::MouseModifiers;
pub use self::io::mouse::MouseTracking;
use self::io::mouse::encode_mouse_event;
use self::io::mouse::should_report;
pub use self::processing::HostInput;
pub use self::processing::HostInputEffects;
pub use self::processing::HostMouse;
pub use self::processing::TerminalProcessor;
pub use self::processing::apply_host_input;
pub(crate) use self::report::deccir_report;
pub(crate) use self::report::dectabsr_report;
pub use self::screen::Screen;
pub use self::screen::StatusDisplayKind;
pub use self::screen::grid::Viewport;
pub use self::screen::hyperlink::HyperlinkRegistry;
use self::screen::palette_sync::apply_screen_palette;
use self::screen::palette_sync::sync_screen_erase_defaults;
use self::screen::resize_screen;
pub use self::screen::row::LineAttr;
pub use self::screen::row::Row;
use crate::dec::color::effective_palette;
use crate::dec::color::rebase_theme_entries;
use crate::dec::color::report_color_table;
use crate::dec::color::restore_color_table;
use crate::selection::Selection;
use crate::selection::search::SearchState;

/// Per-prompt metadata recorded from OSC 133 B/C/D sequences. Keyed by
/// the absolute row of the prompt (`A` mark) in
/// [`TerminalMetadata::command_metas`]. Enables command selection, rerun, text
/// extraction, and duration display in the gutter popup.
#[derive(Debug)]
pub struct CommandMeta {
    /// Column where the user's command text begins (from OSC 133 `B`).
    /// `None` when the shell doesn't emit `B`.
    pub command_col: Option<u32>,
    /// Absolute row where OSC 133 `B` fired. Usually the same as the
    /// prompt row, but multi-line prompts can differ.
    pub command_row: Option<u64>,
    /// Absolute row where OSC 133 `C` fired (command output starts).
    pub output_row: Option<u64>,
    /// When execution started (timestamped at `C`).
    pub started_at: Option<Instant>,
    /// When the command finished (timestamped at `D`).
    pub finished_at: Option<Instant>,
}

impl CommandMeta {
    fn new() -> Self {
        Self {
            command_col: None,
            command_row: None,
            output_row: None,
            started_at: None,
            finished_at: None,
        }
    }
}

/// Host-facing side effects produced while applying terminal input.
#[derive(Debug, Default)]
pub struct TerminalEffects {
    /// Bytes that must be written back to the PTY, such as query replies.
    pub host_bytes: Vec<u8>,
    /// Latest host-driven geometry request emitted by VT controls such as
    /// DECSNLS / DECSCPP.
    pub resize_request: Option<(u32, u32)>,
    /// True if at least one BEL was seen while producing this batch.
    pub bell: bool,
}

impl TerminalEffects {
    /// Return whether this batch produced no host-visible side effects.
    pub fn is_empty(&self) -> bool {
        self.host_bytes.is_empty() && self.resize_request.is_none() && !self.bell
    }

    /// Merge another batch into this one, preserving the latest resize
    /// request and OR-ing bell state.
    pub fn extend(
        &mut self,
        other: Self,
    ) {
        self.host_bytes.extend(other.host_bytes);
        if other.resize_request.is_some() {
            self.resize_request = other.resize_request;
        }
        self.bell |= other.bell;
    }
}

/// Shell/app metadata derived from OSC and window-title sequences.
#[derive(Debug, Default)]
pub struct TerminalMetadata {
    /// Last directory reported by the foreground shell via OSC 7.
    pub current_directory: Option<PathBuf>,
    /// Title last reported by the foreground app via OSC 0 / OSC 2.
    pub current_title: Option<String>,
    /// xterm title stack. CSI 22;0 t pushes, CSI 23;0 t pops.
    pub title_stack: Vec<Option<String>>,
    /// Absolute row index of the most recent OSC 133 `A` (prompt-start) mark.
    pub current_prompt_row: Option<u64>,
    /// Per-prompt metadata (command column, output row, timing) keyed by the
    /// absolute row of the prompt's `A` mark.
    pub command_metas: HashMap<u64, CommandMeta>,
}

/// Security-sensitive protocol state and VT extension storage.
#[derive(Debug, Default)]
pub struct TerminalProtocolState {
    /// Host-configured permission gates for optional terminal features.
    pub feature_permissions: FeaturePermissions,
    /// VT420 macro definitions accumulated from DECDMAC / related controls.
    pub macros: MacroStore,
    /// Tracks nested macro expansion depth to prevent runaway recursion.
    pub macro_invocation_depth: usize,
    /// Soft character-set storage for DRCS loads and reports.
    pub drcs: DrcsStore,
}

/// Image-protocol storage and image-id allocation state.
#[derive(Debug, Default)]
pub struct TerminalImageState {
    next_image_id: u64,
    /// Kitty graphics protocol image store. Images transmitted via `a=t`
    /// live here until placed or deleted.
    pub kitty_images: image41::kitty::KittyImageStore,
    /// Accumulates chunks for multi-part kitty graphics transmissions.
    pub kitty_chunked: image41::kitty::ChunkedTransmission,
    /// Accumulates chunks for multi-part iTerm2 graphics transmissions
    /// (`MultipartFile` → `FilePart*` → `FileEnd`).
    pub iterm_chunked: image41::iterm::ChunkedTransmission,
}

/// State machine for absorbing the two parameter bytes of a VT52
/// `ESC Y Pr Pc` direct cursor address. The bytes arrive as separate
/// parser actions after the `EscDispatch { byte: 'Y' }` is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vt52CursorAddr {
    /// Not inside a VT52 ESC Y sequence.
    Idle,
    /// Got `ESC Y`; the next byte(s) contain the row.
    AwaitingRow,
    /// Got the row byte; waiting for the column byte.
    AwaitingCol(u8),
}

/// Terminal-level modes toggled by escape sequences (DECSET/DECRST, mode
/// 2004, mode 2026, etc.) and reset together by RIS. Grouping them keeps
/// the `Terminal` struct focused and lets handler functions accept a single
/// `&mut TerminalModes` instead of five separate parameters.
#[derive(Debug)]
pub struct TerminalModes {
    /// Currently-active mouse tracking mode requested by the app via DECSET.
    pub mouse_tracking: MouseTracking,
    /// Wire encoding used for mouse events.
    pub mouse_encoding: MouseEncoding,
    /// Mode 2004 — when enabled, pasted text is wrapped in
    /// `\x1b[200~ ... \x1b[201~` so apps can distinguish it from typed input.
    pub bracketed_paste: bool,
    /// Mode `?1004` — when enabled, focus changes are reported to the
    /// foreground app as `\x1b[I` (focus in) and `\x1b[O` (focus out).
    pub focus_reporting: bool,
    /// Mode 2026 — Synchronized Output (BSU/ESU). `Some(t)` from the moment
    /// `CSI ? 2026 h` arrives until either `CSI ? 2026 l` clears it or the
    /// internal synchronized-update safety deadline passes; otherwise `None`.
    pub synchronized_update_since: Option<Instant>,
    /// IRM (ANSI mode 4) — Insert/Replace mode. When `true`, printing a
    /// character shifts existing text right before writing. Default is
    /// replace (overwrite) mode.
    pub insert_mode: bool,
    /// LNM (ANSI mode 20) — Line Feed/New Line mode. When `true`, LF, VT,
    /// and FF perform an implicit CR before the line feed. Default is off.
    pub newline_mode: bool,
    /// DECARM (`?8`) — auto-repeat. Always on at the OS level; tracked
    /// here only so DECRQM can report it. Default is `true`.
    pub decarm: bool,
    /// DECLRMM (`?69`) — when `true`, left/right margins (set by DECSLRM)
    /// are active and constrain cursor movement, scrolling, and
    /// insertion/deletion. Default is `false`.
    pub declrmm: bool,
    /// DECNCSM (`?95`) — when `true`, DECCOLM switching does not clear
    /// the screen. Default is `false`.
    pub decncsm: bool,
    /// DECSCNM (`?5`) — when `true`, the entire screen renders in reverse
    /// video: the default bg becomes fg and vice versa. Per-cell SGR 7
    /// (REVERSE) XORs with this, so reversed cells appear normal.
    pub screen_reverse: bool,
    /// Mode 40 — when `true`, DECCOLM (mode 3) is honoured. Default is
    /// `false`, matching xterm. Without this gate a malicious escape
    /// sequence stream can repeatedly toggle 80/132 columns, triggering
    /// expensive grid resizes.
    pub allow_deccolm: bool,
    /// DECNRCM (`?42`) — when `true`, national replacement character-set
    /// designations replace their ASCII positions and the terminal behaves
    /// as a 7-bit national terminal.
    pub decnrcm: bool,
    /// Saved column count from before DECCOLM switched to 132 columns.
    /// `None` when in normal (80-column) mode.
    pub deccolm_saved_cols: Option<u32>,
    /// Current DEC operating level selected by DECSCL.
    pub conformance_level: ConformanceLevel,
    /// How terminal-generated C1 controls are transmitted to the host.
    pub c1_mode: C1Mode,
    /// How high bytes in ground-state text are interpreted.
    pub text_mode: TextMode,
    /// DECANM (`?2`) — when `true` the terminal operates in VT52 compatibility
    /// mode. Set via `CSI ? 2 l`, cleared by `CSI ? 2 h` or RIS. VT52 mode
    /// uses a completely different (non-CSI) escape sequence vocabulary.
    pub vt52_mode: bool,
}

impl TerminalModes {
    fn new() -> Self {
        Self {
            mouse_tracking: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Default,
            bracketed_paste: false,
            focus_reporting: false,
            synchronized_update_since: None,
            insert_mode: false,
            newline_mode: false,
            decarm: true,
            declrmm: false,
            decncsm: false,
            screen_reverse: false,
            allow_deccolm: false,
            decnrcm: false,
            deccolm_saved_cols: None,
            conformance_level: ConformanceLevel::Level4,
            c1_mode: C1Mode::SevenBit,
            text_mode: TextMode::Utf8,
            vt52_mode: false,
        }
    }
}

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

    /// Cursor shape and blink, settable both via config and at runtime via
    /// DECSCUSR (`CSI Ps SP q`). The renderer reads this each frame; the
    /// blink phase itself is owned by the renderer.
    pub cursor_style: CursorStyle,

    /// Saved private mode states for XTSAVE/XTRESTORE (CSI ? Ps s / r).
    saved_private_modes: HashMap<mode::PrivateMode, bool>,

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
    /// `ESC Y` is dispatched, the next 1–2 byte actions carry the row and
    /// column values. This field persists across `apply` calls so the state
    /// survives the per-action dispatch boundary.
    vt52_cursor_addr: Vt52CursorAddr,
    /// Configured status-line mode used when resetting screens.
    pub default_status_display: StatusDisplayKind,
    /// Security-sensitive optional protocol state and feature storage.
    pub protocol: TerminalProtocolState,
}

/// Safety deadline for mode 2026 synchronized updates. If an app sends BSU
/// (`CSI ? 2026 h`) but never sends ESU (because it crashed, was killed,
/// forgot the terminator, etc.) rendering resumes after this window so the
/// UI doesn't appear frozen. 150ms matches the contour-terminal spec.
const SYNCHRONIZED_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

impl Terminal {
    /// Create a terminal with primary and alternate screen buffers.
    pub fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        default_status_display: StatusDisplayKind,
        feature_permissions: FeaturePermissions,
        cell_height: u32,
        cell_width: u32,
        palette: ColorPalette,
    ) -> Self {
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
            cursor_style: CursorStyle::default(),
            saved_private_modes: HashMap::new(),
            metadata: TerminalMetadata::default(),
            images: TerminalImageState::default(),
            cell_width,
            palette,
            base_palette,
            dec_color,
            vt52_cursor_addr: Vt52CursorAddr::Idle,
            default_status_display,
            protocol: TerminalProtocolState {
                feature_permissions,
                ..TerminalProtocolState::default()
            },
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

    fn restore_dec_color_table(
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

    fn define_macro(
        &mut self,
        params: vtepp::Params,
        payload: &[u8],
    ) {
        feature::define_macro(
            self.macro_feature_enabled(),
            &mut self.protocol.macros,
            params,
            payload,
        );
    }

    /// Current cell width in pixels.
    pub fn cell_width(&self) -> u32 {
        self.cell_width
    }

    /// Current cell height in pixels.
    pub fn cell_height(&self) -> u32 {
        self.cell_height
    }

    /// Whether a non-empty selection is active.
    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Resize the active/stashed screen buffers and viewport.
    pub fn resize(
        &mut self,
        cols: u32,
        rows: u32,
    ) {
        lifecycle_ops::resize(
            &mut self.active,
            &mut self.stash,
            &mut self.viewport,
            cols,
            rows,
        )
    }

    /// Apply a single parsed VTE action to the terminal state. Called by the
    /// terminal thread with the lock held — the parser runs *outside* the lock
    /// so the SIMD byte-scanning path never blocks rendering.
    ///
    /// Hook/Put/Unhook (DCS accumulation) are handled by the terminal thread
    /// directly and should not be passed here.
    #[must_use]
    fn apply(
        &mut self,
        action: Action<'_>,
        effects: &mut TerminalEffects,
    ) -> dispatch::PendingApplication {
        match dispatch::classify_action(
            &self.active,
            &self.modes,
            &self.protocol.drcs,
            &mut self.vt52_cursor_addr,
            action,
        ) {
            TerminalAction::Ignore => dispatch::PendingApplication::None,
            TerminalAction::Basic(action) => {
                let preserve_top_origin_scrollback =
                    !self.on_alt_screen && !screen::page_memory_active(&self.active);
                dispatch::apply_basic_action(
                    action,
                    &mut self.active,
                    &self.viewport,
                    self.modes.insert_mode,
                    self.modes.newline_mode,
                    &mut effects.bell,
                    preserve_top_origin_scrollback,
                );
                dispatch::PendingApplication::None
            }
            TerminalAction::Vt52(action) => {
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
            TerminalAction::Csi(action) => dispatch::apply_csi_action(
                action,
                &mut self.active,
                &mut self.stash,
                &mut self.viewport,
                &mut self.on_alt_screen,
                &mut self.modes,
                &mut self.kitty_keyboard,
                &mut effects.host_bytes,
                &mut effects.resize_request,
                &mut self.cursor_style,
                self.cell_width,
                self.cell_height,
                &mut self.default_status_display,
                &mut self.metadata.title_stack,
                &mut self.metadata.current_title,
                &mut self.saved_private_modes,
                &mut self.metadata.current_prompt_row,
                &mut effects.bell,
                &mut self.vt52_cursor_addr,
                &mut self.protocol.macros,
                self.protocol.macro_invocation_depth,
                &self.protocol.feature_permissions,
                &mut self.protocol.drcs,
                &mut self.palette,
                &self.base_palette,
                &mut self.dec_color,
            ),
            TerminalAction::Esc(action) => {
                dispatch::apply_esc_action(
                    action,
                    &mut self.active,
                    &mut self.stash,
                    &mut self.viewport,
                    &mut self.on_alt_screen,
                    &mut self.modes,
                    &mut self.kitty_keyboard,
                    &mut self.cursor_style,
                    &mut self.metadata.current_title,
                    &mut self.metadata.title_stack,
                    &mut self.saved_private_modes,
                    &mut self.metadata.current_prompt_row,
                    &mut effects.bell,
                    &mut self.palette,
                    &self.base_palette,
                    &mut self.dec_color,
                    &mut self.default_status_display,
                    &mut effects.host_bytes,
                    &mut self.vt52_cursor_addr,
                    &mut self.protocol.macros,
                    &mut self.protocol.drcs,
                );
                dispatch::PendingApplication::None
            }
            TerminalAction::Osc(action) => {
                dispatch::apply_osc_action(
                    action,
                    &mut self.clipboard,
                    &mut effects.host_bytes,
                    self.modes.c1_mode,
                    &mut self.metadata.current_directory,
                    &mut self.hyperlinks,
                    &mut self.active,
                    &self.viewport,
                    &mut self.metadata.current_title,
                    &mut self.metadata.current_prompt_row,
                    &mut self.metadata.command_metas,
                    &self.palette,
                    self.cell_width,
                    self.cell_height,
                    &mut self.images.iterm_chunked,
                    &mut self.images.next_image_id,
                );
                dispatch::PendingApplication::None
            }
            TerminalAction::Apc(action) => {
                dispatch::apply_apc_action(
                    action,
                    &mut self.images.kitty_images,
                    &mut self.images.kitty_chunked,
                    &mut self.active,
                    &self.viewport,
                    &mut self.images.next_image_id,
                    self.cell_height,
                    self.cell_width,
                    self.modes.c1_mode,
                    &mut effects.host_bytes,
                );
                dispatch::PendingApplication::None
            }
        }
    }

    /// Place a fully-decoded sixel image at the current cursor position.
    /// Called by the terminal thread *after* parsing the sixel data outside
    /// the lock, so the CPU-intensive decode doesn't block rendering.
    pub fn place_sixel_image(
        &mut self,
        image: image41::DecodedImage,
    ) {
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
                row,
                col: self.active.cursor.col,
                display_width,
                display_height,
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
    }

    /// Adjust image positions and prune stale command metadata after rows
    /// have been scrolled off the top of the grid.
    fn track_scroll(
        &mut self,
        popped_before: usize,
    ) {
        lifecycle_ops::track_scroll(
            &mut self.active,
            &mut self.metadata.command_metas,
            popped_before,
        )
    }
}

/// Handle to a running terminal thread. Signals the thread to stop on drop.
pub struct TerminalThread {
    stop: Arc<AtomicBool>,
    /// Thread handle populated by the terminal thread after it starts.
    pub thread_handle: Arc<OnceLock<Thread>>,
}

impl Default for TerminalThread {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalThread {
    /// Create a fresh `OnceLock` that the terminal thread will populate with
    /// its `Thread` handle. Pass a clone to `Pty::spawn` so the PTY reader
    /// can unpark the terminal thread.
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread_handle: Arc::new(OnceLock::new()),
        }
    }

    /// Spawn the terminal thread. `thread_handle` must be the same `OnceLock`
    /// that was passed to `Pty::spawn` for this tab.
    pub fn spawn(
        &self,
        name: String,
        terminal: Arc<Mutex<Terminal>>,
        pty_reader: PtyReader,
        render_thread_handle: Arc<OnceLock<Thread>>,
        startup_redraw: Option<Box<dyn Fn() + Send + Sync>>,
        tee_read: Box<dyn Fn(&[u8]) + Send + Sync>,
        deliver_effects: Box<dyn Fn(TerminalEffects) + Send + Sync>,
    ) {
        if self.thread_handle.get().is_some() {
            error!("terminal thread already running");
            return;
        }

        let stop = self.stop.clone();
        let handle_ = self.thread_handle.clone();

        thread::Builder::new()
            .name(name)
            .spawn(move || {
                handle_
                    .set(thread::current())
                    .expect("set terminal thread handle");
                runtime::run_terminal_thread(
                    terminal,
                    pty_reader,
                    stop,
                    render_thread_handle,
                    startup_redraw,
                    tee_read,
                    deliver_effects,
                );
            })
            .expect("spawn terminal thread");
    }
}

impl Drop for TerminalThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread_handle.get() {
            t.unpark();
        }
    }
}
