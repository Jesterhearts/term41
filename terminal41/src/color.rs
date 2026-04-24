use font41::attrs::CellAttrs;
use palette::Srgb;
use utils41::blend_colors;
#[cfg(test)]
use vtepp::Params;

use crate::parser::BorrowedParams;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum SrgExt {
    /// SGR 38/48 subtype: indexed color (`;5;N`).
    Indexed = 5,
    /// SGR 38/48 subtype: direct RGB color (`;2;R;G;B`).
    Rgb = 2,
}

impl TryFrom<u16> for SrgExt {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            5 => Ok(Self::Indexed),
            2 => Ok(Self::Rgb),
            _ => Err(()),
        }
    }
}

// -- SGR attribute selectors (CSI Ps m) ---------------------------------------

const SGR_FG_START: u16 = 30;
const SGR_FG_END: u16 = 37;
const SGR_BG_START: u16 = 40;
const SGR_BG_END: u16 = 47;
const SGR_BRIGHT_FG_START: u16 = 90;
const SGR_BRIGHT_FG_END: u16 = 97;
const SGR_BRIGHT_BG_START: u16 = 100;
const SGR_BRIGHT_BG_END: u16 = 107;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum SgrAction {
    Reset = 0,
    Bold = 1,
    Dim = 2,
    Italic = 3,
    Underline = 4,
    Blink = 5,
    RapidBlink = 6,
    Reverse = 7,
    Hidden = 8,
    Strikethrough = 9,
    DoubleUnderline = 21,
    /// SGR 22 resets both bold and faint per ECMA-48.
    ResetIntensity = 22,
    ResetItalic = 23,
    ResetUnderline = 24,
    ResetBlink = 25,
    ResetReverse = 27,
    ResetHidden = 28,
    ResetStrikethrough = 29,
    FgRange(u16) = SGR_FG_START,
    FgExtended = 38,
    FgDefault = 39,
    BgRange(u16) = SGR_BG_START,
    BgExtended = 48,
    BgDefault = 49,
    Overline = 53,
    ResetOverline = 55,
    UnderlineColor = 58,
    ResetUnderlineColor = 59,
    BrightFgRange(u16) = SGR_BRIGHT_FG_START,
    BrightBgRange(u16) = SGR_BRIGHT_BG_START,
}

impl TryFrom<u16> for SgrAction {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Reset),
            1 => Ok(Self::Bold),
            2 => Ok(Self::Dim),
            3 => Ok(Self::Italic),
            4 => Ok(Self::Underline),
            5 => Ok(Self::Blink),
            6 => Ok(Self::RapidBlink),
            7 => Ok(Self::Reverse),
            8 => Ok(Self::Hidden),
            9 => Ok(Self::Strikethrough),
            21 => Ok(Self::DoubleUnderline),
            22 => Ok(Self::ResetIntensity),
            23 => Ok(Self::ResetItalic),
            24 => Ok(Self::ResetUnderline),
            25 => Ok(Self::ResetBlink),
            27 => Ok(Self::ResetReverse),
            28 => Ok(Self::ResetHidden),
            29 => Ok(Self::ResetStrikethrough),
            n @ SGR_FG_START..=SGR_FG_END => Ok(Self::FgRange(n)),
            38 => Ok(Self::FgExtended),
            39 => Ok(Self::FgDefault),
            n @ SGR_BG_START..=SGR_BG_END => Ok(Self::BgRange(n)),
            48 => Ok(Self::BgExtended),
            49 => Ok(Self::BgDefault),
            53 => Ok(Self::Overline),
            55 => Ok(Self::ResetOverline),
            58 => Ok(Self::UnderlineColor),
            59 => Ok(Self::ResetUnderlineColor),
            n @ SGR_BRIGHT_FG_START..=SGR_BRIGHT_FG_END => Ok(Self::BrightFgRange(n)),
            n @ SGR_BRIGHT_BG_START..=SGR_BRIGHT_BG_END => Ok(Self::BrightBgRange(n)),
            _ => Err(()),
        }
    }
}

