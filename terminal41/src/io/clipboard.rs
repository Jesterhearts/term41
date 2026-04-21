use clip41::Clipboard;
use clip41::ClipboardKind;
use vte_mode41::C1Mode;

use crate::conformance;

pub fn paste(
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    bracketed_paste: bool,
    text: &str,
) {
    const PASTE_END: &str = "\x1b[201~";
    if bracketed_paste {
        conformance::write_csi(pending_output, c1_mode, format_args!("200~"));
        for chunk in text.split(PASTE_END) {
            pending_output.extend_from_slice(chunk.as_bytes());
        }
        conformance::write_csi(pending_output, c1_mode, format_args!("201~"));
    } else {
        for chunk in text.split(PASTE_END) {
            pending_output.extend_from_slice(chunk.as_bytes());
        }
    }
}

pub fn paste_from_clipboard(
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    bracketed_paste: bool,
    kind: ClipboardKind,
) {
    if let Some(text) = clipboard.get(kind)
        && !text.is_empty()
    {
        paste(pending_output, c1_mode, bracketed_paste, &text);
    }
}

pub fn copy_to_clipboard(
    clipboard: &mut Clipboard,
    text: &str,
) {
    clipboard.set(ClipboardKind::Clipboard, text);
}

#[cfg(test)]
mod tests {
    use clip41::Clipboard;

    use super::*;
    use crate::test_support::TestTerm;

    #[test]
    fn paste_default_is_raw() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.paste_text("hello\n");
        assert_eq!(term.take_pending_output(), b"hello\n");
    }

    #[test]
    fn paste_wraps_when_mode_2004_enabled() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        assert!(term.modes.bracketed_paste);
        term.paste_text("hello\n");
        assert_eq!(term.take_pending_output(), b"\x1b[200~hello\n\x1b[201~");
    }

    #[test]
    fn paste_wraps_with_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h\x1b G");
        term.paste_text("hello\n");
        assert_eq!(term.take_pending_output(), b"\x9b200~hello\n\x9b201~");
    }

    #[test]
    fn decrst_2004_disables_bracketed_paste() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        term.process(b"\x1b[?2004l");
        assert!(!term.modes.bracketed_paste);
        term.paste_text("hi");
        assert_eq!(term.take_pending_output(), b"hi");
    }

    #[test]
    fn paste_scrubs_embedded_end_marker() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        term.paste_text("evil\x1b[201~injection");
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[200~evilinjection\x1b[201~"
        );
    }

    #[test]
    fn paste_from_clipboard_round_trips() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        term.clipboard.set(ClipboardKind::Clipboard, "hello");
        term.paste_from_clipboard(ClipboardKind::Clipboard);
        assert_eq!(term.take_pending_output(), b"hello");
    }

    #[test]
    fn paste_from_clipboard_ignores_empty_selection() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        term.paste_from_clipboard(ClipboardKind::Clipboard);
        assert!(term.take_pending_output().is_empty());
    }
}
