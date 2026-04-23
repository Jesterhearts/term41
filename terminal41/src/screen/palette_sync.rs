use palette::Srgb;

use crate::ColorPalette;
use crate::DecColorState;
use crate::Screen;
use crate::dec::color::erase_background_color;

struct PaletteColorRemap {
    old_text_fg: Srgb<u8>,
    new_text_fg: Srgb<u8>,
    old_text_bg: Srgb<u8>,
    new_text_bg: Srgb<u8>,
    old_status_fg: Srgb<u8>,
    new_status_fg: Srgb<u8>,
    old_status_bg: Srgb<u8>,
    new_status_bg: Srgb<u8>,
    ansi: [(Srgb<u8>, Srgb<u8>); 16],
}

impl PaletteColorRemap {
    fn new(
        old_palette: &ColorPalette,
        new_palette: &ColorPalette,
    ) -> Self {
        Self {
            old_text_fg: old_palette.fg,
            new_text_fg: new_palette.fg,
            old_text_bg: old_palette.bg,
            new_text_bg: new_palette.bg,
            old_status_fg: old_palette.status_line_fg,
            new_status_fg: new_palette.status_line_fg,
            old_status_bg: old_palette.status_line_bg,
            new_status_bg: new_palette.status_line_bg,
            ansi: std::array::from_fn(|idx| (old_palette.ansi[idx], new_palette.ansi[idx])),
        }
    }

    fn text_fg(
        &self,
        color: Srgb<u8>,
    ) -> Srgb<u8> {
        if color == self.old_text_fg {
            self.new_text_fg
        } else {
            self.ansi(color)
        }
    }

    fn text_bg(
        &self,
        color: Srgb<u8>,
    ) -> Srgb<u8> {
        if color == self.old_text_bg {
            self.new_text_bg
        } else {
            self.ansi(color)
        }
    }

    fn status_fg(
        &self,
        color: Srgb<u8>,
    ) -> Srgb<u8> {
        if color == self.old_status_fg {
            self.new_status_fg
        } else {
            self.ansi(color)
        }
    }

    fn status_bg(
        &self,
        color: Srgb<u8>,
    ) -> Srgb<u8> {
        if color == self.old_status_bg {
            self.new_status_bg
        } else {
            self.ansi(color)
        }
    }

    fn ansi(
        &self,
        color: Srgb<u8>,
    ) -> Srgb<u8> {
        self.ansi
            .iter()
            .find_map(|(old, new)| (color == *old).then_some(*new))
            .unwrap_or(color)
    }
}

pub(crate) fn apply_screen_palette(
    screen: &mut Screen,
    old_palette: &ColorPalette,
    new_palette: &ColorPalette,
) {
    let remap = PaletteColorRemap::new(old_palette, new_palette);
    remap_screen_palette_colors(screen, &remap);
    screen.grid.default_fg = new_palette.fg;
    screen.grid.default_bg = new_palette.bg;
    screen.fg = remap.text_fg(screen.fg);
    screen.bg = remap.text_bg(screen.bg);
    screen.underline_color = screen.underline_color.map(|color| remap.text_fg(color));
    if let Some(status) = screen.status_line.as_mut() {
        status.fg = remap.status_fg(status.fg);
        status.bg = remap.status_bg(status.bg);
        status.underline_color = status.underline_color.map(|color| remap.status_fg(color));
        for fg in &mut status.row.fg {
            *fg = remap.status_fg(*fg);
        }
        for bg in &mut status.row.bg {
            *bg = remap.status_bg(*bg);
        }
        for underline_color in &mut status.row.underline_color {
            *underline_color = underline_color.map(|color| remap.status_fg(color));
        }
    }
}

pub(crate) fn sync_screen_erase_defaults(
    screen: &mut Screen,
    dec_color: &DecColorState,
) {
    screen.grid.default_bg = erase_background_color(dec_color, screen.bg);
}

fn remap_screen_palette_colors(
    screen: &mut Screen,
    remap: &PaletteColorRemap,
) {
    for row in &mut screen.grid.rows {
        for fg in &mut row.fg {
            *fg = remap.text_fg(*fg);
        }
        for bg in &mut row.bg {
            *bg = remap.text_bg(*bg);
        }
        for underline_color in &mut row.underline_color {
            *underline_color = underline_color.map(|color| remap.text_fg(color));
        }
    }
}