pub const fn default_fg() -> Srgb<u8> {
    Srgb::new(204, 204, 204)
}

pub const fn default_bg() -> Srgb<u8> {
    Srgb::new(0, 0, 0)
}

/// Runtime color palette. Stores the 16 ANSI colors, default fg/bg,
/// cursor colors, and selection colors. Built from the `[colors]` config
/// section (Rio palette format), falling back to the hardcoded defaults
/// for any value not overridden.
#[derive(Debug, Clone)]
pub struct ColorPalette {
    /// Default foreground (SGR 39 / row clear).
    pub fg: Srgb<u8>,
    /// Default background (SGR 49 / row clear / wallpaper transparency).
    pub bg: Srgb<u8>,
    /// Default foreground for the DEC status line.
    pub status_line_fg: Srgb<u8>,
    /// Default background for the DEC status line.
    pub status_line_bg: Srgb<u8>,
    /// Cursor color. `None` = use cell foreground (current behavior).
    pub cursor: Option<Srgb<u8>>,
    /// Text color used under a block cursor. `None` = invert against the
    /// cell background (current behavior).
    pub cursor_text: Option<Srgb<u8>>,
    /// Selection background. `None` = invert (current behavior).
    pub selection_bg: Option<Srgb<u8>>,
    /// Selection text color. `None` = invert (current behavior).
    pub selection_fg: Option<Srgb<u8>>,
    /// The 16 ANSI colors: indices 0–7 are normal, 8–15 are bright.
    pub ansi: [Srgb<u8>; 16],
}

impl Default for ColorPalette {
    fn default() -> Self {
        let fg = default_fg();
        let bg = default_bg();
        Self {
            fg,
            bg,
            status_line_fg: fg,
            status_line_bg: blend_colors(bg, fg, 0.25),
            cursor: None,
            cursor_text: None,
            selection_bg: None,
            selection_fg: None,
            ansi: DEFAULT_ANSI_COLORS,
        }
    }
}

/// The hardcoded 16-color ANSI palette, matching the values in
/// [`ansi_color`] for indices 0–15.
const DEFAULT_ANSI_COLORS: [Srgb<u8>; 16] = [
    Srgb::new(0, 0, 0),       // 0  black           rgb(0, 0, 0)
    Srgb::new(205, 0, 0),     // 1  red             rgb(205, 0, 0)
    Srgb::new(0, 205, 0),     // 2  green           rgb(0, 205, 0)
    Srgb::new(205, 205, 0),   // 3  yellow          rgb(205, 205, 0)
    Srgb::new(0, 0, 238),     // 4  blue            rgb(0, 0, 238)
    Srgb::new(205, 0, 205),   // 5  magenta         rgb(205, 0, 205)
    Srgb::new(0, 205, 205),   // 6  cyan            rgb(0, 205, 205)
    Srgb::new(229, 229, 229), // 7  white           rgb(229, 229, 229)
    Srgb::new(127, 127, 127), // 8  bright black    rgb(127, 127, 127)
    Srgb::new(255, 0, 0),     // 9  bright red      rgb(255, 0, 0)
    Srgb::new(0, 255, 0),     // 10 bright green    rgb(0, 255, 0)
    Srgb::new(255, 255, 0),   // 11 bright yellow   rgb(255, 255, 0)
    Srgb::new(92, 92, 255),   // 12 bright blue     rgb(92, 92, 255)
    Srgb::new(255, 0, 255),   // 13 bright magenta  rgb(255, 0, 255)
    Srgb::new(0, 255, 255),   // 14 bright cyan     rgb(0, 255, 255)
    Srgb::new(255, 255, 255), // 15 bright white    rgb(255, 255, 255)
];

/// Look up a 256-color palette index using the given [`ColorPalette`] for
/// indices 0–15 and the computed cube/grayscale ramp for 16–255.
pub(super) fn palette_color(
    palette: &ColorPalette,
    index: u8,
) -> Srgb<u8> {
    if index < 16 {
        palette.ansi[index as usize]
    } else {
        computed_color(index)
    }
}

