use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::MutexGuard;
use std::time::Duration;
use std::time::Instant;

use font41::FontSystem;
use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use palette::Srgb;
use smol_str::SmolStrBuilder;
use terminal41::ColorPalette;
use terminal41::CursorShape;
use terminal41::LineAttr;
use terminal41::Terminal;
use terminal41::selection::is_cell_active_match;
use terminal41::selection::is_cell_match;
use terminal41::selection::is_cell_selected;
use terminal41::selection::search_active;
use terminal41::selection::search_state;
use terminal41::view;
use unicode_segmentation::UnicodeSegmentation;
use wgpu::PowerPreference;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

use crate::config::VSync;
use crate::renderer::GUTTER_MENU_ITEMS;
use crate::renderer::GutterPopup;
use crate::renderer::POPUP_WIDTH_CELLS;
use crate::renderer::background;
use crate::renderer::background::Background;
use crate::renderer::background::BgImageVertex;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::image_atlas::IMAGE_ATLAS_SIZE;
use crate::renderer::image_atlas::ImageAtlas;
use crate::renderer::paint::blink_animation_enabled;
use crate::renderer::paint::bold_glyph_enabled;
use crate::renderer::paint::build_tab_bar_plan;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::status_line_label_row;
use crate::renderer::paint::status_line_text_row;
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

/// Packed vertex for image quads: position + UV + atlas layer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageVertex {
    pos: [f32; 2],
    /// xy = normalized UV coords, z = atlas layer index.
    uv_layer: [f32; 3],
}

