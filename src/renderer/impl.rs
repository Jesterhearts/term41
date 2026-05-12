use std::cmp::Ordering;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use config41::ColorPalette;
use config41::CursorShape;
use config41::PowerPreference;
use config41::VSync;
use font41::FontSystem;
use font41::attrs::CellAttrs;
use palette::Srgb;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use smol_str::ToSmolStr;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use terminal41::VisibleImage;
use unicode_segmentation::UnicodeSegmentation;
use wgpu::TextureFormat;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

use crate::APP_START_TIME;
use crate::renderer::GUTTER_MENU_ITEMS;
use crate::renderer::GutterPopup;
use crate::renderer::POPUP_WIDTH_CELLS;
use crate::renderer::background;
use crate::renderer::background::Background;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::glyph_atlas::GlyphSlot;
use crate::renderer::gutter_popup_origin;
use crate::renderer::image_atlas::ImageAtlas;
use crate::renderer::paint::blink_animation_enabled;
use crate::renderer::paint::build_tab_bar_plan;
use crate::renderer::paint::centered_ink_origin_x;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::row_paintable_cols;
use crate::renderer::paint::status_line_label_row;
use crate::renderer::paint::underline_style_for_render;
use crate::renderer::paint::visible_row_cols;
use crate::window_host::CommandEditorPopupSide;
use crate::window_host::command_editor_placement_for_cursor;
use crate::window_host::command_editor_popup_side_for_row;

mod chrome;
mod frame;
mod pipelines;
mod text;

#[cfg(test)]
mod tests;

use pipelines::BgImagePipeline;
use pipelines::BgPipeline;
use pipelines::FgPipeline;
use pipelines::ImagePipeline;
use pipelines::LayerPipeline;
use pipelines::build_pipeline_for_format;
pub(crate) use text::blend;
pub(crate) use text::collect_row_glyphs;
pub(crate) use text::drcs_geometry_class;
pub(crate) use text::resolve_cell_colors;

pub const MAX_TAB_WIDTH: f32 = 30.0;
pub const SUCCESS: [u8; 3] = [80, 200, 120];
pub const FAILURE: [u8; 3] = [220, 80, 80];
pub const RUNNING: [u8; 3] = [140, 140, 140];
const IMAGE_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Packed vertex for background quads: position + color.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BgVertex {
    pos: [f32; 2],
    color: u32,
}

/// Packed vertex for foreground (glyph) quads: position + UV + color + flags.
/// `flags & 1` selects the color-glyph shader path (sample atlas RGBA as-is
/// instead of tinting it by `color`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FgVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: u32,
    flags: u32,
}

#[derive(Clone, Copy)]
struct LabelGlyph {
    slot: GlyphSlot,
    col: u16,
    x_offset: f32,
    y_offset: f32,
}

/// Packed vertex for image quads: position + UV.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    z: f32,
}

fn pack_color(
    c: &Srgb<u8>,
    alpha: u8,
) -> u32 {
    u32::from_be_bytes([c.red, c.green, c.blue, alpha])
}

fn label_ink_bounds(
    glyphs: &[LabelGlyph],
    cell_w: f32,
) -> Option<(f32, f32)> {
    let mut left = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;

    for glyph in glyphs {
        let glyph_left = glyph.col as f32 * cell_w + glyph.slot.bearing_x as f32 + glyph.x_offset;
        let glyph_right = glyph_left + glyph.slot.width() as f32;
        left = left.min(glyph_left);
        right = right.max(glyph_right);
    }

    left.is_finite().then_some((left, right))
}

fn label_ink_y_bounds(
    glyphs: &[LabelGlyph],
    baseline: f32,
) -> Option<(f32, f32)> {
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;

    for glyph in glyphs {
        let glyph_top = baseline - glyph.slot.bearing_y as f32 - glyph.y_offset;
        let glyph_bottom = glyph_top + glyph.slot.height() as f32;
        top = top.min(glyph_top);
        bottom = bottom.max(glyph_bottom);
    }

    top.is_finite().then_some((top, bottom))
}

fn fitted_ink_origin_y(
    origin_y: f32,
    region_h: f32,
    ink_top: f32,
    ink_bottom: f32,
) -> f32 {
    const EDGE_INSET: f32 = 1.0;

    if region_h <= EDGE_INSET * 2.0 || ink_bottom <= ink_top {
        return origin_y;
    }

    let target_top = EDGE_INSET;
    let target_bottom = region_h - EDGE_INSET;
    let mut offset = 0.0;

    if ink_top < target_top {
        offset = target_top - ink_top;
    }
    if ink_bottom + offset > target_bottom {
        offset -= ink_bottom + offset - target_bottom;
    }
    if ink_top + offset < target_top {
        offset = target_top - ink_top;
    }

    origin_y + offset
}