/// Compute the RGB value for 256-color palette indices 16–255 (the 6×6×6
/// cube and 24-step grayscale ramp). Indices 0–15 are returned as black;
/// callers that need theme-aware 0–15 should use [`palette_color`] instead.
const fn computed_color(index: u8) -> Srgb<u8> {
    match index {
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
        _ => Srgb::new(0, 0, 0),
    }
}

/// Apply SGR (Select Graphic Rendition) parameters to the current fg/bg
/// colors, underline state, and text attributes. `CSI m` with no params
/// (or param 0) is a full reset — colors go back to defaults and every
/// attribute flag clears.
///
/// Sub-parameters (colon-separated, e.g. `4:3` for curly underline) are
/// preserved through the group iterator; the legacy semicolon form
/// (`38;5;N`) is still supported by consuming subsequent groups.
#[cfg(test)]
pub(super) fn apply_sgr(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    attrs: &mut CellAttrs,
    underline_color: &mut Option<Srgb<u8>>,
    params: &Params,
    palette: &ColorPalette,
) {
    apply_sgr_group_refs(
        fg,
        bg,
        attrs,
        underline_color,
        BorrowedParams::from_vte(params),
        palette,
    );
}

pub(super) fn apply_sgr_groups(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    attrs: &mut CellAttrs,
    underline_color: &mut Option<Srgb<u8>>,
    groups: BorrowedParams,
    palette: &ColorPalette,
) {
    apply_sgr_group_refs(fg, bg, attrs, underline_color, groups, palette);
}

fn apply_sgr_group_refs(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    attrs: &mut CellAttrs,
    underline_color: &mut Option<Srgb<u8>>,
    groups: BorrowedParams,
    palette: &ColorPalette,
) {
    if groups.is_empty() {
        reset_all(fg, bg, attrs, underline_color, palette);
        return;
    }

    let mut i = 0;
    while i < groups.len() {
        let g = &groups[i];
        if let Ok(action) = SgrAction::try_from(g[0]) {
            match action {
                SgrAction::Reset => reset_all(fg, bg, attrs, underline_color, palette),
                SgrAction::Bold => attrs.insert(CellAttrs::BOLD),
                SgrAction::Dim => attrs.insert(CellAttrs::DIM),
                SgrAction::Italic => attrs.insert(CellAttrs::ITALIC),
                SgrAction::Underline => {
                    // Sub-parameter determines style: bare `4` or `4:1` = single,
                    // `4:0` = none, `4:2` = double, `4:3` = curly, etc.
                    let sub = g.get(1).copied().unwrap_or(1);
                    *attrs &= !CellAttrs::UNDERLINE_MASK;
                    attrs.insert(CellAttrs::underline_from_sgr(sub));
                }
                SgrAction::Blink => attrs.insert(CellAttrs::BLINK),
                SgrAction::RapidBlink => attrs.insert(CellAttrs::RAPID_BLINK),
                SgrAction::Reverse => attrs.insert(CellAttrs::REVERSE),
                SgrAction::Hidden => attrs.insert(CellAttrs::HIDDEN),
                SgrAction::Strikethrough => attrs.insert(CellAttrs::STRIKETHROUGH),
                SgrAction::DoubleUnderline => {
                    *attrs &= !CellAttrs::UNDERLINE_MASK;
                    attrs.insert(CellAttrs::DOUBLE_UNDERLINE);
                }
                SgrAction::ResetIntensity => attrs.remove(CellAttrs::BOLD | CellAttrs::DIM),
                SgrAction::ResetItalic => attrs.remove(CellAttrs::ITALIC),
                SgrAction::ResetUnderline => {
                    *attrs &= !CellAttrs::UNDERLINE_MASK;
                }
                SgrAction::ResetBlink => attrs.remove(CellAttrs::BLINK | CellAttrs::RAPID_BLINK),
                SgrAction::ResetReverse => attrs.remove(CellAttrs::REVERSE),
                SgrAction::ResetHidden => attrs.remove(CellAttrs::HIDDEN),
                SgrAction::ResetStrikethrough => attrs.remove(CellAttrs::STRIKETHROUGH),
                SgrAction::Overline => attrs.insert(CellAttrs::OVERLINE),
                SgrAction::ResetOverline => attrs.remove(CellAttrs::OVERLINE),
                SgrAction::FgRange(n) => *fg = palette_color(palette, (n - SGR_FG_START) as u8),
                SgrAction::FgExtended => {
                    if let Some(color) = parse_extended_color(groups, g, &mut i, palette) {
                        *fg = color;
                    }
                }
                SgrAction::FgDefault => *fg = palette.fg,
                SgrAction::BgRange(n) => *bg = palette_color(palette, (n - SGR_BG_START) as u8),
                SgrAction::BgExtended => {
                    if let Some(color) = parse_extended_color(groups, g, &mut i, palette) {
                        *bg = color;
                    }
                }
                SgrAction::BgDefault => *bg = palette.bg,
                SgrAction::UnderlineColor => {
                    if let Some(color) = parse_extended_color(groups, g, &mut i, palette) {
                        *underline_color = Some(color);
                    }
                }
                SgrAction::ResetUnderlineColor => *underline_color = None,
                SgrAction::BrightFgRange(n) => {
                    *fg = palette_color(palette, (n - SGR_BRIGHT_FG_START) as u8 + BRIGHT_OFFSET)
                }
                SgrAction::BrightBgRange(n) => {
                    *bg = palette_color(palette, (n - SGR_BRIGHT_BG_START) as u8 + BRIGHT_OFFSET)
                }
            }
        }
        i += 1;
    }
}

