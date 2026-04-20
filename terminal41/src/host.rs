use std::time::Instant;

use super::*;

pub fn synchronized_update_active(since: Option<Instant>) -> bool {
    since.is_some_and(|start| start.elapsed() < SYNCHRONIZED_UPDATE_TIMEOUT)
}

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

pub fn mouse_tracking_enabled(mouse_tracking: MouseTracking) -> bool {
    !matches!(mouse_tracking, MouseTracking::Off)
}

pub fn mouse_report(
    host_bytes: &mut Vec<u8>,
    c1_mode: C1Mode,
    mouse_tracking: MouseTracking,
    mouse_encoding: MouseEncoding,
    kind: MouseEventKind,
    button: MouseButton,
    col: u32,
    row: u32,
    mods: MouseModifiers,
) -> bool {
    if !should_report(mouse_tracking, kind, button) {
        return false;
    }
    encode_mouse_event(
        c1_mode,
        mouse_encoding,
        kind,
        button,
        col + 1,
        row + 1,
        mods,
        host_bytes,
    );
    true
}
