use palette::Srgb;

use crate::vte;

/// First palette index of the 6×6×6 RGB color cube in the 256-color palette.
const CUBE_PALETTE_START: u8 = 16;
/// Last palette index of the 6×6×6 RGB color cube.
const CUBE_PALETTE_END: u8 = 231;
/// Side length of the RGB cube — each channel takes 6 discrete levels.
const CUBE_SIDE: u8 = 6;
/// Non-zero cube channel value for level `c`: `CUBE_CHANNEL_BASE +
/// CUBE_CHANNEL_STEP * c`.
const CUBE_CHANNEL_BASE: u8 = 55;
const CUBE_CHANNEL_STEP: u8 = 40;

/// First palette index of the grayscale ramp.
const GRAY_PALETTE_START: u8 = 232;
/// Last palette index of the grayscale ramp.
const GRAY_PALETTE_END: u8 = 255;
/// Grayscale ramp value for step `n`: `GRAY_BASE + GRAY_STEP * n`.
const GRAY_BASE: u8 = 8;
const GRAY_STEP: u8 = 10;

/// Offset from a standard ANSI color (0..=7) to its bright variant (8..=15).
const BRIGHT_OFFSET: u8 = 8;

/// SGR 38/48 subtype: indexed color (`;5;N`).
const SGR_EXT_INDEXED: u16 = 5;
/// SGR 38/48 subtype: direct RGB color (`;2;R;G;B`).
const SGR_EXT_RGB: u16 = 2;

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
        CUBE_PALETTE_START..=CUBE_PALETTE_END => {
            const fn to_val(c: u8) -> u8 {
                if c == 0 {
                    0
                } else {
                    CUBE_CHANNEL_BASE + CUBE_CHANNEL_STEP * c
                }
            }

            let idx = index - CUBE_PALETTE_START;
            let r = idx / (CUBE_SIDE * CUBE_SIDE);
            let g = (idx % (CUBE_SIDE * CUBE_SIDE)) / CUBE_SIDE;
            let b = idx % CUBE_SIDE;
            Srgb::new(to_val(r), to_val(g), to_val(b))
        }
        GRAY_PALETTE_START..=GRAY_PALETTE_END => {
            let v = GRAY_BASE + GRAY_STEP * (index - GRAY_PALETTE_START);
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
            90..=97 => *fg = ansi_color((params[i] - 90) as u8 + BRIGHT_OFFSET),
            100..=107 => *bg = ansi_color((params[i] - 100) as u8 + BRIGHT_OFFSET),
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
        SGR_EXT_INDEXED => {
            if *i + 2 < params.len() {
                *i += 2;
                Some(ansi_color(params[*i] as u8))
            } else {
                None
            }
        }
        SGR_EXT_RGB => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vte::Action;
    use crate::vte::Parser;

    /// Drive the VTE parser over `input` and return the `Params` from the
    /// first CSI `m` dispatch it produces.
    fn parse_sgr(input: &[u8]) -> vte::Params {
        let mut parser = Parser::new();
        parser
            .parse(input)
            .find_map(|a| match a {
                Action::CsiDispatch {
                    params,
                    action: 'm',
                    ..
                } => Some(params),
                _ => None,
            })
            .expect("input did not produce a CSI 'm' dispatch")
    }

    fn apply(input: &[u8]) -> (Srgb<u8>, Srgb<u8>) {
        let mut fg = default_fg();
        let mut bg = default_bg();
        apply_sgr(&mut fg, &mut bg, &parse_sgr(input));
        (fg, bg)
    }

    #[test]
    fn default_colors_are_light_gray_on_black() {
        assert_eq!(default_fg(), Srgb::new(204, 204, 204));
        assert_eq!(default_bg(), Srgb::new(0, 0, 0));
    }

    #[test]
    fn ansi_color_returns_standard_sixteen() {
        assert_eq!(ansi_color(0), Srgb::new(0, 0, 0));
        assert_eq!(ansi_color(1), Srgb::new(205, 0, 0));
        assert_eq!(ansi_color(7), Srgb::new(229, 229, 229));
        assert_eq!(ansi_color(8), Srgb::new(127, 127, 127));
        assert_eq!(ansi_color(15), Srgb::new(255, 255, 255));
    }

    #[test]
    fn ansi_color_computes_6x6x6_cube() {
        // Index 16 is the cube origin: all channels at level 0.
        assert_eq!(ansi_color(16), Srgb::new(0, 0, 0));
        // Index 231 is the cube's opposite corner: all channels at level 5
        // → 55 + 40 * 5 = 255.
        assert_eq!(ansi_color(231), Srgb::new(255, 255, 255));
        // Cube diagonal at level 3 in each channel: idx = 16 + 3*36 + 3*6 + 3 = 145,
        // channel value = 55 + 40*3 = 175.
        assert_eq!(ansi_color(145), Srgb::new(175, 175, 175));
        // Pure red at level 1: idx = 16 + 1*36 = 52, value = 55 + 40 = 95.
        assert_eq!(ansi_color(52), Srgb::new(95, 0, 0));
    }

    #[test]
    fn ansi_color_computes_grayscale_ramp() {
        assert_eq!(ansi_color(232), Srgb::new(8, 8, 8));
        assert_eq!(ansi_color(233), Srgb::new(18, 18, 18));
        assert_eq!(ansi_color(255), Srgb::new(238, 238, 238));
    }

    #[test]
    fn empty_sgr_resets_to_defaults() {
        let mut fg = Srgb::new(1, 2, 3);
        let mut bg = Srgb::new(4, 5, 6);
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_0_resets_to_defaults() {
        let mut fg = Srgb::new(1, 2, 3);
        let mut bg = Srgb::new(4, 5, 6);
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[0m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_30_through_37_sets_foreground() {
        let (fg, _) = apply(b"\x1b[31m");
        assert_eq!(fg, ansi_color(1));
        let (fg, _) = apply(b"\x1b[37m");
        assert_eq!(fg, ansi_color(7));
    }

    #[test]
    fn sgr_39_restores_default_foreground() {
        let mut fg = Srgb::new(1, 2, 3);
        let mut bg = default_bg();
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[39m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_40_through_47_sets_background() {
        let (_, bg) = apply(b"\x1b[42m");
        assert_eq!(bg, ansi_color(2));
        let (_, bg) = apply(b"\x1b[47m");
        assert_eq!(bg, ansi_color(7));
    }

    #[test]
    fn sgr_49_restores_default_background() {
        let mut fg = default_fg();
        let mut bg = Srgb::new(1, 2, 3);
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[49m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_90_through_97_sets_bright_foreground() {
        let (fg, _) = apply(b"\x1b[90m");
        assert_eq!(fg, ansi_color(8));
        let (fg, _) = apply(b"\x1b[97m");
        assert_eq!(fg, ansi_color(15));
    }

    #[test]
    fn sgr_100_through_107_sets_bright_background() {
        let (_, bg) = apply(b"\x1b[100m");
        assert_eq!(bg, ansi_color(8));
        let (_, bg) = apply(b"\x1b[107m");
        assert_eq!(bg, ansi_color(15));
    }

    #[test]
    fn sgr_38_5_sets_indexed_foreground() {
        let (fg, _) = apply(b"\x1b[38;5;196m");
        assert_eq!(fg, ansi_color(196));
    }

    #[test]
    fn sgr_48_5_sets_indexed_background() {
        let (_, bg) = apply(b"\x1b[48;5;21m");
        assert_eq!(bg, ansi_color(21));
    }

    #[test]
    fn sgr_38_2_sets_truecolor_foreground() {
        let (fg, _) = apply(b"\x1b[38;2;10;20;30m");
        assert_eq!(fg, Srgb::new(10, 20, 30));
    }

    #[test]
    fn sgr_48_2_sets_truecolor_background() {
        let (_, bg) = apply(b"\x1b[48;2;200;100;50m");
        assert_eq!(bg, Srgb::new(200, 100, 50));
    }

    #[test]
    fn sgr_chained_parameters_apply_in_order() {
        let (fg, bg) = apply(b"\x1b[31;42m");
        assert_eq!(fg, ansi_color(1));
        assert_eq!(bg, ansi_color(2));
    }

    #[test]
    fn sgr_reset_then_colors_applies_colors_after_reset() {
        let (fg, bg) = apply(b"\x1b[0;36;44m");
        assert_eq!(fg, ansi_color(6));
        assert_eq!(bg, ansi_color(4));
    }

    #[test]
    fn sgr_non_color_code_leaves_colors_unchanged() {
        let mut fg = default_fg();
        let mut bg = default_bg();
        // SGR 1 (bold) isn't handled here — colors must stay put.
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[1m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_38_without_subtype_is_ignored() {
        let mut fg = default_fg();
        let mut bg = default_bg();
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[38m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_truncated_truecolor_is_ignored() {
        let mut fg = default_fg();
        let mut bg = default_bg();
        // Missing the blue component.
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[38;2;10;20m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_truncated_indexed_is_ignored() {
        let mut fg = default_fg();
        let mut bg = default_bg();
        // `48;5` with no palette index.
        apply_sgr(&mut fg, &mut bg, &parse_sgr(b"\x1b[48;5m"));
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_color_then_reset_returns_to_defaults() {
        let (fg, bg) = apply(b"\x1b[31;42;0m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }
}
