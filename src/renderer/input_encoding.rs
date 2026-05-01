use terminal41::C1Mode;
use terminal41::KittyFlags;
use terminal41::KittyKeys;
use winit::keyboard::Key;
use winit::keyboard::KeyCode;
use winit::keyboard::KeyLocation;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::keyboard::PhysicalKey;

pub(crate) fn ctrl_byte(c: &str) -> Option<u8> {
    match c.as_bytes() {
        [b @ b'a'..=b'z'] => Some(b - b'a' + 1),
        [b @ b'A'..=b'Z'] => Some(b - b'A' + 1),
        [b'@'] => Some(0x00),
        [b'['] => Some(0x1B),
        [b'\\'] => Some(0x1C),
        [b']'] => Some(0x1D),
        [b'^'] => Some(0x1E),
        [b'_'] => Some(0x1F),
        _ => None,
    }
}

fn kitty_modifier_bits(mods: ModifiersState) -> u8 {
    let mut b = 0;
    if mods.shift_key() {
        b |= KittyKeys::SHIFT.bits();
    }
    if mods.alt_key() {
        b |= KittyKeys::ALT.bits();
    }
    if mods.control_key() {
        b |= KittyKeys::CTRL.bits();
    }
    if mods.super_key() {
        b |= KittyKeys::SUPER.bits();
    }
    b
}

fn encode_csi_bytes(
    args: std::fmt::Arguments<'_>,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mut out = Vec::new();
    if c1_mode == C1Mode::EightBit {
        out.push(0x9B);
    } else {
        out.extend_from_slice(b"\x1b[");
    }
    use std::io::Write as _;
    out.write_fmt(args).expect("write to Vec is infallible");
    out
}

fn encode_ss3_bytes(
    final_byte: char,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mut out = Vec::new();
    if c1_mode == C1Mode::EightBit {
        out.push(0x8F);
    } else {
        out.extend_from_slice(b"\x1bO");
    }
    out.push(final_byte as u8);
    out
}

pub(crate) fn kitty_encode_input(
    key: &Key,
    mods: ModifiersState,
    flags: KittyFlags,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    if !flags.contains(KittyFlags::DISAMBIGUATE_ESCAPE_CODES) {
        return None;
    }

    let mod_bits = kitty_modifier_bits(mods);
    let only_shift_or_none = (mod_bits & !1) == 0;
    let mod_param = mod_bits + 1;
    let report_text = flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT);
    let all_as_escape = flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);

    match key {
        Key::Character(s) => {
            // Pure text input (no modifiers beyond shift) is normally left as
            // the raw byte. REPORT_ALL_KEYS_AS_ESCAPE_CODES forces it into
            // CSI u form too so apps can tell key events apart from pastes.
            if only_shift_or_none && !all_as_escape {
                return None;
            }
            let lower = s.to_lowercase();
            let cp = lower.chars().next()? as u32;
            let text = report_text.then_some(s.as_str());
            Some(format_csi_u(cp, mod_param, text, c1_mode))
        }
        Key::Named(named) => kitty_encode_named(*named, mod_bits, mod_param, report_text, c1_mode),
        _ => None,
    }
}

/// Emit a CSI u sequence. `text`, when `Some` and non-empty, becomes the third
/// parameter as `cp1:cp2:...` — the associated text the key produced. Apps
/// with `REPORT_ASSOCIATED_TEXT` on use this to distinguish "user typed A"
/// from "user typed shift+a then Caps got hit"; the raw CSI u form alone
/// only carries the unmodified key code and the modifiers.
fn format_csi_u(
    cp: u32,
    mod_param: u8,
    text: Option<&str>,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mut out = encode_csi_bytes(format_args!(""), c1_mode);
    match text {
        Some(t) if !t.is_empty() => {
            use std::io::Write as _;
            out.write_fmt(format_args!("{cp};{mod_param};"))
                .expect("write to Vec is infallible");
            let mut first = true;
            for ch in t.chars() {
                if !first {
                    out.push(b':');
                }
                first = false;
                out.write_fmt(format_args!("{}", ch as u32))
                    .expect("write to Vec is infallible");
            }
            out.push(b'u');
            out
        }
        _ => {
            use std::io::Write as _;
            out.write_fmt(format_args!("{cp};{mod_param}u"))
                .expect("write to Vec is infallible");
            out
        }
    }
}

/// Encode an IME commit as a synthetic key event under the kitty protocol.
/// Key code 0 is the spec's sentinel for "this wasn't a physical key" —
/// editors read that plus the text param and can treat the string as a
/// single input block instead of N individual keystrokes. Callers should
/// only route through here when `REPORT_ASSOCIATED_TEXT` is set; without it,
/// the bytes go straight to the PTY unchanged.
pub(crate) fn kitty_encode_ime_commit(
    text: &str,
    c1_mode: C1Mode,
) -> Vec<u8> {
    format_csi_u(0, 0, Some(text), c1_mode)
}

