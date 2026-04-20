use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use palette::Srgb;
use smol_str::SmolStrBuilder;
use terminal41::ColorPalette;
use terminal41::DecColorLookupTable;
use terminal41::LineAttr;
use unicode_segmentation::UnicodeSegmentation;

use crate::renderer::BUTTON_CELLS;
use crate::renderer::BUTTONS_REGION_CELLS;
use crate::renderer::TabBarHover;
use crate::renderer::r#impl::MAX_TAB_WIDTH;
use crate::renderer::r#impl::RowSnapshot;
use crate::renderer::r#impl::TabInfo;
use crate::renderer::r#impl::TermSnapshot;
use crate::renderer::r#impl::blend;
use crate::renderer::r#impl::resolve_cell_colors;

pub(crate) struct TabBarPlan {
    pub base_bg: Srgb<u8>,
    pub tabs: Vec<TabVisual>,
    pub new_tab_button: TabBarButtonVisual,
    pub buttons: [WindowButtonVisual; 3],
}

#[derive(Clone, Copy)]
pub(crate) struct TabBarRegion {
    pub x: f32,
    pub width: f32,
    pub button: Option<TabBarHover>,
}

pub(crate) struct TabBarLayout {
    pub tabs: Vec<TabBarRegion>,
    pub new_tab_button: TabBarRegion,
    pub buttons: [TabBarRegion; 3],
}

pub(crate) struct TabVisual {
    pub x: f32,
    pub width: f32,
    pub bg: Option<Srgb<u8>>,
    pub separator: Option<Srgb<u8>>,
    pub label: String,
    pub label_x: f32,
}

pub(crate) struct TabBarButtonVisual {
    pub x: f32,
    pub width: f32,
    pub bg: Option<Srgb<u8>>,
    pub label: &'static str,
}

pub(crate) struct WindowButtonVisual {
    pub x: f32,
    pub width: f32,
    pub bg: Option<Srgb<u8>>,
    pub label: &'static str,
}

pub(crate) struct PaintedCell {
    pub fg: Srgb<u8>,
    pub base_fg: Srgb<u8>,
    pub fill_bg: Option<Srgb<u8>>,
}

pub(crate) fn blink_animation_enabled(
    snap: &TermSnapshot,
    attrs: CellAttrs,
) -> bool {
    if !attrs.intersects(CellAttrs::BLINK | CellAttrs::RAPID_BLINK) {
        return false;
    }
    match snap.dec_color.lookup_table {
        DecColorLookupTable::AlternateWithAttrs => snap.dec_color.alternate_blink_text,
        DecColorLookupTable::Alternate => false,
        _ => true,
    }
}

pub(crate) fn underline_style_for_render(
    snap: &TermSnapshot,
    underline: UnderlineStyle,
) -> UnderlineStyle {
    match snap.dec_color.lookup_table {
        DecColorLookupTable::AlternateWithAttrs if snap.dec_color.alternate_underline_text => {
            underline
        }
        DecColorLookupTable::AlternateWithAttrs | DecColorLookupTable::Alternate => {
            UnderlineStyle::None
        }
        _ => underline,
    }
}

pub(crate) fn bold_glyph_enabled(snap: &TermSnapshot) -> bool {
    snap.dec_color.lookup_table != DecColorLookupTable::Alternate
}

pub(crate) fn status_line_label_row(
    text: &str,
    palette: &ColorPalette,
) -> RowSnapshot {
    let len = text.graphemes(true).count();
    RowSnapshot {
        line_attr: LineAttr::Normal,
        fg: vec![palette.status_line_fg; len],
        bg: vec![palette.status_line_bg; len],
        attrs: vec![CellAttrs::default(); len],
        selected: vec![false; len],
        matched: vec![false; len],
        active_match: vec![false; len],
        cells: text
            .graphemes(true)
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect(),
        exit_status: None,
        has_link: vec![false; len],
        underline: vec![UnderlineStyle::None; len],
        underline_color: vec![None; len],
        prompt_start: false,
    }
}

pub(crate) fn status_line_text_row(
    text: &str,
    cols: u32,
    palette: &ColorPalette,
) -> RowSnapshot {
    let segments: Vec<_> = text.graphemes(true).collect();
    let clipped = clip_status_line_tail(&segments, cols as usize);
    let mut row = RowSnapshot {
        line_attr: LineAttr::Normal,
        fg: vec![palette.status_line_fg; cols as usize],
        bg: vec![palette.status_line_bg; cols as usize],
        attrs: vec![CellAttrs::default(); cols as usize],
        selected: vec![false; cols as usize],
        matched: vec![false; cols as usize],
        active_match: vec![false; cols as usize],
        cells: vec![smol_str::SmolStr::new_inline(" "); cols as usize],
        exit_status: None,
        has_link: vec![false; cols as usize],
        underline: vec![UnderlineStyle::None; cols as usize],
        underline_color: vec![None; cols as usize],
        prompt_start: false,
    };
    for (idx, grapheme) in clipped.into_iter().enumerate() {
        let mut builder = SmolStrBuilder::new();
        builder.push_str(grapheme);
        row.cells[idx] = builder.finish();
    }
    row
}

