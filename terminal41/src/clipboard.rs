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
