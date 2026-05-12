use std::time::Duration;
use std::time::Instant;

use config41::FeaturePermissions;
use config41::TerminalLimits;

use crate::C1Mode;
use crate::ConformanceLevel;
use crate::MouseEncoding;
use crate::MouseTracking;
use crate::TextMode;
use crate::dec::r#macro::MacroStore;
use crate::dec::udk::UdkState;
use crate::drcs::DrcsStore;

/// Safety deadline for mode 2026 synchronized updates. If an app sends BSU
/// (`CSI ? 2026 h`) but never sends ESU (because it crashed, was killed,
/// forgot the terminator, etc.) rendering resumes after this window so the
/// UI doesn't appear frozen. 150ms matches the contour-terminal spec.
pub(crate) const SYNCHRONIZED_UPDATE_TIMEOUT: Duration = Duration::from_millis(16);

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
    pub(crate) next_image_id: u64,
    /// Kitty graphics protocol image store. Images transmitted via `a=t`
    /// live here until placed or deleted.
    pub kitty_images: image41::kitty::KittyImageStore,
    /// Accumulates chunks for multi-part kitty graphics transmissions.
    pub kitty_chunked: image41::kitty::ChunkedTransmission,
    /// Accumulates chunks for multi-part iTerm2 graphics transmissions
    /// (`MultipartFile` -> `FilePart*` -> `FileEnd`).
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
    /// Mode 2004 - when enabled, pasted text is wrapped in
    /// `\x1b[200~ ... \x1b[201~` so apps can distinguish it from typed input.
    pub bracketed_paste: bool,
    /// Mode `?1004` - when enabled, focus changes are reported to the
    /// foreground app as `\x1b[I` (focus in) and `\x1b[O` (focus out).
    pub focus_reporting: bool,
    /// Mode 2026 - Synchronized Output (BSU/ESU). `Some(t)` from the moment
    /// `CSI ? 2026 h` arrives until either `CSI ? 2026 l` clears it or the
    /// internal synchronized-update safety deadline passes; otherwise `None`.
    pub synchronized_update_since: Option<Instant>,
    /// IRM (ANSI mode 4) - Insert/Replace mode. When `true`, printing a
    /// character shifts existing text right before writing. Default is
    /// replace (overwrite) mode.
    pub insert_mode: bool,
    /// LNM (ANSI mode 20) - Line Feed/New Line mode. When `true`, LF, VT,
    /// and FF perform an implicit CR before the line feed. Default is off.
    pub newline_mode: bool,
    /// DECARM (`?8`) - auto-repeat. Always on at the OS level; tracked
    /// here only so DECRQM can report it. Default is `true`.
    pub decarm: bool,
    /// DECLRMM (`?69`) - when `true`, left/right margins (set by DECSLRM)
    /// are active and constrain cursor movement, scrolling, and
    /// insertion/deletion. Default is `false`.
    pub declrmm: bool,
    /// DECNCSM (`?95`) - when `true`, DECCOLM switching does not clear
    /// the screen. Default is `false`.
    pub decncsm: bool,
    /// DECSCNM (`?5`) - when `true`, the entire screen renders in reverse
    /// video: the default bg becomes fg and vice versa. Per-cell SGR 7
    /// (REVERSE) XORs with this, so reversed cells appear normal.
    pub screen_reverse: bool,
    /// Mode 40 - when `true`, DECCOLM (mode 3) is honoured. Default is
    /// `false`, matching xterm. Without this gate a malicious escape
    /// sequence stream can repeatedly toggle 80/132 columns, triggering
    /// expensive grid resizes.
    pub allow_deccolm: bool,
    /// DECNRCM (`?42`) - when `true`, national replacement character-set
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
    /// DECANM (`?2`) - when `true` the terminal operates in VT52 compatibility
    /// mode. Set via `CSI ? 2 l`, cleared by `CSI ? 2 h` or RIS. VT52 mode
    /// uses a completely different (non-CSI) escape sequence vocabulary.
    pub vt52_mode: bool,
}

impl TerminalModes {
    pub(crate) fn new() -> Self {
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
