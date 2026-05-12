use terminal41::TermSnapshot;

use super::BgVertex;
use super::FgVertex;
use super::FrameLayout;
use super::snapshot_row_y;

#[derive(Clone, Default)]
pub(super) struct BgGeometry {
    pub(super) vertices: Vec<BgVertex>,
    pub(super) indices: Vec<u32>,
}

#[derive(Clone, Default)]
pub(super) struct FgGeometry {
    pub(super) batches: Vec<FgDrawBatch>,
}

#[derive(Clone, Default)]
pub(super) struct FgDrawBatch {
    pub(super) page_index: usize,
    pub(super) vertices: Vec<FgVertex>,
    pub(super) indices: Vec<u32>,
}

pub(super) fn fg_batch_for_page(
    geometry: &mut FgGeometry,
    page_index: usize,
) -> &mut FgDrawBatch {
    if geometry
        .batches
        .last()
        .is_some_and(|batch| batch.page_index == page_index)
    {
        return geometry.batches.last_mut().unwrap();
    }

    geometry.batches.push(FgDrawBatch {
        page_index,
        vertices: Vec::new(),
        indices: Vec::new(),
    });
    geometry.batches.last_mut().unwrap()
}

pub(super) fn push_fg_quad(
    geometry: &mut FgGeometry,
    page_index: usize,
    vertices: [FgVertex; 4],
) {
    let batch = fg_batch_for_page(geometry, page_index);
    let fi = batch.vertices.len() as u32;
    batch.vertices.extend_from_slice(&vertices);
    batch
        .indices
        .extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
}

#[derive(Clone, Default)]
pub(super) struct RowGeometry {
    pub(super) bg: BgGeometry,
    pub(super) fg: FgGeometry,
}

pub(super) struct DirtyLayerRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
}

pub(super) fn append_bg_geometry(
    target_vertices: &mut Vec<BgVertex>,
    target_indices: &mut Vec<u32>,
    source: &BgGeometry,
) {
    let base = target_vertices.len() as u32;
    target_vertices.extend_from_slice(&source.vertices);
    target_indices.extend(source.indices.iter().map(|index| base + *index));
}

pub(super) fn append_fg_geometry(
    target: &mut FgGeometry,
    source: &FgGeometry,
) {
    for source_batch in &source.batches {
        let target_batch = fg_batch_for_page(target, source_batch.page_index);
        let base = target_batch.vertices.len() as u32;
        target_batch
            .vertices
            .extend_from_slice(&source_batch.vertices);
        target_batch
            .indices
            .extend(source_batch.indices.iter().map(|index| base + *index));
    }
}

pub(super) fn append_cached_row_geometry(
    target: &mut RenderGeometry,
    row: &RowGeometry,
) {
    append_bg_geometry(
        &mut target.terminal_bg_vertices,
        &mut target.terminal_bg_indices,
        &row.bg,
    );
    append_fg_geometry(&mut target.terminal_fg, &row.fg);
}

pub(super) fn push_terminal_dirty_rect(
    geometry: &mut RenderGeometry,
    snap: &TermSnapshot,
    row: u32,
    layout: &FrameLayout,
    surface_width: u32,
    surface_height: u32,
) {
    let y = snapshot_row_y(row, snap, layout);
    let top = y.max(0.0);
    let bottom = (y + layout.cell_h).min(surface_height as f32);
    let h = (bottom - top).max(0.0);
    if h <= 0.0 {
        return;
    }
    geometry.terminal_dirty_rects.push(DirtyLayerRect {
        x: 0.0,
        y: top,
        w: surface_width as f32,
        h,
    });
}

pub(super) fn push_terminal_area_dirty_rect(
    geometry: &mut RenderGeometry,
    layout: &FrameLayout,
    surface_width: u32,
    surface_height: u32,
) {
    let y = layout.tab_bar_h.max(0.0);
    let h = (surface_height as f32 - y).max(0.0);
    if h <= 0.0 {
        return;
    }
    geometry.terminal_dirty_rects.push(DirtyLayerRect {
        x: 0.0,
        y,
        w: surface_width as f32,
        h,
    });
}

pub(super) fn dirty_rect_clear_geometry(rects: &[DirtyLayerRect]) -> (Vec<BgVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(rects.len() * 4);
    let mut indices = Vec::with_capacity(rects.len() * 6);
    let transparent = 0_u32;
    for rect in rects {
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&[
            BgVertex {
                pos: [rect.x, rect.y],
                color: transparent,
            },
            BgVertex {
                pos: [rect.x + rect.w, rect.y],
                color: transparent,
            },
            BgVertex {
                pos: [rect.x, rect.y + rect.h],
                color: transparent,
            },
            BgVertex {
                pos: [rect.x + rect.w, rect.y + rect.h],
                color: transparent,
            },
        ]);
        indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    }
    (vertices, indices)
}

#[derive(Default)]
pub(super) struct RenderGeometry {
    pub(super) terminal_dirty_rects: Vec<DirtyLayerRect>,
    pub(super) terminal_bg_vertices: Vec<BgVertex>,
    pub(super) terminal_bg_indices: Vec<u32>,
    pub(super) terminal_fg: FgGeometry,
    pub(super) bg_vertices: Vec<BgVertex>,
    pub(super) bg_indices: Vec<u32>,
    pub(super) fg: FgGeometry,
    pub(super) overlay_bg_vertices: Vec<BgVertex>,
    pub(super) overlay_bg_indices: Vec<u32>,
    pub(super) overlay_fg: FgGeometry,
    pub(super) top_overlay_bg_vertices: Vec<BgVertex>,
    pub(super) top_overlay_bg_indices: Vec<u32>,
    pub(super) top_overlay_fg: FgGeometry,
}
