use std::str::FromStr;

use serde::Deserialize;
use utils41::blend_colors;

use crate::palette_crate::Srgb;

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
    /// The 16 ANSI colors: indices 0-7 are normal, 8-15 are bright.
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
            ansi: [
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
            ],
        }
    }
}

/// Top-level `[colors]` table in the config file.
#[derive(Deserialize, Default)]
pub(crate) struct ColorsConfig {
    /// Cursor color override, either `cursor = "#009fff"` or
    /// `[colors.cursor] cursor = "#009fff" text = "#000000"`.
    cursor: Option<CursorColorsConfig>,
    /// `[colors.primary]` - default foreground / background.
    primary: Option<PrimaryColors>,
    /// `[colors.selection]` - selection highlight colors.
    selection: Option<SelectionColors>,
    /// `[colors.status_line]` - DEC status line default colors.
    status_line: Option<StatusLineColors>,
    /// `[colors.normal]` - the 8 standard ANSI colors.
    normal: Option<AnsiColors>,
    /// `[colors.bright]` - the 8 bright ANSI colors.
    bright: Option<AnsiColors>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CursorColorsConfig {
    Color(String),
    Table(CursorColors),
}

#[derive(Deserialize, Default)]
struct CursorColors {
    cursor: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct PrimaryColors {
    foreground: Option<String>,
    background: Option<String>,
}

#[derive(Deserialize, Default)]
struct SelectionColors {
    background: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct StatusLineColors {
    foreground: Option<String>,
    background: Option<String>,
}

#[derive(Deserialize, Default)]
struct AnsiColors {
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
}

/// Try to parse a hex color, logging a warning on failure and returning the
/// provided fallback.
fn parse_color_or_default(
    s: &Option<String>,
    fallback: Srgb<u8>,
    label: &str,
) -> Srgb<u8> {
    match s {
        Some(string) => match Srgb::from_str(string)
            .ok()
            .or_else(|| crate::palette_crate::named::from_str(string))
        {
            Some(c) => c,
            None => {
                warn!("invalid color for {label}: {string:?}; using default");
                fallback
            }
        },
        None => fallback,
    }
}

fn parse_color_optional(
    s: &str,
    label: &str,
) -> Option<Srgb<u8>> {
    Srgb::from_str(s)
        .ok()
        .or_else(|| crate::palette_crate::named::from_str(s))
        .or_else(|| {
            warn!("invalid color for {label}: {s:?}; ignoring");
            None
        })
}

/// Build a [`ColorPalette`] from the deserialized `[colors]` config,
/// falling back to hardcoded defaults for any value not specified.
pub(crate) fn build_palette(colors: Option<ColorsConfig>) -> ColorPalette {
    let mut pal = ColorPalette::default();
    let Some(c) = colors else {
        return pal;
    };

    if let Some(ref p) = c.primary {
        pal.fg = parse_color_or_default(&p.foreground, pal.fg, "colors.primary.foreground");
        pal.bg = parse_color_or_default(&p.background, pal.bg, "colors.primary.background");
    }

    if let Some(ref status) = c.status_line {
        pal.status_line_fg =
            parse_color_or_default(&status.foreground, pal.fg, "colors.status_line.foreground");
        pal.status_line_bg = parse_color_or_default(
            &status.background,
            blend_colors(pal.bg, pal.status_line_fg, 0.25),
            "colors.status_line.background",
        );
    } else {
        pal.status_line_fg = pal.fg;
        pal.status_line_bg = blend_colors(pal.bg, pal.status_line_fg, 0.25);
    }

    if let Some(ref cursor) = c.cursor {
        match cursor {
            CursorColorsConfig::Color(s) => {
                pal.cursor = parse_color_optional(s, "colors.cursor");
            }
            CursorColorsConfig::Table(table) => {
                pal.cursor = table
                    .cursor
                    .as_ref()
                    .and_then(|s| parse_color_optional(s, "colors.cursor.cursor"));
                pal.cursor_text = table
                    .text
                    .as_ref()
                    .and_then(|s| parse_color_optional(s, "colors.cursor.text"));
            }
        }
    }

    if let Some(ref sel) = c.selection {
        pal.selection_bg = sel.background.as_ref().and_then(|s| {
            Srgb::from_str(s)
                .ok()
                .or_else(|| crate::palette_crate::named::from_str(s))
                .or_else(|| {
                    warn!("invalid hex color for colors.selection.background: {s:?}; ignoring");
                    None
                })
        });
        pal.selection_fg = sel.text.as_ref().and_then(|s| {
            Srgb::from_str(s)
                .ok()
                .or_else(|| crate::palette_crate::named::from_str(s))
                .or_else(|| {
                    warn!("invalid hex color for colors.selection.text: {s:?}; ignoring");
                    None
                })
        });
    }

    if let Some(ref n) = c.normal {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let fields = [
            &n.black, &n.red, &n.green, &n.yellow, &n.blue, &n.magenta, &n.cyan, &n.white,
        ];
        for (i, (field, name)) in fields.iter().zip(names.iter()).enumerate() {
            pal.ansi[i] =
                parse_color_or_default(field, pal.ansi[i], &format!("colors.normal.{name}"));
        }
    }

    if let Some(ref b) = c.bright {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let fields = [
            &b.black, &b.red, &b.green, &b.yellow, &b.blue, &b.magenta, &b.cyan, &b.white,
        ];
        for (i, (field, name)) in fields.iter().zip(names.iter()).enumerate() {
            pal.ansi[8 + i] =
                parse_color_or_default(field, pal.ansi[8 + i], &format!("colors.bright.{name}"));
        }
    }

    pal
}