fn kitty_encode_named(
    named: NamedKey,
    mod_bits: u8,
    mod_param: u8,
    report_text: bool,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    let direct_code = match named {
        NamedKey::Enter => Some(13u32),
        NamedKey::Tab => Some(9),
        NamedKey::Backspace => Some(127),
        NamedKey::Escape => Some(27),
        NamedKey::Space => Some(32),
        _ => None,
    };
    if let Some(cp) = direct_code {
        if (mod_bits & !1) == 0 && mod_bits == 0 {
            return None;
        }
        // Enter/Tab/Space genuinely produce text ("\r", "\t", " "); Backspace
        // and Escape don't — they're control actions, no text param for them.
        let text: Option<&str> = if report_text {
            match named {
                NamedKey::Enter => Some("\r"),
                NamedKey::Tab => Some("\t"),
                NamedKey::Space => Some(" "),
                _ => None,
            }
        } else {
            None
        };
        return Some(format_csi_u(cp, mod_param, text, c1_mode));
    }

    if mod_bits == 0 {
        return None;
    }

    let arrow_action = match named {
        NamedKey::ArrowUp => Some('A'),
        NamedKey::ArrowDown => Some('B'),
        NamedKey::ArrowRight => Some('C'),
        NamedKey::ArrowLeft => Some('D'),
        NamedKey::Home => Some('H'),
        NamedKey::End => Some('F'),
        _ => None,
    };
    if let Some(action) = arrow_action {
        return Some(encode_csi_bytes(
            format_args!("1;{mod_param}{action}"),
            c1_mode,
        ));
    }

    let tilde_code = match named {
        NamedKey::Insert => Some(2u32),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(code) = tilde_code {
        return Some(encode_csi_bytes(
            format_args!("{code};{mod_param}~"),
            c1_mode,
        ));
    }

    None
}

/// Encode a named key for legacy (non-Kitty) mode, using xterm-style
/// modifier encoding. Plain keys use standard VT/xterm sequences;
/// modified keys use the `CSI 1;mod X` (arrows/Home/End) or
/// `CSI code;mod ~` (F-keys/Ins/Del/PgUp/PgDn) format where
/// mod = 1 + Shift(1) + Alt(2) + Ctrl(4).
pub(crate) fn legacy_encode_named(
    key: NamedKey,
    location: KeyLocation,
    mods: ModifiersState,
    app_cursor_keys: bool,
    app_keypad: bool,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    let mod_param = legacy_modifier_param(mods);

    if mod_param == 0
        && app_keypad
        && location == KeyLocation::Numpad
        && let Some(ch) = application_keypad_final(key)
    {
        return Some(encode_ss3_bytes(ch, c1_mode));
    }

    // Simple keys that don't take modifier parameters.
    if mod_param == 0 {
        let plain = match key {
            NamedKey::Enter => Some(&b"\r"[..]),
            NamedKey::Backspace => Some(&b"\x7f"[..]),
            NamedKey::Tab => Some(&b"\t"[..]),
            NamedKey::Escape => Some(&b"\x1b"[..]),
            NamedKey::Space => Some(&b" "[..]),
            _ => None,
        };
        if let Some(bytes) = plain {
            return Some(bytes.to_vec());
        }
    }

    // Shift+Tab → CSI Z (backtab).
    if key == NamedKey::Tab && mods.shift_key() {
        return Some(encode_csi_bytes(format_args!("Z"), c1_mode));
    }

    // Arrow-style keys: CSI [1;mod] X
    // In DECCKM (app cursor keys) mode, unmodified arrows/Home/End send
    // SS3 form (ESC O X) instead of CSI form (ESC [ X).
    let arrow_final = match key {
        NamedKey::ArrowUp => Some('A'),
        NamedKey::ArrowDown => Some('B'),
        NamedKey::ArrowRight => Some('C'),
        NamedKey::ArrowLeft => Some('D'),
        NamedKey::Home => Some('H'),
        NamedKey::End => Some('F'),
        _ => None,
    };
    if let Some(ch) = arrow_final {
        return if mod_param > 0 {
            Some(encode_csi_bytes(format_args!("1;{mod_param}{ch}"), c1_mode))
        } else if app_cursor_keys {
            Some(encode_ss3_bytes(ch, c1_mode))
        } else {
            Some(encode_csi_bytes(format_args!("{ch}"), c1_mode))
        };
    }

    // Tilde-style keys: CSI code [;mod] ~
    let tilde_code = match key {
        NamedKey::Insert => Some(2),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(code) = tilde_code {
        return if mod_param > 0 {
            Some(encode_csi_bytes(
                format_args!("{code};{mod_param}~"),
                c1_mode,
            ))
        } else {
            Some(encode_csi_bytes(format_args!("{code}~"), c1_mode))
        };
    }

    // F1-F4 use SS3 unmodified, CSI 1;mod P/Q/R/S with modifiers.
    let f1_4_final = match key {
        NamedKey::F1 => Some('P'),
        NamedKey::F2 => Some('Q'),
        NamedKey::F3 => Some('R'),
        NamedKey::F4 => Some('S'),
        _ => None,
    };
    if let Some(ch) = f1_4_final {
        return if mod_param > 0 {
            Some(encode_csi_bytes(format_args!("1;{mod_param}{ch}"), c1_mode))
        } else {
            Some(encode_ss3_bytes(ch, c1_mode))
        };
    }

    // F5-F20 use tilde-style: CSI code [;mod] ~. DEC skips 22, 27, and 30.
    let fkey_code = match key {
        NamedKey::F5 => Some(15),
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        NamedKey::F13 => Some(25),
        NamedKey::F14 => Some(26),
        NamedKey::F15 => Some(28),
        NamedKey::F16 => Some(29),
        NamedKey::F17 => Some(31),
        NamedKey::F18 => Some(32),
        NamedKey::F19 => Some(33),
        NamedKey::F20 => Some(34),
        _ => None,
    };
    if let Some(code) = fkey_code {
        return if mod_param > 0 {
            Some(encode_csi_bytes(
                format_args!("{code};{mod_param}~"),
                c1_mode,
            ))
        } else {
            Some(encode_csi_bytes(format_args!("{code}~"), c1_mode))
        };
    }

    None
}

fn application_keypad_final(key: NamedKey) -> Option<char> {
    match key {
        NamedKey::Enter => Some('M'),
        NamedKey::ArrowUp => Some('A'),
        NamedKey::ArrowDown => Some('B'),
        NamedKey::ArrowRight => Some('C'),
        NamedKey::ArrowLeft => Some('D'),
        NamedKey::PageUp => Some('I'),
        NamedKey::PageDown => Some('G'),
        NamedKey::Home => Some('H'),
        NamedKey::End => Some('F'),
        NamedKey::Insert => Some('L'),
        NamedKey::Delete => Some('N'),
        _ => None,
    }
}

pub(crate) fn legacy_encode_numpad_character(
    text: &str,
    location: KeyLocation,
    physical: PhysicalKey,
    mods: ModifiersState,
    app_keypad: bool,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    if location != KeyLocation::Numpad || legacy_modifier_param(mods) != 0 {
        return None;
    }

    let code = match physical {
        PhysicalKey::Code(code) => code,
        _ => return None,
    };

    if app_keypad {
        let ch = match code {
            KeyCode::Numpad0 => 'p',
            KeyCode::Numpad1 => 'q',
            KeyCode::Numpad2 => 'r',
            KeyCode::Numpad3 => 's',
            KeyCode::Numpad4 => 't',
            KeyCode::Numpad5 => 'u',
            KeyCode::Numpad6 => 'v',
            KeyCode::Numpad7 => 'w',
            KeyCode::Numpad8 => 'x',
            KeyCode::Numpad9 => 'y',
            KeyCode::NumpadDecimal => 'n',
            KeyCode::NumpadComma => 'l',
            KeyCode::NumpadDivide => 'o',
            KeyCode::NumpadMultiply => 'j',
            KeyCode::NumpadSubtract => 'm',
            KeyCode::NumpadAdd => 'k',
            _ => return None,
        };
        Some(encode_ss3_bytes(ch, c1_mode))
    } else {
        let bytes = match code {
            KeyCode::Numpad0 => b"0".to_vec(),
            KeyCode::Numpad1 => b"1".to_vec(),
            KeyCode::Numpad2 => b"2".to_vec(),
            KeyCode::Numpad3 => b"3".to_vec(),
            KeyCode::Numpad4 => b"4".to_vec(),
            KeyCode::Numpad5 => b"5".to_vec(),
            KeyCode::Numpad6 => b"6".to_vec(),
            KeyCode::Numpad7 => b"7".to_vec(),
            KeyCode::Numpad8 => b"8".to_vec(),
            KeyCode::Numpad9 => b"9".to_vec(),
            KeyCode::NumpadDecimal => b".".to_vec(),
            KeyCode::NumpadComma => b",".to_vec(),
            KeyCode::NumpadDivide => b"/".to_vec(),
            KeyCode::NumpadMultiply => b"*".to_vec(),
            KeyCode::NumpadSubtract => b"-".to_vec(),
            KeyCode::NumpadAdd => b"+".to_vec(),
            _ => text.as_bytes().to_vec(),
        };
        Some(bytes)
    }
}

/// Compute the xterm modifier parameter: 1 + (shift | alt | ctrl).
/// Returns 0 when no modifiers are held, meaning the plain (unmodified)
/// sequence should be used.
fn legacy_modifier_param(mods: ModifiersState) -> u8 {
    let mut bits: u8 = 0;
    if mods.shift_key() {
        bits |= 1;
    }
    if mods.alt_key() {
        bits |= 2;
    }
    if mods.control_key() {
        bits |= 4;
    }
    if bits == 0 { 0 } else { bits + 1 }
}

mod kitty_encode_tests {
    use winit::keyboard::Key;
    use winit::keyboard::ModifiersState;
    use winit::keyboard::NamedKey;
    use winit::keyboard::SmolStr;

    use super::*;

    fn char_key(s: &str) -> Key {
        Key::Character(SmolStr::new(s))
    }

    #[test]
    fn ctrl_letter_without_text_flag() {
        let bytes = kitty_encode_input(
            &char_key("a"),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;5u");
    }

    #[test]
    fn ctrl_letter_with_text_flag_appends_text_param() {
        let bytes = kitty_encode_input(
            &char_key("a"),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        // text param is the codepoint of the produced char ("a" = 97)
        assert_eq!(bytes, b"\x1b[97;5;97u");
    }

    #[test]
    fn shift_a_with_all_as_escape_and_text() {
        // Plain "A" (shift+a) normally emits no CSI u. With REPORT_ALL_KEYS
        // the key code is the unmodified base ("a" = 97), modifier param is
        // 2 (shift = bit 0 + 1), text param carries the actual produced
        // character so apps can distinguish a true "A" from a synth one.
        let bytes = kitty_encode_input(
            &char_key("A"),
            ModifiersState::SHIFT,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;2;65u");
    }

    #[test]
    fn plain_text_without_all_as_escape_is_not_encoded() {
        // Just REPORT_ASSOCIATED_TEXT shouldn't force plain text into CSI u;
        // the raw byte path still handles it.
        assert!(
            kitty_encode_input(
                &char_key("a"),
                ModifiersState::empty(),
                KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
                C1Mode::SevenBit,
            )
            .is_none()
        );
    }

    #[test]
    fn enter_with_text_flag_reports_cr_as_text() {
        let bytes = kitty_encode_input(
            &Key::Named(NamedKey::Enter),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        // Enter's associated text is "\r" (13).
        assert_eq!(bytes, b"\x1b[13;5;13u");
    }

    #[test]
    fn escape_with_text_flag_has_no_text_param() {
        // Escape is a control action, not a text-producing key.
        let bytes = kitty_encode_input(
            &Key::Named(NamedKey::Escape),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[27;5u");
    }

    #[test]
    fn ime_commit_uses_zero_key_and_zero_mods() {
        // Spec sentinel: key code 0 + modifier param 0 means "not a physical
        // key". Codepoints join with ':'. 啊 = U+554A (0x554A = 21834),
        // 不 = U+4E0D (0x4E0D = 19981).
        let bytes = kitty_encode_ime_commit("啊不", C1Mode::SevenBit);
        assert_eq!(bytes, b"\x1b[0;0;21834:19981u");
    }

    #[test]
    fn ime_commit_single_codepoint() {
        let bytes = kitty_encode_ime_commit("é", C1Mode::SevenBit);
        // é = U+00E9 = 233
        assert_eq!(bytes, b"\x1b[0;0;233u");
    }

    #[test]
    fn kitty_encode_uses_8bit_csi_when_requested() {
        let bytes = kitty_encode_input(
            &char_key("a"),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            C1Mode::EightBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x9b97;5u");
    }

    #[test]
    fn legacy_app_cursor_keys_use_8bit_ss3_when_requested() {
        let bytes = legacy_encode_named(
            NamedKey::ArrowUp,
            KeyLocation::Standard,
            ModifiersState::empty(),
            true,
            false,
            C1Mode::EightBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x8fA");
    }

    #[test]
    fn legacy_app_keypad_encodes_numpad_named_keys_as_ss3() {
        let bytes = legacy_encode_named(
            NamedKey::Enter,
            KeyLocation::Numpad,
            ModifiersState::empty(),
            false,
            true,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1bOM");
    }

    #[test]
    fn legacy_app_keypad_encodes_numpad_digits_as_ss3() {
        let bytes = legacy_encode_numpad_character(
            "7",
            KeyLocation::Numpad,
            PhysicalKey::Code(KeyCode::Numpad7),
            ModifiersState::empty(),
            true,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1bOw");
    }

    #[test]
    fn legacy_numeric_keypad_uses_physical_numpad_digit_even_if_logical_key_varies() {
        let bytes = legacy_encode_numpad_character(
            "Home",
            KeyLocation::Numpad,
            PhysicalKey::Code(KeyCode::Numpad7),
            ModifiersState::empty(),
            false,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"7");
    }
}
