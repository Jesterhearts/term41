use unicode_segmentation::UnicodeSegmentation;

/// Translate a buffer transform into the shell's ordinary editing keys.
pub(super) fn shell_edit_bytes(
    before: &str,
    before_cursor: usize,
    after: &str,
    after_cursor: usize,
    bracketed_paste: bool,
    app_cursor: bool,
) -> Result<Vec<u8>, &'static str> {
    if !before.is_char_boundary(before_cursor) || !after.is_char_boundary(after_cursor) {
        return Err("invalid cursor boundary");
    }
    if after
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return Err("control characters cannot be inserted into shell input");
    }
    if before == after {
        let mut bytes = Vec::new();
        move_cursor(&mut bytes, before, before_cursor, after_cursor, app_cursor);
        return Ok(bytes);
    }
    let prefix = before
        .graphemes(true)
        .zip(after.graphemes(true))
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len())
        .sum::<usize>();
    let suffix = before[prefix..]
        .graphemes(true)
        .rev()
        .zip(after[prefix..].graphemes(true).rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len())
        .sum::<usize>();
    let removed = &before[prefix..before.len() - suffix];
    let inserted = &after[prefix..after.len() - suffix];
    if !bracketed_paste && inserted.contains(['\n', '\t']) {
        return Err("multi-line and tab insertion require shell bracketed paste support");
    }
    let mut bytes = Vec::new();
    move_cursor(&mut bytes, before, before_cursor, prefix, app_cursor);
    for _ in 0..shell_character_count(removed) {
        bytes.extend_from_slice(b"\x1b[3~");
    }
    if !inserted.is_empty() {
        if bracketed_paste {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(inserted.as_bytes());
        if bracketed_paste {
            bytes.extend_from_slice(b"\x1b[201~");
        }
    }
    move_cursor(
        &mut bytes,
        after,
        after.len() - suffix,
        after_cursor,
        app_cursor,
    );
    Ok(bytes)
}

fn move_cursor(
    bytes: &mut Vec<u8>,
    text: &str,
    from: usize,
    to: usize,
    app_cursor: bool,
) {
    let key = match (from > to, app_cursor) {
        (true, false) => b"\x1b[D",
        (true, true) => b"\x1bOD",
        (false, false) => b"\x1b[C",
        (false, true) => b"\x1bOC",
    };
    for _ in 0..shell_character_count(&text[from.min(to)..from.max(to)]) {
        bytes.extend_from_slice(key);
    }
}

fn shell_character_count(text: &str) -> usize {
    // Readline moves over base characters with their zero-width marks. A
    // terminal grapheme such as a ZWJ emoji can contain several such units.
    text.chars()
        .enumerate()
        .filter(|(index, ch)| *index == 0 || unicode_width::UnicodeWidthChar::width(*ch) != Some(0))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_deletion_and_paste_use_shell_keys() {
        assert_eq!(
            shell_edit_bytes("abc", 3, "abc", 1, true, false).unwrap(),
            b"\x1b[D\x1b[D"
        );
        assert_eq!(
            shell_edit_bytes("echo old", 8, "echo new", 8, true, false).unwrap(),
            b"\x1b[D\x1b[D\x1b[D\x1b[3~\x1b[3~\x1b[3~\x1b[200~new\x1b[201~"
        );
        assert_eq!(
            shell_edit_bytes("abc", 1, "ac", 1, true, true).unwrap(),
            b"\x1b[3~"
        );
    }

    #[test]
    fn multiline_paste_is_never_submitted_by_an_edit() {
        assert_eq!(
            shell_edit_bytes("", 0, "echo one\necho two", 17, true, false).unwrap(),
            b"\x1b[200~echo one\necho two\x1b[201~"
        );
        assert!(shell_edit_bytes("", 0, "a\nb", 3, false, false).is_err());
        assert!(shell_edit_bytes("", 0, "\x1b[201~oops", 10, true, false).is_err());
    }
}