fn reset_all(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    attrs: &mut CellAttrs,
    underline_color: &mut Option<Srgb<u8>>,
    palette: &ColorPalette,
) {
    *fg = palette.fg;
    *bg = palette.bg;
    let protected = *attrs & CellAttrs::PROTECTED;
    *attrs = protected;
    *underline_color = None;
}

/// Parse an extended color from either the colon sub-parameter form
/// (`38:5:N` or `38:2:R:G:B`) or the legacy semicolon form (`38;5;N`
/// or `38;2;R;G;B`). In the colon form all values sit in one group; in
/// the semicolon form subsequent groups are consumed and `i` is advanced.
fn parse_extended_color(
    groups: BorrowedParams,
    group: &[u16],
    i: &mut usize,
    palette: &ColorPalette,
) -> Option<Srgb<u8>> {
    // Colon form: sub-parameters sit in the same group (e.g. [38, 5, 196]).
    if group.len() > 1 {
        return parse_color_subparams(&group[1..], palette);
    }

    // Semicolon form: sub-type and value(s) in subsequent groups.
    if *i + 1 >= groups.len() {
        return None;
    }

    let Ok(ext) = SrgExt::try_from(groups[*i + 1][0]) else {
        return None;
    };

    match ext {
        SrgExt::Indexed => {
            if *i + 2 < groups.len() {
                *i += 2;
                Some(palette_color(palette, groups[*i][0] as u8))
            } else {
                None
            }
        }
        SrgExt::Rgb => {
            if *i + 4 < groups.len() {
                *i += 4;
                Some(Srgb::new(
                    groups[*i - 2][0] as u8,
                    groups[*i - 1][0] as u8,
                    groups[*i][0] as u8,
                ))
            } else {
                None
            }
        }
    }
}