fn clip_status_line_tail<'a>(
    segments: &[&'a str],
    cols: usize,
) -> Vec<&'a str> {
    if segments.len() <= cols {
        return segments.to_vec();
    }
    if cols == 0 {
        return Vec::new();
    }
    if cols == 1 {
        return vec!["…"];
    }
    let keep = cols - 1;
    let mut clipped = Vec::with_capacity(cols);
    clipped.push("…");
    clipped.extend_from_slice(&segments[segments.len() - keep..]);
    clipped
}

pub(crate) fn build_tab_bar_plan(
    tabs: &[TabInfo<'_>],
    palette: &ColorPalette,
    hovered_button: Option<TabBarHover>,
    maximized: bool,
    surface_w: f32,
    cell_w: f32,
) -> TabBarPlan {
    let active_bg = palette.bg;
    let inactive_bg = blend(palette.bg, palette.fg, 0.5);
    let layout = build_tab_bar_layout(tabs.len(), surface_w, cell_w);
    let margin = cell_w;

    let tabs = tabs
        .iter()
        .zip(layout.tabs.iter().copied())
        .map(|(tab, region)| {
            let max_label_chars = ((region.width - margin * 2.0) / cell_w).max(1.0) as usize;
            let label = truncate_label(tab.label, max_label_chars);
            TabVisual {
                x: region.x,
                width: region.width,
                bg: tab.active.then_some(active_bg),
                separator: (!tabs.is_empty()).then(|| blend(active_bg, inactive_bg, 0.5)),
                label_x: region.x + margin,
                label,
            }
        })
        .collect();

    let new_tab_button = TabBarButtonVisual {
        x: layout.new_tab_button.x,
        width: layout.new_tab_button.width,
        bg: Some(match hovered_button {
            Some(TabBarHover::NewTab) => blend(inactive_bg, palette.fg, 0.3),
            _ => blend(inactive_bg, palette.fg, 0.15),
        }),
        label: "\u{2795}",
    };

    let button_labels = [
        "\u{1F5D5}",
        if maximized { "\u{1F5D7}" } else { "\u{1F5D6}" },
        "\u{1F5D9}",
    ];
    let buttons = core::array::from_fn(|i| WindowButtonVisual {
        x: layout.buttons[i].x,
        width: layout.buttons[i].width,
        bg: hovered_button
            .and_then(|hover| match hover {
                TabBarHover::NewTab => None,
                TabBarHover::Minimize => Some(0),
                TabBarHover::Maximize => Some(1),
                TabBarHover::Close => Some(2),
            })
            .filter(|&idx| idx == i)
            .map(|_| {
                if i == 2 {
                    Srgb::new(200, 50, 50)
                } else {
                    blend(inactive_bg, palette.fg, 0.3)
                }
            }),
        label: button_labels[i],
    });

    TabBarPlan {
        base_bg: inactive_bg,
        tabs,
        new_tab_button,
        buttons,
    }
}

pub(crate) fn build_tab_bar_layout(
    tab_count: usize,
    surface_w: f32,
    cell_w: f32,
) -> TabBarLayout {
    let buttons_region_w = cell_w * BUTTONS_REGION_CELLS;
    let new_tab_button_w = cell_w * BUTTON_CELLS;
    let tabs_available_w = (surface_w - buttons_region_w - new_tab_button_w).max(0.0);
    let max_tab_w = (cell_w * MAX_TAB_WIDTH).min(tabs_available_w);
    let tab_w = if tab_count == 0 {
        0.0
    } else {
        (tabs_available_w / tab_count as f32).min(max_tab_w)
    };
    let tabs = (0..tab_count)
        .map(|i| TabBarRegion {
            x: i as f32 * tab_w,
            width: tab_w,
            button: None,
        })
        .collect();
    let new_tab_button = TabBarRegion {
        x: tab_count as f32 * tab_w,
        width: new_tab_button_w,
        button: Some(TabBarHover::NewTab),
    };
    let button_w = cell_w * BUTTON_CELLS;
    let buttons_x = surface_w - buttons_region_w;
    let buttons = core::array::from_fn(|i| TabBarRegion {
        x: buttons_x + i as f32 * button_w,
        width: button_w,
        button: match i {
            0 => Some(TabBarHover::Minimize),
            1 => Some(TabBarHover::Maximize),
            2 => Some(TabBarHover::Close),
            _ => None,
        },
    });

    TabBarLayout {
        tabs,
        new_tab_button,
        buttons,
    }
}

pub(crate) fn resolve_painted_cell(
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    row: u32,
    col: u32,
    block_cursor: Option<(u32, u32)>,
    has_background_image: bool,
) -> PaintedCell {
    let selected = snap_row
        .selected
        .get(col as usize)
        .copied()
        .unwrap_or(false);
    let matched = snap_row.matched.get(col as usize).copied().unwrap_or(false);
    let active_match = snap_row
        .active_match
        .get(col as usize)
        .copied()
        .unwrap_or(false);
    let cell_attrs = snap_row.attrs[col as usize];
    let block_cursor_here = block_cursor == Some((row, col));
    let (base_fg, base_bg) = resolve_dec_color_cell(
        snap,
        &snap_row.fg[col as usize],
        &snap_row.bg[col as usize],
        cell_attrs,
        snap_row.underline[col as usize],
    );
    let fg = if active_match {
        base_fg
    } else if selected {
        snap.palette.selection_fg.unwrap_or(base_bg)
    } else if matched || block_cursor_here {
        base_bg
    } else {
        base_fg
    };
    let fill_bg = if active_match {
        Some(blend(base_fg, base_bg, 0.5))
    } else if selected {
        Some(snap.palette.selection_bg.unwrap_or(base_fg))
    } else if block_cursor_here {
        Some(snap.palette.cursor.unwrap_or(base_fg))
    } else if matched {
        Some(base_fg)
    } else if has_background_image && base_bg == snap.palette.bg {
        None
    } else {
        Some(base_bg)
    };

    PaintedCell {
        fg,
        base_fg,
        fill_bg,
    }
}

fn resolve_dec_color_cell(
    snap: &TermSnapshot,
    raw_fg: &Srgb<u8>,
    raw_bg: &Srgb<u8>,
    attrs: CellAttrs,
    underline: UnderlineStyle,
) -> (Srgb<u8>, Srgb<u8>) {
    let mut color_attrs = attrs;
    let mut fg = *raw_fg;
    let mut bg = *raw_bg;
    let default_colored = *raw_fg == snap.palette.fg && *raw_bg == snap.palette.bg;
    let mut recolored_by_alternate_lookup = false;

    match snap.dec_color.lookup_table {
        DecColorLookupTable::AlternateWithAttrs | DecColorLookupTable::Alternate
            if default_colored =>
        {
            let assignment =
                terminal41::dec_alternate_assignment_for_style(&snap.dec_color, attrs, underline);
            fg = terminal41::dec_table_color(&snap.dec_color, assignment.fg);
            bg = terminal41::dec_table_color(&snap.dec_color, assignment.bg);
            color_attrs.remove(CellAttrs::REVERSE);
            recolored_by_alternate_lookup = true;
        }
        _ => {}
    }

    let (mut fg, mut bg) = resolve_cell_colors(&fg, &bg, color_attrs, snap.screen_reverse);

    if snap.dec_color.lookup_table == DecColorLookupTable::Mono {
        fg = grayscale(fg);
        bg = grayscale(bg);
    } else if attrs.contains(CellAttrs::BOLD) && !recolored_by_alternate_lookup {
        fg = brighten_basic_color(fg, &snap.dec_color).unwrap_or(fg);
        if snap.dec_color.bold_blink_affects_background {
            bg = brighten_basic_color(bg, &snap.dec_color).unwrap_or(bg);
        }
    }

    (fg, bg)
}

fn brighten_basic_color(
    color: Srgb<u8>,
    state: &terminal41::DecColorState,
) -> Option<Srgb<u8>> {
    for idx in 0..8u8 {
        if terminal41::dec_table_color(state, idx) == color {
            return Some(terminal41::dec_table_color(state, idx + 8));
        }
    }
    None
}

fn grayscale(color: Srgb<u8>) -> Srgb<u8> {
    let luma = (0.299 * color.red as f32 + 0.587 * color.green as f32 + 0.114 * color.blue as f32)
        .round()
        .clamp(0.0, 255.0) as u8;
    Srgb::new(luma, luma, luma)
}

fn truncate_label(
    label: &str,
    max_chars: usize,
) -> String {
    let label_chars = label.graphemes(true).count();
    if label_chars <= max_chars {
        return label.to_string();
    }
    let ellipsis = "…";
    let truncated_len = max_chars.saturating_sub(1);
    label
        .graphemes(true)
        .take(truncated_len)
        .chain(std::iter::once(ellipsis))
        .collect()
}

#[cfg(test)]
mod tests {
    use terminal41::CursorStyle;

    use super::*;

    fn test_snapshot(dec_color: terminal41::DecColorState) -> TermSnapshot {
        let base = ColorPalette::default();
        let palette = ColorPalette {
            fg: terminal41::dec_table_color(&dec_color, dec_color.text.fg),
            bg: terminal41::dec_table_color(&dec_color, dec_color.text.bg),
            ..base
        };
        TermSnapshot {
            rows: Vec::new(),
            viewport_rows: 1,
            viewport_cols: 1,
            status_line_row: None,
            drcs_glyphs: Default::default(),
            dec_color,
            palette,
            search_active: false,
            search: None,
            cursor: None,
            cursor_style: CursorStyle::default(),
            screen_reverse: false,
        }
    }

    fn test_row(palette: &ColorPalette) -> RowSnapshot {
        RowSnapshot {
            cells: vec![smol_str::SmolStr::new_inline("x")],
            attrs: vec![CellAttrs::BOLD],
            fg: vec![palette.fg],
            bg: vec![palette.bg],
            underline: vec![UnderlineStyle::None],
            underline_color: vec![None],
            has_link: vec![false],
            line_attr: LineAttr::Normal,
            selected: vec![false],
            matched: vec![false],
            active_match: vec![false],
            prompt_start: false,
            exit_status: None,
        }
    }

    #[test]
    fn alternate_lookup_table_recolors_default_cell_by_attrs() {
        let mut dec = terminal41::dec_color_state_from_palette(&ColorPalette::default());
        terminal41::dec_assign_alternate_text_color(&mut dec, 1, 2, 3);
        terminal41::dec_select_lookup_table(&mut dec, 1);
        let snap = test_snapshot(dec);
        let row = test_row(&snap.palette);

        let painted = resolve_painted_cell(&snap, &row, 0, 0, None, false);
        assert_eq!(
            painted.base_fg,
            terminal41::dec_table_color(&snap.dec_color, 2)
        );
        assert_eq!(
            painted.fill_bg,
            Some(terminal41::dec_table_color(&snap.dec_color, 3))
        );
    }

    #[test]
    fn mono_lookup_table_grayscales_colors() {
        let mut dec = terminal41::dec_color_state_from_palette(&ColorPalette::default());
        terminal41::dec_select_lookup_table(&mut dec, 0);
        let mut snap = test_snapshot(dec);
        snap.palette.fg = Srgb::new(255, 0, 0);
        snap.palette.bg = Srgb::new(0, 0, 255);
        let row = RowSnapshot {
            fg: vec![snap.palette.fg],
            bg: vec![snap.palette.bg],
            ..test_row(&snap.palette)
        };

        let painted = resolve_painted_cell(&snap, &row, 0, 0, None, false);
        assert_eq!(painted.base_fg.red, painted.base_fg.green);
        assert_eq!(painted.base_fg.green, painted.base_fg.blue);
        let fill = painted.fill_bg.expect("mono mode still fills the cell");
        assert_eq!(fill.red, fill.green);
        assert_eq!(fill.green, fill.blue);
    }

    #[test]
    fn tab_bar_layout_reserves_space_for_new_tab_button_and_window_buttons() {
        let layout = build_tab_bar_layout(2, 200.0, 10.0);

        assert_eq!(layout.tabs.len(), 2);
        assert_eq!(layout.tabs[0].x, 0.0);
        assert_eq!(layout.tabs[0].width, 40.0);
        assert_eq!(layout.tabs[1].x, 40.0);
        assert_eq!(layout.tabs[1].width, 40.0);
        assert_eq!(layout.new_tab_button.x, 80.0);
        assert_eq!(layout.new_tab_button.width, 30.0);
        assert_eq!(layout.buttons[0].x, 110.0);
        assert_eq!(layout.buttons[1].x, 140.0);
        assert_eq!(layout.buttons[2].x, 170.0);
        assert_eq!(layout.buttons[0].width, 30.0);
    }

    #[test]
    fn new_tab_button_hover_uses_window_button_hover_strength() {
        let palette = ColorPalette::default();
        let normal = build_tab_bar_plan(&[], &palette, None, false, 200.0, 10.0);
        let hovered =
            build_tab_bar_plan(&[], &palette, Some(TabBarHover::NewTab), false, 200.0, 10.0);

        assert_ne!(normal.new_tab_button.bg, hovered.new_tab_button.bg);
        assert_eq!(
            hovered.new_tab_button.bg,
            Some(blend(normal.base_bg, palette.fg, 0.3))
        );
    }
}
