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
pub mod iterm_features;
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
mod snapshot;
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
use config41::ColorPalette;
use config41::CursorStyle;
use config41::EmojiCompatibilityMode;
use config41::FeaturePermissions;
use config41::StatusLineMode;
use config41::TerminalLimits;
use parking_lot::Mutex;
use pty_pipe41::PtyReader;
pub use vte_mode41::TextMode;
use vtepp::Action;

pub use crate::conformance::C1Mode;
pub use crate::conformance::ConformanceLevel;
pub use crate::dec::color::ColorSpace as DecColorSpace;
pub use crate::dec::color::DecColorState;
pub use crate::dec::color::LookupTable as DecColorLookupTable;
pub use crate::dec::color::alternate_assignment_for_style as dec_alternate_assignment_for_style;
pub use crate::dec::color::assign_alternate_text_color as dec_assign_alternate_text_color;
use crate::dec::color::effective_palette;
use crate::dec::color::report_color_table;
use crate::dec::color::restore_color_table;
pub use crate::dec::color::select_lookup_table as dec_select_lookup_table;
pub use crate::dec::color::state_from_palette as dec_color_state_from_palette;
pub use crate::dec::color::table_color as dec_table_color;
use crate::dec::r#macro::MacroStore;
pub use crate::dec::udk::DecModifierKey;
pub use crate::dec::udk::LocalFunctionKeyControl;
pub use crate::dec::udk::ModifierKeyControl;
use crate::dec::udk::UdkState;
use crate::dispatch::TerminalAction;
use crate::drcs::DrcsStore;
pub(crate) use crate::feature::apply_status_display_mode;
pub use crate::graphics::KittyFileRequest;
pub use crate::image::PlacedImage;
pub use crate::image::VisibleImage;
pub use crate::io::clipboard::ClipboardRequest;
pub use crate::io::keyboard::KittyFlags;
pub use crate::io::keyboard::KittyKeyboardState;
pub use crate::io::keyboard::KittyKeys;
pub use crate::io::mouse::MouseButton;
pub use crate::io::mouse::MouseEncoding;
pub use crate::io::mouse::MouseEventKind;
pub use crate::io::mouse::MouseModifiers;
pub use crate::io::mouse::MouseTracking;
pub use crate::processing::HostInput;
pub use crate::processing::HostInputEffects;
pub use crate::processing::HostMouse;
pub use crate::processing::TerminalProcessor;
pub use crate::processing::apply_host_input;
pub(crate) use crate::report::deccir_report;
pub(crate) use crate::report::dectabsr_report;
pub use crate::screen::Screen;
pub use crate::screen::StatusDisplayKind;
pub use crate::screen::grid::Viewport;
pub use crate::screen::hyperlink::HyperlinkRegistry;
use crate::screen::palette_sync::apply_screen_palette;
use crate::screen::palette_sync::sync_screen_erase_defaults;
use crate::screen::resize_screen;
pub use crate::screen::row::LineAttr;
pub use crate::screen::row::Row;
use crate::selection::Selection;
use crate::selection::search::SearchState;
pub use crate::snapshot::RowSnapshot;
pub use crate::snapshot::SearchSnapshot;
use crate::snapshot::SnapshotState;
pub use crate::snapshot::TermSnapshot;
pub use crate::snapshot::TermSnapshotInput;
pub use crate::snapshot::TermSnapshotOutput;
pub use crate::snapshot::TermSnapshotPublisher;
pub use crate::snapshot::publish_terminal_snapshot;
pub use crate::snapshot::terminal_snapshot_buffer;

/// Current OSC 133 shell-integration phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellIntegrationPhase {
    /// No active shell-integration phase is known.
    #[default]
    None,
    /// Prompt text is being printed after `OSC 133;A`.
    Prompt,
    /// User command editing is active after `OSC 133;B`.
    Command,
    /// Command output is active after `OSC 133;C`.
    Output,
    /// The previous command has finished after `OSC 133;D`.
    Finished,
}

