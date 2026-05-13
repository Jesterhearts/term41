use config41::ColorPalette;
use config41::CursorShape;
use font41::attrs::CellAttrs;
use palette::Srgb;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;

use super::ClipRect;
use super::CursorRenderState;
use super::FrameLayout;
use super::terminal_row_y;
use crate::renderer::paint::blink_animation_enabled;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::row_paintable_cols;

pub(super) struct CachedRowKey {
    pub(super) key: RowRenderKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowRenderKey {
    pub(super) layout: RowLayoutKey,
    pub(super) cursor: RowCursorKey,
    pub(super) blink: RowBlinkKey,
    pub(super) gutter_marker: RowGutterMarkerKey,
    pub(super) popup_clip: Option<ClipRectKey>,
    pub(super) background_present: bool,
    pub(super) screen_reverse: bool,
    pub(super) bg_alpha: u8,
    pub(super) viewport_cols: u32,
    pub(super) total_rows: u32,
    pub(super) drcs_generation: usize,
    pub(super) font_generation: u64,
    pub(super) glyph_atlas_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowLayoutKey {
    pub(super) cell_w: u32,
    pub(super) cell_h: u32,
    pub(super) baseline: u32,
    pub(super) gutter_px: u32,
    pub(super) tab_bar_h: u32,
    pub(super) terminal_y_offset: u32,
    pub(super) block_y_offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowCursorKey {
    None,
    Block { col: u32 },
    Underline { col: u32 },
    Beam { col: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RowBlinkKey {
    blink_off: bool,
    rapid_blink_off: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowGutterMarkerKey {
    pub(super) prompt_start: bool,
    pub(super) exit_status: Option<i32>,
    pub(super) block_separator: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ClipRectKey {
    pub(super) left: u32,
    pub(super) top: u32,
    pub(super) right: u32,
    pub(super) bottom: u32,
}

pub(super) fn invalidate_row_cache_with_neighbors(
    row_geometry_cache: &mut [Option<CachedRowKey>],
    row: usize,
) {
    if row_geometry_cache.is_empty() {
        return;
    }
    let start = row.saturating_sub(1);
    let end = (row + 1).min(row_geometry_cache.len().saturating_sub(1));
    for cache in &mut row_geometry_cache[start..=end] {
        *cache = None;
    }
}

pub(super) fn row_cursor_key(
    cursor_state: CursorRenderState,
    row: u32,
) -> RowCursorKey {
    match cursor_state {
        CursorRenderState::Hidden => RowCursorKey::None,
        CursorRenderState::Visible { row: r, col, shape } if r == row => match shape {
            CursorShape::Block => RowCursorKey::Block { col },
            CursorShape::Underline => RowCursorKey::Underline { col },
            CursorShape::Beam => RowCursorKey::Beam { col },
        },
        CursorRenderState::Visible { .. } => RowCursorKey::None,
    }
}

pub(in crate::renderer) fn gutter_fill_bg_for_col0(
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    row: u32,
    block_cursor: Option<(u32, u32)>,
    has_background_image: bool,
) -> Option<Srgb<u8>> {
    let block_cursor = if block_cursor == Some((row, 0)) {
        None
    } else {
        block_cursor
    };
    resolve_painted_cell(snap, snap_row, row, 0, block_cursor, has_background_image).fill_bg
}

pub(super) fn row_blink_key(
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    blink_off: bool,
    rapid_blink_off: bool,
) -> RowBlinkKey {
    let mut key = RowBlinkKey::default();
    for attrs in &snap_row.attrs {
        if blink_animation_enabled(snap, *attrs) && attrs.contains(CellAttrs::BLINK) {
            key.blink_off = blink_off;
        }
        if blink_animation_enabled(snap, *attrs) && attrs.contains(CellAttrs::RAPID_BLINK) {
            key.rapid_blink_off = rapid_blink_off;
        }
        if key.blink_off && key.rapid_blink_off {
            break;
        }
    }
    key
}

pub(super) fn row_popup_clip_key(
    row: u32,
    layout: &FrameLayout,
    popup_clip: Option<&ClipRect>,
) -> Option<ClipRectKey> {
    let clip = popup_clip?;
    let row_top = terminal_row_y(row, layout);
    let row_bottom = row_top + layout.cell_h;
    if row_bottom <= clip.top || row_top >= clip.bottom {
        return None;
    }
    Some(ClipRectKey {
        left: clip.left.to_bits(),
        top: clip.top.to_bits(),
        right: clip.right.to_bits(),
        bottom: clip.bottom.to_bits(),
    })
}

pub(super) fn blank_cached_row(
    screen_row: u32,
    cols: u32,
    palette: &ColorPalette,
) -> RowSnapshot {
    let cols = cols as usize;
    RowSnapshot {
        screen_row,
        generation: 0,
        cells: vec![smol_str::SmolStr::new_inline(" "); cols],
        attrs: vec![CellAttrs::default(); cols],
        fg: vec![palette.fg; cols],
        bg: vec![palette.bg; cols],
        underline_color: vec![None; cols],
        has_link: vec![false; cols],
        line_attr: LineAttr::Normal,
        selected: vec![false; cols],
        matched: vec![false; cols],
        active_match: vec![false; cols],
        prompt_start: false,
        exit_status: None,
        block_separator: false,
        sticky_prompt: false,
    }
}

pub(super) fn cached_rows_match_snapshot_shape(
    rows: &[RowSnapshot],
    snap: &TermSnapshot,
) -> bool {
    rows.iter()
        .all(|row| row_paintable_cols(row) == snap.viewport_cols as usize)
}
