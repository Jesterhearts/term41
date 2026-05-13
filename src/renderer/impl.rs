use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use config41::ColorPalette;
use config41::PowerPreference;
use config41::VSync;
use font41::FontSystem;
use font41::attrs::CellAttrs;
use palette::Srgb;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use smol_str::ToSmolStr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use terminal41::VisibleImage;
use unicode_segmentation::UnicodeSegmentation;
use wgpu::TextureFormat;
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
use crate::renderer::gutter_popup_origin;
use crate::renderer::image_atlas::ImageAtlas;
use crate::renderer::paint::build_tab_bar_plan;
use crate::renderer::paint::centered_ink_origin_x;
use crate::renderer::paint::command_highlight_rgb;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::status_line_label_row;
use crate::renderer::paint::underline_style_for_render;
use crate::renderer::paint::visible_row_cols;
use crate::window_host::CommandEditorPopupSide;
use crate::window_host::command_editor_popup_side_for_row;

mod chrome;
mod cursor;
mod frame;
mod geometry;
mod gutter;
mod images;
mod layers;
mod layout;
mod pipelines;
mod row_cache;
mod text;
mod uploads;
mod vertices;

#[cfg(test)]
mod tests;

use cursor::CursorRenderState;
use geometry::FgGeometry;
use geometry::RenderGeometry;
use geometry::RowGeometry;
use geometry::append_cached_row_geometry;
use geometry::dirty_rect_clear_geometry;
#[cfg(test)]
use geometry::fg_batch_for_page;
use geometry::push_fg_quad;
use geometry::push_terminal_area_dirty_rect;
use geometry::push_terminal_dirty_rect;
use gutter::append_gutter_marker;
pub use gutter::compute_gutter_width;
#[cfg(test)]
use gutter::gutter_marker_color;
use images::ImageGeometry;
use images::ImageQuad;
use images::clip_image_quad;
use images::image_batch_for_page;
use images::image_render_order;
use images::image_vertex_z;
use layers::IMAGE_DEPTH_FORMAT;
use layers::ImageDepthLayer;
use layers::TerminalLayer;
pub(in crate::renderer) use layout::ClipRect;
pub(in crate::renderer) use layout::CommandEditorBoxLayout;
pub(in crate::renderer) use layout::FrameLayout;
pub(in crate::renderer) use layout::apply_terminal_layout_offsets;
pub(in crate::renderer) use layout::command_editor_box_layout;
pub(in crate::renderer) use layout::row_hidden_by_sticky_prompt;
pub(in crate::renderer) use layout::row_suspended_by_terminal_area;
pub(in crate::renderer) use layout::snapshot_row_y;
#[cfg(test)]
pub(in crate::renderer) use layout::terminal_block_y_offset_rows;
pub(in crate::renderer) use layout::terminal_row_y;
pub(in crate::renderer) use layout::visible_command_editor;
use pipelines::BgImagePipeline;
use pipelines::BgPipeline;
use pipelines::FgPipeline;
use pipelines::ImagePipeline;
use pipelines::LayerPipeline;
use pipelines::build_pipeline_for_format;
use row_cache::CachedRowKey;
use row_cache::RowGutterMarkerKey;
use row_cache::RowLayoutKey;
use row_cache::RowRenderKey;
use row_cache::blank_cached_row;
use row_cache::cached_rows_match_snapshot_shape;
pub(in crate::renderer) use row_cache::gutter_fill_bg_for_col0;
use row_cache::invalidate_row_cache_with_neighbors;
use row_cache::row_blink_key;
use row_cache::row_cursor_key;
use row_cache::row_popup_clip_key;
pub(crate) use text::blend;
pub(crate) use text::collect_row_glyphs;
pub(crate) use text::drcs_geometry_class;
pub(crate) use text::resolve_cell_colors;
use uploads::GeometryUpload;
#[cfg(test)]
use uploads::PageDrawRange;
use uploads::PageGeometryUpload;
use uploads::RendererUploads;
use uploads::upload_fg_geometry;
use uploads::upload_image_geometry;
use vertices::BgVertex;
use vertices::FgVertex;
use vertices::ImageVertex;
use vertices::LabelGlyph;
use vertices::fitted_ink_origin_y;
use vertices::label_ink_bounds;
use vertices::label_ink_y_bounds;
use vertices::pack_color;
use vertices::push_rect;
use vertices::push_underline_quads;

pub const MAX_TAB_WIDTH: f32 = 30.0;
pub const SUCCESS: [u8; 3] = [80, 200, 120];
pub const FAILURE: [u8; 3] = [220, 80, 80];
pub const RUNNING: [u8; 3] = [140, 140, 140];

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
        let mut layout = frame::frame_layout(self, font_system, tabs);
        let command_editor = visible_command_editor(command_editor, snap);
        let block_y_offset_rows = apply_terminal_layout_offsets(&mut layout, snap, command_editor);
        if suspend_terminal_area {
            frame::apply_terminal_snapshot_status_row(self, snap);
        } else {
            frame::apply_terminal_snapshot_rows(self, snap, block_y_offset_rows);
        }
        let terminal_rows = std::mem::take(&mut self.terminal_rows);
        self.image_atlas.begin_frame();
        let under_text_image_geometry =
            frame::build_image_geometry(self, visible_images, &layout, true);
        let over_text_image_geometry =
            frame::build_image_geometry(self, visible_images, &layout, false);
        let geometry = frame::build_render_geometry(
            self,
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
        frame::submit_render_passes(
            self,
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

fn command_highlight_color(kind: commands41::HighlightKind) -> u32 {
    let rgb = command_highlight_rgb(kind);
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