/// Emit background-pass quads for the given underline style. `uy` is the
/// baseline Y position for a single underline; `cell_w` and `cell_h` set
/// the horizontal span and vertical budget for multi-line / patterned
/// styles.
fn push_underline_quads(
    style: CellAttrs,
    x: f32,
    uy: f32,
    cell_w: f32,
    thickness: f32,
    cell_h: f32,
    color: u32,
    verts: &mut Vec<BgVertex>,
    idxs: &mut Vec<u32>,
) {
    for style in style & CellAttrs::UNDERLINE_MASK {
        match style {
            CellAttrs::SINGLE_UNDERLINE => {
                push_rect(x, uy, cell_w, thickness, color, verts, idxs);
            }
            CellAttrs::DOUBLE_UNDERLINE => {
                let gap = thickness;
                push_rect(
                    x,
                    uy - gap - thickness,
                    cell_w,
                    thickness,
                    color,
                    verts,
                    idxs,
                );
                push_rect(x, uy, cell_w, thickness, color, verts, idxs);
            }
            CellAttrs::CURLY_UNDERLINE => {
                // Approximate a sine wave with short line-segment quads. Four
                // segments per cell gives a recognisable wave without bloating the
                // vertex count.
                let segments = 4u32;
                let seg_w = cell_w / segments as f32;
                let amplitude = (cell_h * 0.08).max(1.5);
                for s in 0..segments {
                    let t0 = s as f32 / segments as f32;
                    let t1 = (s + 1) as f32 / segments as f32;
                    let y0 = uy - amplitude * (t0 * std::f32::consts::TAU).sin();
                    let y1 = uy - amplitude * (t1 * std::f32::consts::TAU).sin();
                    let sx = x + s as f32 * seg_w;
                    let (top, bot) = if y0 < y1 {
                        (y0, y1 + thickness)
                    } else {
                        (y1, y0 + thickness)
                    };
                    push_rect(sx, top, seg_w, bot - top, color, verts, idxs);
                }
            }
            CellAttrs::DOTTED_UNDERLINE => {
                // Dots spaced at roughly 2× thickness apart.
                let dot_size = thickness.max(1.0);
                let gap = dot_size * 2.0;
                let mut dx = x;
                while dx + dot_size <= x + cell_w {
                    push_rect(dx, uy, dot_size, thickness, color, verts, idxs);
                    dx += gap;
                }
            }
            CellAttrs::DASHED_UNDERLINE => {
                // Three dashes per cell.
                let dash_w = cell_w / 5.0;
                let gap = dash_w;
                let mut dx = x;
                while dx + dash_w <= x + cell_w {
                    push_rect(dx, uy, dash_w, thickness, color, verts, idxs);
                    dx += dash_w + gap;
                }
            }
            _ => {
                unreachable!("unexpected underline style bit set: {style:?}");
            }
        }
    }
}

/// Push a single axis-aligned rectangle into the background vertex/index
/// buffers.
fn push_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
    verts: &mut Vec<BgVertex>,
    idxs: &mut Vec<u32>,
) {
    let bi = verts.len() as u32;
    verts.extend_from_slice(&[
        BgVertex { pos: [x, y], color },
        BgVertex {
            pos: [x + w, y],
            color,
        },
        BgVertex {
            pos: [x, y + h],
            color,
        },
        BgVertex {
            pos: [x + w, y + h],
            color,
        },
    ]);
    idxs.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
}

/// Lightweight snapshot of tab state for the renderer. Built by the host
/// each frame so the renderer doesn't couple to the `App` struct.
pub struct TabInfo<'s> {
    pub label: &'s str,
    pub active: bool,
}

/// CSD window control state passed to the renderer each frame.
pub struct WindowControls {
    /// Which button the mouse is hovering, if any.
    pub hovered: Option<crate::renderer::TabBarHover>,
    /// Whether the window is currently maximized (affects the maximize icon).
    pub maximized: bool,
    /// Tab context menu state, if open: (x position, hovered item index).
    pub tab_menu: Option<(f32, Option<usize>)>,
}

struct FrameLayout {
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    gutter_px: f32,
    tab_bar_h: f32,
    terminal_y_offset: f32,
    block_y_offset: f32,
}

