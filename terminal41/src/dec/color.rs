use font41::attrs::CellAttrs;
use palette::FromColor;
use palette::Hsl;
use palette::RgbHue;
use palette::Srgb;

use crate::color;
use crate::color::ColorPalette;

/// DECRQCRA/DECATC assignment class for normal text colors.
pub const TEXT_COLOR_ASSIGNMENT_CLASS: u16 = 1;
/// DECRQCRA/DECATC assignment class for window-frame colors.
pub const WINDOW_FRAME_ASSIGNMENT_CLASS: u16 = 2;
/// Number of entries in the DEC color table.
pub const DEC_COLOR_TABLE_SIZE: usize = 256;
/// Number of DEC alternate text-color combinations.
pub const DEC_ALT_TEXT_COMBINATIONS: usize = 16;
const DEC_COLOR_SPACE_HLS: u16 = 1;
const DEC_COLOR_SPACE_RGB: u16 = 2;
const DEFAULT_TEXT_FG_INDEX: u8 = 7;
const DEFAULT_TEXT_BG_INDEX: u8 = 0;

/// DEC color lookup table selected by DECATC-style controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupTable {
    /// Monochrome table; bold can map low ANSI colors to bright variants.
    Mono = 0,
    /// Alternate text colors selected by cell attributes.
    AlternateWithAttrs = 1,
    /// Alternate text colors, ignoring normal SGR foreground/background.
    Alternate = 2,
    /// Normal ANSI/SGR palette lookup.
    AnsiSgr = 3,
}

/// Color-space encoding used by DEC color-table reports/restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// Hue/lightness/saturation percentages.
    Hls = 1,
    /// Red/green/blue percentages.
    Rgb = 2,
}

/// Pair of palette-table indices used for foreground/background assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorAssignment {
    /// Foreground color-table index.
    pub fg: u8,
    /// Background color-table index.
    pub bg: u8,
}

/// DEC color-table state and color lookup mode.
#[derive(Debug, Clone)]
pub struct DecColorState {
    /// Normal text foreground/background assignment.
    pub text: ColorAssignment,
    /// Window-frame foreground/background assignment.
    pub window_frame: ColorAssignment,
    /// Alternate text assignments indexed by bold/reverse/underline/blink.
    pub alternate_text: [ColorAssignment; DEC_ALT_TEXT_COMBINATIONS],
    /// Active DEC color lookup table.
    pub lookup_table: LookupTable,
    /// Whether blink participates in alternate text-color indexing.
    pub alternate_blink_text: bool,
    /// Whether underline participates in alternate text-color indexing.
    pub alternate_underline_text: bool,
    /// Whether bold/blink brightening also affects backgrounds.
    pub bold_blink_affects_background: bool,
    /// Whether erase operations use the default screen background color.
    pub erase_to_screen: bool,
    /// Full DEC color table.
    pub table: [Srgb<u8>; DEC_COLOR_TABLE_SIZE],
}

/// Build DEC color state from the current theme palette.
pub fn state_from_palette(palette: &ColorPalette) -> DecColorState {
    let mut table = [Srgb::new(0, 0, 0); DEC_COLOR_TABLE_SIZE];
    for (idx, slot) in table.iter_mut().enumerate() {
        *slot = color::palette_color(palette, idx as u8);
    }
    table[DEFAULT_TEXT_BG_INDEX as usize] = palette.bg;
    table[DEFAULT_TEXT_FG_INDEX as usize] = palette.fg;
    DecColorState {
        text: ColorAssignment {
            fg: DEFAULT_TEXT_FG_INDEX,
            bg: DEFAULT_TEXT_BG_INDEX,
        },
        window_frame: ColorAssignment {
            fg: DEFAULT_TEXT_FG_INDEX,
            bg: DEFAULT_TEXT_BG_INDEX,
        },
        alternate_text: default_alternate_text_assignments(),
        lookup_table: LookupTable::AnsiSgr,
        alternate_blink_text: false,
        alternate_underline_text: false,
        bold_blink_affects_background: false,
        erase_to_screen: false,
        table,
    }
}

