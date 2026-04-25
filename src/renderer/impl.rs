use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use font41::FontSystem;
use font41::attrs::CellAttrs;
use palette::Srgb;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use smol_str::ToSmolStr;
use terminal41::ColorPalette;
use terminal41::CursorShape;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use terminal41::VisibleImage;
use unicode_segmentation::UnicodeSegmentation;
use utils41::lerp_u8;
use wgpu::PowerPreference;
use wgpu::TextureFormat;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

use crate::APP_START_TIME;
use crate::config::VSync;
use crate::renderer::GUTTER_MENU_ITEMS;
use crate::renderer::GutterPopup;
use crate::renderer::POPUP_WIDTH_CELLS;
use crate::renderer::background;
use crate::renderer::background::Background;
use crate::renderer::background::BgImageVertex;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::glyph_atlas::GlyphSlot;
use crate::renderer::image_atlas::IMAGE_ATLAS_SIZE;
use crate::renderer::image_atlas::ImageAtlas;
use crate::renderer::paint::blink_animation_enabled;
use crate::renderer::paint::bold_glyph_enabled;
use crate::renderer::paint::build_tab_bar_plan;
use crate::renderer::paint::centered_ink_origin_x;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::status_line_label_row;
use crate::renderer::paint::underline_style_for_render;

pub const MAX_TAB_WIDTH: f32 = 30.0;
pub const SUCCESS: [u8; 3] = [80, 200, 120];
pub const FAILURE: [u8; 3] = [220, 80, 80];
pub const RUNNING: [u8; 3] = [140, 140, 140];

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

/// Linearly interpolate between two sRGB byte colours in component space.
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
    let shaped = font_system.shape_row(&snap_row.cells, &snap_row.attrs);
    let mut collected = Vec::with_capacity(shaped.len());

    for sg in shaped {
        if sg.col as u32 >= visible_cols {
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
        },
        ImageVertex {
            pos: [right, top],
            uv: [u1, v0],
        },
        ImageVertex {
            pos: [left, bottom],
            uv: [u0, v1],
        },
        ImageVertex {
            pos: [right, bottom],
            uv: [u1, v1],
        },
    ])
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
}

impl<V> Default for PageGeometryUpload<V> {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            ranges: Vec::new(),
            vertex_buffer: UploadBuffer::default(),
            index_buffer: UploadBuffer::default(),
        }
    }
}

