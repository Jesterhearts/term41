use config41::ColorPalette;
use font41::attrs::CellAttrs;
use palette::Srgb;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use terminal41::DecColorLookupTable;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use unicode_segmentation::UnicodeSegmentation;

use crate::renderer::BUTTON_CELLS;
use crate::renderer::BUTTONS_REGION_CELLS;
use crate::renderer::TabBarHover;
use crate::renderer::r#impl::MAX_TAB_WIDTH;
use crate::renderer::r#impl::TabInfo;
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
    pub label: SmolStr,
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
    underline: CellAttrs,
) -> CellAttrs {
    match snap.dec_color.lookup_table {
        DecColorLookupTable::AlternateWithAttrs if snap.dec_color.alternate_underline_text => {
            underline & CellAttrs::UNDERLINE_MASK
        }
        DecColorLookupTable::AlternateWithAttrs | DecColorLookupTable::Alternate => {
            CellAttrs::empty()
        }
        _ => underline & CellAttrs::UNDERLINE_MASK,
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
        screen_row: 0,
        generation: 0,
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
        block_separator: false,
        sticky_prompt: false,
        has_link: vec![false; len],
        underline_color: vec![None; len],
        prompt_start: false,
    }
}

pub(crate) fn local_status_line_row(
    text: &str,
    cols: u32,
    screen_row: u32,
    generation: u64,
    palette: &ColorPalette,
) -> RowSnapshot {
    let cols = cols as usize;
    let mut row = RowSnapshot {
        screen_row,
        generation,
        line_attr: LineAttr::Normal,
        fg: vec![palette.status_line_fg; cols],
        bg: vec![palette.status_line_bg; cols],
        attrs: vec![CellAttrs::default(); cols],
        selected: vec![false; cols],
        matched: vec![false; cols],
        active_match: vec![false; cols],
        cells: vec![smol_str::SmolStr::new_inline(" "); cols],
        exit_status: None,
        block_separator: false,
        sticky_prompt: false,
        has_link: vec![false; cols],
        underline_color: vec![None; cols],
        prompt_start: false,
    };
    for (idx, grapheme) in text.graphemes(true).take(cols).enumerate() {
        let mut builder = SmolStrBuilder::new();
        builder.push_str(grapheme);
        row.cells[idx] = builder.finish();
    }
    row
}

#[cfg(test)]
pub(crate) struct UdkIndicator {
    pub enabled: bool,
    pub locked: bool,
    pub keys: Vec<String>,
}

#[cfg(test)]
pub(crate) fn status_line_indicator_row(
    text: &str,
    udks: UdkIndicator,
    cols: u32,
    palette: &ColorPalette,
) -> RowSnapshot {
    let mut row = blank_status_line_row(cols as usize, palette);
    let right = format_udk_indicator(udks);
    let left_graphemes: Vec<_> = text.graphemes(true).collect();
    let right_graphemes: Vec<_> = right.graphemes(true).collect();
    let left_budget = if right_graphemes.is_empty() {
        cols as usize
    } else {
        (cols as usize).saturating_sub(right_graphemes.len() + 2)
    };
    let clipped_left = clip_status_line_tail(&left_graphemes, left_budget);

    for (idx, grapheme) in clipped_left.into_iter().enumerate() {
        set_status_cell(&mut row, idx, grapheme, palette.status_line_fg);
    }

    if !right_graphemes.is_empty() {
        let start = (cols as usize).saturating_sub(right_graphemes.len());
        let warning_fg = Srgb::new(224, 116, 116);
        let dim_fg = blend(palette.status_line_fg, palette.status_line_bg, 0.45);
        let mut in_badge = false;
        for (offset, grapheme) in right_graphemes.into_iter().enumerate() {
            if grapheme == "[" {
                in_badge = true;
            }
            let fg = if in_badge { warning_fg } else { dim_fg };
            set_status_cell(&mut row, start + offset, grapheme, fg);
            if grapheme == "]" {
                in_badge = false;
            }
        }
    }

    row
}

