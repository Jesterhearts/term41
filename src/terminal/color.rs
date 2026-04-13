use palette::Srgb;

use crate::vte;

pub const fn default_fg() -> Srgb<u8> {
    Srgb::new(204, 204, 204)
}

pub const fn default_bg() -> Srgb<u8> {
    Srgb::new(0, 0, 0)
}

/// The standard 256-color palette.
pub(super) const fn ansi_color(index: u8) -> Srgb<u8> {
    match index {
        0 => Srgb::new(0, 0, 0),
        1 => Srgb::new(205, 0, 0),
        2 => Srgb::new(0, 205, 0),
        3 => Srgb::new(205, 205, 0),
        4 => Srgb::new(0, 0, 238),
        5 => Srgb::new(205, 0, 205),
        6 => Srgb::new(0, 205, 205),
        7 => Srgb::new(229, 229, 229),
        8 => Srgb::new(127, 127, 127),
        9 => Srgb::new(255, 0, 0),
        10 => Srgb::new(0, 255, 0),
        11 => Srgb::new(255, 255, 0),
        12 => Srgb::new(92, 92, 255),
        13 => Srgb::new(255, 0, 255),
        14 => Srgb::new(0, 255, 255),
        15 => Srgb::new(255, 255, 255),
        16..=231 => {
            const fn to_val(c: u8) -> u8 {
                if c == 0 { 0 } else { 55 + 40 * c }
            }

            let idx = index - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            Srgb::new(to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Srgb::new(v, v, v)
        }
    }
}

/// Apply SGR (Select Graphic Rendition) parameters to the current fg/bg colors.
pub(super) fn apply_sgr(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    params: &vte::Params,
) {
    let params: Vec<u16> = params.iter().map(|p| p[0]).collect();

    if params.is_empty() {
        *fg = default_fg();
        *bg = default_bg();
        return;
    }

    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => {
                *fg = default_fg();
                *bg = default_bg();
            }
            30..=37 => *fg = ansi_color((params[i] - 30) as u8),
            39 => *fg = default_fg(),
            40..=47 => *bg = ansi_color((params[i] - 40) as u8),
            49 => *bg = default_bg(),
            90..=97 => *fg = ansi_color((params[i] - 90 + 8) as u8),
            100..=107 => *bg = ansi_color((params[i] - 100 + 8) as u8),
            38 => {
                if let Some(color) = parse_extended_color(&params, &mut i) {
                    *fg = color;
                }
            }
            48 => {
                if let Some(color) = parse_extended_color(&params, &mut i) {
                    *bg = color;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn parse_extended_color(
    params: &[u16],
    i: &mut usize,
) -> Option<Srgb<u8>> {
    if *i + 1 >= params.len() {
        return None;
    }
    match params[*i + 1] {
        5 => {
            if *i + 2 < params.len() {
                *i += 2;
                Some(ansi_color(params[*i] as u8))
            } else {
                None
            }
        }
        2 => {
            if *i + 4 < params.len() {
                *i += 4;
                Some(Srgb::new(
                    params[*i - 2] as u8,
                    params[*i - 1] as u8,
                    params[*i] as u8,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}