fn pack_color(
    c: &Srgb<u8>,
    alpha: u8,
) -> u32 {
    u32::from_be_bytes([c.red, c.green, c.blue, alpha])
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
    let lerp = |x: u8, y: u8| -> u8 {
        (x as f32 + (y as f32 - x as f32) * t)
            .clamp(0.0, 255.0)
            .round() as u8
    };
    Srgb::new(
        lerp(a.red, b.red),
        lerp(a.green, b.green),
        lerp(a.blue, b.blue),
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
    match (snap.viewport_cols, snap.rows.len() as u32) {
        (0..=80, 0..=24) => Some(font41::DrcsGeometryClass::Col80Line24),
        (81.., 0..=24) => Some(font41::DrcsGeometryClass::Col132Line24),
        (0..=80, 25..=36) => Some(font41::DrcsGeometryClass::Col80Line36),
        (81.., 25..=36) => Some(font41::DrcsGeometryClass::Col132Line36),
        (0..=80, 37..) => Some(font41::DrcsGeometryClass::Col80Line48),
        (81.., 37..) => Some(font41::DrcsGeometryClass::Col132Line48),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use font41::DrcsGeometryClass;
    use font41::attrs::CellAttrs;
    use font41::attrs::UnderlineStyle;
    use palette::Srgb;
    use terminal41::ColorPalette;
    use terminal41::CursorStyle;
    use terminal41::LineAttr;

    use super::RowSnapshot;
    use super::TermSnapshot;
    use super::drcs_geometry_class;

    fn blank_row(cols: usize) -> RowSnapshot {
        RowSnapshot {
            cells: vec![smol_str::SmolStr::new_inline(" "); cols],
            attrs: vec![CellAttrs::default(); cols],
            fg: vec![Srgb::new(255, 255, 255); cols],
            bg: vec![Srgb::new(0, 0, 0); cols],
            underline: vec![UnderlineStyle::None; cols],
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
            rows: (0..rows).map(|_| blank_row(cols as usize)).collect(),
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
        }
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

/// Emit background-pass quads for the given underline style. `uy` is the
/// baseline Y position for a single underline; `cell_w` and `cell_h` set
/// the horizontal span and vertical budget for multi-line / patterned
/// styles.
fn push_underline_quads(
    style: UnderlineStyle,
    x: f32,
    uy: f32,
    cell_w: f32,
    thickness: f32,
    cell_h: f32,
    color: u32,
    verts: &mut Vec<BgVertex>,
    idxs: &mut Vec<u32>,
) {
    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => {
            push_rect(x, uy, cell_w, thickness, color, verts, idxs);
        }
        UnderlineStyle::Double => {
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
        UnderlineStyle::Curly => {
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
        UnderlineStyle::Dotted => {
            // Dots spaced at roughly 2× thickness apart.
            let dot_size = thickness.max(1.0);
            let gap = dot_size * 2.0;
            let mut dx = x;
            while dx + dot_size <= x + cell_w {
                push_rect(dx, uy, dot_size, thickness, color, verts, idxs);
                dx += gap;
            }
        }
        UnderlineStyle::Dashed => {
            // Three dashes per cell.
            let dash_w = cell_w / 5.0;
            let gap = dash_w;
            let mut dx = x;
            while dx + dash_w <= x + cell_w {
                push_rect(dx, uy, dash_w, thickness, color, verts, idxs);
                dx += dash_w + gap;
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

// ---------------------------------------------------------------------------
// Terminal snapshot — captured under the lock, consumed without it
// ---------------------------------------------------------------------------

/// Per-row snapshot of terminal state. Cloned from the terminal grid under
/// the lock so that font shaping and glyph caching can happen without
/// holding the terminal mutex.
pub struct RowSnapshot {
    pub cells: Vec<smol_str::SmolStr>,
    pub attrs: Vec<CellAttrs>,
    pub fg: Vec<Srgb<u8>>,
    pub bg: Vec<Srgb<u8>>,
    pub underline: Vec<UnderlineStyle>,
    pub underline_color: Vec<Option<Srgb<u8>>>,
    pub has_link: Vec<bool>,
    pub line_attr: LineAttr,
    pub selected: Vec<bool>,
    pub matched: Vec<bool>,
    pub active_match: Vec<bool>,
    /// Shell-integration: this row starts a prompt.
    pub prompt_start: bool,
    /// Shell-integration: exit status of the command at this prompt.
    pub exit_status: Option<i32>,
}

/// Snapshot of the search bar state for rendering.
pub struct SearchSnapshot {
    pub query: String,
    pub match_count: usize,
    pub active_idx: usize,
}

/// All terminal state needed for one render frame, captured under the lock.
pub struct TermSnapshot {
    pub rows: Vec<RowSnapshot>,
    pub viewport_rows: u32,
    pub viewport_cols: u32,
    pub status_line_row: Option<u32>,
    pub drcs_glyphs: font41::DrcsGlyphMap,
    pub dec_color: terminal41::DecColorState,
    pub palette: ColorPalette,
    pub search_active: bool,
    pub search: Option<SearchSnapshot>,
    /// Cursor position (row, col) if visible and not scrolled off.
    pub cursor: Option<(u32, u32)>,
    pub cursor_style: terminal41::CursorStyle,
    /// DECSCNM — screen-wide reverse video. When true, default fg/bg are
    /// swapped and per-cell REVERSE is XORed with this.
    pub screen_reverse: bool,
}

/// Snapshot the terminal's visible state under the lock. The resulting
/// struct owns all the data — the lock can be released immediately after.
pub fn snapshot_terminal(terminal: &Terminal) -> TermSnapshot {
    let vp_rows = terminal.viewport.rows;
    let vp_cols = terminal.viewport.cols;
    let search_active = search_active(&terminal.search);
    let status_line_row = view::status_line_row(&terminal.active).map(|_| vp_rows);

    let mut rows =
        Vec::with_capacity(view::total_rows(&terminal.active, &terminal.viewport) as usize);
    for row in 0..vp_rows {
        // When the search bar is open it overlays the last row, so we
        // still snapshot it (the bg still renders) but the caller can
        // decide to skip fg glyphs.
        let grid_row = view::visible_row(&terminal.active, &terminal.viewport, row);
        let is_double = !matches!(grid_row.line_attr, LineAttr::Normal);
        let cols = if is_double { vp_cols / 2 } else { vp_cols };

        rows.push(RowSnapshot {
            cells: grid_row.cells.clone(),
            attrs: grid_row.attrs.clone(),
            fg: grid_row.fg.clone(),
            bg: grid_row.bg.clone(),
            underline: grid_row.underline.clone(),
            underline_color: grid_row.underline_color.clone(),
            has_link: grid_row.links.iter().map(|l| l.is_some()).collect(),
            line_attr: grid_row.line_attr,
            selected: (0..cols)
                .map(|c| {
                    is_cell_selected(
                        terminal.selection.as_ref(),
                        &terminal.active,
                        &terminal.viewport,
                        row,
                        c,
                    )
                })
                .collect(),
            matched: (0..cols)
                .map(|c| {
                    is_cell_match(
                        &terminal.search,
                        &terminal.active,
                        &terminal.viewport,
                        row,
                        c,
                    )
                })
                .collect(),
            active_match: (0..cols)
                .map(|c| {
                    is_cell_active_match(
                        &terminal.search,
                        &terminal.active,
                        &terminal.viewport,
                        row,
                        c,
                    )
                })
                .collect(),
            prompt_start: grid_row.prompt_start,
            exit_status: grid_row.exit_status,
        });
    }
    if let Some(status_row) = snapshot_status_line_row(terminal, vp_cols) {
        rows.push(status_row);
    }

    let search = search_state(&terminal.search).map(|s| SearchSnapshot {
        query: s.query.clone(),
        match_count: s.matches.len(),
        active_idx: s.active_idx,
    });

    let cursor = if terminal.active.offset == 0 && terminal.active.cursor_visible {
        if let Some(col) = view::status_line_cursor_col(&terminal.active) {
            Some((vp_rows, col))
        } else {
            Some((terminal.active.cursor.row, terminal.active.cursor.col))
        }
    } else {
        None
    };

    TermSnapshot {
        rows,
        viewport_rows: vp_rows,
        viewport_cols: vp_cols,
        status_line_row,
        drcs_glyphs: terminal.drcs_render_glyphs(),
        dec_color: terminal.dec_color_state().clone(),
        palette: terminal.palette.clone(),
        search_active,
        search,
        cursor,
        cursor_style: terminal.cursor_style,
        screen_reverse: terminal.modes.screen_reverse,
    }
}

fn snapshot_status_line_row(
    terminal: &Terminal,
    vp_cols: u32,
) -> Option<RowSnapshot> {
    if let Some(text) = view::indicator_status_text(&terminal.metadata, &terminal.active) {
        return Some(status_line_text_row(&text, vp_cols, &terminal.palette));
    }
    let grid_row = view::status_line_row(&terminal.active)?;
    Some(RowSnapshot {
        cells: grid_row.cells.clone(),
        attrs: grid_row.attrs.clone(),
        fg: grid_row.fg.clone(),
        bg: grid_row.bg.clone(),
        underline: grid_row.underline.clone(),
        underline_color: grid_row.underline_color.clone(),
        has_link: grid_row.links.iter().map(|l| l.is_some()).collect(),
        line_attr: grid_row.line_attr,
        selected: vec![false; vp_cols as usize],
        matched: vec![false; vp_cols as usize],
        active_match: vec![false; vp_cols as usize],
        prompt_start: false,
        exit_status: None,
    })
}

/// CSD window control state passed to the renderer each frame.
pub struct WindowControls {
    /// Which button the mouse is hovering, if any.
    pub hovered: Option<u8>,
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

struct PopupClip {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

#[derive(Default)]
struct ImageGeometry {
    vertices: Vec<ImageVertex>,
    indices: Vec<u32>,
}

struct RenderGeometry {
    clear_bg: Srgb<u8>,
    bg_vertices: Vec<BgVertex>,
    bg_indices: Vec<u32>,
    fg_vertices: Vec<FgVertex>,
    fg_indices: Vec<u32>,
    overlay_bg_vertices: Vec<BgVertex>,
    overlay_bg_indices: Vec<u32>,
    overlay_fg_vertices: Vec<FgVertex>,
    overlay_fg_indices: Vec<u32>,
}

impl Default for RenderGeometry {
    fn default() -> Self {
        Self {
            clear_bg: Srgb::new(0, 0, 0),
            bg_vertices: Vec::new(),
            bg_indices: Vec::new(),
            fg_vertices: Vec::new(),
            fg_indices: Vec::new(),
            overlay_bg_vertices: Vec::new(),
            overlay_bg_indices: Vec::new(),
            overlay_fg_vertices: Vec::new(),
            overlay_fg_indices: Vec::new(),
        }
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

    /// When the renderer started; used as the reference for the cursor blink
    /// phase. Wall-clock would work too — `Instant` keeps it monotonic
    /// regardless of system clock changes.
    started: Instant,

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
}

pub struct PreparedRenderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline_cache: Option<wgpu::PipelineCache>,
}

/// Half-period of the cursor blink. xterm uses 530ms by default; 500 lands
/// just shy of that and is the common choice for newer terminals.
pub(crate) const CURSOR_BLINK_HALF_PERIOD: Duration = Duration::from_millis(500);

#[cfg(feature = "vulkan")]
fn pipeline_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("term41").join("pipeline_cache.bin"))
}

/// Load a pipeline cache from disk, falling back to an empty cache when the
/// file is missing or the data is rejected by the driver. The cache only has
/// an effect on backends that support it (Vulkan); on GL the driver ignores
/// the data and `get_data()` returns `None`.
#[cfg(feature = "vulkan")]
fn load_pipeline_cache(device: &wgpu::Device) -> wgpu::PipelineCache {
    let data = pipeline_cache_path().and_then(|p| std::fs::read(p).ok());
    if data.is_none() {
        info!("no pipeline cache found");
    }

    // SAFETY: `data` was written by a previous `PipelineCache::get_data` call
    // from this program (or is `None`). `fallback: true` ensures a corrupt
    // or stale file just produces an empty cache instead of an error.
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

        let pipeline_cache: Option<wgpu::PipelineCache> = cfg_select! {
            feature = "vulkan" => {
                tracing::debug_span!("load_pipeline_cache").in_scope(||Some(load_pipeline_cache(&device)))
            }
            _ => None,
        };

        PreparedRenderer {
            instance,
            adapter,
            device,
            queue,
            pipeline_cache,
        }
    }

    pub fn from_prepared(
        prepared: PreparedRenderer,
        window: Arc<Window>,
        opacity: f32,
        gutter_enabled: bool,
        vsync: VSync,
        background_image: Option<PathBuf>,
        background_opacity: f32,
        startup_snapshot_size: (u32, u32),
    ) -> Self {
        let PreparedRenderer {
            instance,
            adapter,
            device,
            queue,
            pipeline_cache,
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

        let glyph_atlas = GlyphAtlas::new(&device);
        let image_atlas = ImageAtlas::new(&device);

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
        let bg_image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bg_image_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bg_image.wgsl").into()),
        });

        // ---- Background pipeline ----
        let bg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bg_pipeline_layout"),
            bind_group_layouts: &[Some(&screen_size_layout)],
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
                    format: surface_format,
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
                Some(&screen_size_layout),
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
                    format: surface_format,
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
        let image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("image_pipeline_layout"),
                bind_group_layouts: &[
                    Some(&screen_size_layout),
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
                        1 => Float32x3,
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
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
        let bg_image_layout = background::bind_group_layout(&device);
        let bg_image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bg_image_pipeline_layout"),
                bind_group_layouts: &[Some(&screen_size_layout), Some(&bg_image_layout)],
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
                    format: surface_format,
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
                && let Some(path) = pipeline_cache_path()
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

        let renderer = Self {
            device,
            queue,
            surface,
            surface_config,
            bg_pipeline,
            bg_image_pipeline,
            fg_pipeline,
            image_pipeline,
            screen_size_buffer,
            screen_size_bind_group,
            glyph_atlas,
            image_atlas,
            bg_image_layout,
            background,
            bg_alpha: (opacity * 255.0) as u8,
            started: Instant::now(),
            bell_started: None,
            gutter_enabled,
        };

        renderer.update_screen_size(size);

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
        terminal: MutexGuard<'_, Terminal>,
        snap: &TermSnapshot,
        tabs: &[TabInfo],
        controls: &WindowControls,
        gutter_popup: Option<&GutterPopup>,
        recording_popup: Option<&crate::renderer::RecordingPopup>,
        preedit: Option<&crate::renderer::PreeditState>,
    ) {
        let layout = self.frame_layout(font_system, tabs);
        let image_geometry = self.build_image_geometry(&terminal, &layout);
        drop(terminal);
        let geometry = self.build_render_geometry(
            font_system,
            snap,
            tabs,
            controls,
            gutter_popup,
            recording_popup,
            preedit,
            &layout,
        );
        self.submit_render_passes(acquired, geometry, image_geometry);
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
        terminal: &Terminal,
        layout: &FrameLayout,
    ) -> ImageGeometry {
        let mut geometry = ImageGeometry::default();
        let now = std::time::Instant::now();
        for vis in view::visible_images(
            &terminal.active,
            &terminal.viewport,
            terminal.cell_height(),
            now,
        ) {
            let entry = match self.image_atlas.ensure_cached(
                &self.queue,
                vis.id,
                vis.frame_index,
                vis.image,
            ) {
                Some(e) => e,
                None => continue,
            };

            let base_x = vis.screen_col as f32 * layout.cell_w + layout.gutter_px;
            let base_y = vis.screen_row as f32 * layout.cell_h + layout.tab_bar_h;
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
                let layer = a.layer as f32;
                let ii = geometry.vertices.len() as u32;
                geometry.vertices.extend_from_slice(&[
                    ImageVertex {
                        pos: [x, y],
                        uv_layer: [u0, v0, layer],
                    },
                    ImageVertex {
                        pos: [x + w, y],
                        uv_layer: [u1, v0, layer],
                    },
                    ImageVertex {
                        pos: [x, y + h],
                        uv_layer: [u0, v1, layer],
                    },
                    ImageVertex {
                        pos: [x + w, y + h],
                        uv_layer: [u1, v1, layer],
                    },
                ]);
                geometry
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
        tabs: &[TabInfo],
        controls: &WindowControls,
        gutter_popup: Option<&GutterPopup>,
        recording_popup: Option<&crate::renderer::RecordingPopup>,
        preedit: Option<&crate::renderer::PreeditState>,
        layout: &FrameLayout,
    ) -> RenderGeometry {
        let mut geometry = RenderGeometry {
            clear_bg: snap.palette.bg,
            ..RenderGeometry::default()
        };
        let cursor_state = self.cursor_state_from_snapshot(snap);
        let popup_clip = self.popup_clip(gutter_popup, layout);
        let blink_off = (self.started.elapsed().as_millis() / 500) & 1 == 1;
        let rapid_blink_off = (self.started.elapsed().as_millis() / 250) & 1 == 1;

        for (row_idx, snap_row) in snap.rows.iter().enumerate() {
            let row = row_idx as u32;
            if snap.search_active && row == snap.viewport_rows - 1 {
                continue;
            }
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
                &mut geometry,
            );
        }

        self.append_visual_bell_overlay(&mut geometry, snap, layout);

        if layout.gutter_px > 0.0 {
            render_gutter_markers(
                snap,
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
            &mut geometry.fg_vertices,
            &mut geometry.fg_indices,
        );

        self.render_tab_bar(
            font_system,
            tabs,
            &snap.palette,
            controls,
            &mut geometry.bg_vertices,
            &mut geometry.bg_indices,
            &mut geometry.fg_vertices,
            &mut geometry.fg_indices,
            &mut geometry.overlay_bg_vertices,
            &mut geometry.overlay_bg_indices,
            &mut geometry.overlay_fg_vertices,
            &mut geometry.overlay_fg_indices,
        );
        self.render_search_bar(
            font_system,
            snap,
            layout.tab_bar_h,
            &mut geometry.bg_vertices,
            &mut geometry.bg_indices,
            &mut geometry.fg_vertices,
            &mut geometry.fg_indices,
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
                &mut geometry.fg_vertices,
                &mut geometry.fg_indices,
            );
        }

        if let Some(popup) = recording_popup {
            self.render_recording_popup(
                font_system,
                popup,
                layout,
                &mut geometry.overlay_bg_vertices,
                &mut geometry.overlay_bg_indices,
                &mut geometry.overlay_fg_vertices,
                &mut geometry.overlay_fg_indices,
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
                &mut geometry.fg_vertices,
                &mut geometry.fg_indices,
            );
        }

        geometry
    }

    fn popup_clip(
        &self,
        gutter_popup: Option<&GutterPopup>,
        layout: &FrameLayout,
    ) -> Option<PopupClip> {
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
            PopupClip {
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
        popup_clip: Option<&PopupClip>,
        blink_off: bool,
        rapid_blink_off: bool,
        layout: &FrameLayout,
        geometry: &mut RenderGeometry,
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
                let bi = geometry.bg_vertices.len() as u32;
                geometry.bg_vertices.extend_from_slice(&[
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
                geometry.bg_indices.extend_from_slice(&[
                    bi,
                    bi + 1,
                    bi + 2,
                    bi + 2,
                    bi + 1,
                    bi + 3,
                ]);
            }

            let ul_style = underline_style_for_render(snap, snap_row.underline[col as usize]);
            let has_link = snap_row.has_link[col as usize];
            let effective_ul = if has_link && ul_style == UnderlineStyle::None {
                UnderlineStyle::Single
            } else {
                ul_style
            };
            if effective_ul != UnderlineStyle::None {
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
                    &mut geometry.bg_vertices,
                    &mut geometry.bg_indices,
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
                    &mut geometry.bg_vertices,
                    &mut geometry.bg_indices,
                );
            }

            if cell_attrs.contains(CellAttrs::STRIKETHROUGH) {
                let st_color = pack_color(&painted.base_fg, 255);
                let thickness = (layout.cell_h * 0.06).max(1.0);
                let sy = y + (layout.cell_h - thickness) * 0.5;
                let bi = geometry.bg_vertices.len() as u32;
                geometry.bg_vertices.extend_from_slice(&[
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

        if let Some(overlay) =
            cursor_state.bar_overlay_at(row, &snap_row.fg, layout.cell_w, layout.cell_h)
        {
            let ox = overlay.x + layout.gutter_px;
            let oy = overlay.y + layout.tab_bar_h;
            let bi = geometry.bg_vertices.len() as u32;
            geometry.bg_vertices.extend_from_slice(&[
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
                .bg_indices
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
        popup_clip: Option<&PopupClip>,
        blink_off: bool,
        rapid_blink_off: bool,
        layout: &FrameLayout,
        geometry: &mut RenderGeometry,
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
            let fi = geometry.fg_vertices.len() as u32;
            geometry.fg_vertices.extend_from_slice(&[
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
            ]);
            geometry
                .fg_indices
                .extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
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

    fn submit_render_passes(
        &mut self,
        acquired: (wgpu::SurfaceTexture, wgpu::TextureView),
        geometry: RenderGeometry,
        image_geometry: ImageGeometry,
    ) {
        let (frame, view) = acquired;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        let bg_vbuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bg_verts"),
                contents: bytemuck::cast_slice(&geometry.bg_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let bg_ibuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bg_idx"),
                contents: bytemuck::cast_slice(&geometry.bg_indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bg_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: geometry.clear_bg.red as f64 / 255.0,
                            g: geometry.clear_bg.green as f64 / 255.0,
                            b: geometry.clear_bg.blue as f64 / 255.0,
                            a: self.bg_alpha as f64 / 255.0,
                        }),
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

            if bg_ibuf.size() > 0 {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
                pass.set_vertex_buffer(0, bg_vbuf.slice(..));
                pass.set_index_buffer(bg_ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..geometry.bg_indices.len() as u32, 0, 0..1);
            }
        }

        if !geometry.fg_indices.is_empty() {
            let fg_vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fg_verts"),
                    contents: bytemuck::cast_slice(&geometry.fg_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let fg_ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fg_idx"),
                    contents: bytemuck::cast_slice(&geometry.fg_indices),
                    usage: wgpu::BufferUsages::INDEX,
                });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fg_pass"),
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

            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
            pass.set_bind_group(1, self.glyph_atlas.bind_group(), &[]);
            pass.set_vertex_buffer(0, fg_vbuf.slice(..));
            pass.set_index_buffer(fg_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..geometry.fg_indices.len() as u32, 0, 0..1);
        }

        if !image_geometry.indices.is_empty() {
            let img_vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("img_verts"),
                    contents: bytemuck::cast_slice(&image_geometry.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let img_ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("img_idx"),
                    contents: bytemuck::cast_slice(&image_geometry.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("image_pass"),
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

            pass.set_pipeline(&self.image_pipeline);
            pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
            pass.set_bind_group(1, self.image_atlas.bind_group(), &[]);
            pass.set_vertex_buffer(0, img_vbuf.slice(..));
            pass.set_index_buffer(img_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..image_geometry.indices.len() as u32, 0, 0..1);
        }

        if !geometry.overlay_bg_indices.is_empty() {
            let overlay_bg_vbuf =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("overlay_bg_verts"),
                        contents: bytemuck::cast_slice(&geometry.overlay_bg_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
            let overlay_bg_ibuf =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("overlay_bg_idx"),
                        contents: bytemuck::cast_slice(&geometry.overlay_bg_indices),
                        usage: wgpu::BufferUsages::INDEX,
                    });

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
            pass.set_vertex_buffer(0, overlay_bg_vbuf.slice(..));
            pass.set_index_buffer(overlay_bg_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..geometry.overlay_bg_indices.len() as u32, 0, 0..1);
        }

        if !geometry.overlay_fg_indices.is_empty() {
            let overlay_fg_vbuf =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("overlay_fg_verts"),
                        contents: bytemuck::cast_slice(&geometry.overlay_fg_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
            let overlay_fg_ibuf =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("overlay_fg_idx"),
                        contents: bytemuck::cast_slice(&geometry.overlay_fg_indices),
                        usage: wgpu::BufferUsages::INDEX,
                    });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay_fg_pass"),
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

            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
            pass.set_bind_group(1, self.glyph_atlas.bind_group(), &[]);
            pass.set_vertex_buffer(0, overlay_fg_vbuf.slice(..));
            pass.set_index_buffer(overlay_fg_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..geometry.overlay_fg_indices.len() as u32, 0, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    /// Clear all cached glyphs so they are re-rasterized at the current
    /// font size. Called when the DPI scale factor changes.
    pub fn reset_glyph_atlas(&mut self) {
        self.glyph_atlas.clear();
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
        controls: &WindowControls,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
        overlay_bg_vertices: &mut Vec<BgVertex>,
        overlay_bg_indices: &mut Vec<u32>,
        overlay_fg_vertices: &mut Vec<FgVertex>,
        overlay_fg_indices: &mut Vec<u32>,
    ) {
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let surface_w = self.surface_config.width as f32;
        let plan = build_tab_bar_plan(
            tabs,
            palette,
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

            self.shape_and_render_label(
                font_system,
                &tab.label,
                tab.label_x,
                0.0,
                baseline,
                cell_w,
                label_fg,
                fg_vertices,
                fg_indices,
            );
        }

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
                button.x + (button.width - cell_w) / 2.0,
                0.0,
                baseline,
                cell_w,
                label_fg,
                fg_vertices,
                fg_indices,
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
                    normal_fg,
                    overlay_fg_vertices,
                    overlay_fg_indices,
                );
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
        color: u32,
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
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

            let fi = fg_vertices.len() as u32;
            fg_vertices.extend_from_slice(&[
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
            ]);
            fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
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
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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

            let fi = fg_vertices.len() as u32;
            fg_vertices.extend_from_slice(&[
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
            ]);
            fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
        }
    }

    fn render_status_line_chrome(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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
            let fi = fg_vertices.len() as u32;
            fg_vertices.extend_from_slice(&[
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
            ]);
            fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
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
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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
                fg_vertices,
                fg_indices,
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
                fg_vertices,
                fg_indices,
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
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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
                fg_vertices,
                fg_indices,
            );
        }
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
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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

            let fi = fg_vertices.len() as u32;
            fg_vertices.extend_from_slice(&[
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
            ]);
            fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
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
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
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

            let fi = fg_vertices.len() as u32;
            fg_vertices.extend_from_slice(&[
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
            ]);
            fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
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
            let elapsed = self.started.elapsed().as_secs_f32();
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
    snap: &TermSnapshot,
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

    for (row_idx, row) in snap.rows.iter().enumerate() {
        if !row.prompt_start {
            continue;
        }
        let rgb = match row.exit_status {
            Some(0) => SUCCESS,
            Some(_) => FAILURE,
            None => RUNNING,
        };
        let color = u32::from_be_bytes([rgb[0], rgb[1], rgb[2], 255]);

        let y0 = row_idx as f32 * cell_h + bar_y + y_offset;
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

#[cfg(test)]
mod preedit_tests {
    use font41::FontSystem;
    use terminal41::ColorPalette;
    use terminal41::FeaturePermissions;
    use terminal41::StatusDisplayKind;
    use vtepp::Parser;

    use super::byte_range_to_char_range;
    use super::collect_row_glyphs;
    use super::snapshot_terminal;

    fn shaped_box_test_terminal() -> terminal41::Terminal {
        let mut terminal = terminal41::Terminal::new(
            80,
            24,
            1000,
            StatusDisplayKind::None,
            false,
            FeaturePermissions::default(),
            16,
            8,
            ColorPalette::default(),
        );
        let mut parser = Parser::new();
        let data = b"\r\n\x1b[1;1H\x1b[3g\
\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\
\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\
\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\x1b[8C\x1bH\
\x1b[8C\x1bH\x1b[8C\x1bH\x1b[?3l\x1b[2J\x1b(0\x1b)B\x0f\
\x1b[8;1H\x1b#3lqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqk\
\x1b[9;1H\x1b#4lqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqk\
\x1b[10;1H\x1b#3x\t\t\t\t\tx\
\x1b[11;1H\x1b#4x\t\t\t\t\tx\
\x1b[12;1H\x1b#3x\t\t\t\t\tx\
\x1b[13;1H\x1b#4x\t\t\t\t\tx\
\x1b)0\x1b(B\x0e\
\x1b[14;1H\x1b#3x                                      x\
\x1b[15;1H\x1b#4x                                      x\
\x1b[16;1H\x1b#3mqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqj\
\x1b[17;1H\x1b#4mqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqj\
\x1b(B\x1b)B\x0f\x1b[1;5m\
\x1b[12;3H* The mad programmer strikes again * \
\x1b[13;3H\t\x1b[6D* The mad programmer strikes again*\
\x1b[0m";
        for action in parser.parse(data) {
            terminal.apply(action);
        }
        terminal
    }

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

    #[test]
    fn double_size_box_text_row_keeps_border_glyph_column() {
        let terminal = shaped_box_test_terminal();
        let snap = snapshot_terminal(&terminal);
        let row = &snap.rows[12];
        let mut font_system = FontSystem::new(None, 18.0, 4);
        let glyphs = collect_row_glyphs(&mut font_system, &snap, row, 12, 40, None, false, false);

        assert!(
            glyphs.iter().any(|g| g.col == 39),
            "expected right-border glyph at col 39, got cols {:?}",
            glyphs.iter().map(|g| g.col).collect::<Vec<_>>()
        );
        let visible_non_space_cols: Vec<u16> = glyphs
            .iter()
            .filter_map(|g| (row.cells[g.col as usize].as_str() != " ").then_some(g.col))
            .collect();
        assert!(
            visible_non_space_cols.iter().all(|&col| col <= 39),
            "non-space glyphs unexpectedly exceed visible row: cols {:?}",
            visible_non_space_cols
        );
        assert_eq!(
            visible_non_space_cols.last().copied(),
            Some(39),
            "expected right border to be the last visible non-space glyph, got {:?}",
            visible_non_space_cols
        );
        let border = glyphs.iter().find(|g| g.col == 39).unwrap();
        let raster = font_system.rasterize_glyph(
            border.font_index,
            border.glyph_id,
            border.cells_wide as u32,
        );
        let effective_cell_w = font_system.cell_width as f32 * 2.0;
        let gx = border.col as f32 * effective_cell_w
            + raster.bearing_x as f32 * 2.0
            + border.x_offset * 2.0;
        let gw = raster.width as f32 * 2.0;
        let surface_w = snap.viewport_cols as f32 * font_system.cell_width as f32;
        assert!(
            gx < surface_w && gx + gw <= surface_w,
            "right border quad spills past surface: gx={gx} gw={gw} surface_w={surface_w} \
             bearing_x={} width={}",
            raster.bearing_x,
            raster.width
        );
    }
}