impl<V: bytemuck::Pod + Copy> PageGeometryUpload<V> {
    fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.ranges.clear();
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
        self.vertex_buffer.write(
            device,
            queue,
            vertex_label,
            wgpu::BufferUsages::VERTEX,
            &self.vertices,
        );
        self.index_buffer.write(
            device,
            queue,
            index_label,
            wgpu::BufferUsages::INDEX,
            &self.indices,
        );
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
        },
        ImageVertex {
            pos: [w, 0.0],
            uv: [1.0, 0.0],
        },
        ImageVertex {
            pos: [0.0, h],
            uv: [0.0, 1.0],
        },
        ImageVertex {
            pos: [w, h],
            uv: [1.0, 1.0],
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
    terminal_fg: PageGeometryUpload<FgVertex>,
    fg: PageGeometryUpload<FgVertex>,
    overlay_fg: PageGeometryUpload<FgVertex>,
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
    row: u32,
    layout: &FrameLayout,
    surface_width: u32,
    surface_height: u32,
) {
    let y = row as f32 * layout.cell_h + layout.tab_bar_h;
    let h = layout.cell_h.min(surface_height as f32 - y).max(0.0);
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
    let row_top = row as f32 * layout.cell_h + layout.tab_bar_h;
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
    }
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
    row_geometry_cache: Vec<Option<CachedRowKey>>,
    terminal_layer: TerminalLayer,
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
                    power_preference,
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
            row_geometry_cache: Vec::new(),
            terminal_layer,
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
        toast: Option<&crate::renderer::Toast>,
        preedit: Option<&crate::renderer::PreeditState>,
    ) {
        let layout = self.frame_layout(font_system, tabs);
        let under_text_image_geometry = self.build_image_geometry(visible_images, &layout, true);
        let over_text_image_geometry = self.build_image_geometry(visible_images, &layout, false);
        self.apply_terminal_snapshot_rows(snap);
        let terminal_rows = std::mem::take(&mut self.terminal_rows);
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
            toast,
            preedit,
            &layout,
        );
        self.terminal_rows = terminal_rows;
        self.submit_render_passes(
            acquired,
            geometry,
            under_text_image_geometry,
            over_text_image_geometry,
        );
    }

    fn apply_terminal_snapshot_rows(
        &mut self,
        snap: &TermSnapshot,
    ) {
        let total_rows = snap.total_rows as usize;
        if snap.reset_cached_rows || self.terminal_rows.len() != total_rows {
            self.terminal_rows = (0..total_rows)
                .map(|row| blank_cached_row(row as u32, snap.viewport_cols, &snap.palette))
                .collect();
            self.terminal_row_generations = vec![u64::MAX; total_rows];
            self.row_geometry_cache.clear();
            self.row_geometry_cache.resize_with(total_rows, || None);
            self.terminal_layer.needs_full_repaint = true;
        } else if self.row_geometry_cache.len() != total_rows {
            self.terminal_row_generations.resize(total_rows, u64::MAX);
            self.row_geometry_cache.clear();
            self.row_geometry_cache.resize_with(total_rows, || None);
            self.terminal_layer.needs_full_repaint = true;
        }

        for row in &snap.rows {
            let idx = row.screen_row as usize;
            if idx >= self.terminal_rows.len() {
                self.terminal_rows.resize_with(idx + 1, || {
                    blank_cached_row(0, snap.viewport_cols, &snap.palette)
                });
                self.terminal_rows[idx].screen_row = idx as u32;
                self.terminal_row_generations.resize(idx + 1, u64::MAX);
                self.row_geometry_cache.resize_with(idx + 1, || None);
            }
            if self
                .terminal_row_generations
                .get(idx)
                .is_some_and(|generation| *generation == row.generation)
            {
                continue;
            }
            self.terminal_rows[idx] = row.clone();
            self.terminal_row_generations[idx] = row.generation;
            invalidate_row_cache_with_neighbors(&mut self.row_geometry_cache, idx);
        }
    }

    fn frame_layout(
        &self,
        font_system: &FontSystem,
        tabs: &[TabInfo],
    ) -> FrameLayout {
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        FrameLayout {
            cell_w,
            cell_h,
            baseline: font_system.baseline_offset(),
            gutter_px: self.gutter_width_px(font_system.cell_width) as f32,
            tab_bar_h: if tabs.is_empty() { 0.0 } else { cell_h },
        }
    }

    fn build_image_geometry(
        &mut self,
        visible_images: &[VisibleImage],
        layout: &FrameLayout,
        under_text: bool,
    ) -> ImageGeometry {
        let mut geometry = ImageGeometry::default();
        let content_clip = ClipRect {
            left: layout.gutter_px,
            top: layout.tab_bar_h,
            right: self.surface_config.width as f32,
            bottom: self.surface_config.height as f32,
        };
        for vis in visible_images {
            if (vis.z_index < 0) != under_text {
                continue;
            }
            let entry = match self.image_atlas.ensure_cached(
                &self.device,
                &self.queue,
                vis.id,
                vis.frame_index,
                &vis.image,
            ) {
                Some(e) => e,
                None => continue,
            };

            let base_x =
                vis.screen_col as f32 * layout.cell_w + layout.gutter_px + vis.cell_x_offset as f32;
            let base_y =
                vis.screen_row as f32 * layout.cell_h + layout.tab_bar_h + vis.cell_y_offset as f32;
            let scale_x = if vis.image.width > 0 {
                vis.display_width as f32 / vis.image.width as f32
            } else {
                1.0
            };
            let scale_y = if vis.image.height > 0 {
                vis.display_height as f32 / vis.image.height as f32
            } else {
                1.0
            };

            for tile in &entry.tiles {
                let a = &tile.alloc;
                let x = base_x + tile.src_x as f32 * scale_x;
                let y = base_y + tile.src_y as f32 * scale_y;
                let w = a.width as f32 * scale_x;
                let h = a.height as f32 * scale_y;
                let u0 = a.x as f32 / IMAGE_ATLAS_SIZE as f32;
                let v0 = a.y as f32 / IMAGE_ATLAS_SIZE as f32;
                let u1 = (a.x + a.width) as f32 / IMAGE_ATLAS_SIZE as f32;
                let v1 = (a.y + a.height) as f32 / IMAGE_ATLAS_SIZE as f32;
                let Some(vertices) = clip_image_quad(
                    ImageQuad {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                        u0,
                        v0,
                        u1,
                        v1,
                    },
                    content_clip,
                ) else {
                    continue;
                };
                let batch = image_batch_for_page(&mut geometry, tile.page_index);
                let ii = batch.vertices.len() as u32;
                batch.vertices.extend_from_slice(&vertices);
                batch
                    .indices
                    .extend_from_slice(&[ii, ii + 1, ii + 2, ii + 2, ii + 1, ii + 3]);
            }
        }
        geometry
    }

    fn build_render_geometry(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        rows: &[RowSnapshot],
        tabs: &[TabInfo],
        new_tab_text: SmolStr,
        controls: &WindowControls,
        gutter_popup: Option<&GutterPopup>,
        recording_popup: Option<&crate::renderer::RecordingPopup>,
        permission_modal: Option<&crate::renderer::PermissionModal>,
        toast: Option<&crate::renderer::Toast>,
        preedit: Option<&crate::renderer::PreeditState>,
        layout: &FrameLayout,
    ) -> RenderGeometry {
        for attempt in 0..2 {
            let glyph_generation = self.glyph_atlas.generation();
            let font_generation = font_system.font_generation();
            let geometry = self.build_render_geometry_once(
                font_system,
                snap,
                rows,
                tabs,
                new_tab_text.clone(),
                controls,
                gutter_popup,
                recording_popup,
                permission_modal,
                toast,
                preedit,
                layout,
            );
            if self.glyph_atlas.generation() == glyph_generation
                && font_system.font_generation() == font_generation
            {
                return geometry;
            }
            self.row_geometry_cache.clear();
            debug!(
                "font/glyph generation changed while building frame geometry; rebuilding \
                 attempt={attempt}"
            );
        }

        self.build_render_geometry_once(
            font_system,
            snap,
            rows,
            tabs,
            new_tab_text,
            controls,
            gutter_popup,
            recording_popup,
            permission_modal,
            toast,
            preedit,
            layout,
        )
    }

    fn build_render_geometry_once(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        rows: &[RowSnapshot],
        tabs: &[TabInfo],
        new_tab_text: SmolStr,
        controls: &WindowControls,
        gutter_popup: Option<&GutterPopup>,
        recording_popup: Option<&crate::renderer::RecordingPopup>,
        permission_modal: Option<&crate::renderer::PermissionModal>,
        toast: Option<&crate::renderer::Toast>,
        preedit: Option<&crate::renderer::PreeditState>,
        layout: &FrameLayout,
    ) -> RenderGeometry {
        let mut geometry = RenderGeometry::default();
        let cursor_state = self.cursor_state_from_snapshot(snap);
        let popup_clip = self.popup_clip(gutter_popup, layout);
        let blink_off = (APP_START_TIME.get().unwrap().elapsed().as_millis() / 500) & 1 == 1;
        let rapid_blink_off = (APP_START_TIME.get().unwrap().elapsed().as_millis() / 250) & 1 == 1;
        let font_generation = font_system.font_generation();

        for snap_row in rows {
            let row = snap_row.screen_row;
            if snap.search_active && row == snap.viewport_rows - 1 {
                push_terminal_dirty_rect(
                    &mut geometry,
                    row,
                    layout,
                    self.surface_config.width,
                    self.surface_config.height,
                );
                if let Some(cache) = self.row_geometry_cache.get_mut(row as usize) {
                    *cache = None;
                }
                continue;
            }
            let cache_key = self.row_render_key(
                snap,
                snap_row,
                row,
                cursor_state,
                popup_clip.as_ref(),
                blink_off,
                rapid_blink_off,
                font_generation,
                layout,
            );
            if let Some(cached) = self
                .row_geometry_cache
                .get(row as usize)
                .and_then(Option::as_ref)
                && cached.key == cache_key
            {
                continue;
            }

            push_terminal_dirty_rect(
                &mut geometry,
                row,
                layout,
                self.surface_config.width,
                self.surface_config.height,
            );
            let mut row_geometry = RowGeometry::default();
            self.append_row_geometry(
                font_system,
                snap,
                snap_row,
                row,
                cursor_state,
                popup_clip.as_ref(),
                blink_off,
                rapid_blink_off,
                layout,
                &mut row_geometry,
            );
            let cache_key = self.row_render_key(
                snap,
                snap_row,
                row,
                cursor_state,
                popup_clip.as_ref(),
                blink_off,
                rapid_blink_off,
                font_generation,
                layout,
            );
            append_cached_row_geometry(&mut geometry, &row_geometry);
            if row as usize >= self.row_geometry_cache.len() {
                self.row_geometry_cache
                    .resize_with(row as usize + 1, || None);
            }
            self.row_geometry_cache[row as usize] = Some(CachedRowKey { key: cache_key });
        }

        self.append_visual_bell_overlay(&mut geometry, snap, layout);

        if layout.gutter_px > 0.0 {
            render_gutter_markers(
                rows,
                layout.gutter_px,
                layout.cell_h,
                layout.tab_bar_h,
                &mut geometry.bg_vertices,
                &mut geometry.bg_indices,
            );
        }

        self.render_status_line_chrome(
            font_system,
            snap,
            layout,
            &mut geometry.bg_vertices,
            &mut geometry.bg_indices,
            &mut geometry.fg,
        );

        self.render_tab_bar(
            font_system,
            tabs,
            &snap.palette,
            new_tab_text,
            controls,
            &mut geometry.bg_vertices,
            &mut geometry.bg_indices,
            &mut geometry.fg,
            &mut geometry.overlay_bg_vertices,
            &mut geometry.overlay_bg_indices,
            &mut geometry.overlay_fg,
        );
        self.render_search_bar(
            font_system,
            snap,
            layout.tab_bar_h,
            &mut geometry.bg_vertices,
            &mut geometry.bg_indices,
            &mut geometry.fg,
        );

        if let Some(popup) = gutter_popup {
            self.render_gutter_popup(
                font_system,
                popup,
                layout.gutter_px,
                layout.cell_w,
                layout.cell_h,
                layout.tab_bar_h,
                &mut geometry.bg_vertices,
                &mut geometry.bg_indices,
                &mut geometry.fg,
            );
        }

        if let Some(popup) = recording_popup {
            self.render_recording_popup(
                font_system,
                popup,
                layout,
                &mut geometry.overlay_bg_vertices,
                &mut geometry.overlay_bg_indices,
                &mut geometry.overlay_fg,
            );
        }

        if let Some(toast) = toast {
            self.render_toast(
                font_system,
                toast,
                layout,
                &mut geometry.overlay_bg_vertices,
                &mut geometry.overlay_bg_indices,
                &mut geometry.overlay_fg,
            );
        }

        if let Some(modal) = permission_modal {
            self.render_permission_modal(
                font_system,
                modal,
                layout,
                &mut geometry.overlay_bg_vertices,
                &mut geometry.overlay_bg_indices,
                &mut geometry.overlay_fg,
            );
        }

        if let Some(preedit) = preedit
            && !snap.search_active
        {
            self.render_preedit(
                font_system,
                snap,
                preedit,
                layout.gutter_px,
                layout.cell_w,
                layout.cell_h,
                layout.baseline,
                layout.tab_bar_h,
                &mut geometry.bg_vertices,
                &mut geometry.bg_indices,
                &mut geometry.fg,
            );
        }

        geometry
    }

    #[allow(clippy::too_many_arguments)]
    fn row_render_key(
        &self,
        snap: &TermSnapshot,
        snap_row: &RowSnapshot,
        row: u32,
        cursor_state: CursorRenderState,
        popup_clip: Option<&ClipRect>,
        blink_off: bool,
        rapid_blink_off: bool,
        font_generation: u64,
        layout: &FrameLayout,
    ) -> RowRenderKey {
        RowRenderKey {
            layout: RowLayoutKey {
                cell_w: layout.cell_w.to_bits(),
                cell_h: layout.cell_h.to_bits(),
                baseline: layout.baseline.to_bits(),
                gutter_px: layout.gutter_px.to_bits(),
                tab_bar_h: layout.tab_bar_h.to_bits(),
            },
            cursor: row_cursor_key(cursor_state, row),
            blink: row_blink_key(snap, snap_row, blink_off, rapid_blink_off),
            popup_clip: row_popup_clip_key(row, layout, popup_clip),
            background_present: self.background.is_some(),
            screen_reverse: snap.screen_reverse,
            bg_alpha: self.bg_alpha,
            viewport_cols: snap.viewport_cols,
            total_rows: snap.total_rows,
            drcs_generation: Arc::as_ptr(&snap.drcs_glyphs) as usize,
            font_generation,
            glyph_atlas_generation: self.glyph_atlas.generation(),
        }
    }

    fn popup_clip(
        &self,
        gutter_popup: Option<&GutterPopup>,
        layout: &FrameLayout,
    ) -> Option<ClipRect> {
        gutter_popup.map(|popup| {
            let header = if popup.duration_text.is_some() { 1 } else { 0 };
            let total = (header + GUTTER_MENU_ITEMS.len()) as f32;
            let width = layout.cell_w * POPUP_WIDTH_CELLS;
            let height = total * layout.cell_h;
            let left = layout.gutter_px;
            let surface_h = self.surface_config.height as f32;
            let top = (popup.screen_row as f32 * layout.cell_h + layout.tab_bar_h)
                .min(surface_h - height)
                .max(layout.tab_bar_h);
            ClipRect {
                left,
                top,
                right: left + width,
                bottom: top + height,
            }
        })
    }

    fn append_row_geometry(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        snap_row: &RowSnapshot,
        row: u32,
        cursor_state: CursorRenderState,
        popup_clip: Option<&ClipRect>,
        blink_off: bool,
        rapid_blink_off: bool,
        layout: &FrameLayout,
        geometry: &mut RowGeometry,
    ) {
        let y = row as f32 * layout.cell_h + layout.tab_bar_h;
        let line_attr = snap_row.line_attr;
        let is_double_wide = !matches!(line_attr, LineAttr::Normal);
        let effective_cell_w = if is_double_wide {
            layout.cell_w * 2.0
        } else {
            layout.cell_w
        };
        let visible_cols = if is_double_wide {
            snap.viewport_cols / 2
        } else {
            snap.viewport_cols
        };

        for col in 0..visible_cols {
            let x = col as f32 * effective_cell_w + layout.gutter_px;
            let block_cursor = cursor_state.block_cursor();
            let painted = resolve_painted_cell(
                snap,
                snap_row,
                row,
                col,
                block_cursor,
                self.background.is_some(),
            );
            let cell_attrs = snap_row.attrs[col as usize];
            if let Some(fill_bg) = painted.fill_bg {
                let bg_color = pack_color(&fill_bg, self.bg_alpha);
                let bi = geometry.bg.vertices.len() as u32;
                geometry.bg.vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [x, y],
                        color: bg_color,
                    },
                    BgVertex {
                        pos: [x + effective_cell_w, y],
                        color: bg_color,
                    },
                    BgVertex {
                        pos: [x, y + layout.cell_h],
                        color: bg_color,
                    },
                    BgVertex {
                        pos: [x + effective_cell_w, y + layout.cell_h],
                        color: bg_color,
                    },
                ]);
                geometry.bg.indices.extend_from_slice(&[
                    bi,
                    bi + 1,
                    bi + 2,
                    bi + 2,
                    bi + 1,
                    bi + 3,
                ]);
            }

            let ul_style = underline_style_for_render(snap, snap_row.attrs[col as usize]);
            let has_link = snap_row.has_link[col as usize];
            let effective_ul =
                if has_link && ul_style & CellAttrs::UNDERLINE_MASK == CellAttrs::empty() {
                    CellAttrs::SINGLE_UNDERLINE
                } else {
                    ul_style
                };
            if effective_ul & CellAttrs::UNDERLINE_MASK != CellAttrs::empty() {
                let ul_rgb = snap_row.underline_color[col as usize].unwrap_or(painted.base_fg);
                let ul_packed = pack_color(&ul_rgb, 255);
                let thickness = (layout.cell_h * 0.06).max(1.0);
                let uy = y + layout.cell_h - thickness;
                push_underline_quads(
                    effective_ul,
                    x,
                    uy,
                    effective_cell_w,
                    thickness,
                    layout.cell_h,
                    ul_packed,
                    &mut geometry.bg.vertices,
                    &mut geometry.bg.indices,
                );
            }

            if cell_attrs.contains(CellAttrs::OVERLINE) {
                let ol_color = pack_color(&painted.base_fg, 255);
                let thickness = (layout.cell_h * 0.06).max(1.0);
                push_rect(
                    x,
                    y,
                    effective_cell_w,
                    thickness,
                    ol_color,
                    &mut geometry.bg.vertices,
                    &mut geometry.bg.indices,
                );
            }

            if cell_attrs.contains(CellAttrs::STRIKETHROUGH) {
                let st_color = pack_color(&painted.base_fg, 255);
                let thickness = (layout.cell_h * 0.06).max(1.0);
                let sy = y + (layout.cell_h - thickness) * 0.5;
                let bi = geometry.bg.vertices.len() as u32;
                geometry.bg.vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [x, sy],
                        color: st_color,
                    },
                    BgVertex {
                        pos: [x + effective_cell_w, sy],
                        color: st_color,
                    },
                    BgVertex {
                        pos: [x, sy + thickness],
                        color: st_color,
                    },
                    BgVertex {
                        pos: [x + effective_cell_w, sy + thickness],
                        color: st_color,
                    },
                ]);
                geometry.bg.indices.extend_from_slice(&[
                    bi,
                    bi + 1,
                    bi + 2,
                    bi + 2,
                    bi + 1,
                    bi + 3,
                ]);
            }
        }

        if let Some(overlay) =
            cursor_state.bar_overlay_at(row, &snap_row.fg, layout.cell_w, layout.cell_h)
        {
            let ox = overlay.x + layout.gutter_px;
            let oy = overlay.y + layout.tab_bar_h;
            let bi = geometry.bg.vertices.len() as u32;
            geometry.bg.vertices.extend_from_slice(&[
                BgVertex {
                    pos: [ox, oy],
                    color: overlay.color,
                },
                BgVertex {
                    pos: [ox + overlay.w, oy],
                    color: overlay.color,
                },
                BgVertex {
                    pos: [ox, oy + overlay.h],
                    color: overlay.color,
                },
                BgVertex {
                    pos: [ox + overlay.w, oy + overlay.h],
                    color: overlay.color,
                },
            ]);
            geometry
                .bg
                .indices
                .extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        self.append_row_glyphs(
            font_system,
            snap,
            snap_row,
            row,
            y,
            line_attr,
            effective_cell_w,
            visible_cols,
            cursor_state,
            popup_clip,
            blink_off,
            rapid_blink_off,
            layout,
            geometry,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn append_row_glyphs(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        snap_row: &RowSnapshot,
        row: u32,
        y: f32,
        line_attr: LineAttr,
        effective_cell_w: f32,
        visible_cols: u32,
        cursor_state: CursorRenderState,
        popup_clip: Option<&ClipRect>,
        blink_off: bool,
        rapid_blink_off: bool,
        layout: &FrameLayout,
        geometry: &mut RowGeometry,
    ) {
        let is_double_wide = !matches!(line_attr, LineAttr::Normal);
        let block_cursor = match cursor_state {
            CursorRenderState::Visible {
                row,
                col,
                shape: CursorShape::Block,
            } => Some((row, col)),
            _ => None,
        };
        let glyphs = collect_row_glyphs(
            font_system,
            snap,
            snap_row,
            row,
            visible_cols,
            block_cursor,
            blink_off,
            rapid_blink_off,
        );

        for glyph in glyphs {
            if let Some(clip) = popup_clip {
                let cx = glyph.col as f32 * effective_cell_w + layout.gutter_px;
                if cx < clip.right
                    && cx + effective_cell_w > clip.left
                    && y < clip.bottom
                    && y + layout.cell_h > clip.top
                {
                    continue;
                }
            }

            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                glyph.font_index,
                glyph.glyph_id,
                glyph.cells_wide,
                glyph.synth_bold,
                drcs_geometry_class(snap).map(|geometry| (geometry, snap.drcs_glyphs.clone())),
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();
            let scale_x = if is_double_wide { 2.0_f32 } else { 1.0 };
            let gx = glyph.col as f32 * effective_cell_w
                + slot.bearing_x as f32 * scale_x
                + glyph.x_offset * scale_x
                + layout.gutter_px;
            let gx = gx.floor();
            let gw = (sw as f32 * scale_x).ceil();

            let is_double_height = matches!(
                line_attr,
                LineAttr::DoubleHeightTop | LineAttr::DoubleHeightBottom
            );
            let (gy, gh, uv_y_top, uv_y_bot) = if is_double_height {
                let y_origin = if matches!(line_attr, LineAttr::DoubleHeightTop) {
                    y
                } else {
                    y - layout.cell_h
                };
                let gy_v =
                    y_origin + (layout.baseline - slot.bearing_y as f32 - glyph.y_offset) * 2.0;
                let gh_v = 2.0 * sh as f32;
                let vis_top = gy_v.max(y);
                let vis_bot = (gy_v + gh_v).min(y + layout.cell_h);
                if vis_bot <= vis_top {
                    continue;
                }
                let uv_top = sy as f32 + sh as f32 * (vis_top - gy_v) / gh_v;
                let uv_bot = sy as f32 + sh as f32 * (vis_bot - gy_v) / gh_v;
                (vis_top, vis_bot - vis_top, uv_top, uv_bot)
            } else {
                let gy = y + layout.baseline - slot.bearing_y as f32 - glyph.y_offset;
                (gy, sh as f32, sy as f32, (sy + sh) as f32)
            };
            let gy = gy.floor();
            let gh = gh.ceil();

            let baseline_y = y + layout.baseline;
            let shear = if glyph.synth_italic { 0.2126_f32 } else { 0.0 };
            let shear_at = |vy: f32| -> f32 { shear * (baseline_y - vy) };
            let fg_color = pack_color(&glyph.fg, 255);
            let flags: u32 = if slot.is_color { 1 } else { 0 };
            push_fg_quad(
                &mut geometry.fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx + shear_at(gy), gy],
                        uv: [sx as f32, uv_y_top],
                        color: fg_color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw + shear_at(gy), gy],
                        uv: [(sx + sw) as f32, uv_y_top],
                        color: fg_color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + shear_at(gy + gh), gy + gh],
                        uv: [sx as f32, uv_y_bot],
                        color: fg_color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw + shear_at(gy + gh), gy + gh],
                        uv: [(sx + sw) as f32, uv_y_bot],
                        color: fg_color,
                        flags,
                    },
                ],
            );
        }
    }

    fn append_visual_bell_overlay(
        &mut self,
        geometry: &mut RenderGeometry,
        _snap: &TermSnapshot,
        _layout: &FrameLayout,
    ) {
        if let Some(start) = self.bell_started {
            let elapsed = start.elapsed();
            if elapsed >= BELL_FLASH_DURATION {
                self.bell_started = None;
            } else {
                let progress = elapsed.as_secs_f32() / BELL_FLASH_DURATION.as_secs_f32();
                let alpha = (BELL_FLASH_PEAK_ALPHA * (1.0 - progress)) as u8;
                let surface_w = self.surface_config.width as f32;
                let surface_h = self.surface_config.height as f32;
                let color = u32::from_be_bytes([255, 255, 255, alpha]);
                let bi = geometry.bg_vertices.len() as u32;
                geometry.bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [0.0, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [surface_w, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [0.0, surface_h],
                        color,
                    },
                    BgVertex {
                        pos: [surface_w, surface_h],
                        color,
                    },
                ]);
                geometry.bg_indices.extend_from_slice(&[
                    bi,
                    bi + 1,
                    bi + 2,
                    bi + 2,
                    bi + 1,
                    bi + 3,
                ]);
            }
        }
    }

    fn upload_render_geometry(
        &mut self,
        geometry: &RenderGeometry,
        under_text_image_geometry: &ImageGeometry,
        over_text_image_geometry: &ImageGeometry,
    ) {
        let device = &self.device;
        let queue = &self.queue;

        let (terminal_clear_vertices, terminal_clear_indices) =
            dirty_rect_clear_geometry(&geometry.terminal_dirty_rects);
        self.uploads.terminal_clear.upload(
            device,
            queue,
            "terminal_clear_verts",
            "terminal_clear_idx",
            &terminal_clear_vertices,
            &terminal_clear_indices,
        );
        self.uploads.terminal_bg.upload(
            device,
            queue,
            "terminal_bg_verts",
            "terminal_bg_idx",
            &geometry.terminal_bg_vertices,
            &geometry.terminal_bg_indices,
        );
        self.uploads.bg.upload(
            device,
            queue,
            "bg_verts",
            "bg_idx",
            &geometry.bg_vertices,
            &geometry.bg_indices,
        );
        self.uploads.overlay_bg.upload(
            device,
            queue,
            "overlay_bg_verts",
            "overlay_bg_idx",
            &geometry.overlay_bg_vertices,
            &geometry.overlay_bg_indices,
        );
        upload_fg_geometry(
            device,
            queue,
            &mut self.uploads.terminal_fg,
            &geometry.terminal_fg,
        );
        upload_fg_geometry(device, queue, &mut self.uploads.fg, &geometry.fg);
        upload_fg_geometry(
            device,
            queue,
            &mut self.uploads.overlay_fg,
            &geometry.overlay_fg,
        );
        upload_image_geometry(
            device,
            queue,
            &mut self.uploads.under_image,
            under_text_image_geometry,
        );
        upload_image_geometry(
            device,
            queue,
            &mut self.uploads.over_image,
            over_text_image_geometry,
        );
    }

    fn submit_render_passes(
        &mut self,
        acquired: (wgpu::SurfaceTexture, wgpu::TextureView),
        geometry: RenderGeometry,
        under_text_image_geometry: ImageGeometry,
        over_text_image_geometry: ImageGeometry,
    ) {
        self.upload_render_geometry(
            &geometry,
            &under_text_image_geometry,
            &over_text_image_geometry,
        );
        let (frame, view) = acquired;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        self.update_terminal_layer(&mut encoder);

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bg_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            if let Some(background) = &self.background {
                pass.set_pipeline(&self.bg_image_pipeline);
                pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
                pass.set_bind_group(1, background.bind_group(), &[]);
                pass.set_vertex_buffer(0, background.vbuf().slice(..));
                pass.set_index_buffer(background.ibuf().slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..6, 0, 0..1);
            }
        }

        self.submit_image_pass(&mut encoder, &view, &self.uploads.under_image);

        self.submit_terminal_layer_pass(&mut encoder, &view);

        if self.uploads.bg.has_indices {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dynamic_bg_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
            pass.set_vertex_buffer(0, self.uploads.bg.vertex_buffer.buffer().unwrap().slice(..));
            pass.set_index_buffer(
                self.uploads.bg.index_buffer.buffer().unwrap().slice(..),
                wgpu::IndexFormat::Uint32,
            );
            pass.draw_indexed(0..geometry.bg_indices.len() as u32, 0, 0..1);
        }

        self.submit_fg_pass(&mut encoder, &view, &self.uploads.fg, "fg_pass");

        self.submit_image_pass(&mut encoder, &view, &self.uploads.over_image);

        if self.uploads.overlay_bg.has_indices {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay_bg_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
            pass.set_vertex_buffer(
                0,
                self.uploads
                    .overlay_bg
                    .vertex_buffer
                    .buffer()
                    .unwrap()
                    .slice(..),
            );
            pass.set_index_buffer(
                self.uploads
                    .overlay_bg
                    .index_buffer
                    .buffer()
                    .unwrap()
                    .slice(..),
                wgpu::IndexFormat::Uint32,
            );
            pass.draw_indexed(0..geometry.overlay_bg_indices.len() as u32, 0, 0..1);
        }

        self.submit_fg_pass(
            &mut encoder,
            &view,
            &self.uploads.overlay_fg,
            "overlay_fg_pass",
        );

        self.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    fn update_terminal_layer(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if !self.terminal_layer.needs_full_repaint
            && !self.uploads.terminal_clear.has_indices
            && !self.uploads.terminal_bg.has_indices
            && self.uploads.terminal_fg.ranges.is_empty()
        {
            return;
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal_layer_update"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.terminal_layer.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if self.terminal_layer.needs_full_repaint {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            if self.uploads.terminal_clear.has_indices {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
                pass.set_vertex_buffer(
                    0,
                    self.uploads
                        .terminal_clear
                        .vertex_buffer
                        .buffer()
                        .unwrap()
                        .slice(..),
                );
                pass.set_index_buffer(
                    self.uploads
                        .terminal_clear
                        .index_buffer
                        .buffer()
                        .unwrap()
                        .slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..self.uploads.terminal_clear.index_count, 0, 0..1);
            }

            if self.uploads.terminal_bg.has_indices {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
                pass.set_vertex_buffer(
                    0,
                    self.uploads
                        .terminal_bg
                        .vertex_buffer
                        .buffer()
                        .unwrap()
                        .slice(..),
                );
                pass.set_index_buffer(
                    self.uploads
                        .terminal_bg
                        .index_buffer
                        .buffer()
                        .unwrap()
                        .slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..self.uploads.terminal_bg.index_count, 0, 0..1);
            }
        }

        self.submit_fg_pass(
            encoder,
            &self.terminal_layer.view,
            &self.uploads.terminal_fg,
            "terminal_layer_fg",
        );

        self.terminal_layer.needs_full_repaint = false;
    }

    fn submit_terminal_layer_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("terminal_layer_composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.layer_pipeline);
        pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
        pass.set_bind_group(1, &self.terminal_layer.bind_group, &[]);
        pass.set_vertex_buffer(0, self.terminal_layer.vertex_buffer.slice(..));
        pass.set_index_buffer(
            self.terminal_layer.index_buffer.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        pass.draw_indexed(0..6, 0, 0..1);
    }

    fn submit_fg_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        fg_upload: &PageGeometryUpload<FgVertex>,
        label: &'static str,
    ) {
        let Some(vertex_buffer) = fg_upload.vertex_buffer.buffer() else {
            return;
        };
        let Some(index_buffer) = fg_upload.index_buffer.buffer() else {
            return;
        };
        if fg_upload.ranges.is_empty() {
            return;
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.fg_pipeline);
        pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for range in &fg_upload.ranges {
            let Some(bind_group) = self.glyph_atlas.bind_group(range.page_index) else {
                continue;
            };
            pass.set_bind_group(1, bind_group, &[]);
            pass.draw_indexed(
                range.index_start..range.index_start + range.index_count,
                range.vertex_base,
                0..1,
            );
        }
    }

    fn submit_image_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        image_upload: &PageGeometryUpload<ImageVertex>,
    ) {
        let Some(vertex_buffer) = image_upload.vertex_buffer.buffer() else {
            return;
        };
        let Some(index_buffer) = image_upload.index_buffer.buffer() else {
            return;
        };
        if image_upload.ranges.is_empty() {
            return;
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("image_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.image_pipeline);
        pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for range in &image_upload.ranges {
            let Some(bind_group) = self.image_atlas.bind_group(range.page_index) else {
                continue;
            };
            pass.set_bind_group(1, bind_group, &[]);
            pass.draw_indexed(
                range.index_start..range.index_start + range.index_count,
                range.vertex_base,
                0..1,
            );
        }
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

    /// Paint the tab bar at the top of the window. Each tab gets a
    /// background quad and a label shaped through the glyph atlas. The
    /// active tab uses the palette bg, inactive tabs use a 50/50 blend
    /// of palette bg and fg. Window control buttons (minimize, maximize,
    /// close) are rendered at the right edge.
    fn render_tab_bar(
        &mut self,
        font_system: &mut FontSystem,
        tabs: &[TabInfo],
        palette: &ColorPalette,
        new_tab_text: SmolStr,
        controls: &WindowControls,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
        overlay_bg_vertices: &mut Vec<BgVertex>,
        overlay_bg_indices: &mut Vec<u32>,
        overlay_fg: &mut FgGeometry,
    ) {
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let surface_w = self.surface_config.width as f32;
        let plan = build_tab_bar_plan(
            tabs,
            palette,
            new_tab_text,
            controls.hovered,
            controls.maximized,
            surface_w,
            cell_w,
        );

        // Full-width bar background (inactive colour as the base).
        let bar_bg = pack_color(&plan.base_bg, 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [0.0, 0.0],
                color: bar_bg,
            },
            BgVertex {
                pos: [surface_w, 0.0],
                color: bar_bg,
            },
            BgVertex {
                pos: [0.0, cell_h],
                color: bar_bg,
            },
            BgVertex {
                pos: [surface_w, cell_h],
                color: bar_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let label_fg = pack_color(&palette.fg, 255);

        for tab in &plan.tabs {
            if let Some(bg) = tab.bg {
                let color = pack_color(&bg, 255);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [tab.x, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [tab.x, cell_h],
                        color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width, cell_h],
                        color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            self.shape_and_render_label(
                font_system,
                &tab.label,
                tab.label_x,
                0.0,
                baseline,
                cell_w,
                None,
                label_fg,
                fg,
            );
        }

        if let Some(bg) = plan.new_tab_button.bg {
            push_rect(
                plan.new_tab_button.x,
                0.0,
                plan.new_tab_button.width,
                cell_h,
                pack_color(&bg, 255),
                bg_vertices,
                bg_indices,
            );
        }
        self.shape_and_render_label(
            font_system,
            &plan.new_tab_button.label.to_smolstr(),
            plan.new_tab_button.x,
            0.0,
            baseline,
            cell_w,
            Some(plan.new_tab_button.width),
            label_fg,
            fg,
        );

        for button in &plan.buttons {
            if let Some(bg) = button.bg {
                push_rect(
                    button.x,
                    0.0,
                    button.width,
                    cell_h,
                    pack_color(&bg, 255),
                    bg_vertices,
                    bg_indices,
                );
            }
            self.shape_and_render_label(
                font_system,
                button.label,
                button.x,
                0.0,
                baseline,
                cell_w,
                Some(button.width),
                label_fg,
                fg,
            );
        }

        // ---- Tab context menu ----
        if let Some((menu_x, hovered_idx)) = controls.tab_menu {
            let menu_items = &crate::renderer::TAB_MENU_ITEMS;
            let menu_w = cell_w * crate::renderer::TAB_MENU_WIDTH_CELLS;
            let menu_h = menu_items.len() as f32 * cell_h;
            let mx = menu_x.min(surface_w - menu_w).max(0.0);
            let my = cell_h; // directly below the tab bar

            // Panel background.
            let panel_bg = pack_color(&Srgb::new(30, 30, 38), 255);
            push_rect(
                mx,
                my,
                menu_w,
                menu_h,
                panel_bg,
                overlay_bg_vertices,
                overlay_bg_indices,
            );

            // Border lines (top and bottom).
            let border_color = pack_color(&Srgb::new(80, 80, 100), 255);
            push_rect(
                mx,
                my,
                menu_w,
                1.0,
                border_color,
                overlay_bg_vertices,
                overlay_bg_indices,
            );
            push_rect(
                mx,
                my + menu_h - 1.0,
                menu_w,
                1.0,
                border_color,
                overlay_bg_vertices,
                overlay_bg_indices,
            );

            let normal_fg = pack_color(&Srgb::new(220, 220, 220), 255);
            let hover_bg = pack_color(&Srgb::new(55, 55, 70), 255);
            let margin = cell_w * 0.5;

            for (i, item) in menu_items.iter().enumerate() {
                let iy = my + i as f32 * cell_h;

                if hovered_idx == Some(i) {
                    push_rect(
                        mx,
                        iy,
                        menu_w,
                        cell_h,
                        hover_bg,
                        overlay_bg_vertices,
                        overlay_bg_indices,
                    );
                }

                self.shape_and_render_label(
                    font_system,
                    item.label,
                    mx + margin,
                    iy,
                    baseline,
                    cell_w,
                    None,
                    normal_fg,
                    overlay_fg,
                );
            }
        }

        for tab in &plan.tabs {
            if let Some(separator) = tab.separator {
                let sep_w = 3.0_f32;
                let sep_color = pack_color(&separator, self.bg_alpha);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [tab.x + tab.width, 0.0],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width + sep_w, 0.0],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width, cell_h],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width + sep_w, cell_h],
                        color: sep_color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }
        }
    }

    /// Shape a short text string and emit foreground glyph quads at the
    /// given position. Used by the tab bar for both tab labels and window
    /// control button glyphs.
    fn shape_and_render_label(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        x: f32,
        y: f32,
        baseline: f32,
        cell_w: f32,
        centered_width: Option<f32>,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let cells: Vec<smol_str::SmolStr> = text
            .graphemes(true)
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);
        let mut glyphs = Vec::with_capacity(shaped.len());

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            glyphs.push(LabelGlyph {
                slot,
                col: sg.col,
                x_offset: sg.x_offset,
                y_offset: sg.y_offset,
            });
        }

        let x = match (centered_width, label_ink_bounds(&glyphs, cell_w)) {
            (Some(width), Some((left, right))) => centered_ink_origin_x(x, width, left, right),
            _ => x,
        };

        for glyph in glyphs {
            let sx = glyph.slot.x();
            let sy = glyph.slot.y();
            let sw = glyph.slot.width();
            let sh = glyph.slot.height();

            let gx = x + glyph.col as f32 * cell_w + glyph.slot.bearing_x as f32 + glyph.x_offset;
            let gx = gx.floor();

            let gy = y + baseline - glyph.slot.bearing_y as f32 - glyph.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;

            let flags: u32 = if glyph.slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                glyph.slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                ],
            );
        }
    }

    /// Paint the bottom-of-viewport search bar. The bar is a dark quad
    /// stretching across the viewport's last row, with a prompt + typed
    /// query + match counter shaped through the normal glyph atlas. A
    /// small caret marks the query's end so the user can see where their
    /// next keystroke will land.
    fn render_search_bar(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        y_offset: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some(search) = &snap.search else {
            return;
        };

        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let cols = snap.viewport_cols;
        let rows = snap.viewport_rows;
        if rows == 0 || cols == 0 {
            return;
        }

        // Build the visible label. The counter only appears once there are
        // matches to count — an empty query draws just the prompt so the
        // user sees something immediately on `Ctrl+Shift+F`.
        let counter = if search.match_count == 0 {
            if search.query.is_empty() {
                String::new()
            } else {
                "  (no match)".to_string()
            }
        } else {
            format!("  ({}/{})", search.active_idx + 1, search.match_count)
        };
        let label = format!("Find: {}{}", search.query, counter);

        // Truncate to fit the viewport width. We measure by char count —
        // one cell per char is the same approximation we use throughout
        // the ASCII-dominant pieces of this code.
        let max_chars = cols as usize;
        let label_graphemes: Vec<&str> = label.graphemes(true).take(max_chars).collect();

        // Caret sits at the end of the typed query, in column terms. The
        // prompt is exactly "Find: " (6 chars); the caret lives right
        // after the query text, clamped to the truncated label width.
        let prompt_len = "Find: ".chars().count() as u32;
        let caret_col = (prompt_len + search.query.chars().count() as u32).min(cols - 1);

        // Bar background: a dark opaque strip across the last row.
        let bar_y = (rows - 1) as f32 * cell_h + y_offset;
        let bar_w = cols as f32 * cell_w;
        let bar_bg = pack_color(&palette::Srgb::new(24, 24, 32), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [0.0, bar_y],
                color: bar_bg,
            },
            BgVertex {
                pos: [bar_w, bar_y],
                color: bar_bg,
            },
            BgVertex {
                pos: [0.0, bar_y + cell_h],
                color: bar_bg,
            },
            BgVertex {
                pos: [bar_w, bar_y + cell_h],
                color: bar_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Caret: a thin bright bar at the query insertion point so the
        // user can see where their next keystroke will go.
        let caret_x = caret_col as f32 * cell_w;
        let caret_w = (cell_w * 0.1).max(1.0);
        let caret_color = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [caret_x, bar_y + cell_h * 0.1],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x + caret_w, bar_y + cell_h * 0.1],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x, bar_y + cell_h * 0.9],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x + caret_w, bar_y + cell_h * 0.9],
                color: caret_color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Label glyphs. Shape through the normal text pipeline so the bar
        // respects whatever font variants are loaded and goes through the
        // atlas LRU like any other glyph.
        let cells: Vec<smol_str::SmolStr> = label_graphemes
            .iter()
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        let label_fg = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();

            let gx = sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = bar_y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;

            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: label_fg,
                        flags,
                    },
                ],
            );
        }
    }

    fn render_status_line_chrome(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some(row) = snap.status_line_row else {
            return;
        };
        let y = layout.tab_bar_h + row as f32 * layout.cell_h;
        let border = pack_color(&snap.palette.status_line_fg, 255);
        let left = 0.0;
        let width = layout.gutter_px + snap.viewport_cols as f32 * layout.cell_w;
        let thickness = 1.0_f32.max((layout.cell_h * 0.04).round());
        push_rect(left, y, width, thickness, border, bg_vertices, bg_indices);
        push_rect(
            left,
            y + layout.cell_h - thickness,
            width,
            thickness,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            left,
            y,
            thickness,
            layout.cell_h,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            left + width - thickness,
            y,
            thickness,
            layout.cell_h,
            border,
            bg_vertices,
            bg_indices,
        );

        if layout.gutter_px <= 0.0 {
            return;
        }

        let row = status_line_label_row("⟫", &snap.palette);
        let shaped = font_system.shape_row(&row.cells, &row.attrs);
        let baseline = font_system.baseline_offset();
        let cell_w = font_system.cell_width as f32;
        let marker_x = ((layout.gutter_px - cell_w) * 0.5).max(0.0);

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }
            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();
            let gx = marker_x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let flags: u32 = if slot.is_color { 1 } else { 0 };
            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx.floor(), gy.floor()],
                        uv: [sx as f32, sy as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor() + sw as f32, gy.floor()],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor(), gy.floor() + sh as f32],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor() + sw as f32, gy.floor() + sh as f32],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: border,
                        flags,
                    },
                ],
            );
        }
    }

    /// Paint the gutter popup: a dark panel with an optional duration header
    /// and four action items, one per row. The hovered item gets a brighter
    /// background so the user sees where their click will land.
    fn render_gutter_popup(
        &mut self,
        font_system: &mut FontSystem,
        popup: &GutterPopup,
        gutter_px: f32,
        cell_w: f32,
        cell_h: f32,
        tab_bar_h: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let baseline = font_system.baseline_offset();
        let surface_h = self.surface_config.height as f32;

        let header_rows = if popup.duration_text.is_some() { 1 } else { 0 };
        let total_rows = header_rows + GUTTER_MENU_ITEMS.len();
        let popup_w = cell_w * POPUP_WIDTH_CELLS;
        let popup_h = total_rows as f32 * cell_h;
        let popup_x = gutter_px;
        let popup_y = (popup.screen_row as f32 * cell_h + tab_bar_h)
            .min(surface_h - popup_h)
            .max(tab_bar_h);

        // Panel background.
        let panel_bg = pack_color(&palette::Srgb::new(30, 30, 38), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Thin border at top and bottom.
        let border_color = pack_color(&palette::Srgb::new(80, 80, 100), 255);
        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border_color,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border_color,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border_color,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border_color,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let margin = cell_w * 0.5;
        let max_chars = ((popup_w - margin * 2.0) / cell_w).max(1.0) as usize;

        // Duration header.
        if let Some(ref dur) = popup.duration_text {
            let label: String = dur.chars().take(max_chars).collect();
            let dim_fg = pack_color(&palette::Srgb::new(140, 140, 160), 255);
            self.shape_popup_line(
                font_system,
                &label,
                popup_x + margin,
                popup_y,
                baseline,
                cell_w,
                cell_h,
                dim_fg,
                fg,
            );
        }

        // Menu items.
        let normal_fg = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        let hover_bg = pack_color(&palette::Srgb::new(55, 55, 70), 255);

        for (i, item) in GUTTER_MENU_ITEMS.iter().enumerate() {
            let row_y = popup_y + (header_rows + i) as f32 * cell_h;

            // Hover highlight.
            if popup.hovered_item == Some(i) {
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [popup_x, row_y],
                        color: hover_bg,
                    },
                    BgVertex {
                        pos: [popup_x + popup_w, row_y],
                        color: hover_bg,
                    },
                    BgVertex {
                        pos: [popup_x, row_y + cell_h],
                        color: hover_bg,
                    },
                    BgVertex {
                        pos: [popup_x + popup_w, row_y + cell_h],
                        color: hover_bg,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            let label: String = item.label.chars().take(max_chars).collect();
            self.shape_popup_line(
                font_system,
                &label,
                popup_x + margin,
                row_y,
                baseline,
                cell_w,
                cell_h,
                normal_fg,
                fg,
            );
        }
    }

    fn render_recording_popup(
        &mut self,
        font_system: &mut FontSystem,
        popup: &crate::renderer::RecordingPopup,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        if popup.lines.is_empty() {
            return;
        }

        let baseline = font_system.baseline_offset();
        let margin_x = layout.cell_w;
        let margin_y = layout.cell_h * 0.5;
        let max_chars = popup
            .lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(1);
        let popup_w = (max_chars as f32 + 2.0) * layout.cell_w;
        let popup_h = popup.lines.len() as f32 * layout.cell_h + margin_y * 2.0;
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let popup_x = ((surface_w - popup_w) * 0.5).max(layout.gutter_px);
        let popup_y = ((surface_h - popup_h + layout.tab_bar_h) * 0.5).max(layout.tab_bar_h);

        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 244);
        let border = pack_color(&palette::Srgb::new(92, 92, 118), 255);
        let text_fg = pack_color(&palette::Srgb::new(232, 232, 236), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let border_w = 1.0_f32;
        for bx in [popup_x, popup_x + popup_w - border_w] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [bx, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx, popup_y + popup_h],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y + popup_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        for (i, line) in popup.lines.iter().enumerate() {
            self.shape_popup_line(
                font_system,
                line,
                popup_x + margin_x,
                popup_y + margin_y + i as f32 * layout.cell_h,
                baseline,
                layout.cell_w,
                layout.cell_h,
                text_fg,
                fg,
            );
        }
    }

    fn render_permission_modal(
        &mut self,
        font_system: &mut FontSystem,
        modal: &crate::renderer::PermissionModal,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let panel = crate::renderer::permission_panel_rect(
            &modal.feature,
            layout.cell_w,
            layout.cell_h,
            surface_w,
            surface_h,
            layout.tab_bar_h,
        );
        let buttons = crate::renderer::permission_button_layout(
            &modal.feature,
            layout.cell_w,
            layout.cell_h,
            surface_w,
            surface_h,
            layout.tab_bar_h,
        );

        let dim = pack_color(&palette::Srgb::new(0, 0, 0), 120);
        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 248);
        let border = pack_color(&palette::Srgb::new(132, 132, 164), 255);
        let button_bg = pack_color(&palette::Srgb::new(46, 46, 58), 255);
        let button_hover = pack_color(&palette::Srgb::new(74, 74, 94), 255);
        let button_no_bg = pack_color(&palette::Srgb::new(52, 42, 46), 255);
        let button_no_hover = pack_color(&palette::Srgb::new(88, 58, 64), 255);
        let text_fg = pack_color(&palette::Srgb::new(238, 238, 244), 255);
        let hint_fg = pack_color(&palette::Srgb::new(202, 202, 214), 255);

        push_rect(0.0, 0.0, surface_w, surface_h, dim, bg_vertices, bg_indices);
        push_rect(
            panel.0,
            panel.1,
            panel.2,
            panel.3,
            panel_bg,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0,
            panel.1,
            panel.2,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0,
            panel.1 + panel.3 - 1.0,
            panel.2,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0,
            panel.1,
            1.0,
            panel.3,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0 + panel.2 - 1.0,
            panel.1,
            1.0,
            panel.3,
            border,
            bg_vertices,
            bg_indices,
        );

        let yes_bg = if modal.hovered == Some(crate::renderer::PermissionChoice::Allow) {
            button_hover
        } else {
            button_bg
        };
        let no_bg = if modal.hovered == Some(crate::renderer::PermissionChoice::Deny) {
            button_no_hover
        } else {
            button_no_bg
        };
        push_rect(
            buttons.yes.0,
            buttons.yes.1,
            buttons.yes.2,
            buttons.yes.3,
            yes_bg,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            buttons.no.0,
            buttons.no.1,
            buttons.no.2,
            buttons.no.3,
            no_bg,
            bg_vertices,
            bg_indices,
        );

        let baseline = font_system.baseline_offset();
        let feature_line = crate::renderer::permission_feature_line(&modal.feature);
        self.shape_centered_popup_line(
            font_system,
            &feature_line,
            panel,
            panel.1 + layout.cell_h,
            baseline,
            layout.cell_w,
            text_fg,
            fg,
        );
        self.shape_centered_popup_line(
            font_system,
            "Would you like to allow this?",
            panel,
            panel.1 + 2.0 * layout.cell_h,
            baseline,
            layout.cell_w,
            text_fg,
            fg,
        );
        self.shape_popup_line(
            font_system,
            "[y]es",
            buttons.yes.0 + layout.cell_w,
            buttons.yes.1,
            baseline,
            layout.cell_w,
            layout.cell_h,
            hint_fg,
            fg,
        );
        self.shape_popup_line(
            font_system,
            "[n]o",
            buttons.no.0 + layout.cell_w,
            buttons.no.1,
            baseline,
            layout.cell_w,
            layout.cell_h,
            hint_fg,
            fg,
        );
    }

    fn shape_centered_popup_line(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        panel: (f32, f32, f32, f32),
        y: f32,
        baseline: f32,
        cell_w: f32,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let width = text.chars().count() as f32 * cell_w;
        let x = panel.0 + (panel.2 - width) * 0.5;
        self.shape_popup_line(font_system, text, x, y, baseline, cell_w, 0.0, color, fg);
    }

    fn render_toast(
        &mut self,
        font_system: &mut FontSystem,
        toast: &crate::renderer::Toast,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let text_chars = toast.text.chars().count();
        if text_chars == 0 {
            return;
        }

        let width_cells = (text_chars + 2).clamp(3, 100);
        let text_capacity = width_cells.saturating_sub(2);
        let text: String = toast.text.chars().take(text_capacity).collect();
        let popup_w = width_cells as f32 * layout.cell_w;
        let popup_h = 3.0 * layout.cell_h;
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let popup_x = (surface_w - popup_w).max(layout.gutter_px);
        let popup_y = (surface_h - popup_h).max(layout.tab_bar_h);

        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 244);
        let border = pack_color(&palette::Srgb::new(92, 92, 118), 255);
        let text_fg = pack_color(&palette::Srgb::new(232, 232, 236), 255);

        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let border_w = 1.0_f32;
        for bx in [popup_x, popup_x + popup_w - border_w] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [bx, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx, popup_y + popup_h],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y + popup_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        self.shape_popup_line(
            font_system,
            &text,
            popup_x + layout.cell_w,
            popup_y + layout.cell_h,
            font_system.baseline_offset(),
            layout.cell_w,
            layout.cell_h,
            text_fg,
            fg,
        );
    }

    /// Shape a single line of popup text and push its glyph quads.
    fn shape_popup_line(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        x: f32,
        y: f32,
        baseline: f32,
        cell_w: f32,
        _cell_h: f32,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let cells: Vec<smol_str::SmolStr> = text
            .chars()
            .map(|c| {
                let mut buf = [0u8; 4];
                smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf))
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();

            let gx = x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;
            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                ],
            );
        }
    }

    /// Paint the IME preedit: a darkened strip at the cursor cell, an
    /// underline that marks the whole composition as uncommitted, a
    /// thicker underline over the caret/highlighted segment the IME
    /// reported (when it did), and the composed glyphs themselves.
    ///
    /// Composition length is clipped to the remaining columns on the
    /// cursor's row; long compositions trail off past the right edge
    /// rather than wrapping. The IME's candidate popup sits below this
    /// overlay via `set_ime_cursor_area`, so the user can still see their
    /// options.
    fn render_preedit(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        preedit: &crate::renderer::PreeditState,
        gutter_px: f32,
        cell_w: f32,
        cell_h: f32,
        baseline: f32,
        tab_bar_h: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some((cursor_row, cursor_col)) = snap.cursor else {
            return;
        };
        if preedit.text.is_empty() {
            return;
        }

        let origin_x = cursor_col as f32 * cell_w + gutter_px;
        let origin_y = cursor_row as f32 * cell_h + tab_bar_h;

        let max_chars = snap.viewport_cols.saturating_sub(cursor_col) as usize;
        if max_chars == 0 {
            return;
        }

        // Per-char iteration keeps the math simple — the overlay treats
        // every codepoint as one cell wide. That's wrong for CJK
        // full-width chars in general, but the preedit is a transient
        // overlay on top of the grid, and the candidate popup does the
        // real work of showing full-width layout options.
        let visible_graphemes: Vec<&str> = preedit.text.graphemes(true).take(max_chars).collect();
        let visible_len = visible_graphemes.len();
        if visible_len == 0 {
            return;
        }

        // Solid dark panel so the glyph being composed doesn't bleed
        // through the cells it's sitting on. Alpha is 255 because we
        // want full occlusion; the whole surface is already composited
        // with the window opacity.
        let panel_bg = pack_color(&palette::Srgb::new(40, 40, 55), 255);
        let panel_w = visible_len as f32 * cell_w;
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [origin_x, origin_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [origin_x + panel_w, origin_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [origin_x, origin_y + cell_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [origin_x + panel_w, origin_y + cell_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Thin underline across the whole composition — the universal
        // "this text isn't committed yet" hint.
        let underline_color = pack_color(&palette::Srgb::new(180, 180, 220), 255);
        let underline_h = (cell_h * 0.08).max(1.5);
        let underline_y = origin_y + cell_h - underline_h;
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [origin_x, underline_y],
                color: underline_color,
            },
            BgVertex {
                pos: [origin_x + panel_w, underline_y],
                color: underline_color,
            },
            BgVertex {
                pos: [origin_x, underline_y + underline_h],
                color: underline_color,
            },
            BgVertex {
                pos: [origin_x + panel_w, underline_y + underline_h],
                color: underline_color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // The IME may mark a selected segment (the part the user is
        // currently editing inside a longer composition) via a byte range.
        // Paint a thicker bar over that segment so the user can see where
        // their next keystroke lands. Empty / full-span ranges just mean
        // "caret at position"; we skip them to avoid double-drawing the
        // whole underline.
        if let Some((start_byte, end_byte)) = preedit.cursor
            && start_byte != end_byte
        {
            let (seg_start_char, seg_end_char) =
                byte_range_to_char_range(&preedit.text, start_byte, end_byte, visible_len);
            if seg_end_char > seg_start_char {
                let seg_x = origin_x + seg_start_char as f32 * cell_w;
                let seg_w = (seg_end_char - seg_start_char) as f32 * cell_w;
                let seg_h = (cell_h * 0.14).max(2.5);
                let seg_y = origin_y + cell_h - seg_h;
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [seg_x, seg_y],
                        color: underline_color,
                    },
                    BgVertex {
                        pos: [seg_x + seg_w, seg_y],
                        color: underline_color,
                    },
                    BgVertex {
                        pos: [seg_x, seg_y + seg_h],
                        color: underline_color,
                    },
                    BgVertex {
                        pos: [seg_x + seg_w, seg_y + seg_h],
                        color: underline_color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }
        }

        // Shape the composing text through the same pipeline normal cells
        // use so fonts, ligatures, and fallback chains behave identically.
        let cells: Vec<smol_str::SmolStr> = visible_graphemes
            .iter()
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        let glyph_fg = pack_color(&palette::Srgb::new(235, 235, 245), 255);
        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();

            let gx = origin_x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = origin_y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.ceil();

            let gw = sw as f32;
            let gh = sh as f32;
            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color: glyph_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: glyph_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: glyph_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: glyph_fg,
                        flags,
                    },
                ],
            );
        }
    }

    /// Resolve "is the cursor visible right now and what does it look like"
    /// once per frame. Hidden cases — scrolled away from live or in the
    /// blink-off phase — collapse to [`CursorRenderState::Hidden`] so the
    /// per-cell loops don't have to know the rules.
    /// Compute the cursor render state from the snapshot.
    fn cursor_state_from_snapshot(
        &self,
        snap: &TermSnapshot,
    ) -> CursorRenderState {
        let Some((row, col)) = snap.cursor else {
            return CursorRenderState::Hidden;
        };
        let style = snap.cursor_style;
        if style.blink {
            let elapsed = APP_START_TIME.get().unwrap().elapsed().as_secs_f32();
            let half = CURSOR_BLINK_HALF_PERIOD.as_secs_f32();
            let phase = (elapsed / half) as u64;
            if phase & 1 == 1 {
                return CursorRenderState::Hidden;
            }
        }
        CursorRenderState::Visible {
            row,
            col,
            shape: style.shape,
        }
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

/// Paint one status bar per visible prompt row. Each bar spans the full
/// height of the row so the prompt boundary is obvious even at a glance,
/// and fills most of the gutter width with a small horizontal margin so
/// the coloured column doesn't butt up against col 0 of the text.
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
/// Drawn as plain rectangular quads in the bg pass so we don't need an
/// extra pipeline.
fn render_gutter_markers(
    rows: &[RowSnapshot],
    gutter_px: f32,
    cell_h: f32,
    y_offset: f32,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
) {
    // Leave a small horizontal margin on both sides so the bar doesn't
    // touch either the window edge or the first text column.
    let bar_w = (gutter_px * 0.6).max(3.0);
    let bar_x = (gutter_px - bar_w) * 0.5;
    let bar_h = cell_h * 0.9;
    let bar_y = (cell_h - bar_h) * 0.5;

    for row in rows {
        if !row.prompt_start {
            continue;
        }
        let rgb = match row.exit_status {
            Some(0) => SUCCESS,
            Some(_) => FAILURE,
            None => RUNNING,
        };
        let color = u32::from_be_bytes([rgb[0], rgb[1], rgb[2], 255]);

        let y0 = row.screen_row as f32 * cell_h + bar_y + y_offset;
        let y1 = y0 + bar_h;
        let x0 = bar_x;
        let x1 = x0 + bar_w;
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [x0, y0],
                color,
            },
            BgVertex {
                pos: [x1, y0],
                color,
            },
            BgVertex {
                pos: [x0, y1],
                color,
            },
            BgVertex {
                pos: [x1, y1],
                color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
    }
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

struct FgPipeline(wgpu::RenderPipeline);
struct BgPipeline(wgpu::RenderPipeline);
struct ImagePipeline(wgpu::RenderPipeline);
struct BgImagePipeline(wgpu::RenderPipeline);
struct LayerPipeline(wgpu::RenderPipeline);

fn build_pipeline_for_format(
    format: TextureFormat,
    device: &wgpu::Device,
    pipeline_cache: Option<wgpu::PipelineCache>,
    screen_size_layout: &wgpu::BindGroupLayout,
    bg_image_layout: &wgpu::BindGroupLayout,
    glyph_atlas: &GlyphAtlas,
    image_atlas: &ImageAtlas,
) -> (
    FgPipeline,
    BgPipeline,
    ImagePipeline,
    BgImagePipeline,
    LayerPipeline,
) {
    // ---- Shaders ----
    let create_pipelines = tracing::debug_span!("create_pipelines").entered();
    let bg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bg_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bg.wgsl").into()),
    });
    let fg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fg_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fg.wgsl").into()),
    });
    let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("image_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
    });
    let layer_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("layer_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/layer.wgsl").into()),
    });
    let bg_image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bg_image_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bg_image.wgsl").into()),
    });

    // ---- Background pipeline ----
    let bg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bg_pipeline_layout"),
        bind_group_layouts: &[Some(screen_size_layout)],
        immediate_size: 0,
    });

    let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bg_pipeline"),
        layout: Some(&bg_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &bg_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<BgVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Uint32],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &bg_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: pipeline_cache.as_ref(),
    });

    // ---- Foreground pipeline ----
    let fg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("fg_pipeline_layout"),
        bind_group_layouts: &[
            Some(screen_size_layout),
            Some(glyph_atlas.bind_group_layout()),
        ],
        immediate_size: 0,
    });

    let fg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fg_pipeline"),
        layout: Some(&fg_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &fg_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<FgVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2,
                    1 => Float32x2,
                    2 => Uint32,
                    3 => Uint32
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fg_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: pipeline_cache.as_ref(),
    });

    // ---- Image pipeline ----
    let image_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("image_pipeline_layout"),
        bind_group_layouts: &[
            Some(screen_size_layout),
            Some(image_atlas.bind_group_layout()),
        ],
        immediate_size: 0,
    });

    let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("image_pipeline"),
        layout: Some(&image_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &image_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ImageVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2,
                    1 => Float32x2,
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &image_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: pipeline_cache.as_ref(),
    });

    // ---- Layer composite pipeline ----
    let layer_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("layer_pipeline_layout"),
        bind_group_layouts: &[
            Some(screen_size_layout),
            Some(image_atlas.bind_group_layout()),
        ],
        immediate_size: 0,
    });

    let layer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("layer_pipeline"),
        layout: Some(&layer_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &layer_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ImageVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2,
                    1 => Float32x2,
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &layer_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: pipeline_cache.as_ref(),
    });

    // ---- Background image pipeline ----
    // Drawn as the very first thing in the bg pass, before cell quads,
    // so that cells skipping their bg quad (default-bg cells) reveal
    // the image while explicitly-coloured SGR cells overpaint it.
    let bg_image_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bg_image_pipeline_layout"),
        bind_group_layouts: &[Some(screen_size_layout), Some(bg_image_layout)],
        immediate_size: 0,
    });
    let bg_image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bg_image_pipeline"),
        layout: Some(&bg_image_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &bg_image_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<BgImageVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2,
                    1 => Float32x2,
                    2 => Float32,
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &bg_image_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // `blend: None` so the image's own alpha lands on the
                // framebuffer directly. The bg pass clears at
                // `bg_alpha` and the image quad covers the whole
                // window; cell quads draw on top with `blend: None`
                // too, overwriting the image where they paint.
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: pipeline_cache.as_ref(),
    });

    drop(create_pipelines);

    #[cfg(feature = "vulkan")]
    std::thread::spawn(move || {
        if let Some(cache) = pipeline_cache
            && let Some(data) = cache.get_data()
            && let Some(path) = pipeline_cache_path(format)
        {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let Ok(mut cache) = atomic_write_file::AtomicWriteFile::options().open(path) else {
                warn!("failed to open pipeline cache for writing");
                return;
            };

            use std::io::Write;
            if let Err(e) = cache.write_all(&data) {
                warn!("failed to write pipeline cache: {e}");
            }
            if let Err(e) = cache.commit() {
                warn!("failed to commit pipeline cache: {e}");
            }

            info!("pipeline cache saved ({} bytes)", data.len());
        }
    });

    (
        FgPipeline(fg_pipeline),
        BgPipeline(bg_pipeline),
        ImagePipeline(image_pipeline),
        BgImagePipeline(bg_image_pipeline),
        LayerPipeline(layer_pipeline),
    )
}