#[derive(Clone, Copy)]
struct ClipRect {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

#[derive(Default)]
struct ImageGeometry {
    batches: Vec<ImageDrawBatch>,
}

#[derive(Default)]
struct ImageDrawBatch {
    page_index: usize,
    vertices: Vec<ImageVertex>,
    indices: Vec<u32>,
}

#[derive(Clone, Copy)]
struct ImageQuad {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    z: f32,
}

fn image_batch_for_page(
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

fn clip_image_quad(
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

fn image_vertex_z(
    draw_index: usize,
    draw_count: usize,
) -> f32 {
    (draw_index + 1) as f32 / (draw_count + 1).max(2) as f32
}

fn image_render_order(
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

fn image_page_y(
    image: &VisibleImage,
    layout: &FrameLayout,
) -> f32 {
    image.screen_row as f32 * layout.cell_h
        + layout.terminal_y_offset
        + layout.block_y_offset
        + image.cell_y_offset as f32
}

fn terminal_row_y(
    row: u32,
    layout: &FrameLayout,
) -> f32 {
    row as f32 * layout.cell_h + layout.tab_bar_h + layout.terminal_y_offset + layout.block_y_offset
}

fn snapshot_row_y(
    row: u32,
    snap: &TermSnapshot,
    layout: &FrameLayout,
) -> f32 {
    let terminal_offset =
        if snap.status_line_row == Some(row) || sticky_prompt_row_at_top(row, snap) {
            0.0
        } else {
            layout.terminal_y_offset + layout.block_y_offset
        };
    row as f32 * layout.cell_h + layout.tab_bar_h + terminal_offset
}

fn sticky_prompt_row_at_top(
    row: u32,
    snap: &TermSnapshot,
) -> bool {
    row == 0
        && snap
            .rows
            .iter()
            .any(|snap_row| snap_row.screen_row == 0 && snap_row.sticky_prompt)
}

fn row_hidden_by_sticky_prompt(
    snap_row: &RowSnapshot,
    snap: &TermSnapshot,
    layout: &FrameLayout,
) -> bool {
    if snap_row.sticky_prompt || snap.status_line_row == Some(snap_row.screen_row) {
        return false;
    }
    if !sticky_prompt_row_at_top(0, snap) {
        return false;
    }

    let sticky_top = layout.tab_bar_h;
    let sticky_bottom = sticky_top + layout.cell_h;
    let row_top = snapshot_row_y(snap_row.screen_row, snap, layout);
    let row_bottom = row_top + layout.cell_h;

    row_top < sticky_bottom && row_bottom > sticky_top
}

fn row_suspended_by_terminal_area(
    snap_row: &RowSnapshot,
    snap: &TermSnapshot,
    suspend_terminal_area: bool,
) -> bool {
    suspend_terminal_area && snap.status_line_row != Some(snap_row.screen_row)
}

fn image_page_x(
    image: &VisibleImage,
    layout: &FrameLayout,
) -> f32 {
    image.screen_col as f32 * layout.cell_w + image.cell_x_offset as f32
}

#[derive(Clone, Default)]
struct BgGeometry {
    vertices: Vec<BgVertex>,
    indices: Vec<u32>,
}

#[derive(Clone, Default)]
struct FgGeometry {
    batches: Vec<FgDrawBatch>,
}

#[derive(Clone, Default)]
struct FgDrawBatch {
    page_index: usize,
    vertices: Vec<FgVertex>,
    indices: Vec<u32>,
}

fn fg_batch_for_page(
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

fn push_fg_quad(
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
struct RowGeometry {
    bg: BgGeometry,
    fg: FgGeometry,
}

struct CachedRowKey {
    key: RowRenderKey,
}

struct DirtyLayerRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowRenderKey {
    layout: RowLayoutKey,
    cursor: RowCursorKey,
    blink: RowBlinkKey,
    gutter_marker: RowGutterMarkerKey,
    popup_clip: Option<ClipRectKey>,
    background_present: bool,
    screen_reverse: bool,
    bg_alpha: u8,
    viewport_cols: u32,
    total_rows: u32,
    drcs_generation: usize,
    font_generation: u64,
    glyph_atlas_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowLayoutKey {
    cell_w: u32,
    cell_h: u32,
    baseline: u32,
    gutter_px: u32,
    tab_bar_h: u32,
    terminal_y_offset: u32,
    block_y_offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowCursorKey {
    None,
    Block { col: u32 },
    Underline { col: u32 },
    Beam { col: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RowBlinkKey {
    blink_off: bool,
    rapid_blink_off: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowGutterMarkerKey {
    prompt_start: bool,
    exit_status: Option<i32>,
    block_separator: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClipRectKey {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PageDrawRange {
    page_index: usize,
    index_start: u32,
    index_count: u32,
    vertex_base: i32,
}

#[derive(Default)]
struct UploadBuffer {
    buffer: Option<wgpu::Buffer>,
    capacity: u64,
}

impl UploadBuffer {
    fn write<T: bytemuck::Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        usage: wgpu::BufferUsages,
        data: &[T],
    ) -> bool {
        let bytes = bytemuck::cast_slice(data);
        if bytes.is_empty() {
            return false;
        }

        let needed = bytes.len() as u64;
        if self.capacity < needed {
            self.capacity = next_upload_buffer_size(needed);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: self.capacity,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        if let Some(buffer) = &self.buffer {
            queue.write_buffer(buffer, 0, bytes);
            true
        } else {
            false
        }
    }

    fn buffer(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }
}

fn next_upload_buffer_size(needed: u64) -> u64 {
    needed.next_power_of_two().max(4096)
}

#[derive(Default)]
struct GeometryUpload {
    vertex_buffer: UploadBuffer,
    index_buffer: UploadBuffer,
    has_indices: bool,
    index_count: u32,
}

impl GeometryUpload {
    fn upload<V: bytemuck::Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_label: &'static str,
        index_label: &'static str,
        vertices: &[V],
        indices: &[u32],
    ) {
        self.vertex_buffer.write(
            device,
            queue,
            vertex_label,
            wgpu::BufferUsages::VERTEX,
            vertices,
        );
        self.has_indices = self.index_buffer.write(
            device,
            queue,
            index_label,
            wgpu::BufferUsages::INDEX,
            indices,
        );
        self.index_count = indices.len() as u32;
    }
}

struct PageGeometryUpload<V> {
    vertices: Vec<V>,
    indices: Vec<u32>,
    ranges: Vec<PageDrawRange>,
    vertex_buffer: UploadBuffer,
    index_buffer: UploadBuffer,
    has_vertices: bool,
    has_indices: bool,
}

impl<V> Default for PageGeometryUpload<V> {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            ranges: Vec::new(),
            vertex_buffer: UploadBuffer::default(),
            index_buffer: UploadBuffer::default(),
            has_vertices: false,
            has_indices: false,
        }
    }
}

impl<V: bytemuck::Pod + Copy> PageGeometryUpload<V> {
    fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.ranges.clear();
        self.has_vertices = false;
        self.has_indices = false;
    }

    fn push_batch(
        &mut self,
        page_index: usize,
        vertices: &[V],
        indices: &[u32],
    ) {
        if indices.is_empty() {
            return;
        }

        let vertex_base = self.vertices.len() as i32;
        let index_start = self.indices.len() as u32;
        self.vertices.extend_from_slice(vertices);
        self.indices.extend_from_slice(indices);
        self.ranges.push(PageDrawRange {
            page_index,
            index_start,
            index_count: indices.len() as u32,
            vertex_base,
        });
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_label: &'static str,
        index_label: &'static str,
    ) {
        self.has_vertices = self.vertex_buffer.write(
            device,
            queue,
            vertex_label,
            wgpu::BufferUsages::VERTEX,
            &self.vertices,
        );
        self.has_indices = self.index_buffer.write(
            device,
            queue,
            index_label,
            wgpu::BufferUsages::INDEX,
            &self.indices,
        );
    }

    fn is_drawable(&self) -> bool {
        self.has_vertices && self.has_indices && !self.ranges.is_empty()
    }
}

fn upload_fg_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload: &mut PageGeometryUpload<FgVertex>,
    geometry: &FgGeometry,
) {
    upload.clear();
    for batch in &geometry.batches {
        upload.push_batch(batch.page_index, &batch.vertices, &batch.indices);
    }
    upload.upload(device, queue, "fg_verts", "fg_idx");
}

fn upload_image_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload: &mut PageGeometryUpload<ImageVertex>,
    geometry: &ImageGeometry,
) {
    upload.clear();
    for batch in &geometry.batches {
        upload.push_batch(batch.page_index, &batch.vertices, &batch.indices);
    }
    upload.upload(device, queue, "img_verts", "img_idx");
}

struct TerminalLayer {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    format: TextureFormat,
    needs_full_repaint: bool,
}

impl TerminalLayer {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        format: TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let texture = create_terminal_layer_texture(device, format, width, height);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal_layer_bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let (vertex_buffer, index_buffer) = create_terminal_layer_quad(device, width, height);

        Self {
            _texture: texture,
            view,
            bind_group,
            vertex_buffer,
            index_buffer,
            width,
            height,
            format,
            needs_full_repaint: true,
        }
    }

    fn resize(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) {
        if self.width == width && self.height == height {
            return;
        }
        *self = Self::new(device, layout, self.format, width, height);
    }
}

struct ImageDepthLayer {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl ImageDepthLayer {
    fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image_depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: IMAGE_DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            _texture: texture,
            view,
            width,
            height,
        }
    }

    fn resize(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) {
        if self.width == width && self.height == height {
            return;
        }
        *self = Self::new(device, width, height);
    }
}

fn create_terminal_layer_texture(
    device: &wgpu::Device,
    format: TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terminal_layer"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn create_terminal_layer_quad(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Buffer, wgpu::Buffer) {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    let vertices = [
        ImageVertex {
            pos: [0.0, 0.0],
            uv: [0.0, 0.0],
            z: 0.0,
        },
        ImageVertex {
            pos: [w, 0.0],
            uv: [1.0, 0.0],
            z: 0.0,
        },
        ImageVertex {
            pos: [0.0, h],
            uv: [0.0, 1.0],
            z: 0.0,
        },
        ImageVertex {
            pos: [w, h],
            uv: [1.0, 1.0],
            z: 0.0,
        },
    ];
    let indices = [0_u32, 1, 2, 2, 1, 3];
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terminal_layer_quad_verts"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terminal_layer_quad_idx"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    (vertex_buffer, index_buffer)
}

#[derive(Default)]
struct RendererUploads {
    terminal_clear: GeometryUpload,
    terminal_bg: GeometryUpload,
    bg: GeometryUpload,
    overlay_bg: GeometryUpload,
    top_overlay_bg: GeometryUpload,
    terminal_fg: PageGeometryUpload<FgVertex>,
    fg: PageGeometryUpload<FgVertex>,
    overlay_fg: PageGeometryUpload<FgVertex>,
    top_overlay_fg: PageGeometryUpload<FgVertex>,
    under_image: PageGeometryUpload<ImageVertex>,
    over_image: PageGeometryUpload<ImageVertex>,
}

fn append_bg_geometry(
    target_vertices: &mut Vec<BgVertex>,
    target_indices: &mut Vec<u32>,
    source: &BgGeometry,
) {
    let base = target_vertices.len() as u32;
    target_vertices.extend_from_slice(&source.vertices);
    target_indices.extend(source.indices.iter().map(|index| base + *index));
}

fn append_fg_geometry(
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

fn append_cached_row_geometry(
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

fn push_terminal_dirty_rect(
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

fn push_terminal_area_dirty_rect(
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

fn dirty_rect_clear_geometry(rects: &[DirtyLayerRect]) -> (Vec<BgVertex>, Vec<u32>) {
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

fn invalidate_row_cache_with_neighbors(
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

fn row_cursor_key(
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

fn gutter_fill_bg_for_col0(
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

fn row_blink_key(
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

fn row_popup_clip_key(
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

#[derive(Default)]
struct RenderGeometry {
    terminal_dirty_rects: Vec<DirtyLayerRect>,
    terminal_bg_vertices: Vec<BgVertex>,
    terminal_bg_indices: Vec<u32>,
    terminal_fg: FgGeometry,
    bg_vertices: Vec<BgVertex>,
    bg_indices: Vec<u32>,
    fg: FgGeometry,
    overlay_bg_vertices: Vec<BgVertex>,
    overlay_bg_indices: Vec<u32>,
    overlay_fg: FgGeometry,
    top_overlay_bg_vertices: Vec<BgVertex>,
    top_overlay_bg_indices: Vec<u32>,
    top_overlay_fg: FgGeometry,
}

fn blank_cached_row(
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

fn cached_rows_match_snapshot_shape(
    rows: &[RowSnapshot],
    snap: &TermSnapshot,
) -> bool {
    rows.iter()
        .all(|row| row_paintable_cols(row) == snap.viewport_cols as usize)
}

fn terminal_block_y_offset_rows(
    rows: &[RowSnapshot],
    snap: &TermSnapshot,
) -> u32 {
    if snap.on_alt_screen || snap.viewport_offset != 0 {
        return 0;
    }
    let terminal_row_count = rows
        .iter()
        .filter(|row| snap.status_line_row != Some(row.screen_row))
        .filter(|row| row.screen_row < snap.viewport_rows)
        .count();
    if terminal_row_count >= snap.viewport_rows as usize {
        return 0;
    }
    let row_content = rows
        .iter()
        .filter(|row| snap.status_line_row != Some(row.screen_row))
        .filter(|row| row.screen_row < snap.viewport_rows)
        .filter(|row| row_has_rendered_content(row))
        .map(|row| row.screen_row + 1)
        .max()
        .unwrap_or(0);
    let cursor_content = snap.cursor.map_or(
        0,
        |(row, _)| {
            if row < snap.viewport_rows { row + 1 } else { 0 }
        },
    );
    let content_rows = row_content.max(cursor_content);
    if content_rows == 0 {
        return 0;
    }
    snap.viewport_rows.saturating_sub(content_rows)
}

fn row_has_rendered_content(row: &RowSnapshot) -> bool {
    row.block_separator
        || row.cells.iter().any(|cell| cell != " ")
        || row.has_link.iter().any(|&v| v)
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,

    bg_pipeline: wgpu::RenderPipeline,
    bg_image_pipeline: wgpu::RenderPipeline,
    fg_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    layer_pipeline: wgpu::RenderPipeline,

    screen_size_buffer: wgpu::Buffer,
    screen_size_bind_group: wgpu::BindGroup,

    glyph_atlas: GlyphAtlas,
    image_atlas: ImageAtlas,

    /// Bind group layout for the background image's texture + sampler. The
    /// pipeline references it; cached here so reloading a different image
    /// (config hot-reload) can build a fresh `Background` without
    /// re-deriving the layout.
    bg_image_layout: wgpu::BindGroupLayout,
    /// Currently loaded background image, if any. `None` when the user
    /// hasn't set `background_image` (or the load failed).
    background: Option<Background>,

    bg_alpha: u8,

    /// When the current visual bell flash started, if one is in progress.
    /// Cleared back to `None` once the flash is past its fade-out window;
    /// `notify_bell` re-arms it.
    bell_started: Option<Instant>,

    /// Show the OSC 133 shell-integration gutter on the left edge. When
    /// `true`, a thin strip reserves pixels to the left of col 0 and
    /// [`Self::gutter_width_px`] returns the actual width (derived from
    /// the current cell metrics); when `false` the gutter is fully
    /// collapsed and every caller gets `0`.
    gutter_enabled: bool,

    /// Materialized terminal rows reconstructed from dirty row snapshots.
    terminal_rows: Vec<RowSnapshot>,
    terminal_row_generations: Vec<u64>,
    terminal_block_y_offset_rows: u32,
    row_geometry_cache: Vec<Option<CachedRowKey>>,
    terminal_layer: TerminalLayer,
    image_depth: ImageDepthLayer,
    uploads: RendererUploads,
}

pub struct PreparedRenderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    screen_size_buffer: wgpu::Buffer,
    screen_size_bind_group: wgpu::BindGroup,
    screen_size_layout: wgpu::BindGroupLayout,
    bg_image_layout: wgpu::BindGroupLayout,
    background: Option<Background>,
    glyph_atlas: GlyphAtlas,
    image_atlas: ImageAtlas,
    pipelines: HashMap<
        TextureFormat,
        (
            FgPipeline,
            BgPipeline,
            ImagePipeline,
            BgImagePipeline,
            LayerPipeline,
        ),
    >,
}

/// Half-period of the cursor blink. xterm uses 530ms by default; 500 lands
/// just shy of that and is the common choice for newer terminals.
pub(crate) const CURSOR_BLINK_HALF_PERIOD: Duration = Duration::from_millis(500);

#[cfg(feature = "vulkan")]
fn pipeline_cache_path(format: TextureFormat) -> Option<PathBuf> {
    let format = match format {
        TextureFormat::Bgra8Unorm => "bgra8unorm",
        TextureFormat::Rgba8Unorm => "rgba8unorm",
        _ => return None,
    };

    dirs::cache_dir().map(|d| {
        d.join("term41")
            .join(format!("pipeline_cache_{}.bin", format))
    })
}

/// Load a pipeline cache from disk, falling back to an empty cache when the
/// file is missing or the data is rejected by the driver. The cache only has
/// an effect on backends that support it (Vulkan); on GL the driver ignores
/// the data and `get_data()` returns `None`.
#[cfg(feature = "vulkan")]
fn load_pipeline_cache(
    device: &wgpu::Device,
    format: TextureFormat,
) -> wgpu::PipelineCache {
    let data = pipeline_cache_path(format).and_then(|p| std::fs::read(p).ok());
    if data.is_none() {
        info!("no pipeline cache found");
    }

    // SAFETY: this cache path is written only from `PipelineCache::get_data`
    // below, and read back as an optional `data` payload for the same wgpu
    // API. Persisted cache files can be stale, corrupted, or from an
    // incompatible adapter after filesystem tampering, driver changes, or
    // upgrades; wgpu validates the payload against the adapter's pipeline
    // cache key before handing it to the backend, and `fallback: true` tells
    // wgpu to create an empty cache when validation rejects unavoidable stale
    // data. This matches wgpu's documented on-disk cache usage pattern.
    unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("pipeline_cache"),
            data: data.as_deref(),
            fallback: true,
        })
    }
}

/// How long the visual bell stays on screen, fading out linearly.
const BELL_FLASH_DURATION: Duration = Duration::from_millis(150);

/// Peak alpha of the bell flash overlay (0–255). Chosen so the flash is
/// noticeable on dark themes without being eye-searing on light ones.
const BELL_FLASH_PEAK_ALPHA: f32 = 80.0;

impl Renderer {
    pub async fn prepare(
        display: OwnedDisplayHandle,
        power_preference: PowerPreference,
        background_image: Option<PathBuf>,
        background_opacity: f32,
        startup_snapshot_size: (u32, u32),
        size: PhysicalSize<u32>,
    ) -> PreparedRenderer {
        let instance = tracing::debug_span!("create_instance").in_scope(|| {
            let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display));
            #[cfg(not(feature = "vulkan"))]
            {
                desc.backends = wgpu::Backends::GL;
            }
            #[cfg(feature = "vulkan")]
            {
                desc.backends = wgpu::Backends::VULKAN;
            }

            wgpu::Instance::new(desc)
        });

        let adapter = {
            let _s = tracing::debug_span!("request_adapter").entered();
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: None,
                    power_preference: match power_preference {
                        PowerPreference::Auto => wgpu::PowerPreference::None,
                        PowerPreference::LowPower => wgpu::PowerPreference::LowPower,
                        PowerPreference::HighPerformance => wgpu::PowerPreference::HighPerformance,
                    },
                    ..Default::default()
                })
                .await
                .expect("request adapter")
        };

        let (device, queue) = {
            let _s = tracing::debug_span!("request_device").entered();

            let descriptor = cfg_select! {
                feature = "vulkan" => {
                    wgpu::DeviceDescriptor {
                    required_features: wgpu::Features::PIPELINE_CACHE,
                        ..Default::default()
                    }
                },
                _ => wgpu::DeviceDescriptor::default()
            };

            adapter
                .request_device(&descriptor)
                .await
                .expect("request device")
        };

        // Screen size uniform (shared by all pipelines).
        let screen_size_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screen_size"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let screen_size_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("screen_size_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                }],
            });

        let screen_size_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("screen_size_bg"),
            layout: &screen_size_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_size_buffer.as_entire_binding(),
            }],
        });

