use crate::C1Mode;
use crate::conformance;
use crate::mode;

/// DEC mouse-tracking mode currently requested by the foreground app.
///
/// Layered in the order the spec describes — each higher variant is a
/// superset of the one above, though we model them as distinct states so the
/// reporter can filter motion appropriately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseTracking {
    /// No mouse events are forwarded.
    Off,
    /// Mode 9. Press events only.
    X10,
    /// Mode 1000. Press and release, no motion.
    Normal,
    /// Mode 1002. Press, release, and motion while a button is held.
    ButtonEvent,
    /// Mode 1003. Press, release, and all motion regardless of buttons.
    AnyEvent,
}

/// On-the-wire encoding for mouse events. The app selects these with
/// DECSET ?1005/?1006/?1015.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEncoding {
    /// Legacy xterm `CSI M Cb Cx Cy` with each byte offset by 32. Cells
    /// beyond column/row 223 saturate, so modern apps prefer SGR.
    Default,
    /// Mode 1005. Same shape as Default but each field is UTF-8 encoded.
    Utf8,
    /// Mode 1006. `CSI < Pb ; Px ; Py M|m` — trailing `m` signals release.
    Sgr,
    /// Mode 1015. `CSI Pb ; Px ; Py M` — decimal, no angle bracket, release
    /// encoded with button code 3.
    Urxvt,
}

/// Kind of event the app is being told about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Motion,
}

/// Physical button that originated the event. `None` is used for motion
/// reports when no button is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    None,
}

/// Keyboard modifiers captured alongside a mouse event.
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Handle DECSET/DECRST bits that drive mouse tracking. Returns true when
/// the mode was a mouse-related one, so the caller knows not to fall
/// through to the generic private-mode handler.
///
/// Tracking modes are modeled as a single state (enabling a new tracking
/// mode replaces the prior one; disabling the tracking bit turns it fully
/// off). That matches how xterm-compatible apps actually use these flags.
pub fn apply_mouse_mode(
    mode: u16,
    enable: bool,
    tracking: &mut MouseTracking,
    encoding: &mut MouseEncoding,
) -> bool {
    let tracking_target = match mode {
        mode::X10_MOUSE => Some(MouseTracking::X10),
        mode::NORMAL_MOUSE => Some(MouseTracking::Normal),
        mode::BUTTON_EVENT_MOUSE => Some(MouseTracking::ButtonEvent),
        mode::ANY_EVENT_MOUSE => Some(MouseTracking::AnyEvent),
        _ => None,
    };
    if let Some(target) = tracking_target {
        *tracking = if enable { target } else { MouseTracking::Off };
        return true;
    }

    let encoding_target = match mode {
        mode::UTF8_MOUSE => Some(MouseEncoding::Utf8),
        mode::SGR_MOUSE => Some(MouseEncoding::Sgr),
        mode::URXVT_MOUSE => Some(MouseEncoding::Urxvt),
        _ => None,
    };
    if let Some(target) = encoding_target {
        *encoding = if enable {
            target
        } else {
            MouseEncoding::Default
        };
        return true;
    }

    false
}

/// Decide whether the given event should be forwarded under the current
/// tracking mode. Release + motion reports under X10, motion reports under
/// Normal, and motion-without-button under ButtonEvent are all suppressed.
pub fn should_report(
    tracking: MouseTracking,
    kind: MouseEventKind,
    button: MouseButton,
) -> bool {
    match tracking {
        MouseTracking::Off => false,
        MouseTracking::X10 => matches!(kind, MouseEventKind::Press),
        MouseTracking::Normal => matches!(kind, MouseEventKind::Press | MouseEventKind::Release),
        MouseTracking::ButtonEvent => match kind {
            MouseEventKind::Press | MouseEventKind::Release => true,
            MouseEventKind::Motion => !matches!(button, MouseButton::None),
        },
        MouseTracking::AnyEvent => true,
    }
}

