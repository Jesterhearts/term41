//! Runtime settings mutation helpers.

use crate::ColorPalette;
use crate::CursorStyle;
use crate::DecColorState;
use crate::EmojiCompatibilityMode;
use crate::FeaturePermissions;
use crate::Screen;
use crate::StatusDisplayKind;
use crate::TerminalProtocolState;
use crate::Viewport;
use crate::dec::color::effective_palette;
use crate::dec::color::rebase_theme_entries;
use crate::feature;
use crate::lifecycle_ops;
use crate::screen::palette_sync::apply_screen_palette;
use crate::screen::palette_sync::sync_screen_erase_defaults;

/// Replace the default cursor style.
pub fn set_default_cursor_style(
    cursor_style: &mut CursorStyle,
    style: CursorStyle,
) {
    *cursor_style = style;
}

/// Replace the default legacy emoji compatibility mode.
pub fn set_emoji_compatibility_mode(
    emoji_compatibility_mode: &mut EmojiCompatibilityMode,
    mode: EmojiCompatibilityMode,
) {
    *emoji_compatibility_mode = mode;
}

/// Replace the base palette and rebase DEC color-table entries that still
/// matched the old theme defaults.
pub fn set_palette(
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &mut ColorPalette,
    dec_color: &mut DecColorState,
    new_palette: ColorPalette,
) {
    let old_palette = palette.clone();
    rebase_theme_entries(dec_color, base_palette, &new_palette);
    *base_palette = new_palette;
    *palette = effective_palette(base_palette, dec_color);
    for screen in [active, stash] {
        apply_screen_palette(screen, &old_palette, palette);
        sync_screen_erase_defaults(screen, dec_color);
    }
}

/// Replace terminal feature-permission gates.
pub fn set_feature_permissions(
    protocol: &mut TerminalProtocolState,
    permissions: FeaturePermissions,
) {
    protocol.feature_permissions = permissions;
}

/// Replace the stored cell pixel dimensions.
pub fn set_cell_dimensions(
    cell_width: &mut u32,
    cell_height: &mut u32,
    new_cell_width: u32,
    new_cell_height: u32,
) {
    *cell_width = new_cell_width;
    *cell_height = new_cell_height;
}

/// Apply a new scrollback row limit to the active screen.
pub fn set_scrollback_policy(
    active: &mut Screen,
    viewport: &Viewport,
    limit: u32,
) {
    feature::apply_scrollback_limit(active, viewport, limit);
}

/// Replace the default status-line display mode and resize screens as needed.
pub fn set_default_status_display(
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    palette: &ColorPalette,
    default_status_display: &mut StatusDisplayKind,
    status_display: StatusDisplayKind,
) {
    lifecycle_ops::set_default_status_display(
        active,
        stash,
        viewport,
        palette,
        default_status_display,
        status_display,
    );
}

#[cfg(test)]
mod tests {
    use palette::Srgb;

    use super::*;
    use crate::CursorShape;
    use crate::test_support::TestTerm;

    #[test]
    fn config_default_cursor_style_overrides_xterm_default() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_default_cursor_style(CursorStyle {
            shape: CursorShape::Underline,
            blink: false,
        });
        assert_eq!(term.cursor_style.shape, CursorShape::Underline);
        assert!(!term.cursor_style.blink);
    }

    #[test]
    fn set_scrollback_limit_takes_effect_on_next_push() {
        let mut term = TestTerm::new(8, 2, 100, 16, 8);
        for i in 0..50u32 {
            term.process(format!("line{i}\n").as_bytes());
        }
        term.set_scrollback_policy(5);
        for i in 0..20u32 {
            term.process(format!("after{i}\n").as_bytes());
        }
        let max_expected = term.viewport.rows as usize + 5;
        assert!(
            term.active.grid.rows.len() <= max_expected,
            "grid kept {} rows after lowering limit to 5 (max {})",
            term.active.grid.rows.len(),
            max_expected,
        );
    }

    #[test]
    fn set_palette_updates_grid_defaults_and_existing_default_cells() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"ab");
        let old = term.palette.clone();
        let mut new = old.clone();
        new.fg = Srgb::new(10, 20, 30);
        new.bg = Srgb::new(40, 50, 60);

        term.set_palette(new.clone());

        assert_eq!(term.palette.fg, new.fg);
        assert_eq!(term.palette.bg, new.bg);
        assert_eq!(term.active.grid.default_fg, new.fg);
        assert_eq!(term.active.grid.default_bg, new.bg);
        assert_eq!(term.active.grid.rows[0].fg[0], new.fg);
        assert_eq!(term.active.grid.rows[0].bg[0], new.bg);
        assert_eq!(term.active.fg, new.fg);
        assert_eq!(term.active.bg, new.bg);
    }

    #[test]
    fn set_palette_preserves_non_default_foreground_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[31mx");
        let old_fg = term.active.grid.rows[0].fg[0];
        let mut new = term.palette.clone();
        new.fg = Srgb::new(10, 20, 30);
        new.bg = Srgb::new(40, 50, 60);

        term.set_palette(new);

        assert_eq!(term.active.grid.rows[0].fg[0], old_fg);
    }

    #[test]
    fn set_palette_updates_existing_ansi_colors() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        term.process(b"\x1b[31;41;58;5;1mX");
        let mut new = term.palette.clone();
        let new_red = Srgb::new(9, 10, 11);
        new.ansi[1] = new_red;

        term.set_palette(new);

        let row = &term.active.grid.rows[0];
        assert_eq!(row.fg[0], new_red);
        assert_eq!(row.bg[0], new_red);
        assert_eq!(row.underline_color[0], Some(new_red));
        assert_eq!(term.active.fg, new_red);
        assert_eq!(term.active.bg, new_red);
        assert_eq!(term.active.underline_color, Some(new_red));
    }

    #[test]
    fn set_palette_preserves_truecolor_values_outside_palette() {
        let mut term = TestTerm::new(4, 2, 10, 16, 8);
        let explicit = Srgb::new(1, 2, 3);
        term.process(b"\x1b[38;2;1;2;3;48;2;4;5;6;58;2;7;8;9mX");
        let mut new = term.palette.clone();
        new.fg = Srgb::new(10, 20, 30);
        new.bg = Srgb::new(40, 50, 60);
        new.ansi[1] = Srgb::new(70, 80, 90);

        term.set_palette(new);

        let row = &term.active.grid.rows[0];
        assert_eq!(row.fg[0], explicit);
        assert_eq!(row.bg[0], Srgb::new(4, 5, 6));
        assert_eq!(row.underline_color[0], Some(Srgb::new(7, 8, 9)));
    }
}
