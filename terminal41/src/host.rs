use std::time::Instant;

use super::*;

pub fn synchronized_update_active(since: Option<Instant>) -> bool {
    since.is_some_and(|start| start.elapsed() < SYNCHRONIZED_UPDATE_TIMEOUT)
}

pub fn take_bell_pending(output: &mut TerminalOutput) -> bool {
    std::mem::replace(&mut output.bell_pending, false)
}

pub fn report_focus_change(
    output: &mut TerminalOutput,
    c1_mode: C1Mode,
    focus_reporting: bool,
    focused: bool,
) {
    if !focus_reporting {
        return;
    }
    conformance::write_csi(
        &mut output.pending_output,
        c1_mode,
        format_args!("{}", if focused { 'I' } else { 'O' }),
    );
}

pub fn take_pending_output(output: &mut TerminalOutput) -> Vec<u8> {
    std::mem::take(&mut output.pending_output)
}

pub fn mouse_tracking_enabled(mouse_tracking: MouseTracking) -> bool {
    !matches!(mouse_tracking, MouseTracking::Off)
}

pub fn mouse_report(
    output: &mut TerminalOutput,
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
        &mut output.pending_output,
    );
    true
}

pub fn take_pending_host_resize(output: &mut TerminalOutput) -> Option<(u32, u32)> {
    output.pending_host_resize.take()
}
