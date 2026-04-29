//! Helpers for terminal-originated reports sent back to the foreground
//! program.

use std::time::Instant;

use vte_mode41::C1Mode;

use crate::MouseButton;
use crate::MouseEncoding;
use crate::MouseEventKind;
use crate::MouseModifiers;
use crate::MouseTracking;
use crate::SYNCHRONIZED_UPDATE_TIMEOUT;
use crate::conformance;
use crate::io::mouse::encode_mouse_event;
use crate::io::mouse::should_report;

/// Whether synchronized-output mode is still within its safety deadline.
pub fn synchronized_update_active(since: Option<Instant>) -> bool {
    since.is_some_and(|start| start.elapsed() < SYNCHRONIZED_UPDATE_TIMEOUT)
}

/// Emit a focus-in/focus-out report when focus reporting is enabled.
pub fn report_focus_change(
    host_bytes: &mut Vec<u8>,
    c1_mode: C1Mode,
    focus_reporting: bool,
    focused: bool,
) {
    if !focus_reporting {
        return;
    }
    conformance::write_csi(
        host_bytes,
        c1_mode,
        format_args!("{}", if focused { 'I' } else { 'O' }),
    );
}

/// Whether any mouse tracking mode is active.
pub fn mouse_tracking_enabled(mouse_tracking: MouseTracking) -> bool {
    !matches!(mouse_tracking, MouseTracking::Off)
}

/// Encode and append a mouse report if the current tracking mode wants it.
///
/// Returns `true` when a report was emitted.
pub fn mouse_report(
    host_bytes: &mut Vec<u8>,
    c1_mode: C1Mode,
    mouse_tracking: MouseTracking,
    mouse_encoding: MouseEncoding,
    kind: MouseEventKind,
    button: MouseButton,
    col: u32,
    row: u32,
    pixel_x: u32,
    pixel_y: u32,
    mods: MouseModifiers,
) -> bool {
    if !should_report(mouse_tracking, kind, button) {
        return false;
    }
    let (x, y) = match mouse_encoding {
        MouseEncoding::SgrPixels => (pixel_x, pixel_y),
        _ => (col, row),
    };
    encode_mouse_event(
        c1_mode,
        mouse_encoding,
        kind,
        button,
        x + 1,
        y + 1,
        mods,
        host_bytes,
    );
    true
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use super::*;
    use crate::HostInput;
    use crate::HostMouse;
    use crate::apply_host_input;
    use crate::test_support::TestTerm;

    #[test]
    fn decset_1006_switches_to_sgr_encoding() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1006h");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::Sgr);
        term.process(b"\x1b[?1006l");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::Default);
    }

    #[test]
    fn decset_1016_switches_to_sgr_pixels_encoding() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1016h");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::SgrPixels);
        term.process(b"\x1b[?1016l");
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
    fn mouse_report_uses_pixels_when_sgr_pixels_is_enabled() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1000h\x1b[?1016h");
        let effects = apply_host_input(
            &mut term.inner,
            HostInput::Mouse(HostMouse {
                kind: MouseEventKind::Press,
                button: MouseButton::Left,
                col: 4,
                row: 9,
                pixel_x: 39,
                pixel_y: 79,
                mods: MouseModifiers::default(),
            }),
        );
        term.effects.host_bytes.extend(effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1b[<0;40;80M");
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

    #[test]
    fn focus_change_silent_when_reporting_disabled() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        let effects = apply_host_input(&mut term.inner, HostInput::FocusChanged { focused: true });
        term.effects.host_bytes.extend(effects.host_bytes);
        assert!(term.take_pending_output().is_empty());
    }

    #[test]
    fn focus_change_emits_csi_i_o_when_enabled() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h");
        let effects = apply_host_input(&mut term.inner, HostInput::FocusChanged { focused: true });
        term.effects.host_bytes.extend(effects.host_bytes);
        let effects = apply_host_input(&mut term.inner, HostInput::FocusChanged { focused: false });
        term.effects.host_bytes.extend(effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1b[I\x1b[O");
    }

    #[test]
    fn focus_change_uses_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b G");
        let effects = apply_host_input(&mut term.inner, HostInput::FocusChanged { focused: true });
        term.effects.host_bytes.extend(effects.host_bytes);
        let effects = apply_host_input(&mut term.inner, HostInput::FocusChanged { focused: false });
        term.effects.host_bytes.extend(effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x9bI\x9bO");
    }

    #[test]
    fn decrst_1004_disables_focus_reporting() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b[?1004l");
        let effects = apply_host_input(&mut term.inner, HostInput::FocusChanged { focused: true });
        term.effects.host_bytes.extend(effects.host_bytes);
        assert!(term.take_pending_output().is_empty());
    }

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
        term.modes.synchronized_update_since =
            Some(Instant::now() - SYNCHRONIZED_UPDATE_TIMEOUT - Duration::from_millis(1));
        assert!(!term.is_synchronized_update_active());
    }
}
