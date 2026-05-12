use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clip41::Clipboard;
use clip41::ClipboardKind;
use config41::ClipboardPermissions;
use config41::PermissionPolicy;

use crate::C1Mode;
use crate::io::clipboard::ClipboardRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClipboardAction {
    Read {
        kind: ClipboardKind,
        response_selector: Vec<u8>,
    },
    Write {
        kinds: Vec<ClipboardKind>,
        text: String,
    },
}

pub(super) fn parse(rest: &[u8]) -> Option<ClipboardAction> {
    let (pc, pd) = super::split_osc(rest);
    let kinds = resolve_selectors(pc);

    if pd == b"?" {
        let kind = *kinds.first()?;
        let response_selector = if pc.is_empty() {
            b"c".to_vec()
        } else {
            pc.to_vec()
        };
        return Some(ClipboardAction::Read {
            kind,
            response_selector,
        });
    }

    let decoded = decode_osc52(pd)?;
    let text = std::str::from_utf8(&decoded).ok()?;
    if kinds.is_empty() {
        return None;
    }
    Some(ClipboardAction::Write {
        kinds,
        text: text.to_owned(),
    })
}

pub(super) fn apply(
    action: ClipboardAction,
    clipboard: &mut Clipboard,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
    clipboard_requests: &mut Vec<ClipboardRequest>,
    clipboard_permissions: &ClipboardPermissions,
) {
    match action {
        ClipboardAction::Read {
            kind,
            response_selector,
        } => match clipboard_permissions.read {
            PermissionPolicy::Allow => {
                pending_output.extend(crate::io::clipboard::osc52_read_response(
                    clipboard,
                    kind,
                    &response_selector,
                    c1_mode,
                ));
            }
            PermissionPolicy::Ask => clipboard_requests.push(ClipboardRequest::Read {
                kind,
                response_selector,
                c1_mode,
            }),
            PermissionPolicy::Deny => {}
        },
        ClipboardAction::Write { kinds, text } => match clipboard_permissions.write {
            PermissionPolicy::Allow => {
                for kind in kinds {
                    clipboard.set(kind, &text);
                }
            }
            PermissionPolicy::Ask => {
                clipboard_requests.push(ClipboardRequest::Write { kinds, text })
            }
            PermissionPolicy::Deny => {}
        },
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

/// Base64 decode with whitespace stripping. Some apps fold long payloads
/// with embedded newlines, and xterm tolerates that.
fn decode_osc52(data: &[u8]) -> Option<Vec<u8>> {
    let filtered: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    BASE64.decode(&filtered).ok()
}
