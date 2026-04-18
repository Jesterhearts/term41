//! Named constants for DEC private modes, ANSI modes, and xterm extensions.
//!
//! These replace the raw numeric literals scattered across the VTE dispatch
//! code, making it immediately obvious which feature a given CSI parameter
//! selects.

// -- DEC private modes (CSI ? Ps h/l) ----------------------------------------

/// DECCKM -- Application Cursor Keys. When set, cursor keys send
/// application-mode sequences (`ESC O A`..`D`) instead of the normal
/// `CSI A`..`D`.
pub const DECCKM: u16 = 1;

/// DECANM -- ANSI/VT52 Mode. Set = ANSI mode (normal); reset = VT52
/// compatibility mode with a completely different escape vocabulary.
pub const DECANM: u16 = 2;

/// DECCOLM -- 80/132 Column Mode. Set = 132 columns; reset = restore the
/// saved column count. Switching clears the screen, resets margins, and
/// homes the cursor.
pub const DECCOLM: u16 = 3;

/// DECARM -- Auto-Repeat Mode (mode 8). Terminal-level auto-repeat is
/// always active (handled by the OS/windowing system), so this is a
/// tracked no-op for DECRQM compatibility. Default is on.
pub const DECARM: u16 = 8;

/// att610 -- Cursor blink control (mode 12). When set, the cursor blinks;
/// when reset, it is steady. This is an xterm extension (not a DEC mode)
/// that overrides the blink axis of the DECSCUSR style.
pub const ATT610_BLINK: u16 = 12;

/// DECNKM -- Numeric Keypad Mode (mode 66). When set, the numeric keypad
/// sends application sequences (same effect as DECKPAM / ESC =); when
/// reset, it sends normal characters (same as DECKPNM / ESC >).
pub const DECNKM: u16 = 66;

/// Allow DECCOLM (mode 40). Gates whether mode 3 (DECCOLM) is honoured.
/// Default is off, matching xterm — prevents unsolicited 80/132 column
/// toggling which is both disruptive and expensive (grid resize + clear).
pub const ALLOW_DECCOLM: u16 = 40;

/// DECNRCM -- National Replacement Character Set Mode. When set, NRC
/// designations replace ASCII positions in a 7-bit national environment.
pub const DECNRCM: u16 = 42;

/// DECSCNM -- Screen Mode (mode 5). When set, the screen displays in
/// reverse video — default background becomes foreground and vice versa.
/// Per-cell SGR 7 (REVERSE) stacks with this, so reversed cells appear
/// normal under DECSCNM.
pub const DECSCNM: u16 = 5;

/// DECOM -- Origin Mode. When set, cursor addressing is relative to the
/// scroll region; when reset, it is relative to the full screen.
pub const DECOM: u16 = 6;

/// DECAWM -- Auto-Wrap Mode. When set, writing past the right margin
/// wraps to the next line; when reset, the cursor stays at the margin.
pub const DECAWM: u16 = 7;

/// X10 mouse tracking (mode 9). Reports button presses only, no releases
/// or motion.
pub const X10_MOUSE: u16 = 9;

/// DECTCEM -- Text Cursor Enable Mode. When set, the text cursor is
/// visible; when reset, it is hidden.
pub const DECTCEM: u16 = 25;

/// Alt screen buffer (mode 47). Switches to the alternate screen buffer
/// on set and back to primary on reset.
pub const ALT_SCREEN: u16 = 47;

/// Normal mouse tracking (mode 1000). Reports button presses and releases
/// but no motion.
pub const NORMAL_MOUSE: u16 = 1000;

/// Button-event mouse tracking (mode 1002). Reports presses, releases,
/// and motion while a button is held.
pub const BUTTON_EVENT_MOUSE: u16 = 1002;

/// Any-event mouse tracking (mode 1003). Reports all presses, releases,
/// and motion regardless of button state.
pub const ANY_EVENT_MOUSE: u16 = 1003;

/// Focus reporting (mode 1004). When set, the terminal sends `CSI I` on
/// focus-in and `CSI O` on focus-out.
pub const FOCUS_REPORTING: u16 = 1004;

/// UTF-8 mouse encoding (mode 1005). Coordinates are UTF-8 encoded.
pub const UTF8_MOUSE: u16 = 1005;

/// SGR mouse encoding (mode 1006). `CSI < Pb;Px;Py M/m` format with
/// a trailing `m` for release events.
pub const SGR_MOUSE: u16 = 1006;

/// urxvt mouse encoding (mode 1015). Decimal format without angle
/// bracket; release is encoded with button code 3.
pub const URXVT_MOUSE: u16 = 1015;

/// Alt screen with clear-on-exit (mode 1047). Leaving the alt screen
/// clears the alt buffer so stale content is not re-shown.
pub const ALT_SCREEN_CLEAR: u16 = 1047;

/// Save/restore cursor (mode 1048). Set performs DECSC, reset performs
/// DECRC.
pub const SAVE_CURSOR: u16 = 1048;

/// Alt screen with save/restore cursor (mode 1049). Entering saves the
/// cursor and switches to the alt screen; leaving restores and switches
/// back.
pub const ALT_SCREEN_SAVE: u16 = 1049;

/// DECLRMM -- Left/Right Margin Mode (mode 69). When set, enables
/// left and right margins set by DECSLRM (`CSI Pl ; Pr s`). Cursor
/// movement, scrolling, and character insertion/deletion are bounded
/// by these margins. Default is off.
pub const DECLRMM: u16 = 69;

/// DECNCSM -- No Clearing Screen on Column Mode change (mode 95). When
/// set, switching DECCOLM (mode 3) does not clear the screen. Default
/// is off (screen is cleared on DECCOLM change, per DEC spec).
pub const DECNCSM: u16 = 95;

/// Bracketed paste (mode 2004). Pasted text is wrapped in
/// `CSI 200~`..`CSI 201~` so apps can distinguish it from typed input.
pub const BRACKETED_PASTE: u16 = 2004;

/// Synchronized output (mode 2026). Batches rendering between BSU and
/// ESU markers so the terminal can present a complete frame.
pub const SYNCHRONIZED_UPDATE: u16 = 2026;

// -- ANSI modes (CSI Ps h/l) --------------------------------------------------

/// IRM -- Insert/Replace Mode (ANSI mode 4). When set, printing shifts
/// existing text right before writing; when reset, printing overwrites.
pub const IRM: u16 = 4;

/// LNM -- Line Feed/New Line Mode (ANSI mode 20). When set, LF/VT/FF
/// perform an implicit CR before the line feed.
pub const LNM: u16 = 20;