#[cfg(test)]
mod preedit_tests {
    use super::byte_range_to_char_range;

    #[test]
    fn ascii_range_maps_straight_through() {
        assert_eq!(byte_range_to_char_range("hello", 1, 4, 5), (1, 4));
    }

    #[test]
    fn full_range_caps_at_visible_len() {
        assert_eq!(byte_range_to_char_range("hi", 0, 2, 2), (0, 2));
    }

    #[test]
    fn range_past_visible_len_clamps() {
        // visible_len of 3 even though the string has 5 chars simulates
        // clipping at the right edge of the terminal.
        assert_eq!(byte_range_to_char_range("abcde", 0, 5, 3), (0, 3));
    }

    #[test]
    fn multibyte_range_counts_chars_not_bytes() {
        // 啊 and 不 are 3 bytes each in UTF-8. Byte range 3..6 = char 1..2.
        let text = "啊不";
        assert_eq!(byte_range_to_char_range(text, 3, 6, 2), (1, 2));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use font41::DrcsGeometryClass;
    use font41::attrs::CellAttrs;
    use palette::Srgb;
    use terminal41::ColorPalette;
    use terminal41::CursorStyle;
    use terminal41::LineAttr;

    use super::ClipRect;
    use super::FgGeometry;
    use super::FgVertex;
    use super::ImageGeometry;
    use super::ImageQuad;
    use super::PageDrawRange;
    use super::PageGeometryUpload;
    use super::RowSnapshot;
    use super::TermSnapshot;
    use super::clip_image_quad;
    use super::drcs_geometry_class;
    use super::fg_batch_for_page;
    use super::image_batch_for_page;

    fn blank_row(cols: usize) -> RowSnapshot {
        RowSnapshot {
            screen_row: 0,
            generation: 0,
            cells: vec![smol_str::SmolStr::new_inline(" "); cols],
            attrs: vec![CellAttrs::default(); cols],
            fg: vec![Srgb::new(255, 255, 255); cols],
            bg: vec![Srgb::new(0, 0, 0); cols],
            underline_color: vec![None; cols],
            has_link: vec![false; cols],
            line_attr: LineAttr::Normal,
            selected: vec![false; cols],
            matched: vec![false; cols],
            active_match: vec![false; cols],
            prompt_start: false,
            exit_status: None,
        }
    }

    fn snapshot(
        cols: u32,
        rows: u32,
    ) -> TermSnapshot {
        let palette = ColorPalette::default();
        TermSnapshot {
            generation: 0,
            rows: (0..rows)
                .map(|row| {
                    let mut snapshot = blank_row(cols as usize);
                    snapshot.screen_row = row;
                    snapshot
                })
                .collect(),
            total_rows: rows,
            viewport_rows: rows,
            viewport_cols: cols,
            status_line_row: None,
            drcs_glyphs: Arc::new(std::collections::HashMap::new()),
            dec_color: terminal41::dec_color_state_from_palette(&palette),
            palette,
            search_active: false,
            search: None,
            cursor: None,
            cursor_style: CursorStyle::default(),
            screen_reverse: false,
            synchronized_update_active: false,
            current_title: None,
            reset_cached_rows: true,
        }
    }

    #[test]
    fn image_batches_coalesce_adjacent_page_runs_only() {
        let mut geometry = ImageGeometry::default();
        image_batch_for_page(&mut geometry, 0);
        image_batch_for_page(&mut geometry, 0);
        image_batch_for_page(&mut geometry, 1);
        image_batch_for_page(&mut geometry, 0);

        let pages: Vec<usize> = geometry
            .batches
            .iter()
            .map(|batch| batch.page_index)
            .collect();
        assert_eq!(pages, vec![0, 1, 0]);
    }

    #[test]
    fn image_quad_clip_trims_position_and_uvs_to_terminal_content() {
        let vertices = clip_image_quad(
            ImageQuad {
                left: 10.0,
                top: -20.0,
                right: 110.0,
                bottom: 80.0,
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            },
            ClipRect {
                left: 20.0,
                top: 10.0,
                right: 90.0,
                bottom: 70.0,
            },
        )
        .expect("quad overlaps clip rect");

        assert_eq!(vertices[0].pos, [20.0, 10.0]);
        assert_eq!(vertices[1].pos, [90.0, 10.0]);
        assert_eq!(vertices[2].pos, [20.0, 70.0]);
        assert_eq!(vertices[3].pos, [90.0, 70.0]);
        assert_close(vertices[0].uv, [0.1, 0.3]);
        assert_close(vertices[3].uv, [0.8, 0.9]);
    }

    #[test]
    fn image_quad_clip_drops_chrome_only_quad() {
        let vertices = clip_image_quad(
            ImageQuad {
                left: 10.0,
                top: 0.0,
                right: 110.0,
                bottom: 9.0,
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            },
            ClipRect {
                left: 0.0,
                top: 10.0,
                right: 120.0,
                bottom: 100.0,
            },
        );

        assert!(vertices.is_none());
    }

    fn assert_close(
        actual: [f32; 2],
        expected: [f32; 2],
    ) {
        assert!((actual[0] - expected[0]).abs() < f32::EPSILON * 8.0);
        assert!((actual[1] - expected[1]).abs() < f32::EPSILON * 8.0);
    }

    #[test]
    fn fg_batches_coalesce_adjacent_page_runs_only() {
        let mut geometry = FgGeometry::default();
        fg_batch_for_page(&mut geometry, 0);
        fg_batch_for_page(&mut geometry, 0);
        fg_batch_for_page(&mut geometry, 1);
        fg_batch_for_page(&mut geometry, 0);

        let pages: Vec<usize> = geometry
            .batches
            .iter()
            .map(|batch| batch.page_index)
            .collect();
        assert_eq!(pages, vec![0, 1, 0]);
    }

    #[test]
    fn page_geometry_upload_records_draw_ranges_without_rewriting_indices() {
        let vertex = FgVertex {
            pos: [0.0, 0.0],
            uv: [0.0, 0.0],
            color: 0,
            flags: 0,
        };
        let mut upload = PageGeometryUpload::default();

        upload.push_batch(2, &[vertex; 4], &[0, 1, 2, 2, 1, 3]);
        upload.push_batch(3, &[vertex; 4], &[0, 1, 2, 2, 1, 3]);

        assert_eq!(
            upload.ranges,
            vec![
                PageDrawRange {
                    page_index: 2,
                    index_start: 0,
                    index_count: 6,
                    vertex_base: 0,
                },
                PageDrawRange {
                    page_index: 3,
                    index_start: 6,
                    index_count: 6,
                    vertex_base: 4,
                },
            ]
        );
        assert_eq!(upload.indices, vec![0, 1, 2, 2, 1, 3, 0, 1, 2, 2, 1, 3]);
    }

    #[test]
    fn drcs_geometry_class_buckets_to_nearest_compatible_size() {
        assert_eq!(
            drcs_geometry_class(&snapshot(80, 24)),
            Some(DrcsGeometryClass::Col80Line24)
        );
        assert_eq!(
            drcs_geometry_class(&snapshot(80, 30)),
            Some(DrcsGeometryClass::Col80Line36)
        );
        assert_eq!(
            drcs_geometry_class(&snapshot(80, 60)),
            Some(DrcsGeometryClass::Col80Line48)
        );
        assert_eq!(
            drcs_geometry_class(&snapshot(100, 30)),
            Some(DrcsGeometryClass::Col132Line36)
        );
    }
}
