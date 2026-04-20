#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

#[macro_use]
extern crate log;

mod charset;
mod color;
mod conformance;
mod cursor;
mod dcs;
mod dec;
mod drcs;
mod feature;
mod graphics;
mod image;
pub mod io;
mod lifecycle_ops;
mod mode;
mod osc;
mod parser;
pub mod prompt;
mod report;
mod runtime;
mod screen;
pub mod selection;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use clip41::Clipboard;
use pty_pipe41::ForegroundProcessSet;
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
use self::osc::OscContext;
use self::osc::handle_osc;
use self::parser::CsiContext;
use self::parser::EscContext;
use self::parser::csi_dispatch;
use self::parser::esc_dispatch;
use self::parser::execute;
use self::parser::execute_status;
use self::parser::put_8bit_byte;
use self::parser::put_ascii_run;
use self::parser::put_printable;
use self::parser::put_status_8bit_byte;
use self::parser::put_status_ascii_run;
use self::parser::put_status_printable;
use self::parser::put_status_text_run;
use self::parser::put_text_run;
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
use crate::dec::color::TEXT_COLOR_ASSIGNMENT_CLASS;
use crate::dec::color::assign_color;
use crate::dec::color::effective_palette;
use crate::dec::color::rebase_theme_entries;
use crate::dec::color::report_color_table;
use crate::dec::color::restore_color_table;
use crate::selection::Selection;
use crate::selection::search::SearchState;

/// Per-prompt metadata recorded from OSC 133 B/C/D sequences. Keyed by
/// the absolute row of the prompt (`A` mark) in
/// [`Terminal::command_metas`]. Enables command selection, rerun, text
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
    /// [`SYNCHRONIZED_UPDATE_TIMEOUT`] safety deadline passes; otherwise
    /// `None`.
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

#[derive(Debug)]
pub struct Terminal {
    pub active: Screen,
    pub stash: Screen,
    pub viewport: Viewport,

    /// `true` when the alt screen is active, `false` when the primary
    /// screen is active. Initialized to `false`; `stash` starts as the alt
    /// screen.
    pub on_alt_screen: bool,

    /// Cell height in pixels, used to convert sixel image pixel height to rows.
    cell_height: u32,
    /// Cell width in pixels. Stored for kitty display-sizing (`c=`/`r=` keys)
    /// once that path is wired up.
    cell_width: u32,

    next_image_id: u64,

    /// System clipboard gateway. Shared between OSC 52 and mouse-driven
    /// copy/paste paths.
    pub clipboard: Clipboard,

    /// Bytes produced by the terminal itself that must be written back to
    /// the PTY — responses to queries like OSC 52 `?` reads. Drained by the
    /// event loop after each [`process`](Self::process) call.
    pub pending_output: Vec<u8>,
    pending_host_resize: Option<(u32, u32)>,

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

    /// Last directory reported by the foreground shell via OSC 7. None when
    /// no shell has reported, or after a remote-session shell sent an empty
    /// payload to disclaim its previous report. Useful for "open new window
    /// here" and any title-bar surfacing of the current directory.
    pub current_directory: Option<PathBuf>,

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

    /// Title last reported by the foreground app via OSC 0 / OSC 2.
    /// `None` means no app has set a title (or one explicitly cleared
    /// it); the host applies its default ("term41") in that case.
    pub current_title: Option<String>,

    /// xterm title stack. CSI 22;0 t pushes, CSI 23;0 t pops. Capped at
    /// 16 entries to bound memory from a misbehaving app.
    title_stack: Vec<Option<String>>,

    /// Saved private mode states for XTSAVE/XTRESTORE (CSI ? Ps s / r).
    saved_private_modes: HashMap<u16, bool>,

    /// Latched true whenever the parser sees a BEL byte (0x07). The host
    /// drains this each frame via [`Self::take_bell_pending`] so it can
    /// flash the screen, ping the compositor, etc. Latched (not
    /// counted) because reacting once per frame is the right grain — a
    /// noisy app that bells in a tight loop should still get one
    /// per-frame response, not a queue that backs up forever.
    bell_pending: bool,

    /// Absolute row index of the most recent OSC 133 `A` (prompt-start)
    /// mark. An OSC 133 `D` resolves to this row and stamps its exit code
    /// there, so the success/failure indicator sits next to the prompt
    /// line — the anchor the user scrolls to — rather than the end of the
    /// command's output. `None` before any shell-integration prompt has
    /// been seen.
    ///
    /// Lives on `Terminal` rather than per-`Screen` because a prompt is
    /// meaningful only on the primary screen; an app on the alt screen
    /// that emits OSC 133 would still write into this slot, but the marks
    /// land on alt's grid and disappear with the alt-screen teardown.
    current_prompt_row: Option<u64>,

    /// Per-prompt metadata (command column, output row, timing). Keyed by
    /// the absolute row of the prompt's `A` mark. Stale entries are pruned
    /// when their rows fall off the front of scrollback.
    pub command_metas: HashMap<u64, CommandMeta>,

    /// Kitty graphics protocol image store. Images transmitted via `a=t`
    /// live here until placed or deleted.
    kitty_images: image41::kitty::KittyImageStore,

    /// Accumulates chunks for multi-part kitty graphics transmissions.
    kitty_chunked: image41::kitty::ChunkedTransmission,

    /// Accumulates chunks for multi-part iTerm2 graphics transmissions
    /// (`MultipartFile` → `FilePart*` → `FileEnd`).
    iterm_chunked: image41::iterm::ChunkedTransmission,

    /// Runtime color palette. Stored here so SGR resets, OSC color queries,
    /// and the renderer can all resolve themed colors.
    pub palette: ColorPalette,
    base_palette: ColorPalette,
    dec_color: DecColorState,

    /// State machine for the VT52 `ESC Y Pr Pc` direct cursor address. After
    /// `ESC Y` is dispatched, the next 1–2 byte actions carry the row and
    /// column values. This field persists across `apply` calls so the state
    /// survives the per-action dispatch boundary.
    vt52_cursor_addr: Vt52CursorAddr,
    default_status_display: StatusDisplayKind,
    strict_altscreen_scrollback: bool,
    feature_permissions: FeaturePermissions,
    foreground_processes: Option<ForegroundProcessSet>,
    foreground_processes_logged: bool,
    macros: MacroStore,
    macro_invocation_depth: usize,
    drcs: DrcsStore,
}

/// Safety deadline for mode 2026 synchronized updates. If an app sends BSU
/// (`CSI ? 2026 h`) but never sends ESU (because it crashed, was killed,
/// forgot the terminator, etc.) rendering resumes after this window so the
/// UI doesn't appear frozen. 150ms matches the contour-terminal spec.
const SYNCHRONIZED_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