/// Numeric button code for the xterm mouse protocol (before adding motion
/// or modifier bits).
fn button_number(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::WheelLeft => 66,
        MouseButton::WheelRight => 67,
    }
}

/// Encode the button/modifier/motion byte (`Cb`) that's common to every
/// protocol. For non-SGR encodings a release collapses to button code 3
/// because there's no other way to distinguish it on the wire.
fn build_mouse_cb(
    encoding: MouseEncoding,
    kind: MouseEventKind,
    button: MouseButton,
    mods: MouseModifiers,
) -> u16 {
    let base = if matches!(kind, MouseEventKind::Release) && !matches!(encoding, MouseEncoding::Sgr)
    {
        3
    } else {
        button_number(button)
    };
    let motion = if matches!(kind, MouseEventKind::Motion) {
        32
    } else {
        0
    };
    let mods = (if mods.shift { 4 } else { 0 })
        | (if mods.alt { 8 } else { 0 })
        | (if mods.ctrl { 16 } else { 0 });
    base + motion + mods
}

/// Append one coordinate byte for the legacy encoding, saturating at 0xFF
/// so we never split a byte range the caller didn't ask for.
fn push_legacy_coord(
    out: &mut Vec<u8>,
    value: u32,
) {
    out.push((value + 32).min(255) as u8);
}

/// Append one coordinate as a UTF-8 code point. Values above U+10FFFF fall
/// back to `?` — realistically unreachable for terminal sizes.
fn push_utf8_coord(
    out: &mut Vec<u8>,
    value: u32,
) {
    match char::from_u32(value + 32) {
        Some(c) => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        None => out.push(b'?'),
    }
}

