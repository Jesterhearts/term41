use config41::CursorShape;
use config41::FeaturePermissions;
use config41::PermissionPolicy;
use vte_mode41::C1Mode;
use vte_mode41::ConformanceLevel;

use crate::Screen;
use crate::Terminal;
use crate::TerminalModes;
use crate::Viewport;
use crate::charset;
use crate::conformance;
use crate::dec::color::report_alternate_text_color;
use crate::dec::color::report_color_assignment;
use crate::drcs;
use crate::drcs::DrcsStore;
use crate::screen;
use crate::screen::grid::AttrChangeExtent;

pub(crate) fn handle_decrqss(
    selector: &[u8],
    terminal: &mut Terminal,
    output: &mut Vec<u8>,
) {
    let out = output;
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
                AttrChangeExtent::Stream => 1,
                AttrChangeExtent::Rectangle => 2,
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps}*x"));
        }
        b" q" => {
            let ps = match (terminal.cursor_style.shape, terminal.cursor_style.blink) {
                (CursorShape::Block, true) => 1,
                (CursorShape::Block, false) => 2,
                (CursorShape::Underline, true) => 3,
                (CursorShape::Underline, false) => 4,
                (CursorShape::Beam, true) => 5,
                (CursorShape::Beam, false) => 6,
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
        b"+q" if terminal.udk_feature_enabled() => {
            let report = terminal.protocol.udks.report_local_functions();
            conformance::write_dcs(out, c1_mode, format_args!("1$r{report}+q"));
        }
        b"*}" if terminal.udk_feature_enabled() => {
            let report = terminal.protocol.udks.report_local_function_keys();
            conformance::write_dcs(out, c1_mode, format_args!("1$r{report}*}}"));
        }
        b"+r" if terminal.udk_feature_enabled() => {
            let report = terminal.protocol.udks.report_modifier_keys();
            conformance::write_dcs(out, c1_mode, format_args!("1$r{report}+r"));
        }
        [item @ b'0'..=b'9', b',', kind @ (b'|' | b'}')] => {
            let item = (item - b'0') as u16;
            let report = if *kind == b'|' {
                report_color_assignment(&terminal.dec_color, item)
            } else {
                report_alternate_text_color(&terminal.dec_color, item)
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
            if let Some(report) = report_alternate_text_color(&terminal.dec_color, item) {
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
    if screen.attrs & font41::attrs::CellAttrs::UNDERLINE_MASK != font41::attrs::CellAttrs::empty()
    {
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
        screen
            .attrs
            .insert(font41::attrs::CellAttrs::SINGLE_UNDERLINE);
    } else {
        screen.attrs &= !font41::attrs::CellAttrs::UNDERLINE_MASK;
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
    feature_permissions: &FeaturePermissions,
    output: &mut Vec<u8>,
) {
    let queries = parse_xtgettcap_payload(payload);
    if queries.is_empty() {
        return;
    }

    let mut report = Vec::new();
    for query in queries {
        let XtGetTCapQuery::Capability { encoded_name, name } = query else {
            write_xtgettcap_failure(output, c1_mode);
            return;
        };
        let Some(value) = xtgettcap_value(&name, feature_permissions) else {
            write_xtgettcap_failure(output, c1_mode);
            return;
        };

        if !report.is_empty() {
            report.push(b';');
        }
        report.extend_from_slice(encoded_name);
        report.push(b'=');
        report.extend_from_slice(hex_encode(value).as_bytes());
    }

    conformance::push_dcs_prefix(output, c1_mode);
    output.extend_from_slice(b"1+r");
    output.extend_from_slice(&report);
    conformance::push_st(output, c1_mode);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum XtGetTCapQuery<'a> {
    Capability {
        encoded_name: &'a [u8],
        name: String,
    },
    Invalid,
}

fn parse_xtgettcap_payload(payload: &[u8]) -> Vec<XtGetTCapQuery<'_>> {
    payload
        .split(|&b| b == b';')
        .filter(|part| !part.is_empty())
        .map(parse_xtgettcap_query)
        .collect()
}

fn parse_xtgettcap_query(encoded_name: &[u8]) -> XtGetTCapQuery<'_> {
    let Some(decoded) = hex_decode(encoded_name) else {
        return XtGetTCapQuery::Invalid;
    };
    let Ok(name) = String::from_utf8(decoded) else {
        return XtGetTCapQuery::Invalid;
    };
    XtGetTCapQuery::Capability { encoded_name, name }
}

fn xtgettcap_value(
    name: &str,
    feature_permissions: &FeaturePermissions,
) -> Option<&'static [u8]> {
    if name == "Ms" {
        return (feature_permissions.clipboard.write != PermissionPolicy::Deny)
            .then_some(b"\x1b]52;%p1%s;%p2%s\x07" as &[u8]);
    }

    match name {
        "Co" | "colors" => Some(b"256"),
        "TN" | "name" => Some(b"xterm-256color"),
        "RGB" => Some(b""),
        "Ss" => Some(b"\x1b[%p1%d q"),
        "Se" => Some(b"\x1b[ q"),
        "Smulx" => Some(b"\x1b[4:%p1%dm"),
        "Setulc" => Some(b"\x1b[58:2::%p1%{65536}%*%p2%{256}%*%+%p3%+m"),
        "setrgbf" => Some(b"\x1b[38:2:%p1%d:%p2%d:%p3%dm"),
        "setrgbb" => Some(b"\x1b[48:2:%p1%d:%p2%d:%p3%dm"),
        "kcuu1" | "ku" => Some(b"\x1b[A"),
        "kcud1" | "kd" => Some(b"\x1b[B"),
        "kcuf1" | "kr" => Some(b"\x1b[C"),
        "kcub1" | "kl" => Some(b"\x1b[D"),
        "khome" | "kh" => Some(b"\x1b[H"),
        "kend" | "@7" => Some(b"\x1b[F"),
        "kich1" | "kI" => Some(b"\x1b[2~"),
        "kdch1" | "kD" => Some(b"\x1b[3~"),
        "kpp" | "kP" => Some(b"\x1b[5~"),
        "knp" | "kN" => Some(b"\x1b[6~"),
        "kcbt" | "kB" => Some(b"\x1b[Z"),
        "kf1" | "k1" => Some(b"\x1bOP"),
        "kf2" | "k2" => Some(b"\x1bOQ"),
        "kf3" | "k3" => Some(b"\x1bOR"),
        "kf4" | "k4" => Some(b"\x1bOS"),
        "kf5" | "k5" => Some(b"\x1b[15~"),
        "kf6" | "k6" => Some(b"\x1b[17~"),
        "kf7" | "k7" => Some(b"\x1b[18~"),
        "kf8" | "k8" => Some(b"\x1b[19~"),
        "kf9" | "k9" => Some(b"\x1b[20~"),
        "kf10" | "k;" => Some(b"\x1b[21~"),
        "kf11" => Some(b"\x1b[23~"),
        "kf12" => Some(b"\x1b[24~"),
        "kf13" => Some(b"\x1b[25~"),
        "kf14" => Some(b"\x1b[26~"),
        "kf15" => Some(b"\x1b[28~"),
        "kf16" => Some(b"\x1b[29~"),
        "kf17" => Some(b"\x1b[31~"),
        "kf18" => Some(b"\x1b[32~"),
        "kf19" => Some(b"\x1b[33~"),
        "kf20" => Some(b"\x1b[34~"),
        _ => None,
    }
}

fn write_xtgettcap_failure(
    output: &mut Vec<u8>,
    c1_mode: C1Mode,
) {
    conformance::push_dcs_prefix(output, c1_mode);
    output.extend_from_slice(b"0+r");
    conformance::push_st(output, c1_mode);
}

fn hex_decode(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.chunks_exact(2)
        .map(|pair| Some((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod integration_tests {
    use config41::ClipboardPermissions;
    use config41::FeaturePermissions;
    use config41::PermissionPolicy;
    use config41::TerminalLimits;
    use palette::Srgb;

    use crate::DecColorSpace;
    use crate::dec::color::report_color_table;
    use crate::settings;
    use crate::test_support::TestTerm;

    #[test]
    fn xtversion_replies_with_name_and_version() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>0q");
        let expected = format!("\x1bP>|term41 {}\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    #[test]
    fn xtgettcap_reports_policy_filtered_capabilities() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP+q544E;636F6C6F7273;6B6631\x1b\\");
        assert_eq!(
            term.take_pending_output(),
            b"\x1bP1+r544E=787465726D2D323536636F6C6F72;636F6C6F7273=323536;6B6631=1B4F50\x1b\\"
        );
    }

    #[test]
    fn xtgettcap_rejects_unknown_or_malformed_capability_names() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP+q544E;6E6F7065\x1b\\");
        assert_eq!(term.take_pending_output(), b"\x1bP0+r\x1b\\");

        term.process(b"\x1bP+q5\x1b\\");
        assert_eq!(term.take_pending_output(), b"\x1bP0+r\x1b\\");
    }

    #[test]
    fn xtgettcap_clipboard_capability_follows_write_policy() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP+q4D73\x1b\\");
        assert_eq!(
            term.take_pending_output(),
            b"\x1bP1+r4D73=1B5D35323B25703125733B257032257307\x1b\\"
        );

        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                clipboard: ClipboardPermissions {
                    write: PermissionPolicy::Deny,
                    ..ClipboardPermissions::default()
                },
                ..FeaturePermissions::default()
            },
        );
        term.process(b"\x1bP+q4D73\x1b\\");
        assert_eq!(term.take_pending_output(), b"\x1bP0+r\x1b\\");
    }

    #[test]
    fn xtgettcap_payload_limit_drops_oversized_queries() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        settings::set_terminal_limits(
            &mut term.inner.protocol,
            TerminalLimits {
                xtgettcap_payload_bytes: 3,
                ..TerminalLimits::default()
            },
        );
        term.process(b"\x1bP+q544E\x1b\\");
        assert!(term.take_pending_output().is_empty());
    }

    #[test]
    fn decrqss_reports_page_geometry_settings() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[36*|\x1b[72t\x1b[132$|");
        super::handle_decrqss(b"t", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r72t\x1b\\");
        super::handle_decrqss(b"*|", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r36*|\x1b\\");
        super::handle_decrqss(b"$|", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r132$|\x1b\\");
    }

    #[test]
    fn decrqss_reports_status_and_attr_change_state() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2$~\x1b[1$}\x1b[2*x");
        super::handle_decrqss(b"$~", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r2$~\x1b\\");
        super::handle_decrqss(b"$}", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1$}\x1b\\");
        super::handle_decrqss(b"*x", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r2*x\x1b\\");
    }

    #[test]
    fn decrqss_reports_normal_text_color_assignment() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        super::handle_decrqss(b"1,|", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1;7;0,|\x1b\\");
    }

    #[test]
    fn decrqss_reports_window_frame_color_assignment() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2;4;5,|");
        super::handle_decrqss(b"2,|", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r2;4;5,|\x1b\\");
    }

    #[test]
    fn decrqss_reports_alternate_text_color_assignment() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[13;4;5,}");
        super::handle_decrqss(b"13,}", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r13;4;5,}\x1b\\");
    }

    #[test]
    fn decctr_reports_current_color_table() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2;2$u");
        let expected = format!(
            "\x1bP2$s{}\x1b\\",
            report_color_table(&term.dec_color, DecColorSpace::Rgb)
        );
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    #[test]
    fn decctr_reports_current_color_table_in_hls() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[2;1$u");
        let expected = format!(
            "\x1bP2$s{}\x1b\\",
            report_color_table(&term.dec_color, DecColorSpace::Hls)
        );
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    #[test]
    fn decac_changes_effective_default_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[1;4;7,|x");

        assert_eq!(term.palette.fg, term.dec_color.table[4]);
        assert_eq!(term.palette.bg, term.dec_color.table[7]);
        assert_eq!(term.active.grid.default_fg, term.dec_color.table[4]);
        assert_eq!(term.active.grid.default_bg, term.dec_color.table[7]);
        assert_eq!(term.active.grid.rows[0].fg[0], term.dec_color.table[4]);
        assert_eq!(term.active.grid.rows[0].bg[0], term.dec_color.table[7]);

        super::handle_decrqss(b"1,|", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1;4;7,|\x1b\\");
    }

    #[test]
    fn decctr_restore_remaps_existing_default_colored_cells() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"ab");
        term.process(b"\x1bP2$p0;2;1;2;3/7;2;10;20;30\x1b\\");

        let expected_bg = Srgb::new(3, 5, 8);
        let expected_fg = Srgb::new(26, 51, 77);

        assert_eq!(term.palette.bg, expected_bg);
        assert_eq!(term.palette.fg, expected_fg);
        assert_eq!(term.active.grid.rows[0].fg[0], expected_fg);
        assert_eq!(term.active.grid.rows[0].bg[0], expected_bg);
        assert_eq!(term.active.grid.rows[0].fg[1], expected_fg);
        assert_eq!(term.active.grid.rows[0].bg[1], expected_bg);
    }

    #[test]
    fn decctr_restore_preserves_explicit_sgr_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[31mx");
        let explicit_fg = term.active.grid.rows[0].fg[0];

        term.process(b"\x1bP2$p0;2;1;2;3/7;2;10;20;30/1;2;200;10;10\x1b\\");

        assert_eq!(term.active.grid.rows[0].fg[0], explicit_fg);
    }

    #[test]
    fn decctr_restore_accepts_hls_entries() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1bP2$p4;1;240;50;100\x1b\\");
        assert_ne!(
            term.dec_color.table[4],
            crate::color::palette_color(&term.base_palette, 4)
        );
    }

    #[test]
    fn decstglt_selects_lookup_table_mode() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[1){");
        assert_eq!(
            term.dec_color.lookup_table,
            crate::DecColorLookupTable::AlternateWithAttrs
        );
        term.process(b"\x1b[3){");
        assert_eq!(
            term.dec_color.lookup_table,
            crate::DecColorLookupTable::AnsiSgr
        );
    }

    #[test]
    fn decrsps_restores_tab_stops() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[3g");
        term.process(b"\x1bP2$t4;9\x1b\\");
        assert!(term.active.tab_stops[3]);
        assert!(term.active.tab_stops[8]);
        assert!(!term.active.tab_stops[7]);
    }

    #[test]
    fn decrqpsr_reports_cursor_information() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[?6h\x1b(0\x0e\x1b[1;4m");
        term.process(b"\x1b[2;3H");
        term.process(b"\x1b[1$w");
        assert_eq!(
            term.take_pending_output(),
            b"\x1bP1$u2;3;1;C;@;A;1;2;@;0B%5%5\x1b\\"
        );
    }

    #[test]
    fn decrsps_restores_cursor_information() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1bP1$t2;3;1;C;A;A;1;2;@;0B%5%5\x1b\\");

        assert_eq!(term.active.cursor.row, 1);
        assert_eq!(term.active.cursor.col, 2);
        assert!(term.active.attrs.contains(font41::attrs::CellAttrs::BOLD));
        assert!(
            term.active
                .attrs
                .contains(font41::attrs::CellAttrs::SINGLE_UNDERLINE)
        );
        assert!(
            term.active
                .attrs
                .contains(font41::attrs::CellAttrs::PROTECTED)
        );
        assert!(term.active.origin_mode);
        assert_eq!(
            term.active.charset.gl_slot(),
            crate::charset::GraphicSetSlot::G1
        );
        assert_eq!(
            term.active.charset.gr_slot(),
            crate::charset::GraphicSetSlot::G2
        );
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G0),
            crate::charset::CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G1),
            crate::charset::CharacterSet::Ascii
        );
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G2),
            crate::charset::CharacterSet::DecSupplemental
        );
    }

    #[test]
    fn decrqtsr_reports_ascii_g0_and_g1_designations() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[1$u");
        assert_eq!(term.take_pending_output(), b"\x1bP1$s\x1b)B\x1b(B\x1b\\");
    }

    #[test]
    fn decrsts_restores_ascii_g0_and_g1_designations() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b(0\x1b)>");
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G0),
            crate::charset::CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G1),
            crate::charset::CharacterSet::DecTechnical
        );

        term.process(b"\x1bP1$p\x1b)B\x1b(B\x1b\\");

        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G0),
            crate::charset::CharacterSet::Ascii
        );
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G1),
            crate::charset::CharacterSet::Ascii
        );
    }

    #[test]
    fn decrsts_accepts_ddd1_without_rejecting_the_report() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1bP1$p\x1b)1\x1b)B\x1b(B\x1b\\");

        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G0),
            crate::charset::CharacterSet::Ascii
        );
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G1),
            crate::charset::CharacterSet::Ascii
        );
    }

    #[test]
    fn ris_resets_dec_color_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        let mut custom = term.inner.palette.clone();
        custom.bg = Srgb::new(24, 32, 48);
        custom.fg = Srgb::new(220, 210, 200);
        term.set_palette(custom.clone());
        term.process(b"\x1b[1;4;7,|\x1bP2$p4;2;8;9;10\x1b\\");
        term.process(b"\x1bc");

        super::handle_decrqss(b"1,|", &mut term.inner, &mut term.effects.host_bytes);
        assert_eq!(term.take_pending_output(), b"\x1bP1$r1;7;0,|\x1b\\");
        assert_eq!(term.palette.fg, custom.fg);
        assert_eq!(term.palette.bg, custom.bg);
        assert_eq!(term.active.grid.default_bg, custom.bg);
        assert_eq!(term.visible_row(0).bg[0], custom.bg);
    }
}