impl Terminal {
    pub fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        default_status_display: StatusDisplayKind,
        strict_altscreen_scrollback: bool,
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
                feature::alt_scrollback_limit(scrollback_limit, strict_altscreen_scrollback),
                palette.fg,
                palette.bg,
                palette.status_line_fg,
                palette.status_line_bg,
            ),
            viewport: Viewport { rows, cols, top: 0 },
            on_alt_screen: false,
            cell_height,
            next_image_id: 0,
            clipboard: Clipboard::new(),
            pending_output: Vec::new(),
            pending_host_resize: None,
            modes: TerminalModes::new(),
            selection: None,
            search: SearchState::new(),
            current_directory: None,
            hyperlinks: HyperlinkRegistry::new(),
            kitty_keyboard: KittyKeyboardState::new(),
            cursor_style: CursorStyle::default(),
            current_title: None,
            title_stack: Vec::new(),
            saved_private_modes: HashMap::new(),
            bell_pending: false,
            current_prompt_row: None,
            command_metas: HashMap::new(),
            kitty_images: image41::kitty::KittyImageStore::new(),
            kitty_chunked: image41::kitty::ChunkedTransmission::new(),
            iterm_chunked: image41::iterm::ChunkedTransmission::new(),
            cell_width,
            palette,
            base_palette,
            dec_color,
            vt52_cursor_addr: Vt52CursorAddr::Idle,
            default_status_display,
            strict_altscreen_scrollback,
            feature_permissions,
            foreground_processes: None,
            foreground_processes_logged: false,
            macros: MacroStore::default(),
            macro_invocation_depth: 0,
            drcs: DrcsStore::default(),
        };
        terminal.set_default_status_display(default_status_display);
        terminal
    }

    /// Returns `true` when the foreground app has opened a synchronized
    /// output window (mode 2026) that has not yet been closed or timed out.
    /// The host should skip rendering while this returns `true` so partial
    /// frames (e.g. mid-scroll, mid-reflow) are never presented.
    pub fn is_synchronized_update_active(&self) -> bool {
        self.modes
            .synchronized_update_since
            .is_some_and(|start| start.elapsed() < SYNCHRONIZED_UPDATE_TIMEOUT)
    }

    /// Drain the bell flag. Returns `true` exactly when at least one BEL
    /// has arrived since the last call, leaving the flag cleared so the
    /// next frame starts fresh.
    pub fn take_bell_pending(&mut self) -> bool {
        std::mem::replace(&mut self.bell_pending, false)
    }

    /// Override the default cursor style. Called once at startup so the
    /// user's `config.toml` preference takes effect before any DECSCUSR
    /// arrives from the shell.
    pub fn set_default_cursor_style(
        &mut self,
        style: CursorStyle,
    ) {
        self.cursor_style = style;
    }

    pub fn set_palette(
        &mut self,
        palette: ColorPalette,
    ) {
        let old_palette = self.palette.clone();
        rebase_theme_entries(&mut self.dec_color, &self.base_palette, &palette);
        self.base_palette = palette;
        self.palette = effective_palette(&self.base_palette, &self.dec_color);
        for screen in [&mut self.active, &mut self.stash] {
            apply_screen_palette(screen, &old_palette, &self.palette);
            sync_screen_erase_defaults(screen, &self.dec_color);
        }
    }

    fn assign_dec_color(
        &mut self,
        item: u16,
        fg: u16,
        bg: u16,
    ) -> bool {
        if !assign_color(&mut self.dec_color, item, fg, bg) {
            return false;
        }
        if item == TEXT_COLOR_ASSIGNMENT_CLASS {
            self.apply_dec_color_defaults();
        }
        true
    }

    fn assign_dec_alternate_text_color(
        &mut self,
        item: u16,
        fg: u16,
        bg: u16,
    ) -> bool {
        dec_assign_alternate_text_color(&mut self.dec_color, item, fg, bg)
    }

    fn select_dec_lookup_table(
        &mut self,
        ps: u16,
    ) -> bool {
        dec_select_lookup_table(&mut self.dec_color, ps)
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

    pub fn set_feature_permissions(
        &mut self,
        permissions: FeaturePermissions,
    ) {
        self.feature_permissions = permissions;
    }

    pub fn dec_color_state(&self) -> &DecColorState {
        &self.dec_color
    }

    pub fn set_foreground_processes(
        &mut self,
        processes: Option<ForegroundProcessSet>,
    ) {
        if !self.foreground_processes_logged || self.foreground_processes != processes {
            feature::log_foreground_process_probe(&self.feature_permissions, processes.as_ref());
            self.foreground_processes_logged = true;
        }
        self.foreground_processes = processes;
    }

    pub fn drcs_render_glyphs(&self) -> font41::DrcsGlyphMap {
        feature::drcs_render_glyphs(&self.drcs)
    }

    pub fn macro_feature_enabled(&self) -> bool {
        feature::macro_feature_enabled(
            &self.feature_permissions,
            self.foreground_processes.as_ref(),
        )
    }

    fn define_macro(
        &mut self,
        params: vtepp::Params,
        payload: &[u8],
    ) {
        feature::define_macro(
            self.macro_feature_enabled(),
            &mut self.macros,
            params,
            payload,
        );
    }

    fn invoke_macro(
        &mut self,
        id: u16,
    ) {
        let Some(bytes) = feature::invoke_macro(
            self.macro_feature_enabled(),
            &self.macros,
            self.macro_invocation_depth,
            id,
        ) else {
            return;
        };
        self.macro_invocation_depth += 1;
        feature::apply_macro_bytes(self, &bytes);
        self.macro_invocation_depth -= 1;
    }

    pub fn set_cell_dimensions(
        &mut self,
        cell_width: u32,
        cell_height: u32,
    ) {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
    }

    /// Update the scrollback policy and immediately trim any history that
    /// exceeds the new cap. Trimming on update (not lazily on next push)
    /// makes the live-reload path feel responsive — the user shrinks the
    /// limit, the unwanted history goes away on the next render.
    pub fn set_scrollback_policy(
        &mut self,
        limit: u32,
        strict_altscreen_scrollback: bool,
    ) {
        self.strict_altscreen_scrollback = strict_altscreen_scrollback;
        feature::apply_scrollback_limit(&mut self.active, &self.viewport, limit);
        let alt_limit = feature::alt_scrollback_limit(limit, self.strict_altscreen_scrollback);
        feature::apply_scrollback_limit(&mut self.stash, &self.viewport, alt_limit);
    }

    /// Queue a focus-in / focus-out report onto `pending_output` if focus
    /// reporting is currently enabled. Safe to call unconditionally.
    pub fn report_focus_change(
        &mut self,
        focused: bool,
    ) {
        lifecycle_ops::report_focus_change(
            &mut self.pending_output,
            self.modes.c1_mode,
            self.modes.focus_reporting,
            focused,
        );
    }

    pub fn total_rows(&self) -> u32 {
        lifecycle_ops::total_rows(&self.active, &self.viewport)
    }

    pub fn status_line_visible(&self) -> bool {
        lifecycle_ops::status_line_visible(&self.active)
    }

    pub fn status_display_kind(&self) -> StatusDisplayKind {
        self.active.status_display
    }

    pub fn status_line_row(&self) -> Option<&Row> {
        lifecycle_ops::status_line_row(&self.active)
    }

    pub fn indicator_status_text(&self) -> Option<String> {
        (self.active.status_display == StatusDisplayKind::Indicator)
            .then(|| {
                prompt::format_indicator_status(
                    self.current_directory.as_deref(),
                    self.current_prompt_row,
                    &self.command_metas,
                    &self.active,
                )
            })
            .filter(|text| !text.is_empty())
    }

    pub fn status_line_cursor_col(&self) -> Option<u32> {
        lifecycle_ops::status_line_cursor_col(&self.active)
    }

    pub fn set_default_status_display(
        &mut self,
        status_display: StatusDisplayKind,
    ) {
        lifecycle_ops::set_default_status_display(
            &mut self.active,
            &mut self.stash,
            &mut self.viewport,
            &self.palette,
            &mut self.default_status_display,
            status_display,
        );
    }

    /// Drain bytes the terminal itself has queued for the PTY (e.g. OSC 52
    /// query responses). Called by the event loop after each `process` call.
    pub fn take_pending_output(&mut self) -> Vec<u8> {
        lifecycle_ops::take_pending_output(&mut self.pending_output)
    }

    /// Returns true if the app has requested any mouse tracking mode.
    pub fn mouse_tracking_enabled(&self) -> bool {
        lifecycle_ops::mouse_tracking_enabled(&self.modes)
    }

    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn open_search(&mut self) {
        self.search.active = true;
        self.search.query.clear();
        self.search.matches.clear();
        self.search.active_idx = 0;
    }

    pub fn search_active(&self) -> bool {
        self.search.active
    }

    /// Report a mouse event to the foreground app. Returns true if an event
    /// was emitted, false if the current tracking mode suppressed it (so the
    /// caller knows it can handle the event locally instead — e.g. for
    /// scrollback on wheel when tracking is off).
    ///
    /// `col` and `row` are 0-based cell coordinates within the viewport.
    pub fn mouse_report(
        &mut self,
        kind: MouseEventKind,
        button: MouseButton,
        col: u32,
        row: u32,
        mods: MouseModifiers,
    ) -> bool {
        lifecycle_ops::mouse_report(
            &mut self.pending_output,
            self.modes.c1_mode,
            self.modes.mouse_tracking,
            self.modes.mouse_encoding,
            kind,
            button,
            col,
            row,
            mods,
        )
    }

    /// Returns the visible row at the given screen position (0 = top of
    /// viewport).
    pub fn visible_row(
        &self,
        screen_row: u32,
    ) -> &Row {
        let base = selection::active_viewport(&self.active, &self.viewport)
            .top_index(self.active.grid.rows.len());
        &self.active.grid.rows[base + screen_row as usize]
    }

    /// Resolve the hyperlink target at the given viewport cell, or `None`
    /// when the cell is not part of an OSC 8 span. Used by the click handler
    /// to decide whether Ctrl+click should open something.
    pub fn hyperlink_at(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> Option<&str> {
        if screen_row >= self.viewport.rows || screen_col >= self.viewport.cols {
            return None;
        }
        let row = self.visible_row(screen_row);
        let id = row.links.get(screen_col as usize).copied().flatten()?;
        self.hyperlinks.get(id)
    }

    /// Scroll the viewport up (into history). Returns actual lines scrolled.
    pub fn scroll_viewport_up(
        &mut self,
        lines: u32,
    ) -> u32 {
        lifecycle_ops::scroll_viewport_up(&mut self.active, &self.viewport, lines)
    }

    /// Move the viewport to the previous OSC 133 prompt (above the current
    /// viewport top). No-op if none exists above or the active screen has
    /// no shell-integration marks.
    pub fn scroll_to_prev_prompt(&mut self) {
        lifecycle_ops::scroll_to_prev_prompt(&mut self.active, &self.viewport)
    }

    /// Move the viewport to the next OSC 133 prompt (below the current
    /// viewport top). No-op if none exists below — importantly, this
    /// includes the case where the user is at the most recent prompt, so
    /// repeated presses at the live prompt are silent rather than
    /// flickering.
    pub fn scroll_to_next_prompt(&mut self) {
        lifecycle_ops::scroll_to_next_prompt(&mut self.active, &self.viewport)
    }

    /// Scroll the viewport down (toward live). Returns actual lines scrolled.
    pub fn scroll_viewport_down(
        &mut self,
        lines: u32,
    ) -> u32 {
        lifecycle_ops::scroll_viewport_down(&mut self.active, lines)
    }

    /// Reset viewport to the bottom (live terminal).
    pub fn reset_viewport(&mut self) {
        lifecycle_ops::reset_viewport(&mut self.active)
    }

    /// Return images whose row range overlaps the current viewport, with
    /// screen-relative row/col positions. `screen_row` is negative when the
    /// image's top edge is above the viewport so the renderer can offset the
    /// quad upward and let the GPU clip to the visible portion.
    ///
    /// `now` anchors the animation clock: each animated image's frame index
    /// is chosen from `now - placed_at`. Passing the same `now` to every
    /// visible image in a render pass keeps the whole frame temporally
    /// consistent.
    pub fn visible_images(
        &self,
        now: Instant,
    ) -> impl Iterator<Item = VisibleImage<'_>> {
        lifecycle_ops::visible_images(&self.active, &self.viewport, self.cell_height, now)
    }

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

    pub fn take_pending_host_resize(&mut self) -> Option<(u32, u32)> {
        lifecycle_ops::take_pending_host_resize(&mut self.pending_host_resize)
    }

    /// Apply a single parsed VTE action to the terminal state. Called by the
    /// terminal thread with the lock held — the parser runs *outside* the lock
    /// so the SIMD byte-scanning path never blocks rendering.
    ///
    /// Hook/Put/Unhook (DCS accumulation) are handled by the terminal thread
    /// directly and should not be passed here.
    pub fn apply(
        &mut self,
        action: Action<'_>,
    ) {
        let popped_before: usize = self.active.grid.total_popped;

        // VT52 ESC Y direct cursor address: absorb the two parameter bytes that
        // follow the EscDispatch. They arrive as PrintAscii or Execute actions
        // because vtepp is still in ANSI ground state; we intercept them here
        // before the normal dispatch so they are not printed as characters.
        if self.vt52_cursor_addr != Vt52CursorAddr::Idle {
            let byte_opt: Option<u8> = match &action {
                Action::PrintAscii(run) => run.first().copied(),
                Action::Execute(b) => Some(*b),
                _ => None,
            };

            match (self.vt52_cursor_addr, byte_opt) {
                (Vt52CursorAddr::AwaitingRow, Some(b)) => {
                    self.vt52_cursor_addr = Vt52CursorAddr::AwaitingCol(b.saturating_sub(0x20));

                    // The two bytes may arrive batched in one PrintAscii run.
                    // If so, consume the second byte (col) immediately and
                    // then fall through to process any remaining bytes normally.
                    if let Action::PrintAscii(run) = &action
                        && run.len() >= 2
                    {
                        let col = run[1].saturating_sub(0x20) as u32;
                        let row = b.saturating_sub(0x20) as u32;
                        self.active.cursor.row = row.min(self.viewport.rows.saturating_sub(1));
                        self.active.cursor.col = col.min(self.viewport.cols.saturating_sub(1));
                        self.vt52_cursor_addr = Vt52CursorAddr::Idle;
                        // Any bytes after the two position bytes are normal text.
                        if run.len() > 2 {
                            let view = screen::screen_viewport(&self.active, &self.viewport);
                            put_ascii_run(
                                &mut self.active,
                                &view,
                                &run[2..],
                                self.modes.insert_mode,
                            );
                        }
                        self.track_scroll(popped_before);
                        return;
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                (Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                    let col = b.saturating_sub(0x20) as u32;
                    self.active.cursor.row = (row as u32).min(self.viewport.rows.saturating_sub(1));
                    self.active.cursor.col = col.min(self.viewport.cols.saturating_sub(1));
                    self.vt52_cursor_addr = Vt52CursorAddr::Idle;

                    // If more bytes follow in the same PrintAscii run, print them.
                    if let Action::PrintAscii(run) = &action
                        && run.len() > 1
                    {
                        let view = screen::screen_viewport(&self.active, &self.viewport);
                        put_ascii_run(&mut self.active, &view, &run[1..], self.modes.insert_mode);
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                _ => {
                    // Unexpected action type: abort the ESC Y sequence and
                    // fall through to process this action normally.
                    self.vt52_cursor_addr = Vt52CursorAddr::Idle;
                }
            }
        }

        // In VT52 mode, CSI sequences are not valid and should be silently
        // dropped — vtepp still parses them because it doesn't know the
        // terminal mode, but executing them would be wrong.
        if self.modes.vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
            self.track_scroll(popped_before);
            return;
        }

        match action {
            Action::PrintAscii(run) => {
                if self.active.active_display == screen::ActiveDisplay::Status
                    && screen::status_line_writable(&self.active)
                {
                    put_status_ascii_run(&mut self.active, run, self.modes.insert_mode);
                } else {
                    let view = screen::screen_viewport(&self.active, &self.viewport);
                    put_ascii_run(&mut self.active, &view, run, self.modes.insert_mode);
                }
            }
            Action::PrintText(run) => {
                if self.active.active_display == screen::ActiveDisplay::Status
                    && screen::status_line_writable(&self.active)
                {
                    put_status_text_run(&mut self.active, run, self.modes.insert_mode);
                } else {
                    let view = screen::screen_viewport(&self.active, &self.viewport);
                    put_text_run(&mut self.active, &view, run, self.modes.insert_mode);
                }
            }
            Action::Print(c) => {
                if self.active.active_display == screen::ActiveDisplay::Status
                    && screen::status_line_writable(&self.active)
                {
                    put_status_printable(&mut self.active, c, self.modes.insert_mode);
                } else {
                    let view = screen::screen_viewport(&self.active, &self.viewport);
                    put_printable(&mut self.active, &view, c, self.modes.insert_mode);
                }
            }
            Action::Print8Bit(byte) => {
                if self.active.active_display == screen::ActiveDisplay::Status
                    && screen::status_line_writable(&self.active)
                {
                    put_status_8bit_byte(&mut self.active, byte, self.modes.insert_mode);
                } else {
                    let view = screen::screen_viewport(&self.active, &self.viewport);
                    put_8bit_byte(&mut self.active, &view, byte, self.modes.insert_mode);
                }
            }
            Action::Execute(byte) => {
                if self.active.active_display == screen::ActiveDisplay::Status
                    && screen::status_line_writable(&self.active)
                {
                    execute_status(
                        &mut self.active,
                        byte,
                        &mut self.bell_pending,
                        self.modes.newline_mode,
                    );
                } else {
                    let view = screen::screen_viewport(&self.active, &self.viewport);
                    execute(
                        &mut self.active,
                        &view,
                        byte,
                        &mut self.bell_pending,
                        self.modes.newline_mode,
                    );
                }
            }
            Action::CsiDispatch {
                params,
                intermediates,
                action,
            } => {
                if intermediates.as_slice() == b"*" && action == 'z' {
                    let id = params
                        .iter()
                        .next()
                        .and_then(|group| group.first().copied())
                        .unwrap_or(0);
                    self.invoke_macro(id);
                    self.track_scroll(popped_before);
                    return;
                }
                if intermediates.as_slice() == b"," && action == '|' {
                    let mut groups = params.iter();
                    let item = groups
                        .next()
                        .and_then(|group| group.first().copied())
                        .unwrap_or(0);
                    if let (Some(fg), Some(bg)) = (
                        groups.next().and_then(|group| group.first().copied()),
                        groups.next().and_then(|group| group.first().copied()),
                    ) {
                        self.assign_dec_color(item, fg, bg);
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                if intermediates.as_slice() == b"," && action == '}' {
                    let mut groups = params.iter();
                    let item = groups
                        .next()
                        .and_then(|group| group.first().copied())
                        .unwrap_or(0);
                    if let (Some(fg), Some(bg)) = (
                        groups.next().and_then(|group| group.first().copied()),
                        groups.next().and_then(|group| group.first().copied()),
                    ) {
                        self.assign_dec_alternate_text_color(item, fg, bg);
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                if intermediates.as_slice() == b")" && action == '{' {
                    let selection = params
                        .iter()
                        .next()
                        .and_then(|group| group.first().copied())
                        .unwrap_or(0);
                    self.select_dec_lookup_table(selection);
                    self.track_scroll(popped_before);
                    return;
                }
                if intermediates.as_slice() == b"$" && action == 'u' {
                    let mut groups = params.iter();
                    let report_type = groups
                        .next()
                        .and_then(|group| group.first().copied())
                        .unwrap_or(0);
                    match report_type {
                        1 => {
                            let payload = report::dectsr_payload(&self.active);
                            conformance::push_dcs_prefix(
                                &mut self.pending_output,
                                self.modes.c1_mode,
                            );
                            self.pending_output.extend_from_slice(b"1$s");
                            self.pending_output.extend_from_slice(&payload);
                            conformance::push_st(&mut self.pending_output, self.modes.c1_mode);
                        }
                        2 => {
                            let space = groups
                                .next()
                                .and_then(|group| group.first().copied())
                                .unwrap_or(0);
                            if let Some(space) = DecColorSpace::from_param(Some(space)) {
                                let report = report_color_table(&self.dec_color, space);
                                conformance::write_dcs(
                                    &mut self.pending_output,
                                    self.modes.c1_mode,
                                    format_args!("2$s{report}"),
                                );
                            }
                        }
                        _ => {}
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                let mut ctx = CsiContext {
                    screen: &mut self.active,
                    stash: &mut self.stash,
                    viewport: &mut self.viewport,
                    on_alt_screen: &mut self.on_alt_screen,
                    modes: &mut self.modes,
                    kitty_keyboard: &mut self.kitty_keyboard,
                    pending_output: &mut self.pending_output,
                    pending_resize: &mut self.pending_host_resize,
                    cursor_style: &mut self.cursor_style,
                    cell_width: self.cell_width,
                    cell_height: self.cell_height,
                    palette: &mut self.palette,
                    base_palette: &self.base_palette,
                    default_status_display: &mut self.default_status_display,
                    title_stack: &mut self.title_stack,
                    current_title: &mut self.current_title,
                    saved_modes: &mut self.saved_private_modes,
                    current_prompt_row: &mut self.current_prompt_row,
                    bell_pending: &mut self.bell_pending,
                    vt52_cursor_addr: &mut self.vt52_cursor_addr,
                    macros: &mut self.macros,
                    dec_color: &mut self.dec_color,
                    feature_permissions: &self.feature_permissions,
                    foreground_processes: &self.foreground_processes,
                    drcs: &mut self.drcs,
                };
                csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
            }
            Action::EscDispatch {
                intermediates,
                byte,
            } => {
                let mut ctx = EscContext {
                    screen: &mut self.active,
                    stash: &mut self.stash,
                    viewport: &mut self.viewport,
                    on_alt_screen: &mut self.on_alt_screen,
                    modes: &mut self.modes,
                    kitty_keyboard: &mut self.kitty_keyboard,
                    cursor_style: &mut self.cursor_style,
                    current_title: &mut self.current_title,
                    title_stack: &mut self.title_stack,
                    saved_modes: &mut self.saved_private_modes,
                    current_prompt_row: &mut self.current_prompt_row,
                    bell_pending: &mut self.bell_pending,
                    palette: &mut self.palette,
                    base_palette: &self.base_palette,
                    default_status_display: &mut self.default_status_display,
                    pending_output: &mut self.pending_output,
                    vt52_cursor_addr: &mut self.vt52_cursor_addr,
                    macros: &mut self.macros,
                    dec_color: &mut self.dec_color,
                    drcs: &mut self.drcs,
                };
                esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
            }
            Action::OscDispatch(data) => {
                // iTerm2 image protocol rides on OSC 1337. Route it next
                // to the other graphics protocols (kitty on APC, sixel
                // on DCS) rather than through the text-OSC dispatcher,
                // which doesn't carry cursor / cell-size state.
                if let Some(rest) = data.strip_prefix(b"1337;")
                    && graphics::is_iterm_image_cmd(rest)
                {
                    graphics::handle_iterm_graphics(
                        rest,
                        &mut self.iterm_chunked,
                        &mut self.active,
                        &self.viewport,
                        &mut self.next_image_id,
                        self.cell_height,
                        self.cell_width,
                    );
                } else {
                    let mut ctx = OscContext {
                        clipboard: &mut self.clipboard,
                        pending_output: &mut self.pending_output,
                        c1_mode: self.modes.c1_mode,
                        current_directory: &mut self.current_directory,
                        hyperlinks: &mut self.hyperlinks,
                        active_screen: &mut self.active,
                        viewport: &self.viewport,
                        current_title: &mut self.current_title,
                        current_prompt_row: &mut self.current_prompt_row,
                        command_metas: &mut self.command_metas,
                        palette: &self.palette,
                        cell_width: self.cell_width,
                        cell_height: self.cell_height,
                    };
                    handle_osc(&data, &mut ctx);
                }
            }
            Action::ApcDispatch(data) => {
                graphics::handle_kitty_graphics(
                    &data,
                    &mut self.kitty_images,
                    &mut self.kitty_chunked,
                    &mut self.active,
                    &self.viewport,
                    &mut self.next_image_id,
                    self.cell_height,
                    self.cell_width,
                    self.modes.c1_mode,
                    &mut self.pending_output,
                );
            }
            // Hook/Put/Unhook are accumulated by the terminal thread and
            // dispatched via place_sixel_image — they never reach here.
            Action::Hook { .. } | Action::Put(_) | Action::Unhook => {}
        }

        self.track_scroll(popped_before);
    }

    /// Place a fully-decoded sixel image at the current cursor position.
    /// Called by the terminal thread *after* parsing the sixel data outside
    /// the lock, so the CPU-intensive decode doesn't block rendering.
    pub fn place_sixel_image(
        &mut self,
        image: image41::DecodedImage,
    ) {
        let popped_before: usize = self.active.grid.total_popped;

        let id = self.next_image_id;
        self.next_image_id += 1;
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
        lifecycle_ops::track_scroll(&mut self.active, &mut self.command_metas, popped_before)
    }
}

/// Handle to a running terminal thread. Signals the thread to stop on drop.
pub struct TerminalThread {
    stop: Arc<AtomicBool>,
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
        output_ready: Box<dyn Fn() + Send + Sync>,
        host_resize: Box<dyn Fn(u32, u32) + Send + Sync>,
    ) {
        if self.thread_handle.get().is_some() {
            error!("terminal thread already running");
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_ = stop.clone();
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
                    stop_,
                    render_thread_handle,
                    startup_redraw,
                    tee_read,
                    output_ready,
                    host_resize,
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
mod tests {
    use std::path::PathBuf;

    use clip41::ClipboardKind;
    use palette::Srgb;
    use pty_pipe41::ForegroundProgram;
    use vtepp::Parser;

    use super::*;
    use crate::io::clipboard::paste;
    use crate::io::clipboard::paste_from_clipboard;
    use crate::selection::SelectionMode;

    /// Test wrapper that bundles a `Terminal` with its own `Parser` so tests
    /// can call `.process()` the same way as before the parser was extracted.
    /// Deref/DerefMut coerce to `&Terminal`/`&mut Terminal` transparently.
    struct TestTerm {
        inner: Terminal,
        parser: Parser,
    }

    impl TestTerm {
        fn new(
            cols: u32,
            rows: u32,
            scrollback: u32,
            cell_h: u32,
            cell_w: u32,
        ) -> Self {
            Self {
                inner: Terminal::new(
                    cols,
                    rows,
                    scrollback,
                    StatusDisplayKind::None,
                    false,
                    FeaturePermissions::default(),
                    cell_h,
                    cell_w,
                    ColorPalette::default(),
                ),
                parser: Parser::new(),
            }
        }

        fn new_with_alt_scrollback_policy(
            cols: u32,
            rows: u32,
            scrollback: u32,
            strict_altscreen_scrollback: bool,
            cell_h: u32,
            cell_w: u32,
        ) -> Self {
            Self {
                inner: Terminal::new(
                    cols,
                    rows,
                    scrollback,
                    StatusDisplayKind::None,
                    strict_altscreen_scrollback,
                    FeaturePermissions::default(),
                    cell_h,
                    cell_w,
                    ColorPalette::default(),
                ),
                parser: Parser::new(),
            }
        }

        fn process(
            &mut self,
            data: &[u8],
        ) {
            let mut hooks: Vec<dcs::HookState> = vec![];
            for action in self.parser.parse(data) {
                match action {
                    Action::Hook {
                        params,
                        intermediates,
                        action,
                    } => dcs::push_hook_state(&mut hooks, params, intermediates, action),
                    Action::Put(chunk) => dcs::append_hook_bytes(&mut hooks, chunk),
                    Action::Unhook => {
                        let hook = hooks.pop().expect("hook bytes");
                        dcs::dispatch_hook(hook, &mut self.inner);
                    }
                    action => self.inner.apply(action),
                }
            }
        }

        fn set_foreground_programs(
            &mut self,
            paths: &[&str],
        ) {
            let programs = paths
                .iter()
                .map(|path| {
                    ForegroundProgram::from_exe_path(PathBuf::from(path)).expect("exe path")
                })
                .collect();
            self.inner
                .set_foreground_processes(Some(ForegroundProcessSet { programs }));
        }

        fn set_macro_permissions(
            &mut self,
            macros: ProgramAllowlist,
        ) {
            self.inner
                .set_feature_permissions(FeaturePermissions { macros });
        }
    }

    impl std::ops::Deref for TestTerm {
        type Target = Terminal;

        fn deref(&self) -> &Terminal {
            &self.inner
        }
    }

    impl std::ops::DerefMut for TestTerm {
        fn deref_mut(&mut self) -> &mut Terminal {
            &mut self.inner
        }
    }

    fn visible_text(term: &Terminal) -> String {
        let mut s = String::new();
        for r in 0..term.viewport.rows {
            let row = term.visible_row(r);
            for cell in &row.cells {
                s.push_str(cell);
            }
            s.push('\n');
        }
        s
    }

    /// Like [`visible_text`] but with row boundaries removed, so assertions
    /// can match logical content that crossed a soft-wrap.
    fn visible_text_flat(term: &Terminal) -> String {
        visible_text(term).replace('\n', "")
    }

    fn status_line_text(term: &Terminal) -> Option<String> {
        term.status_line_row().map(|row| row.cells.concat())
    }

    #[test]
    fn indicator_status_formats_path_and_running_command() {
        let mut term = TestTerm::new(16, 4, 100, 16, 8);
        term.current_directory = Some(PathBuf::from("/tmp/project"));
        term.process(b"\x1b[1$~");
        term.process(b"\x1b]133;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]133;B\x07");
        term.process(b"cargo test");
        term.process(b"\x1b]133;C\x07");

        assert_eq!(
            term.indicator_status_text().as_deref(),
            Some("/ > tmp > project > cargo test")
        );
    }

    #[test]
    fn indicator_status_omits_command_when_not_running() {
        let mut term = TestTerm::new(16, 4, 100, 16, 8);
        term.current_directory = Some(PathBuf::from("/tmp/project"));
        term.process(b"\x1b[1$~");
        term.process(b"\x1b]133;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]133;B\x07");
        term.process(b"cargo test");
        term.process(b"\x1b]133;C\x07");
        term.process(b"\x1b]133;D;0\x07");

        assert_eq!(
            term.indicator_status_text().as_deref(),
            Some("/ > tmp > project")
        );
    }

    #[test]
    fn alt_screen_1049_hides_primary_and_restores() {
        let mut term = TestTerm::new(8, 4, 100, 16, 8);
        term.process(b"hello");
        term.process(b"\x1b[?1049h");

        // Alt is active, blank, cursor at (0,0).
        assert!(term.on_alt_screen);
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        assert!(
            !visible_text(&term).contains("hello"),
            "alt screen should be blank, got {:?}",
            visible_text(&term)
        );

        term.process(b"WORLD");
        assert!(visible_text(&term).contains("WORLD"));

        term.process(b"\x1b[?1049l");

        // Back on primary with saved cursor restored and original content visible.
        assert!(!term.on_alt_screen);
        assert!(visible_text(&term).contains("hello"));
        assert_eq!(term.active.cursor.col, 5);
        assert_eq!(term.active.cursor.row, 0);
    }

    #[test]
    fn decssdt_uses_one_physical_row_for_status_line() {
        let mut term = TestTerm::new(8, 4, 100, 16, 8);

        assert_eq!(term.viewport.rows, 4);
        assert_eq!(term.total_rows(), 4);

        term.process(b"\x1b[2$~");

        assert!(term.status_line_visible());
        assert_eq!(term.viewport.rows, 3);
        assert_eq!(term.total_rows(), 4);

        term.process(b"\x1b[0$~");

        assert!(!term.status_line_visible());
        assert_eq!(term.viewport.rows, 4);
        assert_eq!(term.total_rows(), 4);
    }

    #[test]
    fn decsasd_routes_printing_to_host_writable_status_line() {
        let mut term = TestTerm::new(8, 4, 100, 16, 8);

        term.process(b"\x1b[2$~");
        term.process(b"\x1b[1$}");
        term.process(b"STATUS");
        term.process(b"\x1b[0$}");
        term.process(b"main");

        assert_eq!(status_line_text(&term).unwrap().trim_end(), "STATUS");
        assert!(visible_text(&term).contains("main"));
        assert!(!visible_text(&term).contains("STATUS"));
    }

    #[test]
    fn visible_screen_tracks_live_bottom_after_scrollback_growth() {
        let mut term = TestTerm::new(8, 2, 100, 16, 8);
        term.process(b"111111112222222233333333");
        let text = visible_text(&term);
        assert!(
            text.contains("22222222"),
            "visible text should include the second wrapped row: {text:?}"
        );
        assert!(
            text.contains("33333333"),
            "visible text should include the live bottom row: {text:?}"
        );
    }

    #[test]
    fn alt_screen_1049_resize_preserves_primary() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"primary-content");
        term.process(b"\x1b[?1049h");
        term.process(b"ALT");

        // Resize while on alt — primary must survive with its content.
        term.resize(12, 5);
        term.process(b"\x1b[?1049l");

        // After reflow, the primary text may straddle a soft-wrap boundary.
        let flat = visible_text_flat(&term);
        assert!(
            flat.contains("primary-content"),
            "primary content lost through resize: {:?}",
            flat
        );
        assert_eq!(term.viewport.cols, 12);
        assert_eq!(term.viewport.rows, 5);
    }

    #[test]
    fn alt_screen_inherits_scrollback_by_default() {
        let mut term = TestTerm::new(8, 3, 100, 16, 8);
        term.process(b"\x1b[?1049h");

        for _ in 0..10 {
            term.process(b"line\n");
        }
        assert!(term.active.grid.scrollback_len(&term.viewport) > 0);
    }

    #[test]
    fn strict_alt_screen_has_no_scrollback() {
        let mut term = TestTerm::new_with_alt_scrollback_policy(8, 3, 100, true, 16, 8);
        term.process(b"\x1b[?1049h");

        for _ in 0..10 {
            term.process(b"line\n");
        }
        assert_eq!(term.active.grid.scrollback_len(&term.viewport), 0);
    }

    #[test]
    fn decsc_decrc_restores_cursor_and_colors() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b[3;5H"); // move to row 3 col 5
        term.process(b"\x1b[31m"); // red fg
        term.process(b"\x1b7"); // DECSC
        let saved_fg = term.active.fg;
        term.process(b"\x1b[1;1H\x1b[32m"); // move + change color
        term.process(b"\x1b8"); // DECRC

        assert_eq!(term.active.cursor.row, 2);
        assert_eq!(term.active.cursor.col, 4);
        assert_eq!(term.active.fg, saved_fg);
    }

    #[test]
    fn mode_47_does_not_save_cursor() {
        let mut term = TestTerm::new(8, 3, 100, 16, 8);
        term.process(b"\x1b[2;3H"); // row 2 col 3
        term.process(b"\x1b[?47h");
        term.process(b"\x1b[1;1H"); // move on alt
        term.process(b"\x1b[?47l");

        // ?47 doesn't save/restore cursor — we land wherever we left primary.
        // Primary's cursor before the switch was (row=1, col=2); ?47 preserves
        // the *primary screen's* cursor (untouched because we swapped away
        // before moving), so we should be back at (1,2).
        assert_eq!(term.active.cursor.row, 1);
        assert_eq!(term.active.cursor.col, 2);
    }

    #[test]
    fn decset_1006_switches_to_sgr_encoding() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1006h");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::Sgr);
        term.process(b"\x1b[?1006l");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::Default);
    }

    #[test]
    fn decset_1002_enables_button_event_tracking() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1002h");
        assert_eq!(term.modes.mouse_tracking, MouseTracking::ButtonEvent);
        term.process(b"\x1b[?1002l");
        assert_eq!(term.modes.mouse_tracking, MouseTracking::Off);
    }

    #[test]
    fn tracking_mode_is_replaced_not_layered() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1000h");
        term.process(b"\x1b[?1003h");
        assert_eq!(term.modes.mouse_tracking, MouseTracking::AnyEvent);
    }

    #[test]
    fn mouse_report_emits_into_pending_output() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1000h\x1b[?1006h");
        let emitted = term.mouse_report(
            MouseEventKind::Press,
            MouseButton::Left,
            4,
            9,
            MouseModifiers::default(),
        );
        assert!(emitted);
        // Coordinates pushed are 1-based.
        assert_eq!(term.take_pending_output(), b"\x1b[<0;5;10M");
    }

    #[test]
    fn mouse_report_uses_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1000h\x1b[?1006h\x1b G");
        let emitted = term.mouse_report(
            MouseEventKind::Press,
            MouseButton::Left,
            4,
            9,
            MouseModifiers::default(),
        );
        assert!(emitted);
        assert_eq!(term.take_pending_output(), b"\x9b<0;5;10M");
    }

    #[test]
    fn mouse_report_returns_false_when_tracking_off() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let emitted = term.mouse_report(
            MouseEventKind::Press,
            MouseButton::Left,
            0,
            0,
            MouseModifiers::default(),
        );
        assert!(!emitted);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Bracketed paste (mode 2004) ----

    #[test]
    fn paste_default_is_raw() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        paste(
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            "hello\n",
        );
        assert_eq!(term.take_pending_output(), b"hello\n");
    }

    #[test]
    fn paste_wraps_when_mode_2004_enabled() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        assert!(term.modes.bracketed_paste);
        paste(
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            "hello\n",
        );
        assert_eq!(term.take_pending_output(), b"\x1b[200~hello\n\x1b[201~");
    }

    #[test]
    fn paste_wraps_with_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h\x1b G");
        paste(
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            "hello\n",
        );
        assert_eq!(term.take_pending_output(), b"\x9b200~hello\n\x9b201~");
    }

    #[test]
    fn decrst_2004_disables_bracketed_paste() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        term.process(b"\x1b[?2004l");
        assert!(!term.modes.bracketed_paste);
        paste(
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            "hi",
        );
        assert_eq!(term.take_pending_output(), b"hi");
    }

    #[test]
    fn paste_scrubs_embedded_end_marker() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        // The clipboard tries to break out of the bracket — the injected
        // `\x1b[201~` is stripped and everything else comes through.
        paste(
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            "evil\x1b[201~injection",
        );
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[200~evilinjection\x1b[201~"
        );
    }

    // ---- Sixel image placement ----

    fn place_image(
        term: &mut Terminal,
        row: usize,
        col: u32,
        height_px: u32,
    ) -> u64 {
        let id = term.next_image_id;
        term.next_image_id += 1;
        term.active.images.insert(
            id,
            PlacedImage {
                image: image41::DecodedImage::single_frame(1, height_px, vec![]),
                id,
                row,
                col,
                display_width: 1,
                display_height: height_px,
                placed_at: Instant::now(),
            },
        );
        id
    }

    #[test]
    fn sixel_redraw_at_same_position_replaces_previous() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        // cell_height = 16, so 32px = 2 grid rows.
        let id_a = place_image(&mut term, 5, 0, 32);
        self::image::remove_overlapping(&mut term.active.images, 5, 2, 0, 16);
        // The manual sweep used by the Unhook handler — call it to verify
        // the behavior the handler relies on.
        assert!(!term.active.images.contains_key(&id_a));
    }

    #[test]
    fn sixel_different_columns_coexist() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let id_a = place_image(&mut term, 5, 0, 32);
        let id_b = place_image(&mut term, 5, 10, 32);
        // Dedup sweep for a new image at col 0 must not touch col 10.
        self::image::remove_overlapping(&mut term.active.images, 5, 2, 0, 16);
        assert!(!term.active.images.contains_key(&id_a));
        assert!(term.active.images.contains_key(&id_b));
    }

    #[test]
    fn scroll_region_shifts_images_up() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        // Set scroll region rows 0..=9 (whole screen is the region when
        // we use DECSTBM with a custom bottom). Place image at absolute
        // row 5, then issue CSI M (delete line) from row 0 to scroll the
        // region up by 2.
        term.process(b"\x1b[1;8r"); // DECSTBM top=1, bottom=8 (0-indexed: 0..=7)
        let id = place_image(&mut term, 5, 0, 16);
        term.process(b"\x1b[H"); // cursor home
        term.process(b"\x1b[2M"); // delete 2 lines → scroll_up_in_region n=2
        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 3, "image should shift up by 2 rows");
    }

    #[test]
    fn scroll_region_drops_image_pushed_out_of_top() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        term.process(b"\x1b[1;8r");
        let id = place_image(&mut term, 2, 0, 16);
        term.process(b"\x1b[H");
        term.process(b"\x1b[5M"); // 5 > available space above → image goes past top
        assert!(
            !term.active.images.contains_key(&id),
            "image scrolled past region top should be dropped"
        );
    }

    #[test]
    fn scroll_region_preserves_images_outside_region() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        term.process(b"\x1b[2;5r"); // region rows 1..=4 (abs 1..=4 with no scrollback)
        let id = place_image(&mut term, 8, 0, 16); // below region
        term.process(b"\x1b[2H"); // move into region
        term.process(b"\x1b[2M"); // scroll up inside region
        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 8, "image outside region is unaffected");
    }

    #[test]
    fn ed_2_removes_visible_images() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 3, 0, 16);
        term.process(b"\x1b[2J"); // ED 2 — clear entire screen
        assert!(
            !term.active.images.contains_key(&id),
            "ED 2 should drop images on the visible area"
        );
    }

    #[test]
    fn alt_screen_entry_clears_alt_images() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        // Enter alt once and place an image on the alt buffer.
        term.process(b"\x1b[?1049h");
        assert!(term.on_alt_screen);
        let id = place_image(&mut term, 3, 0, 16);
        // Leave alt — clear_visible should drop the alt's image.
        term.process(b"\x1b[?1049l");
        assert!(!term.on_alt_screen);
        // Re-enter alt; the alt buffer (now `active` again) must not
        // have the old image.
        term.process(b"\x1b[?1049h");
        assert!(!term.active.images.contains_key(&id));
    }

    #[test]
    fn alt_screen_reentry_resets_cursor_and_pen_state() {
        let mut term = TestTerm::new(10, 4, 0, 16, 8);
        term.process(b"\x1b[?1049h");
        term.process(b"\x1b[3;4H\x1b[30;46m");
        term.process(b"\x1b[?25l");
        term.process(b"\x1b[?1049l");
        term.process(b"\x1b[?1049h");

        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.fg, term.active.grid.default_fg);
        assert_eq!(term.active.bg, term.active.grid.default_bg);
        assert_eq!(term.active.attrs, font41::attrs::CellAttrs::default());
        assert_eq!(term.active.underline, font41::attrs::UnderlineStyle::None);
        assert_eq!(term.active.underline_color, None);
        assert!(term.active.cursor_visible);
    }

    // ---- Synchronized output (mode 2026) ----

    #[test]
    fn bsu_sets_synchronized_update_flag() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        assert!(!term.is_synchronized_update_active());
        term.process(b"\x1b[?2026h");
        assert!(term.is_synchronized_update_active());
    }

    #[test]
    fn esu_clears_synchronized_update_flag() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2026h");
        term.process(b"\x1b[?2026l");
        assert!(!term.is_synchronized_update_active());
        assert!(term.modes.synchronized_update_since.is_none());
    }

    #[test]
    fn synchronized_update_expires_after_timeout() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2026h");
        // Back-date the start so the safety deadline has already passed —
        // avoids a real sleep in the test but exercises the timeout path.
        term.modes.synchronized_update_since =
            Some(Instant::now() - SYNCHRONIZED_UPDATE_TIMEOUT - Duration::from_millis(1));
        assert!(!term.is_synchronized_update_active());
    }

    #[test]
    fn paste_from_clipboard_round_trips() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        term.clipboard.set(ClipboardKind::Clipboard, "hello");
        paste_from_clipboard(
            &mut term.inner.clipboard,
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            ClipboardKind::Clipboard,
        );
        assert_eq!(term.take_pending_output(), b"hello");
    }

    #[test]
    fn paste_from_clipboard_ignores_empty_selection() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        paste_from_clipboard(
            &mut term.inner.clipboard,
            &mut term.inner.pending_output,
            term.inner.modes.c1_mode,
            term.inner.modes.bracketed_paste,
            ClipboardKind::Clipboard,
        );
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Selection ----

    fn write_row(
        term: &mut TestTerm,
        screen_row: u32,
        text: &str,
    ) {
        term.process(format!("\x1b[{};1H", screen_row + 1).as_bytes());
        term.process(text.as_bytes());
    }

    #[test]
    fn start_selection_char_mode_is_empty_initially() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            1,
            SelectionMode::Char,
        );
        assert!(term.selection.is_some());
        assert!(!term.has_selection()); // empty Char = not "has selection"
    }

    #[test]
    fn char_selection_extend_produces_text() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        assert_eq!(
            selection::selection_text(term.inner.selection.as_ref(), &term.inner.active,)
                .as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn word_selection_snaps_to_boundaries() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
            SelectionMode::Word,
        );
        assert_eq!(
            selection::selection_text(term.inner.selection.as_ref(), &term.inner.active,)
                .as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn line_selection_covers_full_row() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            5,
            0,
            SelectionMode::Line,
        );
        // Line selection trims trailing padding spaces.
        assert_eq!(
            selection::selection_text(term.inner.selection.as_ref(), &term.inner.active,)
                .as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn selection_spans_rows_with_newline_separator() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abc");
        write_row(&mut term, 1, "def");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            2,
            1,
        );
        // Intermediate row trims trailing spaces, \n joins hard line breaks.
        assert_eq!(
            selection::selection_text(term.inner.selection.as_ref(), &term.inner.active,)
                .as_deref(),
            Some("abc\ndef")
        );
    }

    #[test]
    fn selection_drags_backwards_flips_anchor_head() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            8,
            0,
            SelectionMode::Word,
        ); // in "world"
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
        ); // drag back into "hello"
        assert_eq!(
            selection::selection_text(term.inner.selection.as_ref(), &term.inner.active,)
                .as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn is_cell_selected_matches_contains() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abcdefghij");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            5,
            0,
        );
        assert!(!selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            1,
            0
        ));
        assert!(selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            2,
        ));
        assert!(selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            5,
        ));
        assert!(!selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            1,
            3,
        ));
    }

    #[test]
    fn search_finds_exact_case_sensitive_matches() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "abc foo xyz FOO bar");
        term.open_search();
        assert!(term.search_active());
        term.active.offset = selection::search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        // Only the lowercase occurrence matches.
        assert_eq!(term.search.matches.len(), 1);
        let m = term.search.matches[0];
        assert_eq!((m.start_col, m.end_col), (4, 6));

        assert!(selection::is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            4
        ));
        assert!(selection::is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            5
        ));
        assert!(selection::is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!selection::is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            3
        ));
        assert!(!selection::is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            7
        ));
        assert!(!selection::is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            12
        ));
    }

    #[test]
    fn search_close_clears_state() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.open_search();
        term.active.offset = selection::search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "hello",
        );
        assert_eq!(term.search.matches.len(), 1);
        selection::close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(!term.search_active());
        assert!(term.search.matches.is_empty());
        assert!(term.search.query.is_empty());
    }

    #[test]
    fn search_close_promotes_active_match_to_selection() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "abc foo def");
        term.open_search();
        term.active.offset = selection::search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        selection::close_search(&mut term.inner.search, &mut term.inner.selection);
        // Selection now covers the match columns 4..=6.
        assert!(selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            4
        ));
        assert!(selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            5
        ));
        assert!(selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            3
        ));
        assert!(!selection::is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            7
        ));
    }

    #[test]
    fn search_close_without_matches_leaves_prior_selection() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        assert!(term.has_selection());
        term.open_search();
        term.active.offset = selection::search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "nonexistent",
        );
        selection::close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(selection::is_cell_selected(
            term.selection.as_ref(),
            &term.active,
            &term.inner.viewport,
            0,
            0
        ));
        assert!(selection::is_cell_selected(
            term.selection.as_ref(),
            &term.active,
            &term.inner.viewport,
            0,
            4
        ));
    }

    #[test]
    fn search_next_wraps_around() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "foo");
        write_row(&mut term, 1, "foo");
        write_row(&mut term, 2, "foo");
        term.open_search();
        term.active.offset = selection::search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        assert_eq!(term.search.matches.len(), 3);
        let start_idx = term.search.active_idx;
        term.active.offset = selection::search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        term.active.offset = selection::search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        term.active.offset = selection::search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        assert_eq!(term.search.active_idx, start_idx);
    }

    #[test]
    fn search_backspace_trims_query_and_rescans() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "fox foxy fo");
        term.open_search();
        term.active.offset = selection::search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foxy",
        );
        assert_eq!(term.search.matches.len(), 1);
        term.active.offset = selection::search_backspace(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        assert_eq!(term.search.matches.len(), 2);
    }

    #[test]
    fn copy_selection_writes_to_clipboard() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        write_row(&mut term, 0, "copy-me");

        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            6,
            0,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            6,
            0,
        );
        selection::copy_selection(
            &mut term.inner.clipboard,
            term.inner.selection.as_ref(),
            &term.inner.active,
            ClipboardKind::Clipboard,
        );
        assert_eq!(
            term.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("copy-me")
        );
        // Selection survives copy (callers clear explicitly).
        assert!(term.has_selection());
    }

    #[test]
    fn clear_selection_drops_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.inner.selection = selection::start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = selection::extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        term.inner.selection = None;
        assert!(term.inner.selection.is_none());
        assert!(
            selection::selection_text(term.inner.selection.as_ref(), &term.inner.active).is_none()
        );
    }

    // ---- OSC 7 cwd ----

    #[test]
    fn osc_7_updates_terminal_cwd() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]7;file://localhost/tmp/work\x1b\\");
        assert_eq!(
            term.current_directory.as_deref(),
            Some(std::path::Path::new("/tmp/work"))
        );
    }

    // ---- OSC 8 hyperlinks ----

    #[test]
    fn osc_8_attaches_link_to_subsequent_cells() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\after");
        assert_eq!(term.hyperlink_at(0, 0), Some("https://example.com"));
        assert_eq!(term.hyperlink_at(0, 3), Some("https://example.com"));
        // First cell after the closing OSC 8 carries no link.
        assert_eq!(term.hyperlink_at(0, 4), None);
    }

    #[test]
    fn osc_8_close_clears_current_link() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]8;;https://example.com\x1b\\");
        assert!(term.active.current_hyperlink.is_some());
        term.process(b"\x1b]8;;\x1b\\");
        assert!(term.active.current_hyperlink.is_none());
    }

    // ---- Kitty keyboard protocol ----

    #[test]
    fn kitty_push_records_flags() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>1u");
        assert_eq!(
            term.kitty_keyboard.current(),
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
        );
    }

    #[test]
    fn kitty_pop_default_unwinds_one_frame() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>1u\x1b[<u");
        assert!(term.kitty_keyboard.current().is_empty());
    }

    #[test]
    fn kitty_query_writes_response_to_pending_output() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>3u\x1b[?u");
        assert_eq!(term.take_pending_output(), b"\x1b[?3u");
    }

    // ---- Cursor style (DECSCUSR) ----

    #[test]
    fn decscusr_sets_steady_block() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[2 q");
        assert_eq!(
            term.cursor_style,
            CursorStyle {
                shape: CursorShape::Block,
                blink: false,
            }
        );
    }

    #[test]
    fn decscusr_sets_blinking_beam() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[5 q");
        assert_eq!(
            term.cursor_style,
            CursorStyle {
                shape: CursorShape::Beam,
                blink: true,
            }
        );
    }

    #[test]
    fn config_default_cursor_style_overrides_xterm_default() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_default_cursor_style(CursorStyle {
            shape: CursorShape::Underline,
            blink: false,
        });
        assert_eq!(term.cursor_style.shape, CursorShape::Underline);
        assert!(!term.cursor_style.blink);
    }

    // ---- Focus reporting (?1004) ----

    #[test]
    fn focus_change_silent_when_reporting_disabled() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.report_focus_change(true);
        assert!(term.take_pending_output().is_empty());
    }

    #[test]
    fn focus_change_emits_csi_i_o_when_enabled() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h");
        term.report_focus_change(true);
        term.report_focus_change(false);
        assert_eq!(term.take_pending_output(), b"\x1b[I\x1b[O");
    }

    #[test]
    fn focus_change_uses_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b G");
        term.report_focus_change(true);
        term.report_focus_change(false);
        assert_eq!(term.take_pending_output(), b"\x9bI\x9bO");
    }

    #[test]
    fn decrst_1004_disables_focus_reporting() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b[?1004l");
        term.report_focus_change(true);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Live config reload effects ----

    // ---- Title (OSC 0 / OSC 2) ----

    #[test]
    fn osc_2_updates_terminal_title() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]2;build ok\x1b\\");
        assert_eq!(term.current_title.as_deref(), Some("build ok"));
    }

    #[test]
    fn osc_0_updates_terminal_title() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]0;hi\x1b\\");
        assert_eq!(term.current_title.as_deref(), Some("hi"));
    }

    // ---- Bell ----

    #[test]
    fn bel_byte_sets_bell_pending() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        assert!(!term.take_bell_pending());
        term.process(b"\x07");
        assert!(term.take_bell_pending());
        // Take is destructive — second poll within the same frame returns false.
        assert!(!term.take_bell_pending());
    }

    #[test]
    fn bel_inside_text_is_caught() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"hi\x07there");
        assert!(term.take_bell_pending());
    }

    #[test]
    fn bel_does_not_advance_cursor() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x07");
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.cursor.row, 0);
    }

    // ---- Live config reload ----

    #[test]
    fn set_scrollback_limit_takes_effect_on_next_push() {
        let mut term = TestTerm::new(8, 2, 100, 16, 8);
        // Burn through enough lines to trigger trim-on-push later.
        for i in 0..50u32 {
            term.process(format!("line{i}\n").as_bytes());
        }
        term.set_scrollback_policy(5, false);
        // Push more lines so the per-row trim path runs against the new limit.
        for i in 0..20u32 {
            term.process(format!("after{i}\n").as_bytes());
        }
        // Visible rows + scrollback budget caps total grid rows.
        let max_expected = term.viewport.rows as usize + 5;
        assert!(
            term.active.grid.rows.len() <= max_expected,
            "grid kept {} rows after lowering limit to 5 (max {})",
            term.active.grid.rows.len(),
            max_expected,
        );
    }

    #[test]
    fn set_palette_updates_grid_defaults_and_existing_default_cells() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"ab");
        let old = term.palette.clone();
        let mut new = old.clone();
        new.fg = Srgb::new(10, 20, 30);
        new.bg = Srgb::new(40, 50, 60);

        term.set_palette(new.clone());

        assert_eq!(term.palette.fg, new.fg);
        assert_eq!(term.palette.bg, new.bg);
        assert_eq!(term.active.grid.default_fg, new.fg);
        assert_eq!(term.active.grid.default_bg, new.bg);
        assert_eq!(term.active.grid.rows[0].fg[0], new.fg);
        assert_eq!(term.active.grid.rows[0].bg[0], new.bg);
        assert_eq!(term.active.fg, new.fg);
        assert_eq!(term.active.bg, new.bg);
    }

    #[test]
    fn set_palette_preserves_non_default_foreground_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[31mx");
        let old_fg = term.active.grid.rows[0].fg[0];
        let mut new = term.palette.clone();
        new.fg = Srgb::new(10, 20, 30);
        new.bg = Srgb::new(40, 50, 60);

        term.set_palette(new);

        assert_eq!(term.active.grid.rows[0].fg[0], old_fg);
    }

    // ---- OSC 133 shell integration + prompt navigation ----

    /// Drive a scripted shell session that emits OSC 133 marks into the
    /// terminal, producing enough rows to land some prompts in scrollback.
    /// Each invocation simulates one prompt + one command.
    fn emit_prompt(
        term: &mut TestTerm,
        label: &str,
        output_lines: u32,
        exit: i32,
    ) {
        term.process(b"\x1b]133;A\x1b\\");
        term.process(label.as_bytes());
        term.process(b"\x1b]133;B\x1b\\");
        term.process(b"\n\x1b]133;C\x1b\\");
        for i in 0..output_lines {
            term.process(format!("out{i}\n").as_bytes());
        }
        term.process(format!("\x1b]133;D;{exit}\x1b\\").as_bytes());
    }

    #[test]
    fn osc_133_stamps_exit_status_onto_prompt_row_through_process() {
        let mut term = TestTerm::new(10, 6, 100, 16, 8);
        emit_prompt(&mut term, "$ ls", 1, 0);
        // Prompt landed on row 0 (the first row written to). Exit status
        // should be stamped there, not on the D row further down.
        let prompt_row = &term.active.grid.rows[0];
        assert!(prompt_row.prompt_start);
        assert_eq!(prompt_row.exit_status, Some(0));
    }

    #[test]
    fn osc_133_exit_status_survives_scrollback_pop() {
        // Small viewport so prompts quickly move into scrollback.
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        emit_prompt(&mut term, "$ first", 2, 0);
        emit_prompt(&mut term, "$ second", 2, 1);
        // Both prompt rows are now somewhere in scrollback; find the
        // first one and verify its exit status.
        let first = term
            .active
            .grid
            .rows
            .iter()
            .find(|r| r.prompt_start)
            .expect("first prompt row survived");
        assert_eq!(first.exit_status, Some(0));
    }

    #[test]
    fn scroll_to_prev_prompt_moves_viewport() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        // Starts at live (offset = 0). Prev should scroll back to an
        // earlier prompt.
        let before = term.active.offset;
        term.scroll_to_prev_prompt();
        assert!(
            term.active.offset > before,
            "prev should scroll the viewport into history"
        );
    }

    #[test]
    fn scroll_to_prev_prompt_silent_with_no_marks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"plain\noutput\nwithout\nshell integration\n");
        let before = term.active.offset;
        term.scroll_to_prev_prompt();
        assert_eq!(
            term.active.offset, before,
            "no marks → offset must not change"
        );
    }

    #[test]
    fn scroll_to_next_prompt_walks_forward() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        // Scroll all the way back, then walk forward.
        term.active.offset = term.active.grid.scrollback_len(&term.viewport);
        let start = term.active.offset;
        term.scroll_to_next_prompt();
        assert!(
            term.active.offset < start,
            "next should move the viewport toward live"
        );
    }

    #[test]
    fn scroll_to_next_prompt_silent_at_last_prompt() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ only", 3, 0);
        // At live there's no next prompt — repeated presses shouldn't
        // bounce the viewport.
        let before = term.active.offset;
        term.scroll_to_next_prompt();
        assert_eq!(term.active.offset, before);
    }

    #[test]
    fn prompt_marks_ride_reflow_shrink_then_grow() {
        // 20-col viewport, prompt + long command that will soft-wrap when
        // shrunk. After a shrink/grow round-trip the mark must end up
        // exactly once, on the head of the (re-merged) logical line.
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x1b\\");
        term.process(b"$ this is a long prompt line");
        term.process(b"\x1b]133;B\x1b\\\n");
        term.process(b"\x1b]133;D;0\x1b\\");

        term.resize(8, 6); // forces soft-wrap
        term.resize(20, 6); // re-merge

        let prompt_rows: Vec<_> = term
            .active
            .grid
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.prompt_start)
            .collect();
        assert_eq!(
            prompt_rows.len(),
            1,
            "exactly one prompt mark after reflow round-trip, got {}: {:#?}",
            prompt_rows.len(),
            prompt_rows
                .iter()
                .map(|(i, r)| (i, r.cells.iter().map(|c| c.as_str()).collect::<String>()))
                .collect::<Vec<_>>()
        );
        // Exit status rode along with the prompt mark.
        assert_eq!(prompt_rows[0].1.exit_status, Some(0));
    }

    #[test]
    fn prompt_marks_do_not_duplicate_on_continuation_rows() {
        // After a shrink, marks must live only on the *head* of each
        // logical line — the row that is either the first row or comes
        // right after a row whose `wrapped` flag is false. (`wrapped=true`
        // means "this row spills into the next one", so the head of a
        // soft-wrapped logical line is the one with `wrapped=true` whose
        // predecessor has `wrapped=false`.)
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x1b\\");
        term.process(b"$ a command that will definitely wrap");
        term.process(b"\x1b]133;B\x1b\\\n");

        term.resize(8, 6);

        for i in 0..term.active.grid.rows.len() {
            let is_head = i == 0 || !term.active.grid.rows[i - 1].wrapped;
            if !is_head {
                let row = &term.active.grid.rows[i];
                assert!(
                    !row.prompt_start,
                    "continuation row {i} unexpectedly carries prompt_start"
                );
                assert!(
                    !row.output_start,
                    "continuation row {i} unexpectedly carries output_start"
                );
            }
        }
    }

    #[test]
    fn row_clear_drops_marks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        emit_prompt(&mut term, "$ cmd", 1, 0);
        // ED 2 wipes the entire visible area — including all rows' marks.
        term.process(b"\x1b[2J");
        let any_marks = term
            .active
            .grid
            .rows
            .iter()
            .rev()
            .take(term.viewport.rows as usize)
            .any(|r| r.prompt_start || r.output_start || r.exit_status.is_some());
        assert!(!any_marks, "ED 2 must drop marks on visible rows");
    }

    // ---- DECTCEM cursor visibility (?25) ----

    #[test]
    fn dectcem_hides_and_shows_cursor() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        assert!(term.active.cursor_visible, "default must be visible");
        term.process(b"\x1b[?25l");
        assert!(!term.active.cursor_visible);
        term.process(b"\x1b[?25h");
        assert!(term.active.cursor_visible);
    }

    #[test]
    fn dectcem_state_is_per_screen() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?25l"); // hide on primary
        term.process(b"\x1b[?1049h"); // switch to alt
        // Alt starts with its own default (visible) — hiding the cursor on
        // the primary screen must not bleed through to alt.
        assert!(term.active.cursor_visible);
        term.process(b"\x1b[?1049l"); // back to primary
        // Primary's hidden state survives the round trip.
        assert!(!term.active.cursor_visible);
    }

    // ---- Device Attribute queries ----

    #[test]
    fn da1_replies_vt420() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn da1_with_zero_param_also_replies() {
        // Apps sometimes send `CSI 0 c` explicitly; the reply is the same.
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[0c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn da2_replies_as_vt420_compatible() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>c");
        assert_eq!(term.take_pending_output(), b"\x1b[>41;0;0c");
    }

    #[test]
    fn decscl_level1_changes_da1_prefix_without_resetting_screen() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"hello\x1b[?1004h\x1b[61\"p");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level1);
        assert_eq!(term.modes.c1_mode, C1Mode::SevenBit);
        assert!(term.modes.focus_reporting);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?61;7;21;22;28;29c");
        let row_text: String = term
            .visible_row(0)
            .cells
            .iter()
            .map(|c| c.as_str())
            .collect();
        assert!(row_text.starts_with("hello"), "row text was {row_text:?}");
    }

    #[test]
    fn decscl_with_8bit_controls_switches_reply_encoding() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[64;2\"p\x1b[>c");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level4);
        assert_eq!(term.modes.c1_mode, C1Mode::EightBit);
        assert_eq!(term.take_pending_output(), b"\x9b>41;0;0c");
    }

    #[test]
    fn s8c1t_is_ignored_in_level1_mode() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[61\"p\x1b G\x1b[>c");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level1);
        assert_eq!(term.modes.c1_mode, C1Mode::SevenBit);
        assert_eq!(term.take_pending_output(), b"\x1b[>41;0;0c");
    }

    #[test]
    fn da1_downgrades_when_macros_are_not_allowlisted() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_macro_permissions(ProgramAllowlist::Programs(vec!["vtrex".into()]));
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn da1_reports_level4_when_allowlisted_program_is_foreground() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_macro_permissions(ProgramAllowlist::Programs(vec!["vtrex".into()]));
        term.set_foreground_programs(&["/usr/bin/vtrex"]);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?64;7;21;22;28;29;32c");
    }

    #[test]
    fn macro_definition_and_invocation_require_allowlisted_foreground_processes() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_macro_permissions(ProgramAllowlist::Programs(vec!["vtrex".into()]));
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
        term.process(b"\x1b[1*z");
        assert!(visible_text(&term).trim().is_empty());

        term.set_foreground_programs(&["/usr/bin/vtrex"]);
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
        term.process(b"\x1b[1*z");
        assert!(visible_text(&term).contains("ABC"));
    }

    #[test]
    fn macro_permissions_require_all_foreground_processes_to_match() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_macro_permissions(ProgramAllowlist::Programs(vec!["vtrex".into()]));
        term.set_foreground_programs(&["/usr/bin/vtrex", "/usr/bin/helper"]);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn ris_clears_stored_macros() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_macro_permissions(ProgramAllowlist::AllowAll);
        term.set_foreground_programs(&["/usr/bin/vtrex"]);
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
        term.process(b"\x1bc");
        term.process(b"\x1b[1*z");
        assert!(visible_text(&term).trim().is_empty());
    }

    #[test]
    fn decdld_loads_and_designates_soft_charset() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;1;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @!");

        let expected = font41::encode_drcs_char(0).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn decdld_accepts_pcn_zero_for_94_character_sets() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;0;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @!");

        let expected = font41::encode_drcs_char(0).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn decdld_supports_space_intermediate_designation() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;0;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @!");

        let expected = font41::encode_drcs_char(0).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn bundled_selftest_drcs_script_renders_soft_glyphs() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let script = include_str!("../../selftest41/resources/icon.drcs")
            .replace('\u{0090}', "\x1bP")
            .replace('\u{009c}', "\x1b\\");
        term.process(script.as_bytes());

        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_ne!(actual, '!');
        assert!((actual as u32) >= 0xF0000);
    }

    #[test]
    fn decdld_94_charset_maps_colon_to_its_own_glyph_slot() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;26;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @:");

        let expected = font41::encode_drcs_char((b':' - b'!') as u16).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn vtrex_cactus_snippet_writes_soft_glyphs_into_two_rows() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;55;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;87;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @");
        term.process(b"\x1b[10;30Hw\x08\x1bMW");

        let lower = term.visible_row(9).cells[29].chars().next().unwrap();
        let upper = term.visible_row(8).cells[29].chars().next().unwrap();
        assert_eq!(
            lower,
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        assert_eq!(
            upper,
            font41::encode_drcs_char((b'W' - b'!') as u16).unwrap()
        );
    }

    #[test]
    fn vtrex_trex_snippet_writes_soft_glyphs_into_two_rows() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;15;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;26;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;28;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;64;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @");
        term.process(b"\x1b[7;8H:<\x08\x08\x0b/`");

        let top_left = term.visible_row(6).cells[7].chars().next().unwrap();
        let top_right = term.visible_row(6).cells[8].chars().next().unwrap();
        let bottom_left = term.visible_row(7).cells[7].chars().next().unwrap();
        let bottom_right = term.visible_row(7).cells[8].chars().next().unwrap();
        assert_eq!(
            top_left,
            font41::encode_drcs_char((b':' - b'!') as u16).unwrap()
        );
        assert_eq!(
            top_right,
            font41::encode_drcs_char((b'<' - b'!') as u16).unwrap()
        );
        assert_eq!(
            bottom_left,
            font41::encode_drcs_char((b'/' - b'!') as u16).unwrap()
        );
        assert_eq!(
            bottom_right,
            font41::encode_drcs_char((b'`' - b'!') as u16).unwrap()
        );
    }

    #[test]
    fn vtrex_soft_font_load_contains_trex_and_cactus_glyph_defs() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        for pcn in [15u16, 26, 28, 55, 64, 65, 78, 87] {
            term.process(format!("\x1bP1;{pcn};1;6;0;2;16;0{{ @~~~~~~\x1b\\").as_bytes());
        }
        let glyphs = term.drcs_render_glyphs();
        let geometry = font41::DrcsGeometryClass::Col80Line24;

        for byte in [b':', b'<', b'/', b'`', b'w', b'W', b'n', b'a'] {
            let glyph_id = byte as u16 - b'!' as u16;
            assert!(
                glyphs.contains_key(&(geometry, glyph_id)),
                "missing DRCS glyph for byte {byte:?} -> id {glyph_id}"
            );
        }
    }

    #[test]
    fn vtrex_trex_and_cactus_drcs_glyphs_rasterize_non_empty() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        for pcn in [15u16, 26, 28, 55, 64, 65, 78, 87] {
            term.process(format!("\x1bP1;{pcn};1;6;0;2;16;0{{ @~~~~~~\x1b\\").as_bytes());
        }

        let mut font_system = font41::FontSystem::new(None, 16.0, 1);
        let _guard = font41::set_drcs_context(
            Some(font41::DrcsGeometryClass::Col80Line24),
            Some(term.drcs_render_glyphs()),
        );

        for byte in [b':', b'<', b'/', b'`', b'w', b'W', b'n', b'a'] {
            let glyph_id = byte as u16 - b'!' as u16;
            let cell = font41::encode_drcs_char(glyph_id).unwrap().to_string();
            let shaped = font_system.shape_row(
                &[smol_str::SmolStr::new(cell)],
                &[font41::attrs::CellAttrs::default()],
            );
            let raster = font_system.rasterize_glyph(shaped[0].font_index, shaped[0].glyph_id, 1);
            assert!(
                raster.width > 0 && raster.height > 0 && !raster.bitmap.is_empty(),
                "empty raster for byte {byte:?} -> id {glyph_id}"
            );
        }
    }

    #[test]
    fn vtrex_page_composition_copies_cactus_and_trex_to_visible_page() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        for pcn in [15u16, 26, 28, 55, 64, 65, 78, 87] {
            term.process(format!("\x1bP1;{pcn};1;6;0;2;16;0{{ @~~~~~~\x1b\\").as_bytes());
        }

        term.process(b"\x1b[?64l");
        term.process(b"\x1b[2 P\x1b( @");
        term.process(b"\x1b[10;30Hw\x08\x1bMW");
        let page2 = screen::page_viewport(&term.active, &term.viewport, 2).unwrap();
        assert_eq!(
            term.active.grid.rows[page2.top + 9].cells[29]
                .chars()
                .next()
                .unwrap(),
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        term.process(b"\x1b[1;1;10;30;2;1;1;3$v");
        let page3 = screen::page_viewport(&term.active, &term.viewport, 3).unwrap();
        assert_eq!(
            term.active.grid.rows[page3.top + 9].cells[29]
                .chars()
                .next()
                .unwrap(),
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        term.process(b"\x1b[3 P\x1b[7;8H:<\x08\x08\x0b/`");
        assert_eq!(
            term.active.grid.rows[page3.top + 6].cells[7]
                .chars()
                .next()
                .unwrap(),
            font41::encode_drcs_char((b':' - b'!') as u16).unwrap()
        );
        term.process(b"\x1b[1 P\x1b[1;1;10;30;3;1;1;1$v");

        let cactus_lower = term.visible_row(9).cells[29].chars().next().unwrap();
        let cactus_upper = term.visible_row(8).cells[29].chars().next().unwrap();
        let trex_top_left = term.visible_row(6).cells[7].chars().next().unwrap();
        let trex_top_right = term.visible_row(6).cells[8].chars().next().unwrap();
        let trex_bottom_left = term.visible_row(7).cells[7].chars().next().unwrap();
        let trex_bottom_right = term.visible_row(7).cells[8].chars().next().unwrap();

        assert_eq!(
            cactus_lower,
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        assert_eq!(
            cactus_upper,
            font41::encode_drcs_char((b'W' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_top_left,
            font41::encode_drcs_char((b':' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_top_right,
            font41::encode_drcs_char((b'<' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_bottom_left,
            font41::encode_drcs_char((b'/' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_bottom_right,
            font41::encode_drcs_char((b'`' - b'!') as u16).unwrap()
        );
    }

    #[test]
    fn ris_clears_loaded_soft_charsets() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;1;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bc");
        term.process(b"\x1b( @!");
        assert_eq!(term.visible_row(0).cells[0].as_str(), "!");
    }

    #[test]
    fn oversized_drcs_payload_is_discarded() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let mut seq = b"\x1bP1;1;1;6;0;2;16;0{ @".to_vec();
        seq.extend(std::iter::repeat_n(b'~', drcs::MAX_DRCS_PAYLOAD_BYTES + 32));
        seq.extend_from_slice(b"\x1b\\");
        term.process(&seq);
        term.process(b"\x1b( @!");
        assert_eq!(term.visible_row(0).cells[0].as_str(), "!");
    }

    #[test]
    fn xtversion_replies_with_name_and_version() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>0q");
        let expected = format!("\x1bP>|term41 {}\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    #[test]
    fn decrqss_reports_page_geometry_settings() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[36*|\x1b[72t\x1b[132$|");
        report::handle_decrqss(b"t", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r72t\x1b\\");
        report::handle_decrqss(b"*|", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r36*|\x1b\\");
        report::handle_decrqss(b"$|", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r132$|\x1b\\");
    }

    #[test]
    fn decrqss_reports_status_and_attr_change_state() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2$~\x1b[1$}\x1b[2*x");
        report::handle_decrqss(b"$~", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r2$~\x1b\\");
        report::handle_decrqss(b"$}", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1$}\x1b\\");
        report::handle_decrqss(b"*x", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r2*x\x1b\\");
    }

    #[test]
    fn decrqss_reports_normal_text_color_assignment() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        report::handle_decrqss(b"1,|", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1;7;0,|\x1b\\");
    }

    #[test]
    fn decrqss_reports_window_frame_color_assignment() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2;4;5,|");
        report::handle_decrqss(b"2,|", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r2;4;5,|\x1b\\");
    }

    #[test]
    fn decrqss_reports_alternate_text_color_assignment() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[13;4;5,}");
        report::handle_decrqss(b"13,}", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r13;4;5,}\x1b\\");
    }

    #[test]
    fn decctr_reports_current_color_table() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2;2$u");
        let expected = format!(
            "\x1bP2$s{}\x1b\\",
            report_color_table(&term.dec_color, DecColorSpace::Rgb)
        );
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    #[test]
    fn decctr_reports_current_color_table_in_hls() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2;1$u");
        let expected = format!(
            "\x1bP2$s{}\x1b\\",
            report_color_table(&term.dec_color, DecColorSpace::Hls)
        );
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    #[test]
    fn decac_changes_effective_default_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[1;4;7,|x");

        assert_eq!(term.palette.fg, term.dec_color.table[4]);
        assert_eq!(term.palette.bg, term.dec_color.table[7]);
        assert_eq!(term.active.grid.default_fg, term.dec_color.table[4]);
        assert_eq!(term.active.grid.default_bg, term.dec_color.table[7]);
        assert_eq!(term.active.grid.rows[0].fg[0], term.dec_color.table[4]);
        assert_eq!(term.active.grid.rows[0].bg[0], term.dec_color.table[7]);

        report::handle_decrqss(b"1,|", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1;4;7,|\x1b\\");
    }

    #[test]
    fn decctr_restore_remaps_existing_default_colored_cells() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"ab");
        term.process(b"\x1bP2$p0;2;1;2;3/7;2;10;20;30\x1b\\");

        let expected_bg = Srgb::new(3, 5, 8);
        let expected_fg = Srgb::new(26, 51, 77);

        assert_eq!(term.palette.bg, expected_bg);
        assert_eq!(term.palette.fg, expected_fg);
        assert_eq!(term.active.grid.rows[0].fg[0], expected_fg);
        assert_eq!(term.active.grid.rows[0].bg[0], expected_bg);
        assert_eq!(term.active.grid.rows[0].fg[1], expected_fg);
        assert_eq!(term.active.grid.rows[0].bg[1], expected_bg);
    }

    #[test]
    fn decctr_restore_preserves_explicit_sgr_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[31mx");
        let explicit_fg = term.active.grid.rows[0].fg[0];

        term.process(b"\x1bP2$p0;2;1;2;3/7;2;10;20;30/1;2;200;10;10\x1b\\");

        assert_eq!(term.active.grid.rows[0].fg[0], explicit_fg);
    }

    #[test]
    fn decctr_restore_accepts_hls_entries() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1bP2$p4;1;240;50;100\x1b\\");
        assert_ne!(
            term.dec_color.table[4],
            color::palette_color(&term.base_palette, 4)
        );
    }

    #[test]
    fn decstglt_selects_lookup_table_mode() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[1){");
        assert_eq!(
            term.dec_color.lookup_table,
            DecColorLookupTable::AlternateWithAttrs
        );
        term.process(b"\x1b[3){");
        assert_eq!(term.dec_color.lookup_table, DecColorLookupTable::AnsiSgr);
    }

    #[test]
    fn decrqm_reports_vt525_color_private_modes() {
        let mut term = TestTerm::new(10, 3, 10, 16, 8);
        term.process(b"\x1b[?114h\x1b[?115h\x1b[?116h\x1b[?117h");
        term.process(b"\x1b[?114$p\x1b[?115$p\x1b[?116$p\x1b[?117$p");
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[?114;1$y\x1b[?115;1$y\x1b[?116;1$y\x1b[?117;1$y"
        );
    }

    #[test]
    fn page_geometry_commands_queue_host_resize() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[36*|");
        assert_eq!(term.take_pending_host_resize(), Some((80, 36)));
        term.process(b"\x1b[132$|");
        assert_eq!(term.take_pending_host_resize(), Some((132, 36)));
    }

    #[test]
    fn decrsps_restores_tab_stops() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[3g");
        term.process(b"\x1bP2$t4;9\x1b\\");
        assert!(term.active.tab_stops[3]);
        assert!(term.active.tab_stops[8]);
        assert!(!term.active.tab_stops[7]);
    }

    #[test]
    fn decrqpsr_reports_cursor_information() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[?6h\x1b(0\x0e\x1b[1;4m");
        term.process(b"\x1b[2;3H");
        term.process(b"\x1b[1$w");
        assert_eq!(
            term.take_pending_output(),
            b"\x1bP1$u2;3;1;C;@;A;1;2;@;0B%5%5\x1b\\"
        );
    }

    #[test]
    fn decrsps_restores_cursor_information() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1bP1$t2;3;1;C;A;A;1;2;@;0B%5%5\x1b\\");

        assert_eq!(term.active.cursor.row, 1);
        assert_eq!(term.active.cursor.col, 2);
        assert!(term.active.attrs.contains(font41::attrs::CellAttrs::BOLD));
        assert_eq!(term.active.underline, font41::attrs::UnderlineStyle::Single);
        assert!(
            term.active
                .attrs
                .contains(font41::attrs::CellAttrs::PROTECTED)
        );
        assert!(term.active.origin_mode);
        assert_eq!(term.active.charset.gl_slot(), charset::GraphicSetSlot::G1);
        assert_eq!(term.active.charset.gr_slot(), charset::GraphicSetSlot::G2);
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G0),
            charset::CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G1),
            charset::CharacterSet::Ascii
        );
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G2),
            charset::CharacterSet::DecSupplemental
        );
    }

    #[test]
    fn decrqtsr_reports_ascii_g0_and_g1_designations() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[1$u");
        assert_eq!(term.take_pending_output(), b"\x1bP1$s\x1b)B\x1b(B\x1b\\");
    }

    #[test]
    fn decrsts_restores_ascii_g0_and_g1_designations() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b(0\x1b)>");
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G0),
            charset::CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G1),
            charset::CharacterSet::DecTechnical
        );

        term.process(b"\x1bP1$p\x1b)B\x1b(B\x1b\\");

        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G0),
            charset::CharacterSet::Ascii
        );
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G1),
            charset::CharacterSet::Ascii
        );
    }

    #[test]
    fn decrsts_accepts_ddd1_without_rejecting_the_report() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1bP1$p\x1b)1\x1b)B\x1b(B\x1b\\");

        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G0),
            charset::CharacterSet::Ascii
        );
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G1),
            charset::CharacterSet::Ascii
        );
    }

    #[test]
    fn decrqm_reports_permanent_mode_states() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[10$p\x1b[20$p\x1b[?60$p");
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[10;4$y\x1b[20;2$y\x1b[?60;4$y"
        );
    }

    // ---- RIS (ESC c) ----

    #[test]
    fn ris_clears_visible_and_resets_cursor() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"hello\x1b[5;5H"); // print + move cursor
        term.process(b"\x1bc");
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        // Visible content is gone.
        for r in term.active.grid.rows.iter().rev().take(3) {
            assert_eq!(r.content_len(), 0);
        }
    }

    #[test]
    fn dectst_power_up_self_test_resets_terminal_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b(0hello");
        term.process(b"\x1b[4;1y");

        assert!(!term.modes.focus_reporting);
        assert_eq!(
            term.active.charset.designated(charset::GraphicSetSlot::G0),
            charset::CharacterSet::Ascii
        );
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        for r in term.active.grid.rows.iter().rev().take(3) {
            assert_eq!(r.content_len(), 0);
        }
    }

    #[test]
    fn ris_returns_to_primary_screen() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"\x1b[?1049h");
        assert!(term.on_alt_screen);
        term.process(b"\x1bc");
        assert!(!term.on_alt_screen);
    }

    #[test]
    fn ris_resets_dec_color_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        let mut custom = term.inner.palette.clone();
        custom.bg = Srgb::new(24, 32, 48);
        custom.fg = Srgb::new(220, 210, 200);
        term.inner.set_palette(custom.clone());
        term.process(b"\x1b[1;4;7,|\x1bP2$p4;2;8;9;10\x1b\\");
        term.process(b"\x1bc");

        report::handle_decrqss(b"1,|", &mut term.inner);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1;7;0,|\x1b\\");
        assert_eq!(term.palette.fg, custom.fg);
        assert_eq!(term.palette.bg, custom.bg);
        assert_eq!(term.active.grid.default_bg, custom.bg);
        assert_eq!(term.visible_row(0).bg[0], custom.bg);
    }

    #[test]
    fn status_line_demo_ris_round_trip_keeps_visible_rows_in_bounds() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.inner
            .set_default_status_display(StatusDisplayKind::Indicator);

        term.process(b"\x1b[?1049h");
        term.process(b"\x1b[?1049l");
        term.process(b"\x1b[2$~");
        term.process(b"\x1b[1$}STATUS > selftest41 > host-writable demo");
        term.process(b"\x1bc");
        term.process(b"\x1b[?1049h");
        term.process(b"\x1b[2J\x1b[H");
        term.process(b"\x1b[?1049l");
        term.process(b"\x1b[?1049h");
        term.process(b"\x1b[2J\x1b[H");

        assert!(
            term.inner.active.grid.rows.len() >= term.inner.viewport.rows as usize,
            "active grid shorter than viewport: len={} rows={}",
            term.inner.active.grid.rows.len(),
            term.inner.viewport.rows
        );
        for row in 0..term.inner.viewport.rows {
            let _ = term.inner.visible_row(row);
        }
    }

    #[test]
    fn ris_resets_modes_the_app_flipped() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"\x1b[?2004h"); // bracketed paste
        term.process(b"\x1b[?1004h"); // focus reporting
        term.process(b"\x1b[?1000h"); // mouse tracking
        term.process(b"\x1b[?25l"); // hide cursor
        term.process(b"\x1bc");
        assert!(!term.modes.bracketed_paste);
        assert!(!term.modes.focus_reporting);
        assert_eq!(term.modes.mouse_tracking, MouseTracking::Off);
        assert!(term.active.cursor_visible);
    }

    #[test]
    fn ris_preserves_scrollback() {
        // A misbehaving app's reset shouldn't nuke the user's history.
        let mut term = TestTerm::new(4, 2, 100, 16, 8);
        for _ in 0..5 {
            term.process(b"x\r\n");
        }
        let rows_before = term.active.grid.rows.len();
        assert!(rows_before > 2, "setup should have produced scrollback");
        term.process(b"\x1bc");
        // Rows count stays the same — visible area cleared in place, history kept.
        assert_eq!(term.active.grid.rows.len(), rows_before);
    }

    #[test]
    fn deccolm_ignored_without_mode_40() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?3h");
        // DECCOLM is gated by mode 40 — without it, the resize is ignored.
        assert_eq!(term.viewport.cols, 80);
    }

    #[test]
    fn deccolm_set_resizes_to_132_and_clears() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"hello");
        assert_eq!(term.viewport.cols, 80);
        // Enable mode 40 (allow DECCOLM), then set DECCOLM.
        term.process(b"\x1b[?40h\x1b[?3h");
        assert_eq!(term.viewport.cols, 132);
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.scroll_top, 0);
        // First visible row should be blank (cleared).
        let first_vis = term.active.grid.rows.len() - 24;
        assert_eq!(term.active.grid.rows[first_vis].cells[0].as_str(), " ");
    }

    #[test]
    fn deccolm_reset_restores_original_width() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        // Enable mode 40 first.
        term.process(b"\x1b[?40h");
        term.process(b"\x1b[?3h"); // 132 cols
        assert_eq!(term.viewport.cols, 132);
        term.process(b"\x1b[?3l"); // back to 80
        assert_eq!(term.viewport.cols, 80);
        assert_eq!(term.active.cursor.row, 0);
    }

    #[test]
    fn terminal_batch_budget_trips_on_time_limit() {
        let start = std::time::Instant::now() - runtime::TERMINAL_BATCH_TIME_BUDGET;
        assert!(runtime::terminal_batch_budget_exhausted(start));
    }
}