pub fn rebase_theme_entries(
    state: &mut DecColorState,
    old_palette: &ColorPalette,
    new_palette: &ColorPalette,
) {
    for idx in 0..16 {
        let old_color = match idx as u8 {
            DEFAULT_TEXT_BG_INDEX => old_palette.bg,
            DEFAULT_TEXT_FG_INDEX => old_palette.fg,
            idx => color::palette_color(old_palette, idx),
        };
        let new_color = match idx as u8 {
            DEFAULT_TEXT_BG_INDEX => new_palette.bg,
            DEFAULT_TEXT_FG_INDEX => new_palette.fg,
            idx => color::palette_color(new_palette, idx),
        };
        if state.table[idx] == old_color {
            state.table[idx] = new_color;
        }
    }
}

pub fn effective_palette(
    base_palette: &ColorPalette,
    state: &DecColorState,
) -> ColorPalette {
    let mut palette = base_palette.clone();
    palette.fg = table_color(state, state.text.fg);
    palette.bg = table_color(state, state.text.bg);
    palette
}

pub fn assign_color(
    state: &mut DecColorState,
    item: u16,
    fg: u16,
    bg: u16,
) -> bool {
    if fg >= 16 || bg >= 16 {
        return false;
    }
    let assignment = ColorAssignment {
        fg: fg as u8,
        bg: bg as u8,
    };
    match item {
        TEXT_COLOR_ASSIGNMENT_CLASS => state.text = assignment,
        WINDOW_FRAME_ASSIGNMENT_CLASS => state.window_frame = assignment,
        _ => return false,
    }
    true
}

pub fn report_color_assignment(
    state: &DecColorState,
    item: u16,
) -> Option<String> {
    let assignment = match item {
        TEXT_COLOR_ASSIGNMENT_CLASS => state.text,
        WINDOW_FRAME_ASSIGNMENT_CLASS => state.window_frame,
        _ => return None,
    };
    Some(format!("{item};{};{},|", assignment.fg, assignment.bg))
}

/// Assign one alternate text-color pair.
pub fn assign_alternate_text_color(
    state: &mut DecColorState,
    item: u16,
    fg: u16,
    bg: u16,
) -> bool {
    if item as usize >= DEC_ALT_TEXT_COMBINATIONS || fg >= 16 || bg >= 16 {
        return false;
    }
    state.alternate_text[item as usize] = ColorAssignment {
        fg: fg as u8,
        bg: bg as u8,
    };
    true
}

pub fn report_alternate_text_color(
    state: &DecColorState,
    item: u16,
) -> Option<String> {
    let assignment = state.alternate_text.get(item as usize)?;
    Some(format!("{item};{};{},}}", assignment.fg, assignment.bg))
}

/// Select the active DEC lookup table from a protocol parameter.
pub fn select_lookup_table(
    state: &mut DecColorState,
    ps: u16,
) -> bool {
    let Some(table) = lookup_table_from_param(ps) else {
        return false;
    };
    state.lookup_table = table;
    true
}

pub fn restore_color_table(
    state: &mut DecColorState,
    payload: &[u8],
) -> bool {
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    let mut changed = false;
    for entry in text.split('/') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.split(';');
        let Some(index) = parse_u8(parts.next()) else {
            continue;
        };
        let Some(space) = ColorSpace::from_param(parse_u16(parts.next())) else {
            continue;
        };
        let Some(x) = parse_u16(parts.next()) else {
            continue;
        };
        let Some(y) = parse_u16(parts.next()) else {
            continue;
        };
        let Some(z) = parse_u16(parts.next()) else {
            continue;
        };
        if parts.next().is_some() {
            continue;
        }
        let Some(color) = restore_color(space, x, y, z) else {
            continue;
        };
        state.table[index as usize] = color;
        changed = true;
    }
    changed
}