/// Decode `;5;N` or `;2;[CS;]R;G;B` from colon-separated sub-parameters.
/// `sub` starts after the leading `38`/`48`/`58`.
fn parse_color_subparams(
    sub: &[u16],
    palette: &ColorPalette,
) -> Option<Srgb<u8>> {
    let Ok(ext) = SrgExt::try_from(*sub.first()?) else {
        return None;
    };

    match ext {
        SrgExt::Indexed => {
            let idx = *sub.get(1)?;
            Some(palette_color(palette, idx as u8))
        }
        SrgExt::Rgb => {
            // The full form is `2:CS:R:G:B` (5 values after the lead param).
            // When CS (color space) is omitted the shorter `2:R:G:B` form
            // has 4 values. We accept both.
            if sub.len() >= 5 {
                Some(Srgb::new(sub[2] as u8, sub[3] as u8, sub[4] as u8))
            } else if sub.len() >= 4 {
                Some(Srgb::new(sub[1] as u8, sub[2] as u8, sub[3] as u8))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;

    /// Drive the VTE parser over `input` and return the `Params` from the
    /// first CSI `m` dispatch it produces.
    fn parse_sgr(input: &[u8]) -> Params {
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
        let pal = ColorPalette::default();
        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut attrs = CellAttrs::default();
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(input),
            &pal,
        );
        (fg, bg)
    }

    fn apply_with_attrs(
        input: &[u8],
        attrs: &mut CellAttrs,
    ) -> (Srgb<u8>, Srgb<u8>) {
        let pal = ColorPalette::default();
        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            attrs,
            &mut ul_color,
            &parse_sgr(input),
            &pal,
        );
        (fg, bg)
    }

    /// Like `apply_with_attrs` but also returns underline state.
    fn apply_full(input: &[u8]) -> (CellAttrs, Option<Srgb<u8>>) {
        let pal = ColorPalette::default();
        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut attrs = CellAttrs::default();
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(input),
            &pal,
        );
        (attrs, ul_color)
    }

    #[test]
    fn default_colors_are_light_gray_on_black() {
        assert_eq!(default_fg(), Srgb::new(204, 204, 204));
        assert_eq!(default_bg(), Srgb::new(0, 0, 0));
    }

    #[test]
    fn empty_sgr_resets_to_defaults() {
        let pal = ColorPalette::default();
        let mut fg = Srgb::new(1, 2, 3);
        let mut bg = Srgb::new(4, 5, 6);
        let mut attrs = CellAttrs::BOLD | CellAttrs::SINGLE_UNDERLINE;
        let mut ul_color = Some(Srgb::new(255, 0, 0));
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[m"),
            &pal,
        );
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
        assert_eq!(attrs, CellAttrs::default());
        assert_eq!(ul_color, None);
    }

    #[test]
    fn sgr_0_resets_to_defaults() {
        let pal = ColorPalette::default();
        let mut fg = Srgb::new(1, 2, 3);
        let mut bg = Srgb::new(4, 5, 6);
        let mut attrs = CellAttrs::BOLD | CellAttrs::CURLY_UNDERLINE;
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[0m"),
            &pal,
        );
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
        assert_eq!(attrs, CellAttrs::default());
    }

    #[test]
    fn sgr_30_through_37_sets_foreground() {
        let (fg, _) = apply(b"\x1b[31m");
        assert_eq!(fg, DEFAULT_ANSI_COLORS[1]);
        let (fg, _) = apply(b"\x1b[37m");
        assert_eq!(fg, DEFAULT_ANSI_COLORS[7]);
    }

    #[test]
    fn sgr_39_restores_default_foreground() {
        let (fg, bg) = apply(b"\x1b[39m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_40_through_47_sets_background() {
        let (_, bg) = apply(b"\x1b[42m");
        assert_eq!(bg, DEFAULT_ANSI_COLORS[2]);
        let (_, bg) = apply(b"\x1b[47m");
        assert_eq!(bg, DEFAULT_ANSI_COLORS[7]);
    }

    #[test]
    fn sgr_49_restores_default_background() {
        let (fg, bg) = apply(b"\x1b[49m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_90_through_97_sets_bright_foreground() {
        let (fg, _) = apply(b"\x1b[90m");
        assert_eq!(fg, DEFAULT_ANSI_COLORS[8]);
        let (fg, _) = apply(b"\x1b[97m");
        assert_eq!(fg, DEFAULT_ANSI_COLORS[15]);
    }

    #[test]
    fn sgr_100_through_107_sets_bright_background() {
        let (_, bg) = apply(b"\x1b[100m");
        assert_eq!(bg, DEFAULT_ANSI_COLORS[8]);
        let (_, bg) = apply(b"\x1b[107m");
        assert_eq!(bg, DEFAULT_ANSI_COLORS[15]);
    }

    #[test]
    fn sgr_38_5_sets_indexed_foreground() {
        let (fg, _) = apply(b"\x1b[38;5;196m");
        assert_eq!(fg, computed_color(196));
    }

    #[test]
    fn sgr_48_5_sets_indexed_background() {
        let (_, bg) = apply(b"\x1b[48;5;21m");
        assert_eq!(bg, computed_color(21));
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
        assert_eq!(fg, DEFAULT_ANSI_COLORS[1]);
        assert_eq!(bg, DEFAULT_ANSI_COLORS[2]);
    }

    #[test]
    fn sgr_reset_then_colors_applies_colors_after_reset() {
        let (fg, bg) = apply(b"\x1b[0;36;44m");
        assert_eq!(fg, DEFAULT_ANSI_COLORS[6]);
        assert_eq!(bg, DEFAULT_ANSI_COLORS[4]);
    }

    #[test]
    fn sgr_1_sets_bold() {
        let mut attrs = CellAttrs::default();
        apply_with_attrs(b"\x1b[1m", &mut attrs);
        assert!(attrs.contains(CellAttrs::BOLD));
        assert!(!attrs.contains(CellAttrs::ITALIC));
    }

    #[test]
    fn sgr_3_sets_italic_and_4_sets_underline() {
        let (attrs, _) = apply_full(b"\x1b[3;4m");
        assert!(attrs.contains(CellAttrs::ITALIC | CellAttrs::SINGLE_UNDERLINE));
    }

    #[test]
    fn sgr_22_23_24_clear_individual_attrs() {
        let pal = ColorPalette::default();
        let (attrs, _) = apply_full(b"\x1b[1;3;4m");
        assert!(attrs.contains(CellAttrs::BOLD));
        assert!(attrs.contains(CellAttrs::ITALIC));
        assert!(attrs.contains(CellAttrs::SINGLE_UNDERLINE));

        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut attrs = attrs;
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[22m"),
            &pal,
        );
        assert!(!attrs.contains(CellAttrs::BOLD));
        assert!(attrs.contains(CellAttrs::ITALIC));
        assert!(attrs.contains(CellAttrs::SINGLE_UNDERLINE));

        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[23;24m"),
            &pal,
        );
        assert_eq!(attrs, CellAttrs::default());
    }

    #[test]
    fn sgr_0_clears_attrs() {
        let (attrs, _) = apply_full(b"\x1b[1;3;4m");
        assert_eq!(
            attrs,
            CellAttrs::BOLD | CellAttrs::ITALIC | CellAttrs::SINGLE_UNDERLINE
        );
        let (attrs, _) = apply_full(b"\x1b[0m");
        assert_eq!(attrs, CellAttrs::default());
    }

    #[test]
    fn sgr_38_without_subtype_is_ignored() {
        let (fg, bg) = apply(b"\x1b[38m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_truncated_truecolor_is_ignored() {
        let (fg, bg) = apply(b"\x1b[38;2;10;20m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_truncated_indexed_is_ignored() {
        let (fg, bg) = apply(b"\x1b[48;5m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_color_then_reset_returns_to_defaults() {
        let (fg, bg) = apply(b"\x1b[31;42;0m");
        assert_eq!(fg, default_fg());
        assert_eq!(bg, default_bg());
    }

    #[test]
    fn sgr_2_sets_dim() {
        let mut attrs = CellAttrs::default();
        apply_with_attrs(b"\x1b[2m", &mut attrs);
        assert!(attrs.contains(CellAttrs::DIM));
        assert!(!attrs.contains(CellAttrs::BOLD));
    }

    #[test]
    fn sgr_7_sets_reverse() {
        let mut attrs = CellAttrs::default();
        apply_with_attrs(b"\x1b[7m", &mut attrs);
        assert!(attrs.contains(CellAttrs::REVERSE));
    }

    #[test]
    fn sgr_27_clears_reverse() {
        let mut attrs = CellAttrs::default();
        apply_with_attrs(b"\x1b[7m", &mut attrs);
        assert!(attrs.contains(CellAttrs::REVERSE));
        apply_with_attrs(b"\x1b[27m", &mut attrs);
        assert!(!attrs.contains(CellAttrs::REVERSE));
    }

    #[test]
    fn sgr_22_clears_both_bold_and_dim() {
        let mut attrs = CellAttrs::default();
        apply_with_attrs(b"\x1b[1;2m", &mut attrs);
        assert!(attrs.contains(CellAttrs::BOLD));
        assert!(attrs.contains(CellAttrs::DIM));
        apply_with_attrs(b"\x1b[22m", &mut attrs);
        assert!(!attrs.contains(CellAttrs::BOLD));
        assert!(!attrs.contains(CellAttrs::DIM));
    }

    #[test]
    fn sgr_0_clears_reverse_and_dim() {
        let mut attrs = CellAttrs::default();
        apply_with_attrs(b"\x1b[2;7m", &mut attrs);
        apply_with_attrs(b"\x1b[0m", &mut attrs);
        assert_eq!(attrs, CellAttrs::default());
    }

    #[test]
    fn sgr_0_preserves_protected_attribute() {
        let mut attrs = CellAttrs::PROTECTED | CellAttrs::REVERSE | CellAttrs::DIM;
        apply_with_attrs(b"\x1b[0m", &mut attrs);
        assert_eq!(attrs, CellAttrs::PROTECTED);
    }

    // -- strikethrough -------------------------------------------------------

    #[test]
    fn sgr_9_sets_strikethrough() {
        let (attrs, _) = apply_full(b"\x1b[9m");
        assert!(attrs.contains(CellAttrs::STRIKETHROUGH));
    }

    #[test]
    fn sgr_29_clears_strikethrough() {
        let pal = ColorPalette::default();
        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut attrs = CellAttrs::default();
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[9m"),
            &pal,
        );
        assert!(attrs.contains(CellAttrs::STRIKETHROUGH));
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[29m"),
            &pal,
        );
        assert!(!attrs.contains(CellAttrs::STRIKETHROUGH));
    }

    // -- curly / styled underline --------------------------------------------

    #[test]
    fn sgr_4_bare_sets_single_underline() {
        let (attrs, _) = apply_full(b"\x1b[4m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::SINGLE_UNDERLINE
        );
    }

    #[test]
    fn sgr_4_colon_0_clears_underline() {
        let (attrs, _) = apply_full(b"\x1b[4:0m");
        assert_eq!(attrs & CellAttrs::UNDERLINE_MASK, CellAttrs::empty());
    }

    #[test]
    fn sgr_4_colon_1_sets_single() {
        let (attrs, _) = apply_full(b"\x1b[4:1m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::SINGLE_UNDERLINE
        );
    }

    #[test]
    fn sgr_4_colon_2_sets_double() {
        let (attrs, _) = apply_full(b"\x1b[4:2m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::DOUBLE_UNDERLINE
        );
    }

    #[test]
    fn sgr_4_colon_3_sets_curly() {
        let (attrs, _) = apply_full(b"\x1b[4:3m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::CURLY_UNDERLINE
        );
    }

    #[test]
    fn sgr_4_colon_4_sets_dotted() {
        let (attrs, _) = apply_full(b"\x1b[4:4m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::DOTTED_UNDERLINE
        );
    }

    #[test]
    fn sgr_4_colon_5_sets_dashed() {
        let (attrs, _) = apply_full(b"\x1b[4:5m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::DASHED_UNDERLINE
        );
    }

    #[test]
    fn sgr_21_sets_double_underline() {
        let (attrs, _) = apply_full(b"\x1b[21m");
        assert_eq!(
            attrs & CellAttrs::UNDERLINE_MASK,
            CellAttrs::DOUBLE_UNDERLINE
        );
    }

    #[test]
    fn sgr_24_clears_underline() {
        let pal = ColorPalette::default();
        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut attrs = CellAttrs::CURLY_UNDERLINE;
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[24m"),
            &pal,
        );
        assert_eq!(attrs & CellAttrs::UNDERLINE_MASK, CellAttrs::empty());
    }

    // -- underline color (SGR 58 / 59) ---------------------------------------

    #[test]
    fn sgr_58_5_sets_indexed_underline_color() {
        let (_, ul_color) = apply_full(b"\x1b[58;5;196m");
        assert_eq!(ul_color, Some(computed_color(196)));
    }

    #[test]
    fn sgr_58_2_sets_truecolor_underline_color() {
        let (_, ul_color) = apply_full(b"\x1b[58;2;10;20;30m");
        assert_eq!(ul_color, Some(Srgb::new(10, 20, 30)));
    }

    #[test]
    fn sgr_58_colon_5_sets_indexed_underline_color() {
        let (_, ul_color) = apply_full(b"\x1b[58:5:196m");
        assert_eq!(ul_color, Some(computed_color(196)));
    }

    #[test]
    fn sgr_58_colon_2_sets_truecolor_underline_color() {
        let (_, ul_color) = apply_full(b"\x1b[58:2:10:20:30m");
        assert_eq!(ul_color, Some(Srgb::new(10, 20, 30)));
    }

    #[test]
    fn sgr_59_resets_underline_color() {
        let pal = ColorPalette::default();
        let mut fg = default_fg();
        let mut bg = default_bg();
        let mut attrs = CellAttrs::default();
        let mut ul_color = None;
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[58;5;196m"),
            &pal,
        );
        assert!(ul_color.is_some());
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[59m"),
            &pal,
        );
        assert_eq!(ul_color, None);
    }

    // -- colon-form extended colors ------------------------------------------

    #[test]
    fn sgr_38_colon_5_sets_indexed_foreground() {
        let (fg, _) = apply(b"\x1b[38:5:196m");
        assert_eq!(fg, computed_color(196));
    }

    #[test]
    fn sgr_38_colon_2_sets_truecolor_foreground() {
        let (fg, _) = apply(b"\x1b[38:2:10:20:30m");
        assert_eq!(fg, Srgb::new(10, 20, 30));
    }

    #[test]
    fn sgr_38_colon_2_with_colorspace_sets_truecolor_foreground() {
        // 38:2:0:10:20:30 — the 0 is the color-space id, skipped.
        let (fg, _) = apply(b"\x1b[38:2:0:10:20:30m");
        assert_eq!(fg, Srgb::new(10, 20, 30));
    }

    // -- overline (SGR 53/55) ------------------------------------------------

    #[test]
    fn sgr_53_sets_overline() {
        let (attrs, _) = apply_full(b"\x1b[53m");
        assert!(attrs.contains(CellAttrs::OVERLINE));
    }

    #[test]
    fn sgr_55_clears_overline() {
        let pal = ColorPalette::default();
        let mut attrs = CellAttrs::default();
        let mut ul_color = None;
        let mut fg = default_fg();
        let mut bg = default_bg();
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[53m"),
            &pal,
        );
        assert!(attrs.contains(CellAttrs::OVERLINE));
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[55m"),
            &pal,
        );
        assert!(!attrs.contains(CellAttrs::OVERLINE));
    }

    // -- hidden text (SGR 8/28) ----------------------------------------------

    #[test]
    fn sgr_8_sets_hidden() {
        let (attrs, _) = apply_full(b"\x1b[8m");
        assert!(attrs.contains(CellAttrs::HIDDEN));
    }

    #[test]
    fn sgr_28_clears_hidden() {
        let pal = ColorPalette::default();
        let mut attrs = CellAttrs::default();
        let mut ul_color = None;
        let mut fg = default_fg();
        let mut bg = default_bg();
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[8m"),
            &pal,
        );
        assert!(attrs.contains(CellAttrs::HIDDEN));
        apply_sgr(
            &mut fg,
            &mut bg,
            &mut attrs,
            &mut ul_color,
            &parse_sgr(b"\x1b[28m"),
            &pal,
        );
        assert!(!attrs.contains(CellAttrs::HIDDEN));
    }
}