        let bg_image_layout = background::bind_group_layout(&device);

        let glyph_atlas = GlyphAtlas::new(&device);
        let image_atlas = ImageAtlas::new(&device);

        let mut pipelines = HashMap::new();
        for format in [TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm] {
            let pipeline_cache: Option<wgpu::PipelineCache> = cfg_select! {
                feature = "vulkan" => {
                    tracing::debug_span!("load_pipeline_cache").in_scope(||Some(load_pipeline_cache(&device, format)))
                }
                _ => None,
            };

            pipelines.insert(
                format,
                build_pipeline_for_format(
                    format,
                    &device,
                    pipeline_cache,
                    &screen_size_layout,
                    &bg_image_layout,
                    &glyph_atlas,
                    &image_atlas,
                ),
            );
        }

        let background = tracing::debug_span!("load_background").in_scope(|| {
            background_image.and_then(|p| {
                Background::load(
                    &device,
                    &queue,
                    &bg_image_layout,
                    p,
                    background_opacity.clamp(0.0, 1.0),
                    (size.width.max(1), size.height.max(1)),
                    startup_snapshot_size,
                )
            })
        });

        PreparedRenderer {
            instance,
            adapter,
            device,
            queue,
            pipelines,
            screen_size_buffer,
            screen_size_bind_group,
            screen_size_layout,
            glyph_atlas,
            image_atlas,
            bg_image_layout,
            background,
        }
    }

    pub fn from_prepared(
        prepared: PreparedRenderer,
        window: Arc<Window>,
        opacity: f32,
        gutter_enabled: bool,
        vsync: VSync,
    ) -> Self {
        let PreparedRenderer {
            instance,
            adapter,
            device,
            queue,
            screen_size_buffer,
            screen_size_bind_group,
            screen_size_layout,
            glyph_atlas,
            image_atlas,
            mut pipelines,
            bg_image_layout,
            background,
        } = prepared;

        let size = window.inner_size();
        let surface = tracing::debug_span!("create_surface")
            .in_scope(|| instance.create_surface(window).expect("create surface"));

        let surface_caps = surface.get_capabilities(&adapter);
        let preferred_formats = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ];
        let surface_format = preferred_formats
            .iter()
            .find(|f| surface_caps.formats.contains(f))
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let transparent = opacity < 1.0;
        let alpha_mode = if transparent {
            let preferred = [
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::PostMultiplied,
                wgpu::CompositeAlphaMode::Inherit,
                wgpu::CompositeAlphaMode::Auto,
            ];
            preferred
                .into_iter()
                .find(|m| surface_caps.alpha_modes.contains(m))
                .unwrap_or(surface_caps.alpha_modes[0])
        } else {
            surface_caps.alpha_modes[0]
        };

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: match vsync {
                VSync::Auto => wgpu::PresentMode::AutoVsync,
                VSync::Fast => wgpu::PresentMode::Mailbox,
                VSync::On => wgpu::PresentMode::AutoVsync,
                VSync::Off => wgpu::PresentMode::AutoNoVsync,
            },
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let (
            FgPipeline(fg_pipeline),
            BgPipeline(bg_pipeline),
            ImagePipeline(image_pipeline),
            BgImagePipeline(bg_image_pipeline),
            LayerPipeline(layer_pipeline),
        ) = if let Some(pipelines) = pipelines.remove(&surface_format) {
            pipelines
        } else {
            warn!(
                "surface format {:?} wasn't in the prepared pipelines; this should be rare.",
                surface_format
            );
            let pipeline_cache: Option<wgpu::PipelineCache> = cfg_select! {
                feature = "vulkan" => {
                    tracing::debug_span!("load_pipeline_cache").in_scope(||Some(load_pipeline_cache(&device, surface_format)))
                }
                _ => None,
            };

            build_pipeline_for_format(
                surface_format,
                &device,
                pipeline_cache,
                &screen_size_layout,
                &bg_image_layout,
                &glyph_atlas,
                &image_atlas,
            )
        };
        let terminal_layer = TerminalLayer::new(
            &device,
            image_atlas.bind_group_layout(),
            surface_format,
            surface_config.width,
            surface_config.height,
        );
        let image_depth =
            ImageDepthLayer::new(&device, surface_config.width, surface_config.height);

        let mut renderer = Self {
            device,
            queue,
            surface,
            surface_config,
            bg_pipeline,
            bg_image_pipeline,
            fg_pipeline,
            image_pipeline,
            layer_pipeline,
            screen_size_buffer,
            screen_size_bind_group,
            glyph_atlas,
            image_atlas,
            bg_image_layout,
            background,
            bg_alpha: (opacity * 255.0) as u8,
            bell_started: None,
            gutter_enabled,
            terminal_rows: Vec::new(),
            terminal_row_generations: Vec::new(),
            terminal_block_y_offset_rows: 0,
            row_geometry_cache: Vec::new(),
            terminal_layer,
            image_depth,
            uploads: RendererUploads::default(),
        };

        renderer.update_screen_size(size);
        if let Some(background) = renderer.background.as_mut() {
            background.resize(&renderer.queue, (size.width, size.height));
        }

        renderer
    }

    pub fn resize(
        &mut self,
        size: PhysicalSize<u32>,
    ) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface.configure(&self.device, &self.surface_config);
        self.update_screen_size(size);
        self.terminal_layer.resize(
            &self.device,
            self.image_atlas.bind_group_layout(),
            size.width,
            size.height,
        );
        self.image_depth
            .resize(&self.device, size.width, size.height);
        self.row_geometry_cache.clear();
        if let Some(background) = self.background.as_mut() {
            background.resize(&self.queue, (size.width, size.height));
        }
    }

    /// Configure whether the shell-integration gutter is drawn. Returns
    /// `true` when the setting actually changed, so the host can decide
    /// whether to push a resize through the grid/pty (changing gutter
    /// visibility shifts how many cells fit in the window).
    pub fn set_gutter_enabled(
        &mut self,
        enabled: bool,
    ) -> bool {
        if self.gutter_enabled == enabled {
            return false;
        }
        self.gutter_enabled = enabled;
        true
    }

    /// Reload the background image to match the supplied `path` and
    /// `opacity`. Cheap when only opacity changed (rewrites four floats);
    /// re-decodes from disk only when the path is actually different.
    /// Logs and proceeds without a background on load failure.
    pub fn set_background(
        &mut self,
        path: Option<&std::path::Path>,
        opacity: f32,
        startup_snapshot_size: (u32, u32),
    ) {
        let opacity = opacity.clamp(0.0, 1.0);
        let window = (
            self.surface_config.width.max(1),
            self.surface_config.height.max(1),
        );
        match (path, self.background.as_mut()) {
            (None, _) => {
                self.background = None;
            }
            (Some(p), Some(bg)) if bg.path() == p => {
                bg.set_dim(&self.queue, opacity);
            }
            (Some(p), _) => {
                self.background = Background::load(
                    &self.device,
                    &self.queue,
                    &self.bg_image_layout,
                    p.to_path_buf(),
                    opacity,
                    window,
                    startup_snapshot_size,
                );
            }
        }
    }

    /// Advance the background's animation clock. No-op for static images
    /// or when no background is loaded. Returns `true` when the background
    /// is animated (caller should keep the render loop ticking instead of
    /// blocking on input idleness).
    pub fn advance_background_frame(&mut self) -> bool {
        let Some(bg) = self.background.as_mut() else {
            return false;
        };
        bg.frame_advance(&self.queue);
        bg.is_animated()
    }

    pub fn has_animated_background(&self) -> bool {
        self.background
            .as_ref()
            .is_some_and(Background::is_animated)
    }

    pub fn visual_bell_active(&self) -> bool {
        self.bell_started
            .is_some_and(|start| start.elapsed() < BELL_FLASH_DURATION)
    }

    /// Width reserved for the gutter at the given cell width, in pixels.
    /// Returns `0` when disabled so callers can add it unconditionally.
    /// Scaled to `cell_width / 3` (min 4px) so the gutter stays
    /// proportional across font-size changes without ever collapsing to
    /// a hairline that wouldn't fit a visible dot.
    pub fn gutter_width_px(
        &self,
        cell_width: u32,
    ) -> u32 {
        if !self.gutter_enabled {
            return 0;
        }
        compute_gutter_width(cell_width)
    }

    fn update_screen_size(
        &self,
        size: PhysicalSize<u32>,
    ) {
        self.queue.write_buffer(
            &self.screen_size_buffer,
            0,
            bytemuck::cast_slice(&[size.width as f32, size.height as f32, 0.0f32, 0.0f32]),
        );
    }

    /// Acquire the next swapchain image. This is where vsync blocks — call
    /// it BEFORE locking the terminal so the lock isn't held during the wait.
    /// Returns `None` on surface errors (reconfigures automatically).
    pub fn acquire_frame(&mut self) -> Option<(wgpu::SurfaceTexture, wgpu::TextureView)> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return None;
            }
            other => {
                error!("surface error: {other:?}");
                return None;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        Some((frame, view))
    }

    pub fn render(
        &mut self,
        acquired: (wgpu::SurfaceTexture, wgpu::TextureView),
        font_system: &mut FontSystem,
        visible_images: &[VisibleImage],
        snap: &TermSnapshot,
        tabs: &[TabInfo],
        new_tab_text: SmolStr,
        controls: &WindowControls,
        gutter_popup: Option<&GutterPopup>,
        recording_popup: Option<&crate::renderer::RecordingPopup>,
        permission_modal: Option<&crate::renderer::PermissionModal>,
        command_palette: Option<&crate::window_host::CommandPaletteView>,
        history_confirmation: Option<&crate::renderer::HistoryConfirmationModal>,
        history_deletion: Option<&crate::window_host::HistoryDeletionView>,
        toast: Option<&crate::renderer::Toast>,
        preedit: Option<&crate::renderer::PreeditState>,
        command_editor: Option<&commands41::CommandLineView>,
        suspend_terminal_area: bool,
    ) {
        let mut layout = self.frame_layout(font_system, tabs);
        let command_editor = visible_command_editor(command_editor, snap);
        let block_y_offset_rows = terminal_block_y_offset_rows(&snap.rows, snap);
        layout.block_y_offset = block_y_offset_rows as f32 * layout.cell_h;
        if command_editor.is_some() {
            let cursor_row = snap
                .cursor
                .map_or(0, |(row, _)| row.saturating_add(block_y_offset_rows));
            let placement = command_editor_placement_for_cursor(cursor_row, snap.viewport_rows);
            layout.terminal_y_offset = -(placement.terminal_row_offset as f32) * layout.cell_h;
        }
        if suspend_terminal_area {
            self.apply_terminal_snapshot_status_row(snap);
        } else {
            self.apply_terminal_snapshot_rows(snap, block_y_offset_rows);
        }
        let terminal_rows = std::mem::take(&mut self.terminal_rows);
        self.image_atlas.begin_frame();
        let under_text_image_geometry = self.build_image_geometry(visible_images, &layout, true);
        let over_text_image_geometry = self.build_image_geometry(visible_images, &layout, false);
        let geometry = self.build_render_geometry(
            font_system,
            snap,
            &terminal_rows,
            tabs,
            new_tab_text,
            controls,
            gutter_popup,
            recording_popup,
            permission_modal,
            command_palette,
            history_confirmation,
            history_deletion,
            toast,
            preedit,
            command_editor,
            &layout,
            suspend_terminal_area,
        );
        self.terminal_rows = terminal_rows;
        self.submit_render_passes(
            acquired,
            geometry,
            under_text_image_geometry,
            over_text_image_geometry,
        );
        self.image_atlas.end_frame();
    }

    /// Clear all cached glyphs so they are re-rasterized at the current
    /// font size. Called when the DPI scale factor changes.
    pub fn reset_glyph_atlas(&mut self) {
        self.glyph_atlas.clear();
        self.row_geometry_cache.clear();
    }

    /// Trigger a visual bell flash. Idempotent within the flash window:
    /// re-arming mid-flash restarts the fade-out from full alpha, which
    /// is the desired behaviour for back-to-back bells (the user sees
    /// each one rather than a single blended pulse).
    pub fn notify_bell(&mut self) {
        self.bell_started = Some(Instant::now());
    }
}

