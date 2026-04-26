use std::collections::BTreeMap;

use font41::attrs::CellAttrs;
use palette::Srgb;
use smol_str::SmolStr;

use crate::Viewport;
use crate::image::PlacedImage;
use crate::image::clear_anchored_cells;
use crate::screen::grid::AttrChangeExtent;
use crate::screen::grid::Grid;

#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_rect(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
    ch: SmolStr,
    fg: Srgb<u8>,
    bg: Srgb<u8>,
    attrs: CellAttrs,
    underline_color: Option<Srgb<u8>>,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let left = left as usize;
    let right_excl = (right as usize + 1).min(viewport.cols as usize);
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        let row = &mut grid.rows[abs];
        for c in left..right_excl {
            row.cells[c] = ch.clone();
            row.fg[c] = fg;
            row.bg[c] = bg;
            row.attrs[c] = attrs;
            row.underline_color[c] = underline_color;
            row.links[c] = None;
        }
    }
}

pub(crate) fn erase_rect(
    grid: &mut Grid,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let left = left as usize;
    let right_excl = (right as usize + 1).min(viewport.cols as usize);
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        grid.rows[abs].clear_range(left..right_excl, grid.default_fg, grid.default_bg);
    }
    clear_anchored_cells(
        images,
        first_visible + top as usize,
        first_visible + bottom as usize + 1,
        left,
        right_excl,
    );
}

pub(crate) fn erase_rect_selective(
    grid: &mut Grid,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let left = left as usize;
    let right_excl = (right as usize + 1).min(viewport.cols as usize);
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        grid.rows[abs].clear_range_selective(left..right_excl, grid.default_fg, grid.default_bg);
    }
    clear_anchored_cells(
        images,
        first_visible + top as usize,
        first_visible + bottom as usize + 1,
        left,
        right_excl,
    );
}

pub(crate) fn copy_rect(
    grid: &mut Grid,
    src_viewport: &Viewport,
    src_top: u32,
    src_left: u32,
    src_bottom: u32,
    src_right: u32,
    dst_top: u32,
    dst_left: u32,
    dst_viewport: &Viewport,
) {
    let src_first_visible = src_viewport.top_index(grid.rows.len());
    let dst_first_visible = dst_viewport.top_index(grid.rows.len());
    let src_rows = src_viewport.rows as usize;
    let src_cols = src_viewport.cols as usize;
    let dst_rows = dst_viewport.rows as usize;
    let dst_cols = dst_viewport.cols as usize;

    let src_left = src_left as usize;
    let src_right_excl = (src_right as usize + 1).min(src_cols);
    let dst_left = dst_left as usize;

    let snaps: Vec<_> = (src_top..=src_bottom)
        .filter(|&r| (r as usize) < src_rows)
        .map(|r| {
            let abs = src_first_visible + r as usize;
            grid.rows[abs].snap_range(src_left, src_right_excl)
        })
        .collect();

    for (i, snap) in snaps.iter().enumerate() {
        let dst_r = dst_top as usize + i;
        if dst_r >= dst_rows || dst_left >= dst_cols {
            break;
        }
        let abs = dst_first_visible + dst_r;
        grid.rows[abs].paste_range(snap, dst_left);
    }
}

pub(crate) fn change_attrs_rect(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
    sgr_params: &[u16],
    extent: AttrChangeExtent,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    match extent {
        AttrChangeExtent::Rectangle => {
            let left = left as usize;
            let right_excl = (right as usize + 1).min(viewport.cols as usize);
            for r in top..=bottom {
                let abs = first_visible + r as usize;
                grid.rows[abs].apply_attrs_in_range(left, right_excl, sgr_params);
            }
        }
        AttrChangeExtent::Stream => {
            let cols = viewport.cols as usize;
            for r in top..=bottom {
                let abs = first_visible + r as usize;
                let row = &mut grid.rows[abs];
                let start = if r == top { left as usize } else { 0 };
                let end_excl = if r == bottom {
                    (right as usize + 1).min(cols)
                } else {
                    cols
                };
                for c in start..end_excl {
                    if row.has_drawn_cell_at(c) {
                        row.apply_attrs_at(c, sgr_params);
                    }
                }
            }
        }
    }
}

pub(crate) fn reverse_attrs_rect(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
    sgr_params: &[u16],
    extent: AttrChangeExtent,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    match extent {
        AttrChangeExtent::Rectangle => {
            let left = left as usize;
            let right_excl = (right as usize + 1).min(viewport.cols as usize);
            for r in top..=bottom {
                let abs = first_visible + r as usize;
                grid.rows[abs].toggle_attrs_in_range(left, right_excl, sgr_params);
            }
        }
        AttrChangeExtent::Stream => {
            let cols = viewport.cols as usize;
            for r in top..=bottom {
                let abs = first_visible + r as usize;
                let row = &mut grid.rows[abs];
                let start = if r == top { left as usize } else { 0 };
                let end_excl = if r == bottom {
                    (right as usize + 1).min(cols)
                } else {
                    cols
                };
                for c in start..end_excl {
                    if row.has_drawn_cell_at(c) {
                        row.toggle_attrs_at(c, sgr_params);
                    }
                }
            }
        }
    }
}
