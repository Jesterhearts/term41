use palette::Srgb;

use crate::ColorPalette;
use crate::DecColorState;
use crate::Screen;
use crate::color::ColorSource;
use crate::dec::color::DEFAULT_TEXT_BG_INDEX;
use crate::dec::color::erase_background_color;
use crate::dec::color::table_color;

struct PaletteColorRemap<'a> {
    new_text_fg: Srgb<u8>,
    new_text_bg: Srgb<u8>,
    new_status_fg: Srgb<u8>,
    new_status_bg: Srgb<u8>,
    palette: &'a ColorPalette,
    dec_color: &'a DecColorState,
}

impl<'a> PaletteColorRemap<'a> {
    fn new(
        new_palette: &'a ColorPalette,
        dec_color: &'a DecColorState,
    ) -> Self {
        Self {
            new_text_fg: new_palette.fg,
            new_text_bg: new_palette.bg,
            new_status_fg: new_palette.status_line_fg,
            new_status_bg: new_palette.status_line_bg,
            palette: new_palette,
            dec_color,
        }
    }

    fn text_fg(
        &self,
        color: Srgb<u8>,
        source: ColorSource,
    ) -> Srgb<u8> {
        match source {
            ColorSource::Default => self.new_text_fg,
            ColorSource::Indexed(index) => self.palette.indexed_color(index),
            ColorSource::Dec(index) => table_color(self.dec_color, index),
            ColorSource::Direct => color,
        }
    }

    fn text_bg(
        &self,
        color: Srgb<u8>,
        source: ColorSource,
    ) -> Srgb<u8> {
        match source {
            ColorSource::Default => self.new_text_bg,
            ColorSource::Indexed(index) => self.palette.indexed_color(index),
            ColorSource::Dec(index) => table_color(self.dec_color, index),
            ColorSource::Direct => color,
        }
    }

    fn status_fg(
        &self,
        color: Srgb<u8>,
        source: ColorSource,
    ) -> Srgb<u8> {
        match source {
            ColorSource::Default => self.new_status_fg,
            ColorSource::Indexed(index) => self.palette.indexed_color(index),
            ColorSource::Dec(index) => table_color(self.dec_color, index),
            ColorSource::Direct => color,
        }
    }

    fn status_bg(
        &self,
        color: Srgb<u8>,
        source: ColorSource,
    ) -> Srgb<u8> {
        match source {
            ColorSource::Default => self.new_status_bg,
            ColorSource::Indexed(index) => self.palette.indexed_color(index),
            ColorSource::Dec(index) => table_color(self.dec_color, index),
            ColorSource::Direct => color,
        }
    }
}

pub(crate) fn apply_screen_palette(
    screen: &mut Screen,
    new_palette: &ColorPalette,
    dec_color: &DecColorState,
) {
    let remap = PaletteColorRemap::new(new_palette, dec_color);
    remap_screen_palette_colors(screen, &remap);
    screen.grid.default_fg = new_palette.fg;
    screen.grid.default_fg_source = ColorSource::Default;
    screen.grid.default_bg = new_palette.bg;
    screen.grid.default_bg_source = ColorSource::Default;
    screen.fg = remap.text_fg(screen.fg, screen.fg_index);
    screen.bg = remap.text_bg(screen.bg, screen.bg_index);
    screen.underline_color = screen
        .underline_color
        .map(|color| remap.text_fg(color, screen.underline_index));
    if let Some(saved) = screen.saved_cursor.as_mut() {
        saved.fg = remap.text_fg(saved.fg, saved.fg_index);
        saved.bg = remap.text_bg(saved.bg, saved.bg_index);
        saved.underline_color = saved
            .underline_color
            .map(|color| remap.text_fg(color, saved.underline_index));
    }
    if let Some(status) = screen.status_line.as_mut() {
        status.fg = remap.status_fg(status.fg, status.fg_index);
        status.bg = remap.status_bg(status.bg, status.bg_index);
        status.underline_color = status
            .underline_color
            .map(|color| remap.status_fg(color, status.underline_index));
        remap_row_palette_colors(&mut status.row, &remap, true);
    }
}

pub(crate) fn sync_screen_erase_defaults(
    screen: &mut Screen,
    dec_color: &DecColorState,
) {
    screen.grid.default_bg = erase_background_color(dec_color, screen.bg);
    screen.grid.default_bg_source = if dec_color.erase_to_screen {
        ColorSource::Dec(DEFAULT_TEXT_BG_INDEX)
    } else {
        screen.bg_index
    };
}

fn remap_screen_palette_colors(
    screen: &mut Screen,
    remap: &PaletteColorRemap,
) {
    for row in &mut screen.grid.rows {
        remap_row_palette_colors(row, remap, false);
    }
    for block in &mut screen.scrollback_blocks {
        block.grid.default_fg = remap.text_fg(block.grid.default_fg, block.grid.default_fg_source);
        block.grid.default_bg = remap.text_bg(block.grid.default_bg, block.grid.default_bg_source);
        for row in &mut block.grid.rows {
            remap_row_palette_colors(row, remap, false);
        }
    }
}

fn remap_row_palette_colors(
    row: &mut crate::Row,
    remap: &PaletteColorRemap,
    status_line: bool,
) {
    for (color, index) in row.fg.iter_mut().zip(&row.fg_index) {
        *color = if status_line {
            remap.status_fg(*color, *index)
        } else {
            remap.text_fg(*color, *index)
        };
    }
    for (color, index) in row.bg.iter_mut().zip(&row.bg_index) {
        *color = if status_line {
            remap.status_bg(*color, *index)
        } else {
            remap.text_bg(*color, *index)
        };
    }
    for (color, index) in row.underline_color.iter_mut().zip(&row.underline_index) {
        *color = color.map(|color| {
            if status_line {
                remap.status_fg(color, *index)
            } else {
                remap.text_fg(color, *index)
            }
        });
    }
}
