use std::cmp::Ordering;

use terminal41::VisibleImage;

use super::ClipRect;
use super::FrameLayout;
use super::ImageVertex;

#[derive(Default)]
pub(super) struct ImageGeometry {
    pub(super) batches: Vec<ImageDrawBatch>,
}

#[derive(Default)]
pub(super) struct ImageDrawBatch {
    pub(super) page_index: usize,
    pub(super) vertices: Vec<ImageVertex>,
    pub(super) indices: Vec<u32>,
}

#[derive(Clone, Copy)]
pub(super) struct ImageQuad {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) right: f32,
    pub(super) bottom: f32,
    pub(super) u0: f32,
    pub(super) v0: f32,
    pub(super) u1: f32,
    pub(super) v1: f32,
    pub(super) z: f32,
}

pub(super) fn image_batch_for_page(
    geometry: &mut ImageGeometry,
    page_index: usize,
) -> &mut ImageDrawBatch {
    if geometry
        .batches
        .last()
        .is_some_and(|batch| batch.page_index == page_index)
    {
        return geometry.batches.last_mut().unwrap();
    }

    geometry.batches.push(ImageDrawBatch {
        page_index,
        vertices: Vec::new(),
        indices: Vec::new(),
    });
    geometry.batches.last_mut().unwrap()
}

pub(super) fn clip_image_quad(
    quad: ImageQuad,
    clip: ClipRect,
) -> Option<[ImageVertex; 4]> {
    let left = quad.left.max(clip.left);
    let top = quad.top.max(clip.top);
    let right = quad.right.min(clip.right);
    let bottom = quad.bottom.min(clip.bottom);
    if left >= right || top >= bottom || quad.left >= quad.right || quad.top >= quad.bottom {
        return None;
    }

    let u_per_px = (quad.u1 - quad.u0) / (quad.right - quad.left);
    let v_per_px = (quad.v1 - quad.v0) / (quad.bottom - quad.top);
    let u0 = quad.u0 + (left - quad.left) * u_per_px;
    let u1 = quad.u1 - (quad.right - right) * u_per_px;
    let v0 = quad.v0 + (top - quad.top) * v_per_px;
    let v1 = quad.v1 - (quad.bottom - bottom) * v_per_px;

    Some([
        ImageVertex {
            pos: [left, top],
            uv: [u0, v0],
            z: quad.z,
        },
        ImageVertex {
            pos: [right, top],
            uv: [u1, v0],
            z: quad.z,
        },
        ImageVertex {
            pos: [left, bottom],
            uv: [u0, v1],
            z: quad.z,
        },
        ImageVertex {
            pos: [right, bottom],
            uv: [u1, v1],
            z: quad.z,
        },
    ])
}

pub(super) fn image_vertex_z(
    draw_index: usize,
    draw_count: usize,
) -> f32 {
    (draw_index + 1) as f32 / (draw_count + 1).max(2) as f32
}

pub(super) fn image_render_order(
    left: &VisibleImage,
    right: &VisibleImage,
    layout: &FrameLayout,
) -> Ordering {
    left.z_index
        .cmp(&right.z_index)
        .then_with(|| image_page_y(left, layout).total_cmp(&image_page_y(right, layout)))
        .then_with(|| image_page_x(left, layout).total_cmp(&image_page_x(right, layout)))
        .then_with(|| {
            left.kitty_image_id
                .unwrap_or(u32::MAX)
                .cmp(&right.kitty_image_id.unwrap_or(u32::MAX))
        })
        .then_with(|| left.id.cmp(&right.id))
}

pub(super) fn image_page_y(
    image: &VisibleImage,
    layout: &FrameLayout,
) -> f32 {
    image.screen_row as f32 * layout.cell_h
        + layout.terminal_y_offset
        + layout.block_y_offset
        + image.cell_y_offset as f32
}

pub(super) fn image_page_x(
    image: &VisibleImage,
    layout: &FrameLayout,
) -> f32 {
    image.screen_col as f32 * layout.cell_w + image.cell_x_offset as f32
}
