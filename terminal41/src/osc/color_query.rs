use config41::ColorPalette;

use crate::C1Mode;
use crate::color;
use crate::conformance;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ColorQueryAction<'a> {
    PaletteColor { index: u8, index_text: &'a str },
    Foreground,
    Background,
    Cursor,
}

pub(super) fn parse_palette(rest: &[u8]) -> Option<ColorQueryAction<'_>> {
    let (idx_bytes, query) = super::split_osc(rest);
    if query != b"?" {
        return None;
    }
    let index_text = std::str::from_utf8(idx_bytes).ok()?;
    let index = index_text.parse::<u8>().ok()?;
    Some(ColorQueryAction::PaletteColor { index, index_text })
}

pub(super) fn parse_current<'a>(
    rest: &[u8],
    query_action: ColorQueryAction<'a>,
) -> Option<ColorQueryAction<'a>> {
    (rest == b"?").then_some(query_action)
}

pub(super) fn apply(
    action: ColorQueryAction<'_>,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    palette: &ColorPalette,
) {
    match action {
        ColorQueryAction::PaletteColor { index, index_text } => {
            let c = color::palette_color(palette, index);
            let reply = rgb_reply(c.red, c.green, c.blue);
            conformance::write_osc(
                pending_output,
                c1_mode,
                format_args!("4;{index_text};{reply}"),
            );
        }
        ColorQueryAction::Foreground => {
            write_color_query_reply(pending_output, c1_mode, 10, palette.fg);
        }
        ColorQueryAction::Background => {
            write_color_query_reply(pending_output, c1_mode, 11, palette.bg);
        }
        ColorQueryAction::Cursor => {
            let c = palette.cursor.unwrap_or(palette.fg);
            write_color_query_reply(pending_output, c1_mode, 12, c);
        }
    }
}

fn write_color_query_reply(
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    cmd: u8,
    current: palette::Srgb<u8>,
) {
    let reply = rgb_reply(current.red, current.green, current.blue);
    conformance::write_osc(pending_output, c1_mode, format_args!("{cmd};{reply}"));
}

/// Format an 8-bit color channel as the 16-bit hex representation used in
/// X11 color replies. Each 8-bit value is scaled to 16 bits by repeating the
/// byte (e.g. 0xCC -> 0xCCCC).
fn rgb_reply(
    r: u8,
    g: u8,
    b: u8,
) -> String {
    let r16 = (r as u16) << 8 | r as u16;
    let g16 = (g as u16) << 8 | g as u16;
    let b16 = (b as u16) << 8 | b as u16;
    format!("rgb:{r16:04x}/{g16:04x}/{b16:04x}")
}
