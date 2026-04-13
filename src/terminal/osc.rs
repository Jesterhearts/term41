use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::clipboard::Clipboard;
use crate::clipboard::ClipboardKind;

/// Split an OSC payload into its numeric command prefix and the remainder.
///
/// OSC commands have the shape `cmd;args`; when no semicolon is present the
/// whole payload is the command and `args` is empty.
fn split_osc(payload: &[u8]) -> (&[u8], &[u8]) {
    match payload.iter().position(|&b| b == b';') {
        Some(i) => (&payload[..i], &payload[i + 1..]),
        None => (payload, &[]),
    }
}

/// Resolve xterm OSC 52 selector characters into concrete clipboard kinds.
///
/// Selectors: `c` and digits `0`..`7` target the clipboard; `p`, `s`, `q`
/// target the primary selection. An empty selector defaults to the clipboard
/// (matches how most apps use OSC 52 in practice).
fn resolve_selectors(pc: &[u8]) -> Vec<ClipboardKind> {
    let mut seen_clipboard = false;
    let mut seen_primary = false;
    for &b in pc {
        match b {
            b'c' | b'0'..=b'7' => seen_clipboard = true,
            b'p' | b's' | b'q' => seen_primary = true,
            _ => {}
        }
    }
    let mut out = Vec::new();
    if pc.is_empty() || seen_clipboard {
        out.push(ClipboardKind::Clipboard);
    }
    if seen_primary {
        out.push(ClipboardKind::Primary);
    }
    out
}

/// Base64 decode with whitespace stripping — some apps fold long payloads
/// with embedded newlines, and xterm tolerates that.
fn decode_osc52(data: &[u8]) -> Option<Vec<u8>> {
    let filtered: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    BASE64.decode(&filtered).ok()
}

/// Dispatch an OSC payload to the appropriate handler. Unrecognised commands
/// are silently dropped — that's the standard behavior and avoids spurious
/// noise from apps probing for terminal features.
pub(super) fn handle_osc(
    payload: &[u8],
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
) {
    let (cmd, rest) = split_osc(payload);
    if cmd == b"52" {
        handle_osc_52(rest, clipboard, pending_output)
    }
}

/// Implements OSC 52 clipboard read/write as used by vim, tmux, etc.
///
/// Format: `OSC 52 ; Pc ; Pd ST` — Pc is one or more selector characters and
/// Pd is either base64-encoded text to copy, or `?` to query the clipboard
/// and have the terminal echo the result back over the PTY.
fn handle_osc_52(
    rest: &[u8],
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
) {
    let (pc, pd) = split_osc(rest);
    let kinds = resolve_selectors(pc);

    if pd == b"?" {
        // Only one response is meaningful even when multiple selectors are
        // requested — pick the first resolved kind.
        let Some(&kind) = kinds.first() else { return };
        let Some(text) = clipboard.get(kind) else {
            return;
        };
        let encoded = BASE64.encode(text.as_bytes());
        let pc_resp: &[u8] = if pc.is_empty() { b"c" } else { pc };
        pending_output.extend_from_slice(b"\x1b]52;");
        pending_output.extend_from_slice(pc_resp);
        pending_output.push(b';');
        pending_output.extend_from_slice(encoded.as_bytes());
        pending_output.extend_from_slice(b"\x1b\\");
        return;
    }

    let Some(decoded) = decode_osc52(pd) else {
        return;
    };
    let Ok(text) = std::str::from_utf8(&decoded) else {
        return;
    };
    for kind in kinds {
        clipboard.set(kind, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_osc(payload: &[u8]) -> (Clipboard, Vec<u8>) {
        let mut clipboard = Clipboard::in_memory();
        let mut pending = Vec::new();
        handle_osc(payload, &mut clipboard, &mut pending);
        (clipboard, pending)
    }

    #[test]
    fn osc_52_writes_clipboard_with_c_selector() {
        let (mut cb, pending) = call_osc(b"52;c;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
        assert!(pending.is_empty());
    }

    #[test]
    fn osc_52_writes_primary_with_p_selector() {
        let (mut cb, _) = call_osc(b"52;p;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Primary).as_deref(), Some("hello"));
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
    }

    #[test]
    fn osc_52_empty_selector_defaults_to_clipboard() {
        let (mut cb, _) = call_osc(b"52;;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
    }

    #[test]
    fn osc_52_multi_selector_sets_both() {
        let (mut cb, _) = call_osc(b"52;cp;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
        assert_eq!(cb.get(ClipboardKind::Primary).as_deref(), Some("hello"));
    }

    #[test]
    fn osc_52_tolerates_embedded_whitespace_in_base64() {
        let (mut cb, _) = call_osc(b"52;c;aGVs\nbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
    }

    #[test]
    fn osc_52_rejects_invalid_base64() {
        let (mut cb, _) = call_osc(b"52;c;!!not-base64!!");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
    }

    #[test]
    fn osc_52_query_emits_base64_response() {
        let mut clipboard = Clipboard::in_memory();
        clipboard.set(ClipboardKind::Clipboard, "hi");
        let mut pending = Vec::new();
        handle_osc(b"52;c;?", &mut clipboard, &mut pending);
        assert_eq!(pending, b"\x1b]52;c;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_query_echoes_original_selector() {
        let mut clipboard = Clipboard::in_memory();
        clipboard.set(ClipboardKind::Primary, "hi");
        let mut pending = Vec::new();
        handle_osc(b"52;p;?", &mut clipboard, &mut pending);
        assert_eq!(pending, b"\x1b]52;p;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_ignored_for_unknown_command() {
        let (mut cb, pending) = call_osc(b"0;some-title");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
        assert!(pending.is_empty());
    }

    #[test]
    fn osc_52_ignored_when_non_utf8() {
        // \xFF\xFE is valid base64 of 0xF5 0xFD 0xBF which is invalid UTF-8.
        let (mut cb, _) = call_osc(b"52;c;//2/");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
    }
}