/// Tells the per-cell loops what (if anything) the cursor wants drawn at a
/// given coordinate. Computed once per frame so blink and viewport-offset
/// checks don't repeat for every cell.
#[derive(Debug, Clone, Copy)]
enum CursorRenderState {
    Hidden,
    Visible {
        row: u32,
        col: u32,
        shape: CursorShape,
    },
}

/// Geometry of an underline / beam overlay. Not used for block — that path
/// inverts the cell instead and needs no separate quad.
struct BarOverlay {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
}

impl CursorRenderState {
    fn block_cursor(self) -> Option<(u32, u32)> {
        match self {
            CursorRenderState::Visible {
                row,
                col,
                shape: CursorShape::Block,
            } => Some((row, col)),
            _ => None,
        }
    }

    /// Build a thin overlay quad for underline / beam shapes when this row
    /// holds the cursor cell. Returns `None` for block, hidden, or the
    /// wrong row. `fg_row` is the row's per-cell fg colors so the bar
    /// adopts the cell's text colour and stays visible against any theme.
    fn bar_overlay_at(
        self,
        row: u32,
        fg_row: &[Srgb<u8>],
        cell_w: f32,
        cell_h: f32,
    ) -> Option<BarOverlay> {
        let CursorRenderState::Visible { row: r, col, shape } = self else {
            return None;
        };
        if r != row {
            return None;
        }
        let fg = fg_row.get(col as usize)?;
        let color = pack_color(fg, 255);
        let x0 = col as f32 * cell_w;
        let y0 = row as f32 * cell_h;
        match shape {
            CursorShape::Block => None,
            CursorShape::Underline => {
                // 2-px-ish strip along the bottom; matches xterm's default.
                let h = (cell_h * 0.12).max(2.0);
                Some(BarOverlay {
                    x: x0,
                    y: y0 + cell_h - h,
                    w: cell_w,
                    h,
                    color,
                })
            }
            CursorShape::Beam => {
                let w = (cell_w * 0.12).max(2.0);
                Some(BarOverlay {
                    x: x0,
                    y: y0,
                    w,
                    h: cell_h,
                    color,
                })
            }
        }
    }
}

