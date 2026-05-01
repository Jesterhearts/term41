use super::*;

/// `t = 0` returns `a`, `t = 1` returns `b`. Kept byte-space on purpose —
/// the renderer already treats the existing cell fg/bg as sRGB8 throughout,
/// so a gamma-correct blend would be inconsistent with the rest of the
/// pipeline and is overkill for the search-bar highlight use case.
pub(crate) fn blend(
    a: Srgb<u8>,
    b: Srgb<u8>,
    t: f32,
) -> Srgb<u8> {
    Srgb::new(
        lerp_u8(a.red, b.red, t),
        lerp_u8(a.green, b.green, t),
        lerp_u8(a.blue, b.blue, t),
    )
}

/// Apply SGR reverse (swap fg/bg) and dim (halve fg brightness) to a cell's
/// stored colours, returning the effective (fg, bg) pair for rendering.
pub(crate) fn resolve_cell_colors(
    fg: &Srgb<u8>,
    bg: &Srgb<u8>,
    attrs: CellAttrs,
    screen_reverse: bool,
) -> (Srgb<u8>, Srgb<u8>) {
    let (mut fg, mut bg) = (*fg, *bg);
    // DECSCNM (screen reverse) XORs with per-cell SGR 7 (REVERSE):
    // both off or both on → normal; one on → swap.
    if attrs.contains(CellAttrs::REVERSE) != screen_reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if attrs.contains(CellAttrs::DIM) {
        fg = Srgb::new(fg.red / 2, fg.green / 2, fg.blue / 2);
    }
    // SGR 8 — concealed text. Foreground matches background so the text
    // is invisible but still selectable / copyable.
    if attrs.contains(CellAttrs::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// Shared row-layout result consumed by both the GPU renderer and the
/// startup software presenter. This keeps shaping, style synthesis, and
/// effective-foreground decisions in one place even though the two
/// backends rasterize/blit differently.
pub(crate) struct CollectedGlyph {
    pub font_index: usize,
    pub glyph_id: u16,
    pub cells_wide: u8,
    pub col: u16,
    pub x_offset: f32,
    pub y_offset: f32,
    pub fg: Srgb<u8>,
    pub synth_bold: bool,
    pub synth_italic: bool,
}

pub(crate) fn collect_row_glyphs(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    row: u32,
    visible_cols: u32,
    block_cursor: Option<(u32, u32)>,
    blink_off: bool,
    rapid_blink_off: bool,
) -> Vec<CollectedGlyph> {
    let _drcs = font41::set_drcs_context(drcs_geometry_class(snap), Some(snap.drcs_glyphs.clone()));
    let paintable_cols = row_paintable_cols(snap_row);
    let shaped = font_system.shape_row(
        &snap_row.cells[..paintable_cols],
        &snap_row.attrs[..paintable_cols],
    );
    let mut collected = Vec::with_capacity(shaped.len());

    for sg in shaped {
        if sg.col as u32 >= visible_cols {
            continue;
        }
        if terminal41::is_kitty_unicode_placeholder_cell(&snap_row.cells[sg.col as usize]) {
            continue;
        }

        let cell_attrs = snap_row.attrs[sg.col as usize];
        if blink_animation_enabled(snap, cell_attrs)
            && cell_attrs.contains(CellAttrs::BLINK)
            && blink_off
        {
            continue;
        }
        if blink_animation_enabled(snap, cell_attrs)
            && cell_attrs.contains(CellAttrs::RAPID_BLINK)
            && rapid_blink_off
        {
            continue;
        }

        let wants_bold = cell_attrs.contains(CellAttrs::BOLD) && bold_glyph_enabled(snap);
        let wants_italic = cell_attrs.contains(CellAttrs::ITALIC);
        let synth_bold = wants_bold && !font_system.font_is_bold(sg.font_index);
        let synth_italic = wants_italic && !font_system.font_is_italic(sg.font_index);

        let painted = resolve_painted_cell(snap, snap_row, row, sg.col as u32, block_cursor, false);

        collected.push(CollectedGlyph {
            font_index: sg.font_index,
            glyph_id: sg.glyph_id,
            cells_wide: sg.cells_wide,
            col: sg.col,
            x_offset: sg.x_offset,
            y_offset: sg.y_offset,
            fg: painted.fg,
            synth_bold,
            synth_italic,
        });
    }

    collected
}

pub(crate) fn drcs_geometry_class(snap: &TermSnapshot) -> Option<font41::DrcsGeometryClass> {
    match (snap.viewport_cols, snap.total_rows) {
        (0..=80, 0..=24) => Some(font41::DrcsGeometryClass::Col80Line24),
        (81.., 0..=24) => Some(font41::DrcsGeometryClass::Col132Line24),
        (0..=80, 25..=36) => Some(font41::DrcsGeometryClass::Col80Line36),
        (81.., 25..=36) => Some(font41::DrcsGeometryClass::Col132Line36),
        (0..=80, 37..) => Some(font41::DrcsGeometryClass::Col80Line48),
        (81.., 37..) => Some(font41::DrcsGeometryClass::Col132Line48),
    }
}