#[cfg(test)]
fn blank_status_line_row(
    cols: usize,
    palette: &ColorPalette,
) -> RowSnapshot {
    RowSnapshot {
        screen_row: 0,
        generation: 0,
        line_attr: LineAttr::Normal,
        fg: vec![palette.status_line_fg; cols],
        bg: vec![palette.status_line_bg; cols],
        attrs: vec![CellAttrs::default(); cols],
        selected: vec![false; cols],
        matched: vec![false; cols],
        active_match: vec![false; cols],
        cells: vec![smol_str::SmolStr::new_inline(" "); cols],
        exit_status: None,
        block_separator: false,
        sticky_prompt: false,
        has_link: vec![false; cols],
        underline_color: vec![None; cols],
        prompt_start: false,
    }
}

#[cfg(test)]
fn set_status_cell(
    row: &mut RowSnapshot,
    idx: usize,
    grapheme: &str,
    fg: Srgb<u8>,
) {
    if idx >= row.cells.len() {
        return;
    }
    let mut builder = SmolStrBuilder::new();
    builder.push_str(grapheme);
    row.cells[idx] = builder.finish();
    row.fg[idx] = fg;
}

#[cfg(test)]
fn format_udk_indicator(udks: UdkIndicator) -> String {
    if !udks.enabled {
        return String::new();
    }
    if udks.keys.is_empty() {
        return "UDK enabled".to_string();
    }
    let mut out = if udks.locked {
        "UDK locked".to_string()
    } else {
        "UDK".to_string()
    };
    for key in udks.keys {
        out.push(' ');
        out.push('[');
        out.push_str(&key);
        out.push(']');
    }
    out
}

#[cfg(test)]
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
    let keep = cols - 2;
    let mut clipped = Vec::with_capacity(cols);
    clipped.push("… ");
    clipped.extend_from_slice(&segments[segments.len() - keep..]);
    clipped
}