/// Per-prompt metadata recorded from OSC 133 / OSC 633 shell-integration
/// sequences. Keyed by the absolute row of the prompt (`A` mark) in
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
    /// Command line reported by OSC 633 `E`. This is host-provided metadata,
    /// not terminal-observed text. Screen-extracted command text remains the
    /// preferred source; UI code may only display this as an annotation or
    /// use it as a lower-trust fallback when no observed command text exists.
    pub untrusted_command_line: Option<String>,
}

impl CommandMeta {
    fn new() -> Self {
        Self {
            command_col: None,
            command_row: None,
            output_row: None,
            started_at: None,
            finished_at: None,
            untrusted_command_line: None,
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
    /// Host-driven OSC 52 clipboard requests that need app-level approval.
    pub clipboard_requests: Vec<ClipboardRequest>,
    /// Host-driven kitty graphics file reads that need app-level approval.
    pub kitty_file_requests: Vec<KittyFileRequest>,
}

impl TerminalEffects {
    /// Return whether this batch produced no host-visible side effects.
    pub fn is_empty(&self) -> bool {
        self.host_bytes.is_empty()
            && self.resize_request.is_none()
            && !self.bell
            && self.clipboard_requests.is_empty()
            && self.kitty_file_requests.is_empty()
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
        self.clipboard_requests.extend(other.clipboard_requests);
        self.kitty_file_requests.extend(other.kitty_file_requests);
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
    /// Most recent OSC 133 / OSC 633 phase. Used only as a compatibility hint;
    /// terminal semantics still come from explicit VT input.
    pub shell_integration_phase: ShellIntegrationPhase,
}

/// Security-sensitive protocol state and VT extension storage.
#[derive(Debug, Default)]
pub struct TerminalProtocolState {
    /// Host-configured permission gates for optional terminal features.
    pub feature_permissions: FeaturePermissions,
    /// Host-configured resource limits for protocol-owned state.
    pub limits: TerminalLimits,
    /// VT420 macro definitions accumulated from DECDMAC / related controls.
    pub macros: MacroStore,
    /// Tracks nested macro expansion depth to prevent runaway recursion.
    pub macro_invocation_depth: usize,
    /// DEC user-defined keys and related keyboard-control state.
    pub udks: UdkState,
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

    /// Configured cursor shape and blink used when an app asks for the
    /// default cursor style.
    pub default_cursor_style: CursorStyle,

    /// Runtime cursor shape and blink, settable via DECSCUSR (`CSI Ps SP q`)
    /// and cursor-blink private mode 12. The renderer reads this each frame;
    /// the blink phase itself is owned by the renderer.
    pub cursor_style: CursorStyle,

    /// Cursor style saved while an application owns the 1049 alternate screen.
    saved_alt_cursor_style: Option<CursorStyle>,

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
    /// User-selected legacy emoji compatibility mode.
    pub emoji_compatibility_mode: EmojiCompatibilityMode,
    /// Security-sensitive optional protocol state and feature storage.
    pub protocol: TerminalProtocolState,
    /// Row-level snapshot invalidation state. The dirty rows live in one
    /// sidecar vector instead of on individual [`Row`] values.
    pub(crate) snapshot: SnapshotState,
}

/// Safety deadline for mode 2026 synchronized updates. If an app sends BSU
/// (`CSI ? 2026 h`) but never sends ESU (because it crashed, was killed,
/// forgot the terminator, etc.) rendering resumes after this window so the
/// UI doesn't appear frozen. 150ms matches the contour-terminal spec.
const SYNCHRONIZED_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotDirtyScope {
    None,
    CursorRows,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotDirtyBaseline {
    active_display: screen::ActiveDisplay,
    cursor_row: u32,
    cursor_col: u32,
    scroll_bottom: u32,
    grid_rows_len: usize,
    total_popped: usize,
    viewport_top: usize,
    viewport_rows: u32,
    viewport_cols: u32,
    offset: u32,
    total_rows: u32,
    status_line_row: Option<u32>,
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

    /// Whether DEC user-defined keys and related keyboard controls are allowed.
    pub fn udk_feature_enabled(&self) -> bool {
        feature::udk_feature_enabled(&self.protocol.feature_permissions)
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
            self.protocol.limits,
        );
    }

    fn define_udk(
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

    /// Whether a non-empty selection is active.
    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Cycle the runtime emoji compatibility mode and return the new mode.
    pub fn cycle_emoji_compatibility_mode(&mut self) -> EmojiCompatibilityMode {
        self.emoji_compatibility_mode = self.emoji_compatibility_mode.next();
        self.emoji_compatibility_mode
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
        );
        self.snapshot.mark_all();
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
        let pending = match action {
            TerminalAction::Ignore => dispatch::PendingApplication::None,
            TerminalAction::Basic(action) => {
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
            TerminalAction::Esc(action) => {
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
            TerminalAction::Osc(action) => {
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
            TerminalAction::Apc(action) => {
                dispatch::apply_apc_action(
                    action,
                    &mut self.images.kitty_images,
                    &mut self.images.kitty_chunked,
                    &mut effects.kitty_file_requests,
                    self.protocol.feature_permissions.kitty_graphics_files,
                    self.protocol.limits,
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
        };
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

    /// Mark every cached terminal row dirty. UI code should call this after
    /// mutating renderer-visible state such as selection or search matches.
    pub fn invalidate_snapshot_rows(&mut self) {
        self.snapshot.mark_all();
    }

    fn snapshot_dirty_baseline(&self) -> SnapshotDirtyBaseline {
        let status_line_row = view::status_line_row(&self.active).map(|_| self.viewport.rows);
        SnapshotDirtyBaseline {
            active_display: self.active.active_display,
            cursor_row: self.active.cursor.row,
            cursor_col: self.active.cursor.col,
            scroll_bottom: self.active.scroll_bottom,
            grid_rows_len: self.active.grid.rows.len(),
            total_popped: self.active.grid.total_popped,
            viewport_top: self.viewport.top_index(self.active.grid.rows.len()),
            viewport_rows: self.viewport.rows,
            viewport_cols: self.viewport.cols,
            offset: self.active.offset,
            total_rows: self.viewport.rows + u32::from(status_line_row.is_some()),
            status_line_row,
        }
    }

    fn snapshot_dirty_scope(
        &self,
        action: &TerminalAction<'_>,
        before: SnapshotDirtyBaseline,
    ) -> SnapshotDirtyScope {
        match action {
            TerminalAction::Ignore => SnapshotDirtyScope::None,
            TerminalAction::Basic(action) => self.basic_action_dirty_scope(action, before),
            TerminalAction::Vt52(action) => match action {
                dispatch::Vt52Action::AwaitCursorColumn => SnapshotDirtyScope::None,
                dispatch::Vt52Action::CursorPosition { trailing_ascii, .. } => {
                    if trailing_ascii.is_empty() {
                        SnapshotDirtyScope::CursorRows
                    } else {
                        SnapshotDirtyScope::All
                    }
                }
            },
            TerminalAction::Csi(_)
            | TerminalAction::Esc(_)
            | TerminalAction::Osc(_)
            | TerminalAction::Apc(_) => SnapshotDirtyScope::All,
        }
    }

    fn basic_action_dirty_scope(
        &self,
        action: &dispatch::BasicAction<'_>,
        before: SnapshotDirtyBaseline,
    ) -> SnapshotDirtyScope {
        if before.active_display == screen::ActiveDisplay::Status {
            return SnapshotDirtyScope::CursorRows;
        }
        if before.cursor_row != before.scroll_bottom {
            return SnapshotDirtyScope::CursorRows;
        }

        match action {
            dispatch::BasicAction::Execute(b'\n' | b'\x0b' | b'\x0c') => SnapshotDirtyScope::All,
            dispatch::BasicAction::PrintAscii(run) => {
                let cols = self.viewport.cols.max(1);
                if before.cursor_col.saturating_add(run.len() as u32) > cols {
                    SnapshotDirtyScope::All
                } else {
                    SnapshotDirtyScope::CursorRows
                }
            }
            dispatch::BasicAction::PrintText(run) => {
                // UTF-8 byte length is a cheap conservative upper bound for
                // terminal column width, so it can detect possible wrapping
                // without recounting chars on every mixed text run.
                if before.cursor_col.saturating_add(run.len() as u32) > self.viewport.cols.max(1) {
                    SnapshotDirtyScope::All
                } else {
                    SnapshotDirtyScope::CursorRows
                }
            }
            dispatch::BasicAction::Print(_) | dispatch::BasicAction::Print8Bit(_) => {
                if before.cursor_col.saturating_add(1) > self.viewport.cols.max(1) {
                    SnapshotDirtyScope::All
                } else {
                    SnapshotDirtyScope::CursorRows
                }
            }
            dispatch::BasicAction::Execute(_) => SnapshotDirtyScope::CursorRows,
        }
    }

    fn mark_snapshot_dirty_after(
        &mut self,
        before: SnapshotDirtyBaseline,
        scope: SnapshotDirtyScope,
    ) {
        if scope == SnapshotDirtyScope::None {
            return;
        }

        let after = self.snapshot_dirty_baseline();
        if scope == SnapshotDirtyScope::All
            || before.grid_rows_len != after.grid_rows_len
            || before.total_popped != after.total_popped
            || before.viewport_top != after.viewport_top
            || before.viewport_rows != after.viewport_rows
            || before.viewport_cols != after.viewport_cols
            || before.offset != after.offset
            || before.total_rows != after.total_rows
            || before.status_line_row != after.status_line_row
        {
            self.snapshot.mark_all();
            return;
        }

        match before.active_display {
            screen::ActiveDisplay::Main => {
                self.snapshot.mark_rows(before.cursor_row, after.cursor_row)
            }
            screen::ActiveDisplay::Status => {
                if let Some(row) = before.status_line_row.or(after.status_line_row) {
                    self.snapshot.mark_row(row);
                }
            }
        }

        if after.active_display != before.active_display {
            self.snapshot.mark_all();
        }
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
        output_streaming: Arc<AtomicBool>,
        snapshot_publisher: TermSnapshotPublisher,
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
                    output_streaming,
                    snapshot_publisher,
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

#[cfg(test)]
mod emoji_compatibility_tests {
    use super::*;
    use crate::test_support::TestTerm;

    const BASH_ZWJ_EMOJI: &str = "👩🏼\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👩🏽";

    #[test]
    fn auto_uses_legacy_width_inside_osc_133_command_phase() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        term.process(BASH_ZWJ_EMOJI.as_bytes());

        assert_eq!(term.cursor(), (0, 13));
        term.process(&[0x08; 11]);
        assert_eq!(term.cursor(), (0, 2));
    }

    #[test]
    fn off_keeps_normal_cluster_width_even_inside_command_phase() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);
        term.emoji_compatibility_mode = EmojiCompatibilityMode::Off;

        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        term.process(BASH_ZWJ_EMOJI.as_bytes());

        assert_eq!(term.cursor(), (0, 4));
    }

    #[test]
    fn on_uses_legacy_width_without_shell_integration() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);
        term.emoji_compatibility_mode = EmojiCompatibilityMode::On;

        term.process(BASH_ZWJ_EMOJI.as_bytes());

        assert_eq!(term.cursor(), (0, 11));
    }

    #[test]
    fn mode_cycles_in_requested_order() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);

        assert_eq!(term.emoji_compatibility_mode, EmojiCompatibilityMode::Auto);
        assert_eq!(
            term.cycle_emoji_compatibility_mode(),
            EmojiCompatibilityMode::Off
        );
        assert_eq!(
            term.cycle_emoji_compatibility_mode(),
            EmojiCompatibilityMode::On
        );
        assert_eq!(
            term.cycle_emoji_compatibility_mode(),
            EmojiCompatibilityMode::Auto
        );
    }
}
