use crate::ColorPalette;
use crate::DecColorState;
use crate::Screen;
use crate::dec::color::erase_background_color;

pub(crate) fn apply_screen_palette(
    screen: &mut Screen,
    old_palette: &ColorPalette,
    new_palette: &ColorPalette,
) {
    remap_screen_default_colors(screen, old_palette, new_palette);
    screen.grid.default_fg = new_palette.fg;
    screen.grid.default_bg = new_palette.bg;
    if screen.fg == old_palette.fg {
        screen.fg = new_palette.fg;
    }
    if screen.bg == old_palette.bg {
        screen.bg = new_palette.bg;
    }
    if let Some(status) = screen.status_line.as_mut() {
        if status.fg == old_palette.status_line_fg {
            status.fg = new_palette.status_line_fg;
        }
        if status.bg == old_palette.status_line_bg {
            status.bg = new_palette.status_line_bg;
        }
        for fg in &mut status.row.fg {
            if *fg == old_palette.status_line_fg {
                *fg = new_palette.status_line_fg;
            }
        }
        for bg in &mut status.row.bg {
            if *bg == old_palette.status_line_bg {
                *bg = new_palette.status_line_bg;
            }
        }
    }
}

pub(crate) fn sync_screen_erase_defaults(
    screen: &mut Screen,
    dec_color: &DecColorState,
) {
    screen.grid.default_bg = erase_background_color(dec_color, screen.bg);
}

fn remap_screen_default_colors(
    screen: &mut Screen,
    old_palette: &ColorPalette,
    new_palette: &ColorPalette,
) {
    for row in &mut screen.grid.rows {
        for fg in &mut row.fg {
            if *fg == old_palette.fg {
                *fg = new_palette.fg;
            }
        }
        for bg in &mut row.bg {
            if *bg == old_palette.bg {
                *bg = new_palette.bg;
            }
        }
    }
}