pub(crate) fn build_tab_bar_plan(
    tabs: &[TabInfo<'_>],
    palette: &ColorPalette,
    new_tab_text: SmolStr,
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
        label: new_tab_text,
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
    let new_tab_button_w = cell_w * 4.0;
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

pub(crate) fn centered_ink_origin_x(
    region_x: f32,
    region_w: f32,
    ink_left: f32,
    ink_right: f32,
) -> f32 {
    region_x + (region_w - (ink_right - ink_left)) * 0.5 - ink_left
}

pub(crate) fn row_paintable_cols(row: &RowSnapshot) -> usize {
    [
        row.cells.len(),
        row.attrs.len(),
        row.fg.len(),
        row.bg.len(),
        row.underline_color.len(),
        row.has_link.len(),
    ]
    .into_iter()
    .min()
    .unwrap_or(0)
}

pub(crate) fn visible_row_cols(
    snap: &TermSnapshot,
    row: &RowSnapshot,
) -> u32 {
    let display_cols = if matches!(row.line_attr, LineAttr::Normal) {
        snap.viewport_cols
    } else {
        snap.viewport_cols / 2
    };
    display_cols.min(row_paintable_cols(row) as u32)
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
    );
    let fg = if active_match {
        base_fg
    } else if selected {
        snap.palette.selection_fg.unwrap_or(base_bg)
    } else if block_cursor_here {
        snap.palette.cursor_text.unwrap_or(base_bg)
    } else if matched {
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
            let assignment = terminal41::dec_alternate_assignment_for_style(&snap.dec_color, attrs);
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
    let ellipsis = "… ";
    let truncated_len = max_chars.saturating_sub(2);
    label
        .graphemes(true)
        .take(truncated_len)
        .chain(std::iter::once(ellipsis))
        .collect()
}

#[cfg(test)]
mod tests {
    use config41::CursorStyle;
    use smol_str::ToSmolStr;

    use super::*;

    fn test_snapshot(dec_color: terminal41::DecColorState) -> TermSnapshot {
        let base = ColorPalette::default();
        let palette = ColorPalette {
            fg: terminal41::dec_table_color(&dec_color, dec_color.text.fg),
            bg: terminal41::dec_table_color(&dec_color, dec_color.text.bg),
            ..base
        };
        TermSnapshot {
            generation: 0,
            rows: Vec::new(),
            total_rows: 1,
            viewport_rows: 1,
            viewport_cols: 1,
            viewport_offset: 0,
            status_line_row: None,
            drcs_glyphs: Default::default(),
            dec_color,
            palette,
            search_active: false,
            search: None,
            cursor: None,
            cursor_style: CursorStyle::default(),
            screen_reverse: false,
            on_alt_screen: false,
            command_editor_hidden: false,
            synchronized_update_active: false,
            current_title: None,
            reset_cached_rows: true,
        }
    }

    fn test_row(palette: &ColorPalette) -> RowSnapshot {
        RowSnapshot {
            screen_row: 0,
            generation: 0,
            cells: vec![smol_str::SmolStr::new_inline("x")],
            attrs: vec![CellAttrs::BOLD],
            fg: vec![palette.fg],
            bg: vec![palette.bg],
            underline_color: vec![None],
            has_link: vec![false],
            line_attr: LineAttr::Normal,
            selected: vec![false],
            matched: vec![false],
            active_match: vec![false],
            prompt_start: false,
            exit_status: None,
            block_separator: false,
            sticky_prompt: false,
        }
    }

    fn row_text(row: &RowSnapshot) -> String {
        row.cells.concat()
    }

    #[test]
    fn status_line_indicator_right_aligns_programmed_udks() {
        let palette = ColorPalette::default();
        let row = status_line_indicator_row(
            "cwd",
            UdkIndicator {
                enabled: true,
                locked: false,
                keys: vec!["F6".to_string(), "F12".to_string()],
            },
            36,
            &palette,
        );

        assert_eq!(row_text(&row), "cwd                   UDK [F6] [F12]");
        let badge_start = row_text(&row).find("[F6]").expect("badge");
        assert_eq!(row.fg[badge_start], Srgb::new(224, 116, 116));
        assert_eq!(row.fg[badge_start + 1], Srgb::new(224, 116, 116));
    }

    #[test]
    fn status_line_indicator_shows_enabled_udks_without_programmed_keys() {
        let palette = ColorPalette::default();
        let row = status_line_indicator_row(
            "",
            UdkIndicator {
                enabled: true,
                locked: false,
                keys: vec![],
            },
            16,
            &palette,
        );

        assert_eq!(row_text(&row), "     UDK enabled");
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
    fn block_cursor_uses_configured_cursor_text_color() {
        let dec = terminal41::dec_color_state_from_palette(&ColorPalette::default());
        let mut snap = test_snapshot(dec);
        snap.palette.cursor_text = Some(Srgb::new(1, 2, 3));
        let row = test_row(&snap.palette);

        let painted = resolve_painted_cell(&snap, &row, 0, 0, Some((0, 0)), false);
        assert_eq!(painted.fg, Srgb::new(1, 2, 3));
    }

    #[test]
    fn visible_row_cols_clamps_to_short_row_data() {
        let dec = terminal41::dec_color_state_from_palette(&ColorPalette::default());
        let mut snap = test_snapshot(dec);
        snap.viewport_cols = 2;
        let row = test_row(&snap.palette);

        assert_eq!(visible_row_cols(&snap, &row), 1);
    }

    #[test]
    fn centered_ink_origin_centers_visible_bounds_in_region() {
        let origin = centered_ink_origin_x(100.0, 40.0, 3.0, 11.0);

        assert_eq!(origin, 113.0);
        assert_eq!(origin + (3.0 + 11.0) * 0.5, 120.0);
    }

    #[test]
    fn new_tab_button_hover_uses_window_button_hover_strength() {
        let palette = ColorPalette::default();
        let normal = build_tab_bar_plan(&[], &palette, '🞦'.to_smolstr(), None, false, 200.0, 10.0);
        let hovered = build_tab_bar_plan(
            &[],
            &palette,
            '🞦'.to_smolstr(),
            Some(TabBarHover::NewTab),
            false,
            200.0,
            10.0,
        );

        assert_ne!(normal.new_tab_button.bg, hovered.new_tab_button.bg);
        assert_eq!(
            hovered.new_tab_button.bg,
            Some(blend(normal.base_bg, palette.fg, 0.3))
        );
    }
}
