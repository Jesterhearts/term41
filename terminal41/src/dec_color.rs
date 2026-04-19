use palette::Srgb;

use crate::color::ColorPalette;

pub const TEXT_COLOR_ASSIGNMENT_CLASS: u16 = 1;
pub const DEC_COLOR_TABLE_SIZE: usize = 16;
const DEC_COLOR_SPACE_RGB: u16 = 2;
const DEFAULT_TEXT_FG_INDEX: u8 = 7;
const DEFAULT_TEXT_BG_INDEX: u8 = 0;

#[derive(Debug, Clone)]
pub struct DecColorState {
    pub text_fg: u8,
    pub text_bg: u8,
    pub table: [Srgb<u8>; DEC_COLOR_TABLE_SIZE],
}

pub fn state_from_palette(palette: &ColorPalette) -> DecColorState {
    let mut table = palette.ansi;
    table[DEFAULT_TEXT_BG_INDEX as usize] = palette.bg;
    table[DEFAULT_TEXT_FG_INDEX as usize] = palette.fg;
    DecColorState {
        text_fg: DEFAULT_TEXT_FG_INDEX,
        text_bg: DEFAULT_TEXT_BG_INDEX,
        table,
    }
}

pub fn rebase_theme_entries(
    state: &mut DecColorState,
    old_palette: &ColorPalette,
    new_palette: &ColorPalette,
) {
    if state.table[DEFAULT_TEXT_BG_INDEX as usize] == old_palette.bg {
        state.table[DEFAULT_TEXT_BG_INDEX as usize] = new_palette.bg;
    }
    if state.table[DEFAULT_TEXT_FG_INDEX as usize] == old_palette.fg {
        state.table[DEFAULT_TEXT_FG_INDEX as usize] = new_palette.fg;
    }
    for i in 0..DEC_COLOR_TABLE_SIZE {
        if i == DEFAULT_TEXT_BG_INDEX as usize || i == DEFAULT_TEXT_FG_INDEX as usize {
            continue;
        }
        if state.table[i] == old_palette.ansi[i] {
            state.table[i] = new_palette.ansi[i];
        }
    }
}

pub fn effective_palette(
    base_palette: &ColorPalette,
    state: &DecColorState,
) -> ColorPalette {
    let mut palette = base_palette.clone();
    palette.fg = table_color(state, state.text_fg);
    palette.bg = table_color(state, state.text_bg);
    palette
}

pub fn assign_normal_text_colors(
    state: &mut DecColorState,
    fg: u16,
    bg: u16,
) -> bool {
    if fg as usize >= DEC_COLOR_TABLE_SIZE || bg as usize >= DEC_COLOR_TABLE_SIZE {
        return false;
    }
    state.text_fg = fg as u8;
    state.text_bg = bg as u8;
    true
}

pub fn report_text_color_assignment(state: &DecColorState) -> String {
    format!(
        "{TEXT_COLOR_ASSIGNMENT_CLASS};{};{},|",
        state.text_fg, state.text_bg
    )
}

pub fn report_color_table(state: &DecColorState) -> String {
    state
        .table
        .iter()
        .enumerate()
        .map(|(index, color)| {
            format!(
                "{index};{DEC_COLOR_SPACE_RGB};{};{};{}",
                color.red, color.green, color.blue
            )
        })
        .collect::<Vec<_>>()
        .join("/")
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
        let Some(color_space) = parse_u16(parts.next()) else {
            continue;
        };
        let Some(red) = parse_u8(parts.next()) else {
            continue;
        };
        let Some(green) = parse_u8(parts.next()) else {
            continue;
        };
        let Some(blue) = parse_u8(parts.next()) else {
            continue;
        };
        if parts.next().is_some()
            || color_space != DEC_COLOR_SPACE_RGB
            || index as usize >= DEC_COLOR_TABLE_SIZE
        {
            continue;
        }
        state.table[index as usize] = Srgb::new(red, green, blue);
        changed = true;
    }
    changed
}

fn table_color(
    state: &DecColorState,
    index: u8,
) -> Srgb<u8> {
    state.table[index as usize]
}

fn parse_u8(part: Option<&str>) -> Option<u8> {
    part?.parse().ok()
}

fn parse_u16(part: Option<&str>) -> Option<u16> {
    part?.parse().ok()
}