pub fn report_color_table(
    state: &DecColorState,
    space: ColorSpace,
) -> String {
    state
        .table
        .iter()
        .enumerate()
        .map(|(index, color)| match space {
            ColorSpace::Rgb => {
                format!(
                    "{index};{DEC_COLOR_SPACE_RGB};{};{};{}",
                    color.red, color.green, color.blue
                )
            }
            ColorSpace::Hls => {
                let (h, l, s) = rgb_to_hls(*color);
                format!("{index};{DEC_COLOR_SPACE_HLS};{h};{l};{s}")
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Return a color table entry by index.
pub fn table_color(
    state: &DecColorState,
    index: u8,
) -> Srgb<u8> {
    state.table[index as usize]
}

pub fn erase_background_color(
    state: &DecColorState,
    current_bg: Srgb<u8>,
) -> Srgb<u8> {
    if state.erase_to_screen {
        table_color(state, DEFAULT_TEXT_BG_INDEX)
    } else {
        current_bg
    }
}

/// Resolve alternate text-color assignment for the given cell style.
pub fn alternate_assignment_for_style(
    state: &DecColorState,
    attrs: CellAttrs,
) -> ColorAssignment {
    state.alternate_text[alternate_text_index(attrs) as usize]
}

pub fn alternate_text_index(attrs: CellAttrs) -> u8 {
    let mut index = 0u8;
    if attrs.contains(CellAttrs::BOLD) {
        index |= 1;
    }
    if attrs.contains(CellAttrs::REVERSE) {
        index |= 2;
    }
    if attrs.intersects(CellAttrs::UNDERLINE_MASK) {
        index |= 4;
    }
    if attrs.intersects(CellAttrs::BLINK | CellAttrs::RAPID_BLINK) {
        index |= 8;
    }
    index
}

impl ColorSpace {
    /// Parse a DEC color-space parameter.
    pub fn from_param(value: Option<u16>) -> Option<Self> {
        match value? {
            DEC_COLOR_SPACE_HLS => Some(Self::Hls),
            DEC_COLOR_SPACE_RGB => Some(Self::Rgb),
            _ => None,
        }
    }
}

fn lookup_table_from_param(ps: u16) -> Option<LookupTable> {
    match ps {
        0 => Some(LookupTable::Mono),
        1 => Some(LookupTable::AlternateWithAttrs),
        2 => Some(LookupTable::Alternate),
        3 => Some(LookupTable::AnsiSgr),
        _ => None,
    }
}

fn restore_color(
    space: ColorSpace,
    x: u16,
    y: u16,
    z: u16,
) -> Option<Srgb<u8>> {
    match space {
        ColorSpace::Rgb => {
            if x > 100 || y > 100 || z > 100 {
                return None;
            }
            Some(Srgb::new(
                scale_percent(x),
                scale_percent(y),
                scale_percent(z),
            ))
        }
        ColorSpace::Hls => {
            if x > 360 || y > 100 || z > 100 {
                return None;
            }
            let hsl = Hsl::new(
                RgbHue::from_degrees(x as f32),
                y as f32 / 100.0,
                z as f32 / 100.0,
            );
            let rgb: Srgb<f32> = Srgb::from_color(hsl);
            Some(rgb.into_format())
        }
    }
}

fn rgb_to_hls(color: Srgb<u8>) -> (u16, u16, u16) {
    let hsl = Hsl::from_color(color.into_format::<f32>());
    let hue = hsl.hue.into_degrees().rem_euclid(360.0).round() as u16;
    let lightness = (hsl.lightness * 100.0).round().clamp(0.0, 100.0) as u16;
    let saturation = (hsl.saturation * 100.0).round().clamp(0.0, 100.0) as u16;
    (hue, lightness, saturation)
}

fn scale_percent(value: u16) -> u8 {
    ((value as f32 / 100.0) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn default_alternate_text_assignments() -> [ColorAssignment; DEC_ALT_TEXT_COMBINATIONS] {
    core::array::from_fn(|idx| {
        let bold = (idx & 1) != 0;
        let reverse = (idx & 2) != 0;
        let fg = if bold {
            DEFAULT_TEXT_FG_INDEX + 8
        } else {
            DEFAULT_TEXT_FG_INDEX
        };
        let bg = DEFAULT_TEXT_BG_INDEX;
        if reverse {
            ColorAssignment { fg: bg, bg: fg }
        } else {
            ColorAssignment { fg, bg }
        }
    })
}

fn parse_u8(part: Option<&str>) -> Option<u8> {
    part?.parse().ok()
}

fn parse_u16(part: Option<&str>) -> Option<u16> {
    part?.parse().ok()
}
