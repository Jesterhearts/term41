use super::*;

pub(crate) fn handle_decrqss(
    selector: &[u8],
    terminal: &mut Terminal,
) {
    let out = &mut terminal.pending_output;
    let c1_mode = terminal.modes.c1_mode;

    match selector {
        b"m" => {
            let screen = &terminal.active;
            let mut parts: Vec<String> = Vec::new();
            let attrs = screen.attrs;
            if attrs.contains(font41::attrs::CellAttrs::BOLD) {
                parts.push("1".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::DIM) {
                parts.push("2".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::ITALIC) {
                parts.push("3".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::REVERSE) {
                parts.push("7".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::HIDDEN) {
                parts.push("8".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::STRIKETHROUGH) {
                parts.push("9".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::OVERLINE) {
                parts.push("53".into());
            }
            if parts.is_empty() {
                parts.push("0".into());
            }
            let sgr = parts.join(";");
            conformance::write_dcs(out, c1_mode, format_args!("1$r{sgr}m"));
        }
        b"r" => {
            let top = terminal.active.scroll_top + 1;
            let bottom = terminal.active.scroll_bottom + 1;
            conformance::write_dcs(out, c1_mode, format_args!("1$r{top};{bottom}r"));
        }
        b"s" => {
            let left = terminal.active.left_margin + 1;
            let right = terminal.active.right_margin + 1;
            conformance::write_dcs(out, c1_mode, format_args!("1$r{left};{right}s"));
        }
        b"t" => {
            let lines = screen::page_rows(&terminal.active).unwrap_or(terminal.viewport.rows);
            conformance::write_dcs(out, c1_mode, format_args!("1$r{lines}t"));
        }
        b"$|" => {
            conformance::write_dcs(
                out,
                c1_mode,
                format_args!("1$r{}$|", terminal.viewport.cols),
            );
        }
        b"*|" => {
            conformance::write_dcs(
                out,
                c1_mode,
                format_args!("1$r{}*|", terminal.viewport.rows),
            );
        }
        b"\"p" => {
            let level = terminal.modes.conformance_level.da1_code();
            if terminal.modes.conformance_level == ConformanceLevel::Level1 {
                conformance::write_dcs(out, c1_mode, format_args!("1$r{level}\"p"));
            } else {
                conformance::write_dcs(
                    out,
                    c1_mode,
                    format_args!("1$r{level};{}\"p", terminal.modes.c1_mode.decscl_param()),
                );
            }
        }
        b"*x" => {
            let ps = match terminal.active.attr_change_extent {
                grid::AttrChangeExtent::Stream => 1,
                grid::AttrChangeExtent::Rectangle => 2,
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps}*x"));
        }
        b" q" => {
            let ps = match (terminal.cursor_style.shape, terminal.cursor_style.blink) {
                (cursor::CursorShape::Block, true) => 1,
                (cursor::CursorShape::Block, false) => 2,
                (cursor::CursorShape::Underline, true) => 3,
                (cursor::CursorShape::Underline, false) => 4,
                (cursor::CursorShape::Beam, true) => 5,
                (cursor::CursorShape::Beam, false) => 6,
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps} q"));
        }
        b"$}" => {
            let ps = match terminal.active.active_display {
                screen::ActiveDisplay::Main => 0,
                screen::ActiveDisplay::Status => 1,
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps}$}}"));
        }
        b"$~" => {
            let ps = match terminal.active.status_display {
                screen::StatusDisplayKind::None => 0,
                screen::StatusDisplayKind::Indicator => 1,
                screen::StatusDisplayKind::HostWritable => 2,
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps}$~"));
        }
        [item @ b'0'..=b'9', b',', kind @ (b'|' | b'}')] => {
            let item = (item - b'0') as u16;
            let report = if *kind == b'|' {
                dec_color::report_color_assignment(&terminal.dec_color, item)
            } else {
                dec_color::report_alternate_text_color(&terminal.dec_color, item)
            };
            if let Some(report) = report {
                conformance::write_dcs(out, c1_mode, format_args!("1$r{report}"));
            } else {
                conformance::write_dcs(out, c1_mode, format_args!("0$r"));
            }
        }
        [b'1', b'0'..=b'5', b',', b'}'] => {
            let item = selector[0..2]
                .iter()
                .fold(0u16, |acc, b| acc * 10 + (b - b'0') as u16);
            if let Some(report) = dec_color::report_alternate_text_color(&terminal.dec_color, item)
            {
                conformance::write_dcs(out, c1_mode, format_args!("1$r{report}"));
            } else {
                conformance::write_dcs(out, c1_mode, format_args!("0$r"));
            }
        }
        b"\"q" => {
            let ps = if terminal
                .active
                .attrs
                .contains(font41::attrs::CellAttrs::PROTECTED)
            {
                1
            } else {
                0
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps}\"q"));
        }
        _ => {
            conformance::write_dcs(out, c1_mode, format_args!("0$r"));
        }
    }
}

fn current_page_number(screen: &Screen) -> u32 {
    screen
        .page_memory
        .as_ref()
        .map(|page| page.active_page + 1)
        .unwrap_or(1)
}

fn encode_report_byte(bits: u8) -> char {
    char::from(0x40 | (bits & 0x1f))
}

fn decode_report_byte(byte: u8) -> Option<u8> {
    (0x40..=0x5f).contains(&byte).then_some(byte & 0x1f)
}

fn encode_srend(screen: &Screen) -> String {
    let mut bits = 0u8;
    if screen.attrs.contains(font41::attrs::CellAttrs::BOLD) {
        bits |= 1;
    }
    if screen.underline != font41::attrs::UnderlineStyle::None {
        bits |= 2;
    }
    if screen
        .attrs
        .intersects(font41::attrs::CellAttrs::BLINK | font41::attrs::CellAttrs::RAPID_BLINK)
    {
        bits |= 4;
    }
    if screen.attrs.contains(font41::attrs::CellAttrs::REVERSE) {
        bits |= 8;
    }
    encode_report_byte(bits).to_string()
}

fn encode_satt(screen: &Screen) -> String {
    let bits = if screen.attrs.contains(font41::attrs::CellAttrs::PROTECTED) {
        1
    } else {
        0
    };
    encode_report_byte(bits).to_string()
}

fn encode_sflag(
    screen: &Screen,
    _modes: &TerminalModes,
) -> String {
    let mut bits = 0u8;
    if screen.origin_mode {
        bits |= 1;
    }
    match screen.charset.single_shift {
        Some(charset::GraphicSetSlot::G2) => bits |= 2,
        Some(charset::GraphicSetSlot::G3) => bits |= 4,
        _ => {}
    }
    encode_report_byte(bits).to_string()
}

fn charset_size_bit(charset: charset::CharacterSet) -> u8 {
    match charset {
        charset::CharacterSet::IsoLatin1Supplemental
        | charset::CharacterSet::Drcs(_, drcs::CharsetSize::Cs96) => 1,
        _ => 0,
    }
}

fn encode_scss(screen: &Screen) -> String {
    let bits = charset_size_bit(screen.charset.designated(charset::GraphicSetSlot::G0))
        | (charset_size_bit(screen.charset.designated(charset::GraphicSetSlot::G1)) << 1)
        | (charset_size_bit(screen.charset.designated(charset::GraphicSetSlot::G2)) << 2)
        | (charset_size_bit(screen.charset.designated(charset::GraphicSetSlot::G3)) << 3);
    encode_report_byte(bits).to_string()
}

fn charset_designator_bytes(
    charset: charset::CharacterSet,
    drcs: &DrcsStore,
) -> Option<Vec<u8>> {
    match charset {
        charset::CharacterSet::Drcs(buffer_id, _) => drcs
            .designation_for_buffer(buffer_id)
            .map(|bytes| bytes.to_vec()),
        charset => charset::designator_for_charset(charset).map(|bytes| bytes.to_vec()),
    }
}

fn encode_sdesig(
    screen: &Screen,
    drcs: &DrcsStore,
) -> Option<String> {
    let mut data = vec![];
    for slot in [
        charset::GraphicSetSlot::G0,
        charset::GraphicSetSlot::G1,
        charset::GraphicSetSlot::G2,
        charset::GraphicSetSlot::G3,
    ] {
        data.extend(charset_designator_bytes(
            screen.charset.designated(slot),
            drcs,
        )?);
    }
    String::from_utf8(data).ok()
}

pub(crate) fn deccir_report(
    screen: &Screen,
    viewport: &Viewport,
    modes: &TerminalModes,
    drcs: &DrcsStore,
) -> Option<String> {
    let row = screen.cursor.row.min(viewport.rows.saturating_sub(1)) + 1;
    let col = screen.cursor.col.min(viewport.cols.saturating_sub(1)) + 1;
    let pgl = screen.charset.gl_slot() as u8;
    let pgr = screen.charset.gr_slot() as u8;
    let sdesig = encode_sdesig(screen, drcs)?;
    Some(format!(
        "{row};{col};{};{};{};{};{pgl};{pgr};{};{sdesig}",
        current_page_number(screen),
        encode_srend(screen),
        encode_satt(screen),
        encode_sflag(screen, modes),
        encode_scss(screen),
    ))
}

pub(crate) fn dectabsr_report(screen: &Screen) -> String {
    screen
        .tab_stops
        .iter()
        .enumerate()
        .filter_map(|(idx, &set)| set.then_some((idx + 1).to_string()))
        .collect::<Vec<_>>()
        .join(";")
}

fn append_ddd2_payload(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b)B");
}

fn append_ddd3_payload(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b(B");
}

pub(crate) fn dectsr_payload(screen: &Screen) -> Vec<u8> {
    let mut payload = Vec::new();
    if screen.charset.designated(charset::GraphicSetSlot::G1) == charset::CharacterSet::Ascii {
        append_ddd2_payload(&mut payload);
    }
    if screen.charset.designated(charset::GraphicSetSlot::G0) == charset::CharacterSet::Ascii {
        append_ddd3_payload(&mut payload);
    }
    payload
}

fn parse_dectsr_payload(payload: &[u8]) -> Vec<&[u8]> {
    let mut controls = Vec::new();
    let mut i = 0;
    while i + 2 < payload.len() {
        if payload[i] == 0x1b
            && matches!(payload[i + 1], b'(' | b')')
            && matches!(payload[i + 2], b'1' | b'B')
        {
            controls.push(&payload[i..i + 3]);
            i += 3;
        } else {
            i += 1;
        }
    }
    controls
}

pub(crate) fn restore_dectsr(
    payload: &[u8],
    screen: &mut Screen,
) -> bool {
    let controls = parse_dectsr_payload(payload);
    if controls.is_empty() {
        return false;
    }

    let mut restored = false;
    for control in controls {
        match control {
            b"\x1b)1" => restored = true,
            b"\x1b)B" => {
                screen
                    .charset
                    .designate(charset::GraphicSetSlot::G1, charset::CharacterSet::Ascii);
                restored = true;
            }
            b"\x1b(B" => {
                screen
                    .charset
                    .designate(charset::GraphicSetSlot::G0, charset::CharacterSet::Ascii);
                restored = true;
            }
            _ => {}
        }
    }
    restored
}

fn parse_deccir_designators(mut bytes: &[u8]) -> Option<[Vec<u8>; 4]> {
    let mut parsed: [Vec<u8>; 4] = std::array::from_fn(|_| Vec::new());
    for slot in &mut parsed {
        while slot.len() < 2 && matches!(bytes.first(), Some(0x20..=0x2f)) {
            slot.push(bytes[0]);
            bytes = &bytes[1..];
        }
        let final_byte = *bytes.first()?;
        if !(0x30..=0x7e).contains(&final_byte) {
            return None;
        }
        slot.push(final_byte);
        bytes = &bytes[1..];
    }
    bytes.is_empty().then_some(parsed)
}

fn decode_charset_sizes(bytes: &[u8]) -> Option<[drcs::CharsetSize; 4]> {
    let bits = decode_report_byte(*bytes.first()?)?;
    Some(std::array::from_fn(|idx| {
        if bits & (1 << idx) != 0 {
            drcs::CharsetSize::Cs96
        } else {
            drcs::CharsetSize::Cs94
        }
    }))
}

pub(crate) fn restore_deccir(
    payload: &[u8],
    screen: &mut Screen,
    viewport: &Viewport,
    _modes: &mut TerminalModes,
    drcs: &DrcsStore,
) -> bool {
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    let mut fields = text.split(';');
    let Some(row) = fields.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    let Some(col) = fields.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    let page = fields
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    let Some(srend) = fields.next().map(str::as_bytes) else {
        return false;
    };
    let Some(satt) = fields.next().map(str::as_bytes) else {
        return false;
    };
    let Some(sflag) = fields.next().map(str::as_bytes) else {
        return false;
    };
    let Some(pgl) = fields.next().and_then(|s| s.parse::<u8>().ok()) else {
        return false;
    };
    let Some(pgr) = fields.next().and_then(|s| s.parse::<u8>().ok()) else {
        return false;
    };
    let Some(scss) = fields.next().map(str::as_bytes) else {
        return false;
    };
    let Some(sdesig) = fields.next().map(str::as_bytes) else {
        return false;
    };
    if fields.next().is_some() {
        return false;
    }

    if let Some(page_memory) = screen.page_memory.as_mut() {
        page_memory.active_page = page
            .saturating_sub(1)
            .min(page_memory.page_count().saturating_sub(1));
    }
    screen.cursor.row = row.saturating_sub(1).min(viewport.rows.saturating_sub(1));
    screen.cursor.col = col.saturating_sub(1).min(viewport.cols.saturating_sub(1));

    let srend_bits = decode_report_byte(*srend.first().unwrap_or(&0x40)).unwrap_or(0);
    screen.attrs.remove(
        font41::attrs::CellAttrs::BOLD
            | font41::attrs::CellAttrs::BLINK
            | font41::attrs::CellAttrs::RAPID_BLINK
            | font41::attrs::CellAttrs::REVERSE,
    );
    if srend_bits & 1 != 0 {
        screen.attrs.insert(font41::attrs::CellAttrs::BOLD);
    }
    if srend_bits & 2 != 0 {
        screen.underline = font41::attrs::UnderlineStyle::Single;
    } else {
        screen.underline = font41::attrs::UnderlineStyle::None;
    }
    if srend_bits & 4 != 0 {
        screen.attrs.insert(font41::attrs::CellAttrs::BLINK);
    }
    if srend_bits & 8 != 0 {
        screen.attrs.insert(font41::attrs::CellAttrs::REVERSE);
    }

    let satt_bits = decode_report_byte(*satt.first().unwrap_or(&0x40)).unwrap_or(0);
    if satt_bits & 1 != 0 {
        screen.attrs.insert(font41::attrs::CellAttrs::PROTECTED);
    } else {
        screen.attrs.remove(font41::attrs::CellAttrs::PROTECTED);
    }

    let sflag_bits = decode_report_byte(*sflag.first().unwrap_or(&0x40)).unwrap_or(0);
    screen.origin_mode = sflag_bits & 1 != 0;
    screen.charset.single_shift = match sflag_bits & 0b110 {
        0b010 => Some(charset::GraphicSetSlot::G2),
        0b100 => Some(charset::GraphicSetSlot::G3),
        _ => None,
    };

    let gl = match pgl {
        0 => charset::GraphicSetSlot::G0,
        1 => charset::GraphicSetSlot::G1,
        2 => charset::GraphicSetSlot::G2,
        3 => charset::GraphicSetSlot::G3,
        _ => return false,
    };
    let gr = match pgr {
        0 => charset::GraphicSetSlot::G0,
        1 => charset::GraphicSetSlot::G1,
        2 => charset::GraphicSetSlot::G2,
        3 => charset::GraphicSetSlot::G3,
        _ => return false,
    };
    let sizes = match decode_charset_sizes(scss) {
        Some(sizes) => sizes,
        None => return false,
    };
    let designators = match parse_deccir_designators(sdesig) {
        Some(designators) => designators,
        None => return false,
    };
    for (slot, (size, designator)) in [
        charset::GraphicSetSlot::G0,
        charset::GraphicSetSlot::G1,
        charset::GraphicSetSlot::G2,
        charset::GraphicSetSlot::G3,
    ]
    .into_iter()
    .zip(sizes.into_iter().zip(designators))
    {
        let charset = charset::charset_from_designator(&designator, size)
            .or_else(|| drcs.charset_for_designator(&designator));
        let Some(charset) = charset else {
            return false;
        };
        screen.charset.designate(slot, charset);
    }
    screen.charset.set_gl(gl);
    screen.charset.set_gr(gr);
    true
}

pub(crate) fn restore_dectabsr(
    payload: &[u8],
    screen: &mut Screen,
) -> bool {
    screen.tab_stops.fill(false);
    if payload.is_empty() {
        return true;
    }
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    for part in text.split(';') {
        let Some(col) = part.parse::<usize>().ok() else {
            return false;
        };
        let idx = col.saturating_sub(1);
        if idx < screen.tab_stops.len() {
            screen.tab_stops[idx] = true;
        }
    }
    true
}

pub(crate) fn handle_xtgettcap(
    payload: &[u8],
    c1_mode: C1Mode,
    output: &mut Vec<u8>,
) {
    for cap_hex in payload.split(|&b| b == b';') {
        if cap_hex.is_empty() {
            continue;
        }
        let cap_name = hex_decode(cap_hex);
        let cap_str = std::str::from_utf8(&cap_name).unwrap_or("");
        if let Some(value) = xtgettcap_value(cap_str) {
            let value_hex = hex_encode(value.as_bytes());
            conformance::push_dcs_prefix(output, c1_mode);
            output.extend_from_slice(b"1+r");
            output.extend_from_slice(cap_hex);
            output.push(b'=');
            output.extend_from_slice(value_hex.as_bytes());
            conformance::push_st(output, c1_mode);
        } else {
            conformance::push_dcs_prefix(output, c1_mode);
            output.extend_from_slice(b"0+r");
            output.extend_from_slice(cap_hex);
            conformance::push_st(output, c1_mode);
        }
    }
}

fn xtgettcap_value(name: &str) -> Option<&'static str> {
    match name {
        "RGB" => Some(""),
        "colors" => Some("256"),
        "Ss" => Some("\x1b[%p1%d q"),
        "Se" => Some("\x1b[2 q"),
        "Smulx" => Some("\x1b[4:%p1%dm"),
        "Setulc" => Some("\x1b[58:2::%p1%{65536}%*%p2%{256}%*%+%p3%+m"),
        "setrgbf" => Some("\x1b[38:2:%p1%d:%p2%d:%p3%dm"),
        "setrgbb" => Some("\x1b[48:2:%p1%d:%p2%d:%p3%dm"),
        "TN" => Some("xterm-256color"),
        _ => None,
    }
}

fn hex_decode(hex: &[u8]) -> Vec<u8> {
    hex.chunks(2)
        .filter_map(|pair| {
            if pair.len() == 2 {
                u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()
            } else {
                None
            }
        })
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}