/// Encode a mouse event using the active protocol and push it into `out`.
pub fn encode_mouse_event(
    c1_mode: C1Mode,
    encoding: MouseEncoding,
    kind: MouseEventKind,
    button: MouseButton,
    col_1based: u32,
    row_1based: u32,
    mods: MouseModifiers,
    out: &mut Vec<u8>,
) {
    use std::io::Write as _;

    let cb = build_mouse_cb(encoding, kind, button, mods);

    match encoding {
        MouseEncoding::Sgr => {
            let release = matches!(kind, MouseEventKind::Release);
            conformance::push_csi_prefix(out, c1_mode);
            let _ = write!(out, "<{cb};{col_1based};{row_1based}");
            out.push(if release { b'm' } else { b'M' });
        }
        MouseEncoding::Urxvt => {
            // URXVT adds 32 to Cb just like legacy — the `32` is the xterm
            // legacy bias, not the motion bit, so we apply it here.
            conformance::write_csi(
                out,
                c1_mode,
                format_args!("{};{};{}M", cb + 32, col_1based, row_1based),
            );
        }
        MouseEncoding::Default => {
            conformance::push_csi_prefix(out, c1_mode);
            out.push(b'M');
            out.push((cb + 32).min(255) as u8);
            push_legacy_coord(out, col_1based);
            push_legacy_coord(out, row_1based);
        }
        MouseEncoding::Utf8 => {
            conformance::push_csi_prefix(out, c1_mode);
            out.push(b'M');
            push_utf8_coord(out, cb as u32);
            push_utf8_coord(out, col_1based);
            push_utf8_coord(out, row_1based);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(
        encoding: MouseEncoding,
        kind: MouseEventKind,
        button: MouseButton,
        col: u32,
        row: u32,
        mods: MouseModifiers,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        encode_mouse_event(
            C1Mode::SevenBit,
            encoding,
            kind,
            button,
            col,
            row,
            mods,
            &mut out,
        );
        out
    }

    #[test]
    fn sgr_encodes_press_and_release_with_mcase() {
        let mods = MouseModifiers::default();
        let press = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Press,
            MouseButton::Left,
            3,
            5,
            mods,
        );
        let release = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Release,
            MouseButton::Left,
            3,
            5,
            mods,
        );
        assert_eq!(press, b"\x1b[<0;3;5M");
        assert_eq!(release, b"\x1b[<0;3;5m");
    }

    #[test]
    fn sgr_motion_adds_bit_32() {
        let out = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Motion,
            MouseButton::Left,
            10,
            12,
            MouseModifiers::default(),
        );
        assert_eq!(out, b"\x1b[<32;10;12M");
    }

    #[test]
    fn sgr_modifiers_combine() {
        let mods = MouseModifiers {
            shift: true,
            alt: true,
            ctrl: true,
        };
        let out = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Press,
            MouseButton::Right,
            1,
            1,
            mods,
        );
        // button 2 + shift 4 + alt 8 + ctrl 16 = 30
        assert_eq!(out, b"\x1b[<30;1;1M");
    }

    #[test]
    fn sgr_wheel_encodes_button_64() {
        let out = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Press,
            MouseButton::WheelUp,
            4,
            2,
            MouseModifiers::default(),
        );
        assert_eq!(out, b"\x1b[<64;4;2M");
    }

    #[test]
    fn legacy_encoding_offsets_bytes_by_32() {
        let out = encode(
            MouseEncoding::Default,
            MouseEventKind::Press,
            MouseButton::Left,
            3,
            5,
            MouseModifiers::default(),
        );
        assert_eq!(out, &[0x1B, b'[', b'M', 32, 35, 37]);
    }

    #[test]
    fn legacy_release_collapses_button_to_three() {
        let out = encode(
            MouseEncoding::Default,
            MouseEventKind::Release,
            MouseButton::Right,
            3,
            5,
            MouseModifiers::default(),
        );
        // 3 (release) + 32 = 35, coords +32
        assert_eq!(out, &[0x1B, b'[', b'M', 35, 35, 37]);
    }

    #[test]
    fn utf8_encoding_handles_large_coords() {
        let out = encode(
            MouseEncoding::Utf8,
            MouseEventKind::Press,
            MouseButton::Left,
            300,
            1,
            MouseModifiers::default(),
        );
        // Button byte: 0 + 32 = 32 (single byte ' ')
        assert_eq!(&out[..4], b"\x1b[M ");
        // Col 300 + 32 = 332, which is 0xC5 0x8C in UTF-8
        assert_eq!(&out[4..6], &[0xC5, 0x8C]);
        // Row 1 + 32 = 33 '!'
        assert_eq!(out[6], b'!');
    }

    #[test]
    fn urxvt_encoding_uses_decimal_with_32_bias() {
        let out = encode(
            MouseEncoding::Urxvt,
            MouseEventKind::Press,
            MouseButton::Left,
            3,
            5,
            MouseModifiers::default(),
        );
        // Cb 0 + 32 = 32
        assert_eq!(out, b"\x1b[32;3;5M");
    }

    #[test]
    fn should_report_filters_by_tracking_mode() {
        assert!(!should_report(
            MouseTracking::Off,
            MouseEventKind::Press,
            MouseButton::Left
        ));
        assert!(should_report(
            MouseTracking::X10,
            MouseEventKind::Press,
            MouseButton::Left
        ));
        assert!(!should_report(
            MouseTracking::X10,
            MouseEventKind::Release,
            MouseButton::Left
        ));
        assert!(should_report(
            MouseTracking::Normal,
            MouseEventKind::Release,
            MouseButton::Left
        ));
        assert!(!should_report(
            MouseTracking::Normal,
            MouseEventKind::Motion,
            MouseButton::Left
        ));
        assert!(should_report(
            MouseTracking::ButtonEvent,
            MouseEventKind::Motion,
            MouseButton::Left
        ));
        assert!(!should_report(
            MouseTracking::ButtonEvent,
            MouseEventKind::Motion,
            MouseButton::None
        ));
        assert!(should_report(
            MouseTracking::AnyEvent,
            MouseEventKind::Motion,
            MouseButton::None
        ));
    }
}
