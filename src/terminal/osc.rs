use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use percent_encoding::percent_decode;

use crate::clipboard::Clipboard;
use crate::clipboard::ClipboardKind;
use crate::terminal::hyperlink::HyperlinkId;
use crate::terminal::hyperlink::HyperlinkRegistry;

/// Bundles the bits of [`Terminal`](super::Terminal) state that OSC handlers
/// are allowed to read or mutate. Passing a single context keeps the call
/// signature stable as new OSC commands (8 hyperlinks, 7 cwd, 0/2 title, 4
/// palette, …) get wired in.
pub(super) struct OscContext<'a> {
    pub clipboard: &'a mut Clipboard,
    pub pending_output: &'a mut Vec<u8>,
    pub current_directory: &'a mut Option<PathBuf>,
    pub hyperlinks: &'a mut HyperlinkRegistry,
    pub current_hyperlink: &'a mut Option<HyperlinkId>,
}

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
    ctx: &mut OscContext<'_>,
) {
    let (cmd, rest) = split_osc(payload);
    match cmd {
        b"7" => handle_osc_7(rest, ctx.current_directory),
        b"8" => handle_osc_8(rest, ctx.hyperlinks, ctx.current_hyperlink),
        b"52" => handle_osc_52(rest, ctx.clipboard, ctx.pending_output),
        _ => {}
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

/// OSC 7 — current working directory reporting. Shells emit
/// `OSC 7 ; file://hostname/percent-encoded/path ST` after each `cd` so the
/// terminal can offer "open new window in this directory" or surface the
/// path in the title bar without parsing the prompt.
///
/// The hostname segment is informational (most terminals honour the path
/// regardless of host); we accept and ignore it. Empty payloads clear the
/// stored cwd, matching the behaviour shells use to indicate "I no longer
/// know where I am" (e.g. after a remote SSH session ends).
fn handle_osc_7(
    rest: &[u8],
    current_directory: &mut Option<PathBuf>,
) {
    if rest.is_empty() {
        *current_directory = None;
        return;
    }

    let Ok(uri) = std::str::from_utf8(rest) else {
        return;
    };

    // Strip the scheme. We only honour file://; ignoring other schemes keeps
    // remote shells (where the path is not meaningful locally) from poisoning
    // local features like "open new window here".
    let Some(rest) = uri.strip_prefix("file://") else {
        return;
    };

    // Drop the hostname between `file://` and the first `/`.
    let path_start = rest.find('/').unwrap_or(rest.len());
    let encoded_path = &rest[path_start..];
    if encoded_path.is_empty() {
        return;
    }

    let decoded = percent_decode(encoded_path.as_bytes()).collect::<Vec<u8>>();
    let Ok(path) = std::str::from_utf8(&decoded) else {
        return;
    };

    *current_directory = Some(PathBuf::from(path));
}

/// OSC 8 — hyperlinks. `OSC 8 ; params ; URI ST` opens a hyperlink span;
/// subsequent printed cells carry the link until a closing
/// `OSC 8 ; ; ST` (empty params + empty URI) ends it.
///
/// Params is a colon-separated `key=value` list — `id=…` is the only widely
/// used one, distinguishing adjacent links to the same URI. We honour `id`
/// when present so two distinct buttons pointing at the same URL still
/// register as two links.
fn handle_osc_8(
    rest: &[u8],
    registry: &mut HyperlinkRegistry,
    current: &mut Option<HyperlinkId>,
) {
    let (params, uri) = split_osc(rest);

    if uri.is_empty() {
        *current = None;
        return;
    }

    let Ok(uri_str) = std::str::from_utf8(uri) else {
        *current = None;
        return;
    };

    let id_param = params.split(|&b| b == b':').find_map(|kv| {
        let mut it = kv.splitn(2, |&b| b == b'=');
        let key = it.next()?;
        let value = it.next()?;
        if key == b"id" {
            std::str::from_utf8(value).ok()
        } else {
            None
        }
    });

    *current = Some(registry.intern(id_param, uri_str));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::hyperlink::HyperlinkRegistry;

    struct Bag {
        clipboard: Clipboard,
        pending: Vec<u8>,
        cwd: Option<PathBuf>,
        registry: HyperlinkRegistry,
        current_link: Option<HyperlinkId>,
    }

    impl Bag {
        fn new() -> Self {
            Self {
                clipboard: Clipboard::in_memory(),
                pending: Vec::new(),
                cwd: None,
                registry: HyperlinkRegistry::new(),
                current_link: None,
            }
        }

        fn dispatch(
            &mut self,
            payload: &[u8],
        ) {
            let mut ctx = OscContext {
                clipboard: &mut self.clipboard,
                pending_output: &mut self.pending,
                current_directory: &mut self.cwd,
                hyperlinks: &mut self.registry,
                current_hyperlink: &mut self.current_link,
            };
            handle_osc(payload, &mut ctx);
        }
    }

    #[test]
    fn osc_52_writes_clipboard_with_c_selector() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_52_writes_primary_with_p_selector() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;p;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Primary).as_deref(),
            Some("hello")
        );
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
    }

    #[test]
    fn osc_52_empty_selector_defaults_to_clipboard() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn osc_52_multi_selector_sets_both() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;cp;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Primary).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn osc_52_tolerates_embedded_whitespace_in_base64() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;aGVs\nbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn osc_52_rejects_invalid_base64() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;!!not-base64!!");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
    }

    #[test]
    fn osc_52_query_emits_base64_response() {
        let mut bag = Bag::new();
        bag.clipboard.set(ClipboardKind::Clipboard, "hi");
        bag.dispatch(b"52;c;?");
        assert_eq!(bag.pending, b"\x1b]52;c;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_query_echoes_original_selector() {
        let mut bag = Bag::new();
        bag.clipboard.set(ClipboardKind::Primary, "hi");
        bag.dispatch(b"52;p;?");
        assert_eq!(bag.pending, b"\x1b]52;p;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_ignored_for_unknown_command() {
        let mut bag = Bag::new();
        bag.dispatch(b"99;nothing");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_52_ignored_when_non_utf8() {
        // \xFF\xFE is valid base64 of 0xF5 0xFD 0xBF which is invalid UTF-8.
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;//2/");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
    }

    // ---- OSC 7 ----

    #[test]
    fn osc_7_decodes_simple_path() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;file://localhost/home/jessica");
        assert_eq!(bag.cwd, Some(PathBuf::from("/home/jessica")));
    }

    #[test]
    fn osc_7_percent_decodes_path() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;file:///home/has%20space/proj");
        assert_eq!(bag.cwd, Some(PathBuf::from("/home/has space/proj")));
    }

    #[test]
    fn osc_7_empty_clears() {
        let mut bag = Bag::new();
        bag.cwd = Some(PathBuf::from("/old"));
        bag.dispatch(b"7;");
        assert_eq!(bag.cwd, None);
    }

    #[test]
    fn osc_7_ignores_non_file_scheme() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;ftp://server/some/path");
        assert_eq!(bag.cwd, None);
    }

    #[test]
    fn osc_7_ignores_invalid_utf8() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;file:///\xFF\xFE");
        assert_eq!(bag.cwd, None);
    }

    // ---- OSC 8 ----

    #[test]
    fn osc_8_sets_current_link_with_uri() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;;https://example.com");
        let id = bag.current_link.expect("link set");
        assert_eq!(bag.registry.get(id), Some("https://example.com"));
    }

    #[test]
    fn osc_8_empty_uri_clears_current_link() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;;https://example.com");
        bag.dispatch(b"8;;");
        assert!(bag.current_link.is_none());
    }

    #[test]
    fn osc_8_distinct_id_keys_separate_link_ids() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;id=a;https://example.com");
        let id_a = bag.current_link.unwrap();
        bag.dispatch(b"8;id=b;https://example.com");
        let id_b = bag.current_link.unwrap();
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn osc_8_same_id_reuses_link_id() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;id=foo;https://example.com");
        let id_first = bag.current_link.unwrap();
        bag.dispatch(b"8;;"); // close
        bag.dispatch(b"8;id=foo;https://example.com");
        let id_again = bag.current_link.unwrap();
        assert_eq!(id_first, id_again);
    }
}