pub fn compute_gutter_width(cell_width: u32) -> u32 {
    (cell_width / 3).max(12)
}

/// Paint a status bar for a prompt row. It spans most of the row height so the
/// prompt boundary is obvious at a glance, and leaves a small horizontal margin
/// so the coloured column doesn't butt up against col 0 of the text.
///
/// Colors:
///
/// * **Green** — command finished with exit `0`.
/// * **Red** — command finished with a non-zero exit code.
/// * **Gray** — prompt seen but no `D` yet: either the command is still
///   running, the shell doesn't emit `D`, or the command was superseded by the
///   next prompt before D arrived. All three look the same at the terminal
///   layer, so we show one "unknown" colour for all of them.
///
/// Drawn into the cached terminal row layer, not the dynamic frame overlay:
/// marker state changes with row contents, so caching it with the row avoids
/// rebuilding and uploading the whole gutter every output-heavy frame.
fn append_gutter_marker(
    row: &RowSnapshot,
    gutter_px: f32,
    cell_h: f32,
    y: f32,
    geometry: &mut RowGeometry,
) {
    if gutter_px <= 0.0 || !row.prompt_start {
        return;
    }

    // Leave a small horizontal margin on both sides so the bar doesn't
    // touch either the window edge or the first text column.
    let bar_w = (gutter_px * 0.6).max(3.0);
    let bar_x = (gutter_px - bar_w) * 0.5;
    let bar_h = cell_h * 0.9;
    let bar_y = (cell_h - bar_h) * 0.5;
    let color = gutter_marker_color(row.exit_status);
    let y0 = y + bar_y;

    push_rect(
        bar_x,
        y0,
        bar_w,
        bar_h,
        color,
        &mut geometry.bg.vertices,
        &mut geometry.bg.indices,
    );
}

