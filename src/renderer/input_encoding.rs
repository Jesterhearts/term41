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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KittyKeyEventType {
    Press = 1,
    Repeat = 2,
    Release = 3,
}

pub(crate) struct KittyInput<'a> {
    pub(crate) key: &'a Key,
    pub(crate) key_without_modifiers: &'a Key,
    pub(crate) text: Option<&'a str>,
    pub(crate) location: KeyLocation,
    pub(crate) physical: PhysicalKey,
    pub(crate) modifiers: ModifiersState,
    pub(crate) event_type: KittyKeyEventType,
}

pub(crate) fn kitty_encode_input(
    input: KittyInput<'_>,
    flags: KittyFlags,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    if flags.is_empty()
        || (input.event_type == KittyKeyEventType::Release
            && !flags.contains(KittyFlags::REPORT_EVENT_TYPES))
    {
        return None;
    }

    let mod_bits = kitty_modifier_bits(input.modifiers);
    let all_as_escape = flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let disambiguate = all_as_escape || flags.contains(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
    let report_events = flags.contains(KittyFlags::REPORT_EVENT_TYPES);
    let report_alternates = flags.contains(KittyFlags::REPORT_ALTERNATE_KEYS);
    let report_text = all_as_escape
        && flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT)
        && input.event_type != KittyKeyEventType::Release;
    let produces_text = input
        .text
        .is_some_and(|text| text.chars().any(is_associated_text_codepoint));

    match input.key {
        Key::Character(text) => {
            if input.location == KeyLocation::Numpad
                && (all_as_escape || disambiguate && !produces_text)
                && let Some(code) = kitty_numpad_code(input.key, input.physical)
            {
                return Some(format_csi_u(
                    KittyKeyCodes::new(code),
                    mod_bits,
                    report_events.then_some(input.event_type),
                    report_text.then_some(input.text).flatten(),
                    c1_mode,
                ));
            }

            let has_non_shift_modifier = mod_bits & !KittyKeys::SHIFT.bits() != 0;
            if !(all_as_escape
                || disambiguate && has_non_shift_modifier
                || report_events && !produces_text)
            {
                return None;
            }

            let primary = key_character(input.key_without_modifiers)
                .or_else(|| text.to_lowercase().chars().next())? as u32;
            let shifted = report_alternates
                .then(|| shifted_key_code(input.key_without_modifiers, input.key, input.modifiers))
                .flatten()
                .filter(|code| *code != primary);
            let base = report_alternates
                .then(|| kitty_base_layout_code(input.physical))
                .flatten()
                .filter(|code| *code != primary);
            Some(format_csi_u(
                KittyKeyCodes {
                    primary,
                    shifted,
                    base,
                },
                mod_bits,
                report_events.then_some(input.event_type),
                report_text.then_some(input.text).flatten(),
                c1_mode,
            ))
        }
        Key::Named(named) => kitty_encode_named(
            *named,
            input.location,
            input.physical,
            input.text,
            produces_text,
            mod_bits,
            flags,
            input.event_type,
            c1_mode,
        ),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct KittyKeyCodes {
    primary: u32,
    shifted: Option<u32>,
    base: Option<u32>,
}

impl KittyKeyCodes {
    fn new(primary: u32) -> Self {
        Self {
            primary,
            shifted: None,
            base: None,
        }
    }
}

/// Emit the canonical CSI-u form, including optional alternate key codes,
/// event type, and safe associated text.
fn format_csi_u(
    codes: KittyKeyCodes,
    mod_bits: u8,
    event_type: Option<KittyKeyEventType>,
    text: Option<&str>,
    c1_mode: C1Mode,
) -> Vec<u8> {
    use std::io::Write as _;

    let mut out = encode_csi_bytes(format_args!("{}", codes.primary), c1_mode);
    if codes.shifted.is_some() || codes.base.is_some() {
        out.push(b':');
        if let Some(shifted) = codes.shifted {
            out.write_fmt(format_args!("{shifted}"))
                .expect("write to Vec is infallible");
        }
        if let Some(base) = codes.base {
            out.write_fmt(format_args!(":{base}"))
                .expect("write to Vec is infallible");
        }
    }

    out.write_fmt(format_args!(";{}", mod_bits + 1))
        .expect("write to Vec is infallible");
    if let Some(event_type) = event_type {
        out.write_fmt(format_args!(":{}", event_type as u8))
            .expect("write to Vec is infallible");
    }

    let safe_text = text.filter(|text| text.chars().any(is_associated_text_codepoint));
    if let Some(text) = safe_text {
        out.push(b';');
        let mut first = true;
        for ch in text.chars().filter(|ch| is_associated_text_codepoint(*ch)) {
            if !first {
                out.push(b':');
            }
            first = false;
            out.write_fmt(format_args!("{}", ch as u32))
                .expect("write to Vec is infallible");
        }
    }
    out.push(b'u');
    out
}

fn is_associated_text_codepoint(ch: char) -> bool {
    !matches!(ch as u32, 0x00..=0x1f | 0x7f..=0x9f)
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
    format_csi_u(KittyKeyCodes::new(0), 0, None, Some(text), c1_mode)
}

fn kitty_encode_named(
    named: NamedKey,
    location: KeyLocation,
    physical: PhysicalKey,
    text: Option<&str>,
    produces_text: bool,
    mod_bits: u8,
    flags: KittyFlags,
    event_type: KittyKeyEventType,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    let all_as_escape = flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let disambiguate = all_as_escape || flags.contains(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
    let report_events = flags.contains(KittyFlags::REPORT_EVENT_TYPES);
    let report_text = all_as_escape
        && flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT)
        && event_type != KittyKeyEventType::Release;

    if location == KeyLocation::Numpad
        && disambiguate
        && let Some(code) = kitty_numpad_code(&Key::Named(named), physical)
    {
        return Some(format_csi_u(
            KittyKeyCodes::new(code),
            mod_bits,
            report_events.then_some(event_type),
            None,
            c1_mode,
        ));
    }

    let direct_code = match named {
        NamedKey::Enter => Some(13u32),
        NamedKey::Tab => Some(9),
        NamedKey::Backspace => Some(127),
        NamedKey::Escape => Some(27),
        NamedKey::Space => Some(32),
        _ => None,
    };
    if let Some(cp) = direct_code {
        let legacy_reset_key =
            matches!(named, NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace);
        let space_needs_disambiguation =
            named == NamedKey::Space && mod_bits & !KittyKeys::SHIFT.bits() != 0;
        let encode = all_as_escape
            || (!legacy_reset_key
                && ((report_events && !produces_text)
                    || (disambiguate
                        && (named == NamedKey::Escape || space_needs_disambiguation))));
        if !encode {
            return None;
        }
        return Some(format_csi_u(
            KittyKeyCodes::new(cp),
            mod_bits,
            report_events.then_some(event_type),
            report_text.then_some(text).flatten(),
            c1_mode,
        ));
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
        if !all_as_escape && !report_events && (!disambiguate || mod_bits == 0) {
            return None;
        }
        if mod_bits == 0 && !report_events {
            return Some(encode_csi_bytes(format_args!("{action}"), c1_mode));
        }
        return Some(format_legacy_function_key(
            1,
            mod_bits,
            report_events.then_some(event_type),
            action,
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
        if !all_as_escape && !report_events && (!disambiguate || mod_bits == 0) {
            return None;
        }
        return Some(format_legacy_tilde_key(
            code,
            mod_bits,
            report_events.then_some(event_type),
            c1_mode,
        ));
    }

    let f1_4 = match named {
        NamedKey::F1 => Some('P'),
        NamedKey::F2 => Some('Q'),
        NamedKey::F3 => None,
        NamedKey::F4 => Some('S'),
        _ => None,
    };
    if let Some(action) = f1_4 {
        if !all_as_escape && !report_events && (!disambiguate || mod_bits == 0) {
            return None;
        }
        if mod_bits == 0 && !report_events {
            return Some(encode_csi_bytes(format_args!("{action}"), c1_mode));
        }
        return Some(format_legacy_function_key(
            1,
            mod_bits,
            report_events.then_some(event_type),
            action,
            c1_mode,
        ));
    }

    let f_tilde = match named {
        NamedKey::F3 => Some(13),
        NamedKey::F5 => Some(15),
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        _ => None,
    };
    if let Some(code) = f_tilde {
        if !all_as_escape && !report_events && (!disambiguate || mod_bits == 0) {
            return None;
        }
        return Some(format_legacy_tilde_key(
            code,
            mod_bits,
            report_events.then_some(event_type),
            c1_mode,
        ));
    }

    if is_modifier_key(named) && !all_as_escape {
        return None;
    }
    let code = kitty_named_code(named, location, physical)?;
    (all_as_escape || report_events || disambiguate).then(|| {
        format_csi_u(
            KittyKeyCodes::new(code),
            mod_bits,
            report_events.then_some(event_type),
            None,
            c1_mode,
        )
    })
}

fn is_modifier_key(named: NamedKey) -> bool {
    matches!(
        named,
        NamedKey::Alt
            | NamedKey::AltGraph
            | NamedKey::Control
            | NamedKey::Hyper
            | NamedKey::Meta
            | NamedKey::Shift
            | NamedKey::Super
    )
}

fn format_legacy_function_key(
    first_param: u32,
    mod_bits: u8,
    event_type: Option<KittyKeyEventType>,
    action: char,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mod_param = mod_bits + 1;
    match event_type {
        Some(event_type) => encode_csi_bytes(
            format_args!("{first_param};{mod_param}:{}{action}", event_type as u8),
            c1_mode,
        ),
        None => encode_csi_bytes(format_args!("{first_param};{mod_param}{action}"), c1_mode),
    }
}

fn format_legacy_tilde_key(
    code: u32,
    mod_bits: u8,
    event_type: Option<KittyKeyEventType>,
    c1_mode: C1Mode,
) -> Vec<u8> {
    if mod_bits == 0 && event_type.is_none() {
        return encode_csi_bytes(format_args!("{code}~"), c1_mode);
    }
    format_legacy_function_key(code, mod_bits, event_type, '~', c1_mode)
}

fn key_character(key: &Key) -> Option<char> {
    match key {
        Key::Character(text) => text.chars().next(),
        Key::Named(NamedKey::Space) => Some(' '),
        _ => None,
    }
}

fn shifted_key_code(
    _key_without_modifiers: &Key,
    key: &Key,
    modifiers: ModifiersState,
) -> Option<u32> {
    if !modifiers.shift_key() || modifiers.alt_key() {
        return None;
    }
    key_character(key).map(|ch| ch as u32)
}

fn kitty_base_layout_code(physical: PhysicalKey) -> Option<u32> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    let ch = match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Backquote => '`',
        KeyCode::Backslash => '\\',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Comma => ',',
        KeyCode::Equal => '=',
        KeyCode::Minus => '-',
        KeyCode::Period => '.',
        KeyCode::Quote => '\'',
        KeyCode::Semicolon => ';',
        KeyCode::Slash => '/',
        _ => return None,
    };
    Some(ch as u32)
}

fn kitty_numpad_code(
    key: &Key,
    physical: PhysicalKey,
) -> Option<u32> {
    let navigation = match key {
        Key::Named(NamedKey::ArrowLeft) => Some(57417),
        Key::Named(NamedKey::ArrowRight) => Some(57418),
        Key::Named(NamedKey::ArrowUp) => Some(57419),
        Key::Named(NamedKey::ArrowDown) => Some(57420),
        Key::Named(NamedKey::PageUp) => Some(57421),
        Key::Named(NamedKey::PageDown) => Some(57422),
        Key::Named(NamedKey::Home) => Some(57423),
        Key::Named(NamedKey::End) => Some(57424),
        Key::Named(NamedKey::Insert) => Some(57425),
        Key::Named(NamedKey::Delete) => Some(57426),
        Key::Named(NamedKey::Clear) => Some(57427),
        _ => None,
    };
    if navigation.is_some() {
        return navigation;
    }
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::Numpad0 => 57399,
        KeyCode::Numpad1 => 57400,
        KeyCode::Numpad2 => 57401,
        KeyCode::Numpad3 => 57402,
        KeyCode::Numpad4 => 57403,
        KeyCode::Numpad5 => 57404,
        KeyCode::Numpad6 => 57405,
        KeyCode::Numpad7 => 57406,
        KeyCode::Numpad8 => 57407,
        KeyCode::Numpad9 => 57408,
        KeyCode::NumpadDecimal => 57409,
        KeyCode::NumpadDivide => 57410,
        KeyCode::NumpadMultiply => 57411,
        KeyCode::NumpadSubtract => 57412,
        KeyCode::NumpadAdd => 57413,
        KeyCode::NumpadEnter => 57414,
        KeyCode::NumpadEqual => 57415,
        KeyCode::NumpadComma => 57416,
        _ => return None,
    })
}