fn gutter_marker_color(exit_status: Option<i32>) -> u32 {
    let rgb = match exit_status {
        Some(0) => SUCCESS,
        Some(_) => FAILURE,
        None => RUNNING,
    };
    u32::from_be_bytes([rgb[0], rgb[1], rgb[2], 255])
}

fn visible_command_editor<'a>(
    command_editor: Option<&'a commands41::CommandLineView>,
    snap: &TermSnapshot,
) -> Option<&'a commands41::CommandLineView> {
    command_editor.filter(|_| {
        !snap.command_editor_hidden
            && !snap.on_alt_screen
            && !snap.search_active
            && snap.viewport_offset == 0
    })
}

fn command_highlight_color(kind: commands41::HighlightKind) -> u32 {
    let rgb = match kind {
        commands41::HighlightKind::Plain => Srgb::new(224, 228, 236),
        commands41::HighlightKind::Command => Srgb::new(132, 210, 255),
        commands41::HighlightKind::Keyword => Srgb::new(255, 196, 112),
        commands41::HighlightKind::Builtin => Srgb::new(140, 230, 170),
        commands41::HighlightKind::String => Srgb::new(232, 214, 128),
        commands41::HighlightKind::Variable => Srgb::new(198, 170, 255),
        commands41::HighlightKind::Operator => Srgb::new(255, 145, 145),
        commands41::HighlightKind::Comment => Srgb::new(128, 140, 156),
    };
    pack_color(&rgb, 255)
}

/// Convert a byte-indexed `(start, end)` range on `text` to a character-index
/// `(start, end)` range, clamped to `visible_len`. winit reports the IME
/// cursor/selection as byte offsets into the preedit string, but the renderer
/// paints one cell per char — so every per-cell overlay needs the char-index
/// form. Byte offsets that fall inside a multi-byte codepoint collapse onto
/// the next char boundary, which is the behaviour every sane IME is after.
fn byte_range_to_char_range(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    visible_len: usize,
) -> (usize, usize) {
    let mut seg_start = visible_len;
    let mut seg_end = visible_len;
    let mut byte_offset = 0usize;
    for (char_idx, ch) in text.chars().enumerate() {
        if byte_offset >= start_byte && seg_start == visible_len {
            seg_start = char_idx;
        }
        if byte_offset >= end_byte {
            seg_end = char_idx;
            break;
        }
        byte_offset += ch.len_utf8();
        if char_idx + 1 >= visible_len {
            seg_end = visible_len;
            break;
        }
    }
    let seg_start = seg_start.min(visible_len);
    let seg_end = seg_end.min(visible_len).max(seg_start);
    (seg_start, seg_end)
}