fn kitty_named_code(
    named: NamedKey,
    location: KeyLocation,
    physical: PhysicalKey,
) -> Option<u32> {
    if location == KeyLocation::Numpad
        && let Some(code) = kitty_numpad_code(&Key::Named(named), physical)
    {
        return Some(code);
    }
    Some(match named {
        NamedKey::CapsLock => 57358,
        NamedKey::ScrollLock => 57359,
        NamedKey::NumLock => 57360,
        NamedKey::PrintScreen => 57361,
        NamedKey::Pause => 57362,
        NamedKey::ContextMenu => 57363,
        NamedKey::F13 => 57376,
        NamedKey::F14 => 57377,
        NamedKey::F15 => 57378,
        NamedKey::F16 => 57379,
        NamedKey::F17 => 57380,
        NamedKey::F18 => 57381,
        NamedKey::F19 => 57382,
        NamedKey::F20 => 57383,
        NamedKey::F21 => 57384,
        NamedKey::F22 => 57385,
        NamedKey::F23 => 57386,
        NamedKey::F24 => 57387,
        NamedKey::F25 => 57388,
        NamedKey::F26 => 57389,
        NamedKey::F27 => 57390,
        NamedKey::F28 => 57391,
        NamedKey::F29 => 57392,
        NamedKey::F30 => 57393,
        NamedKey::F31 => 57394,
        NamedKey::F32 => 57395,
        NamedKey::F33 => 57396,
        NamedKey::F34 => 57397,
        NamedKey::F35 => 57398,
        NamedKey::MediaPlay => 57428,
        NamedKey::MediaPause => 57429,
        NamedKey::MediaPlayPause => 57430,
        NamedKey::MediaStop => 57432,
        NamedKey::MediaFastForward => 57433,
        NamedKey::MediaRewind => 57434,
        NamedKey::MediaTrackNext => 57435,
        NamedKey::MediaTrackPrevious => 57436,
        NamedKey::MediaRecord => 57437,
        NamedKey::AudioVolumeDown => 57438,
        NamedKey::AudioVolumeUp => 57439,
        NamedKey::AudioVolumeMute => 57440,
        NamedKey::Shift => match location {
            KeyLocation::Right => 57447,
            _ => 57441,
        },
        NamedKey::Control => match location {
            KeyLocation::Right => 57448,
            _ => 57442,
        },
        NamedKey::Alt => match location {
            KeyLocation::Right => 57449,
            _ => 57443,
        },
        NamedKey::Super => match location {
            KeyLocation::Right => 57450,
            _ => 57444,
        },
        NamedKey::Hyper => match location {
            KeyLocation::Right => 57451,
            _ => 57445,
        },
        NamedKey::Meta => match location {
            KeyLocation::Right => 57452,
            _ => 57446,
        },
        NamedKey::AltGraph => 57453,
        _ => return None,
    })
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

    if let Some(mut bytes) = legacy_direct_key(key, mods, c1_mode) {
        if mods.alt_key() {
            bytes.insert(0, 0x1b);
        }
        return Some(bytes);
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
        NamedKey::ContextMenu => Some(29),
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

    // F1, F2, and F4 use SS3 unmodified and CSI with modifiers. F3 uses
    // CSI 13 ~ because SS3 R conflicts with cursor-position reports.
    let f1_4_final = match key {
        NamedKey::F1 => Some('P'),
        NamedKey::F2 => Some('Q'),
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
        NamedKey::F3 => Some(13),
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

fn legacy_direct_key(
    key: NamedKey,
    mods: ModifiersState,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    match key {
        NamedKey::Enter => Some(b"\r".to_vec()),
        NamedKey::Backspace if mods.control_key() => Some(b"\x08".to_vec()),
        NamedKey::Backspace => Some(b"\x7f".to_vec()),
        NamedKey::Tab if mods.shift_key() => Some(encode_csi_bytes(format_args!("Z"), c1_mode)),
        NamedKey::Tab => Some(b"\t".to_vec()),
        NamedKey::Escape => Some(b"\x1b".to_vec()),
        NamedKey::Space if mods.control_key() => Some(b"\0".to_vec()),
        NamedKey::Space => Some(b" ".to_vec()),
        _ => None,
    }
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

#[cfg(test)]
mod kitty_encode_tests {
    use winit::keyboard::Key;
    use winit::keyboard::ModifiersState;
    use winit::keyboard::NamedKey;
    use winit::keyboard::SmolStr;

    use super::*;

    fn char_key(s: &str) -> Key {
        Key::Character(SmolStr::new(s))
    }

    fn encode(
        key: &Key,
        key_without_modifiers: &Key,
        physical: PhysicalKey,
        mods: ModifiersState,
        flags: KittyFlags,
        event_type: KittyKeyEventType,
    ) -> Option<Vec<u8>> {
        let produced_text = match key {
            Key::Character(text) if mods.control_key() => {
                ctrl_byte(text).map(char::from).map(|ch| ch.to_string())
            }
            Key::Character(text) => Some(text.to_string()),
            Key::Named(NamedKey::Space) if mods.control_key() => Some("\0".to_string()),
            Key::Named(NamedKey::Space) => Some(" ".to_string()),
            Key::Named(NamedKey::Enter) => Some("\r".to_string()),
            Key::Named(NamedKey::Tab) => Some("\t".to_string()),
            _ => None,
        };
        kitty_encode_input(
            KittyInput {
                key,
                key_without_modifiers,
                text: produced_text.as_deref(),
                location: KeyLocation::Standard,
                physical,
                modifiers: mods,
                event_type,
            },
            flags,
            C1Mode::SevenBit,
        )
    }

    #[test]
    fn ctrl_letter_without_text_flag() {
        let key = char_key("a");
        let bytes = encode(
            &key,
            &key,
            PhysicalKey::Code(KeyCode::KeyA),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            KittyKeyEventType::Press,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;5u");
    }

    #[test]
    fn ctrl_letter_with_text_flag_omits_control_text() {
        let key = char_key("a");
        let bytes = encode(
            &key,
            &key,
            PhysicalKey::Code(KeyCode::KeyA),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KittyFlags::REPORT_ASSOCIATED_TEXT,
            KittyKeyEventType::Press,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;5u");
    }

    #[test]
    fn shift_a_with_all_as_escape_and_text() {
        // Plain "A" (shift+a) normally emits no CSI u. With REPORT_ALL_KEYS
        // the key code is the unmodified base ("a" = 97), modifier param is
        // 2 (shift = bit 0 + 1), text param carries the actual produced
        // character so apps can distinguish a true "A" from a synth one.
        let key = char_key("A");
        let unmodified = char_key("a");
        let bytes = encode(
            &key,
            &unmodified,
            PhysicalKey::Code(KeyCode::KeyA),
            ModifiersState::SHIFT,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KittyFlags::REPORT_ASSOCIATED_TEXT,
            KittyKeyEventType::Press,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;2;65u");
    }

    #[test]
    fn plain_text_without_all_as_escape_is_not_encoded() {
        // Just REPORT_ASSOCIATED_TEXT shouldn't force plain text into CSI u;
        // the raw byte path still handles it.
        assert!(
            encode(
                &char_key("a"),
                &char_key("a"),
                PhysicalKey::Code(KeyCode::KeyA),
                ModifiersState::empty(),
                KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
                KittyKeyEventType::Press,
            )
            .is_none()
        );
    }

    #[test]
    fn enter_associated_text_omits_control_code() {
        let bytes = encode(
            &Key::Named(NamedKey::Enter),
            &Key::Named(NamedKey::Enter),
            PhysicalKey::Code(KeyCode::Enter),
            ModifiersState::empty(),
            KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            KittyKeyEventType::Press,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[13;1u");
    }

    #[test]
    fn escape_with_text_flag_has_no_text_param() {
        // Escape is a control action, not a text-producing key.
        let bytes = encode(
            &Key::Named(NamedKey::Escape),
            &Key::Named(NamedKey::Escape),
            PhysicalKey::Code(KeyCode::Escape),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            KittyKeyEventType::Press,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[27;5u");
    }

    #[test]
    fn ime_commit_uses_zero_key_and_no_modifiers() {
        // Spec sentinel: key code 0 means "not a physical key". The modifier
        // parameter is offset by one, so no modifiers is 1. Codepoints join
        // with ':'. 啊 = U+554A (0x554A = 21834),
        // 不 = U+4E0D (0x4E0D = 19981).
        let bytes = kitty_encode_ime_commit("啊不", C1Mode::SevenBit);
        assert_eq!(bytes, b"\x1b[0;1;21834:19981u");
    }

    #[test]
    fn ime_commit_single_codepoint() {
        let bytes = kitty_encode_ime_commit("é", C1Mode::SevenBit);
        // é = U+00E9 = 233
        assert_eq!(bytes, b"\x1b[0;1;233u");
    }

    #[test]
    fn kitty_encode_uses_8bit_csi_when_requested() {
        let key = char_key("a");
        let bytes = kitty_encode_input(
            KittyInput {
                key: &key,
                key_without_modifiers: &key,
                text: Some("a"),
                location: KeyLocation::Standard,
                physical: PhysicalKey::Code(KeyCode::KeyA),
                modifiers: ModifiersState::CONTROL,
                event_type: KittyKeyEventType::Press,
            },
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            C1Mode::EightBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x9b97;5u");
    }

    #[test]
    fn repeat_and_release_events_are_encoded() {
        let key = Key::Named(NamedKey::ArrowUp);
        let flags = KittyFlags::REPORT_EVENT_TYPES;
        let repeat = encode(
            &key,
            &key,
            PhysicalKey::Code(KeyCode::ArrowUp),
            ModifiersState::empty(),
            flags,
            KittyKeyEventType::Repeat,
        )
        .expect("repeat encoded");
        let release = encode(
            &key,
            &key,
            PhysicalKey::Code(KeyCode::ArrowUp),
            ModifiersState::empty(),
            flags,
            KittyKeyEventType::Release,
        )
        .expect("release encoded");
        assert_eq!(repeat, b"\x1b[1;1:2A");
        assert_eq!(release, b"\x1b[1;1:3A");
    }

    #[test]
    fn report_events_encodes_non_text_control_character_keys() {
        let key = char_key("a");
        let bytes = encode(
            &key,
            &key,
            PhysicalKey::Code(KeyCode::KeyA),
            ModifiersState::CONTROL,
            KittyFlags::REPORT_EVENT_TYPES,
            KittyKeyEventType::Repeat,
        )
        .expect("repeat encoded");
        assert_eq!(bytes, b"\x1b[97;5:2u");
    }

    #[test]
    fn enhanced_unmodified_function_keys_use_csi() {
        let arrow = Key::Named(NamedKey::ArrowUp);
        let bytes = encode(
            &arrow,
            &arrow,
            PhysicalKey::Code(KeyCode::ArrowUp),
            ModifiersState::empty(),
            KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
            KittyKeyEventType::Press,
        )
        .expect("arrow encoded");
        assert_eq!(bytes, b"\x1b[A");

        let f1 = Key::Named(NamedKey::F1);
        let bytes = encode(
            &f1,
            &f1,
            PhysicalKey::Code(KeyCode::F1),
            ModifiersState::empty(),
            KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
            KittyKeyEventType::Press,
        )
        .expect("F1 encoded");
        assert_eq!(bytes, b"\x1b[P");
    }

    #[test]
    fn disambiguated_numpad_navigation_uses_navigation_code() {
        let key = Key::Named(NamedKey::ArrowLeft);
        let bytes = kitty_encode_input(
            KittyInput {
                key: &key,
                key_without_modifiers: &key,
                text: None,
                location: KeyLocation::Numpad,
                physical: PhysicalKey::Code(KeyCode::Numpad4),
                modifiers: ModifiersState::empty(),
                event_type: KittyKeyEventType::Press,
            },
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            C1Mode::SevenBit,
        )
        .expect("numpad key encoded");
        assert_eq!(bytes, b"\x1b[57417;1u");
    }

    #[test]
    fn shifted_and_base_layout_alternates_are_encoded() {
        let shifted = char_key("A");
        let unmodified = char_key("a");
        let bytes = encode(
            &shifted,
            &unmodified,
            PhysicalKey::Code(KeyCode::KeyA),
            ModifiersState::SHIFT,
            KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES | KittyFlags::REPORT_ALTERNATE_KEYS,
            KittyKeyEventType::Press,
        )
        .expect("shifted alternate encoded");
        assert_eq!(bytes, b"\x1b[97:65;2u");

        let shifted = char_key("?");
        let unmodified = char_key("ß");
        let bytes = encode(
            &shifted,
            &unmodified,
            PhysicalKey::Code(KeyCode::Minus),
            ModifiersState::SHIFT,
            KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES | KittyFlags::REPORT_ALTERNATE_KEYS,
            KittyKeyEventType::Press,
        )
        .expect("layout shift-level alternate encoded");
        assert_eq!(bytes, b"\x1b[223:63:45;2u");

        let cyrillic = char_key("с");
        let bytes = encode(
            &cyrillic,
            &cyrillic,
            PhysicalKey::Code(KeyCode::KeyC),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ALTERNATE_KEYS,
            KittyKeyEventType::Press,
        )
        .expect("base-layout alternate encoded");
        assert_eq!(bytes, b"\x1b[1089::99;5u");
    }

    #[test]
    fn text_key_release_requires_all_keys_mode() {
        let key = char_key("a");
        assert!(
            encode(
                &key,
                &key,
                PhysicalKey::Code(KeyCode::KeyA),
                ModifiersState::empty(),
                KittyFlags::REPORT_EVENT_TYPES,
                KittyKeyEventType::Release,
            )
            .is_none()
        );
        let bytes = encode(
            &key,
            &key,
            PhysicalKey::Code(KeyCode::KeyA),
            ModifiersState::empty(),
            KittyFlags::REPORT_EVENT_TYPES | KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
            KittyKeyEventType::Release,
        )
        .expect("release encoded");
        assert_eq!(bytes, b"\x1b[97;1:3u");
    }

    #[test]
    fn reported_control_key_release_survives_modifier_release_order() {
        let key = char_key("a");
        let bytes = kitty_encode_input(
            KittyInput {
                key: &key,
                key_without_modifiers: &key,
                text: None,
                location: KeyLocation::Standard,
                physical: PhysicalKey::Code(KeyCode::KeyA),
                modifiers: ModifiersState::empty(),
                event_type: KittyKeyEventType::Release,
            },
            KittyFlags::REPORT_EVENT_TYPES,
            C1Mode::SevenBit,
        )
        .expect("owned release encoded");
        assert_eq!(bytes, b"\x1b[97;1:3u");
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
    fn legacy_modified_direct_keys_follow_kitty_compatibility_table() {
        assert_eq!(
            legacy_encode_named(
                NamedKey::Enter,
                KeyLocation::Standard,
                ModifiersState::ALT | ModifiersState::SHIFT,
                false,
                false,
                C1Mode::SevenBit,
            ),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            legacy_encode_named(
                NamedKey::Backspace,
                KeyLocation::Standard,
                ModifiersState::CONTROL,
                false,
                false,
                C1Mode::SevenBit,
            ),
            Some(b"\x08".to_vec())
        );
        assert_eq!(
            legacy_encode_named(
                NamedKey::Tab,
                KeyLocation::Standard,
                ModifiersState::ALT | ModifiersState::SHIFT,
                false,
                false,
                C1Mode::SevenBit,
            ),
            Some(b"\x1b\x1b[Z".to_vec())
        );
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
