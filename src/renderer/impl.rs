use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use palette::Srgb;
use wgpu::PowerPreference;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

use crate::config::VSync;
use crate::font::FontSystem;
use crate::renderer::GUTTER_MENU_ITEMS;
use crate::renderer::GutterPopup;
use crate::renderer::POPUP_WIDTH_CELLS;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::image_atlas::IMAGE_ATLAS_SIZE;
use crate::renderer::image_atlas::ImageAtlas;
use crate::terminal::CursorShape;
use crate::terminal::Terminal;

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
fn blend(
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

/// Lightweight snapshot of tab state for the renderer. Built by the host
/// each frame so the renderer doesn't couple to the `App` struct.
pub struct TabInfo<'s> {
    pub label: &'s str,
    pub active: bool,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,

    bg_pipeline: wgpu::RenderPipeline,
    fg_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,

    screen_size_buffer: wgpu::Buffer,
    screen_size_bind_group: wgpu::BindGroup,

    glyph_atlas: GlyphAtlas,
    image_atlas: ImageAtlas,

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

    /// Cached compiled pipelines. Persisted to disk on drop so subsequent
    /// launches skip shader recompilation. Only effective on backends that
    /// support it (Vulkan); a no-op on GL.
    pipeline_cache: wgpu::PipelineCache,
}

/// Half-period of the cursor blink. xterm uses 530ms by default; 500 lands
/// just shy of that and is the common choice for newer terminals.
const CURSOR_BLINK_HALF_PERIOD: Duration = Duration::from_millis(500);

fn pipeline_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("term41").join("pipeline_cache.bin"))
}

/// Load a pipeline cache from disk, falling back to an empty cache when the
/// file is missing or the data is rejected by the driver. The cache only has
/// an effect on backends that support it (Vulkan); on GL the driver ignores
/// the data and `get_data()` returns `None`.
fn load_pipeline_cache(device: &wgpu::Device) -> wgpu::PipelineCache {
    let data = pipeline_cache_path().and_then(|p| std::fs::read(p).ok());
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
    pub async fn new(
        window: Arc<Window>,
        display: OwnedDisplayHandle,
        opacity: f32,
        gutter_enabled: bool,
        power_preference: PowerPreference,
        vsync: VSync,
    ) -> Self {
        let size = window.inner_size();

        let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display));
        desc.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window).expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference,
                ..Default::default()
            })
            .await
            .expect("request adapter");

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: wgpu::Features::PIPELINE_CACHE,
                ..Default::default()
            })
            .await
            .expect("request device");

        let pipeline_cache = load_pipeline_cache(&device);

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
            cache: Some(&pipeline_cache),
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
            cache: Some(&pipeline_cache),
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
            cache: Some(&pipeline_cache),
        });

        let renderer = Self {
            device,
            queue,
            surface,
            surface_config,
            bg_pipeline,
            fg_pipeline,
            image_pipeline,
            screen_size_buffer,
            screen_size_bind_group,
            glyph_atlas,
            image_atlas,
            bg_alpha: (opacity.clamp(0.0, 1.0) * 255.0) as u8,
            started: Instant::now(),
            bell_started: None,
            gutter_enabled,
            pipeline_cache,
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
        (cell_width / 3).max(12)
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

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        terminal: &Terminal,
        tabs: &[TabInfo],
        gutter_popup: Option<&GutterPopup>,
    ) {
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let gutter_px = self.gutter_width_px(font_system.cell_width) as f32;
        // When tabs are shown, shift all terminal content down by one cell
        // height to make room for the tab bar.
        let tab_bar_h = if tabs.is_empty() { 0.0 } else { cell_h };

        let mut bg_vertices: Vec<BgVertex> = Vec::new();
        let mut bg_indices: Vec<u32> = Vec::new();
        let mut fg_vertices: Vec<FgVertex> = Vec::new();
        let mut fg_indices: Vec<u32> = Vec::new();

        let cursor_state = self.cursor_state(terminal);

        // Pre-compute the popup's pixel bounds so we can clip terminal
        // text that would otherwise bleed through the opaque panel.
        let popup_clip: Option<(f32, f32, f32, f32)> = gutter_popup.map(|p| {
            let header = if p.duration_text.is_some() { 1 } else { 0 };
            let total = (header + GUTTER_MENU_ITEMS.len()) as f32;
            let pw = cell_w * POPUP_WIDTH_CELLS;
            let ph = total * cell_h;
            let px = gutter_px;
            let surface_h = self.surface_config.height as f32;
            let py = (p.screen_row as f32 * cell_h + tab_bar_h)
                .min(surface_h - ph)
                .max(tab_bar_h);
            (px, py, px + pw, py + ph)
        });

        let skip_bottom_row = terminal.search_active();

        for row in 0..terminal.viewport.rows {
            if skip_bottom_row && row == terminal.viewport.rows - 1 {
                continue;
            }
            let y = row as f32 * cell_h + tab_bar_h;

            // Background quads for the whole row.
            let grid_row = terminal.visible_row(row);
            for col in 0..terminal.viewport.cols {
                let x = col as f32 * cell_w + gutter_px;
                // A cell is rendered with altered fg/bg when it is selected,
                // matches the search query, or sits under a visible block
                // cursor. Selection / non-active match / block cursor fully
                // invert — the glyph reads as its own bg-on-fg. The focused
                // search match is softer: bg is halfway between the cell's
                // fg and bg so the user can spot which hit is active at a
                // glance without the strong pop of a full inversion.
                let selected = terminal.is_cell_selected(row, col);
                let matched = terminal.is_cell_match(row, col);
                let active_match = terminal.is_cell_active_match(row, col);
                let block_cursor_here = cursor_state.is_block_at(row, col);
                let cell_fg = grid_row.fg[col as usize];
                let cell_bg = grid_row.bg[col as usize];
                let bg_effective = if active_match {
                    blend(cell_fg, cell_bg, 0.5)
                } else if selected || matched || block_cursor_here {
                    cell_fg
                } else {
                    cell_bg
                };
                let bg_color = pack_color(&bg_effective, self.bg_alpha);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [x, y],
                        color: bg_color,
                    },
                    BgVertex {
                        pos: [x + cell_w, y],
                        color: bg_color,
                    },
                    BgVertex {
                        pos: [x, y + cell_h],
                        color: bg_color,
                    },
                    BgVertex {
                        pos: [x + cell_w, y + cell_h],
                        color: bg_color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

                // Underline quad. Drawn for either of two sources:
                //   * OSC 8 hyperlinks — so the user can see what's clickable.
                //   * SGR underline (CSI 4m) — the normal text attribute.
                // Either case stacks a thin quad at the cell baseline using
                // the foreground colour; a cell that carries both still
                // only draws the line once. Sits in the bg pass so the
                // glyph paints over any pixels the line would otherwise eat.
                let has_link = grid_row.links[col as usize].is_some();
                let has_attr_underline =
                    grid_row.attrs[col as usize].contains(crate::terminal::CellAttrs::UNDERLINE);
                if has_link || has_attr_underline {
                    let underline_color = pack_color(&grid_row.fg[col as usize], 255);
                    let thickness = (cell_h * 0.06).max(1.0);
                    let uy = y + cell_h - thickness;
                    let bi = bg_vertices.len() as u32;
                    bg_vertices.extend_from_slice(&[
                        BgVertex {
                            pos: [x, uy],
                            color: underline_color,
                        },
                        BgVertex {
                            pos: [x + cell_w, uy],
                            color: underline_color,
                        },
                        BgVertex {
                            pos: [x, uy + thickness],
                            color: underline_color,
                        },
                        BgVertex {
                            pos: [x + cell_w, uy + thickness],
                            color: underline_color,
                        },
                    ]);
                    bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
                }
            }

            // Underline / beam cursor overlays sit in the bg pass so the
            // glyph keeps its normal colour and the bar paints behind /
            // beside the character rather than over it.
            if let Some(overlay) = cursor_state.bar_overlay_at(row, &grid_row.fg, cell_w, cell_h) {
                let ox = overlay.x + gutter_px;
                let oy = overlay.y + tab_bar_h;
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
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
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            // Shape the entire row for foreground glyphs — borrows the cell
            // strings and their attribute bitmasks directly so each cell can
            // pick its bold/italic variant of the primary family.
            let shaped = font_system.shape_row(&grid_row.cells, &grid_row.attrs);

            for sg in &shaped {
                // Skip glyphs that fall behind the popup panel so
                // terminal text doesn't bleed through the overlay.
                if let Some((cl, ct, cr, cb)) = popup_clip {
                    let cx = sg.col as f32 * cell_w + gutter_px;
                    if cx < cr && cx + cell_w > cl && y < cb && y + cell_h > ct {
                        continue;
                    }
                }

                let cell_attrs = grid_row.attrs[sg.col as usize];
                let wants_bold = cell_attrs.contains(crate::terminal::CellAttrs::BOLD);
                let wants_italic = cell_attrs.contains(crate::terminal::CellAttrs::ITALIC);
                // Synth flags: true when the cell asks for a style the face
                // the shaper actually used doesn't natively cover. The atlas
                // only acts on `synth_bold` for colour fonts; italic
                // synthesis is a vertex-level shear below.
                let synth_bold = wants_bold && !font_system.font_is_bold(sg.font_index);
                let synth_italic = wants_italic && !font_system.font_is_italic(sg.font_index);

                let slot = match self.glyph_atlas.ensure_cached(
                    &self.queue,
                    font_system,
                    sg.font_index,
                    sg.glyph_id,
                    sg.cells_wide,
                    synth_bold,
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

                let gx = sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset + gutter_px;
                let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
                let gw = sw as f32;
                let gh = sh as f32;

                // Fake italic by shearing the glyph quad around the cell
                // baseline: vertices above the baseline shift right, below
                // shift left. The baseline pins so glyph rows stay aligned
                // with neighbouring regular text. The shear factor is the
                // tangent of ~12°, a common italic angle.
                let baseline_y = y + baseline;
                let shear = if synth_italic { 0.2126_f32 } else { 0.0 };
                let shear_at = |vy: f32| -> f32 { shear * (baseline_y - vy) };

                // Match the bg-pass logic: selection / non-active match /
                // block cursor fully invert the text, the active match
                // keeps the normal fg so it reads naturally against the
                // softened bg.
                let selected = terminal.is_cell_selected(row, sg.col as u32);
                let matched = terminal.is_cell_match(row, sg.col as u32);
                let active_match = terminal.is_cell_active_match(row, sg.col as u32);
                let block_cursor_here = cursor_state.is_block_at(row, sg.col as u32);
                let cell_fg = grid_row.fg[sg.col as usize];
                let cell_bg = grid_row.bg[sg.col as usize];
                let fg_effective = if active_match {
                    cell_fg
                } else if selected || matched || block_cursor_here {
                    cell_bg
                } else {
                    cell_fg
                };
                let fg_color = pack_color(&fg_effective, 255);
                let flags: u32 = if slot.is_color { 1 } else { 0 };
                let fi = fg_vertices.len() as u32;
                fg_vertices.extend_from_slice(&[
                    FgVertex {
                        pos: [gx + shear_at(gy), gy],
                        uv: [sx as f32, sy as f32],
                        color: fg_color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw + shear_at(gy), gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: fg_color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + shear_at(gy + gh), gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: fg_color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw + shear_at(gy + gh), gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: fg_color,
                        flags,
                    },
                ]);
                fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
            }
        }

        // ---- Visual bell flash overlay ----
        // Drawn after the row content as a semi-transparent white quad
        // covering the whole window. Fades out linearly across
        // BELL_FLASH_DURATION; once the fade is done we clear the timer
        // so subsequent frames do nothing until the next BEL.
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
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
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
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }
        }

        // ---- Shell-integration gutter markers ----
        // Painted before the search bar so the bar overlays the gutter on
        // its own row (the search bar is a full-width overlay and doesn't
        // care about cell columns underneath it).
        if gutter_px > 0.0 {
            render_gutter_markers(
                terminal,
                gutter_px,
                cell_h,
                tab_bar_h,
                &mut bg_vertices,
                &mut bg_indices,
            );
        }

        // ---- Search bar overlay ----
        // Drawn last in the glyph pass so it paints over the bottom row of
        // whatever the terminal was showing. Only fires while the search
        // bar is open; when closed, this path is a cheap early return.
        // ---- Tab bar ----
        self.render_tab_bar(
            font_system,
            tabs,
            &mut bg_vertices,
            &mut bg_indices,
            &mut fg_vertices,
            &mut fg_indices,
        );

        self.render_search_bar(
            font_system,
            terminal,
            tab_bar_h,
            &mut bg_vertices,
            &mut bg_indices,
            &mut fg_vertices,
            &mut fg_indices,
        );

        // ---- Gutter popup overlay ----
        if let Some(popup) = gutter_popup {
            self.render_gutter_popup(
                font_system,
                popup,
                gutter_px,
                cell_w,
                cell_h,
                tab_bar_h,
                &mut bg_vertices,
                &mut bg_indices,
                &mut fg_vertices,
                &mut fg_indices,
            );
        }

        // ---- Build image quads ----
        let mut image_vertices: Vec<ImageVertex> = Vec::new();
        let mut image_indices: Vec<u32> = Vec::new();

        let now = std::time::Instant::now();
        for vis in terminal.visible_images(now) {
            let entry = match self.image_atlas.ensure_cached(
                &self.queue,
                vis.id,
                vis.frame_index,
                vis.image,
            ) {
                Some(e) => e,
                None => continue,
            };

            let base_x = vis.screen_col as f32 * cell_w + gutter_px;
            let base_y = vis.screen_row as f32 * cell_h + tab_bar_h;

            // Scale factor from source-image pixels to display pixels. For
            // sixel these are equal; kitty's `c=`/`r=` keys can request a
            // smaller (or larger) display than the source, and we honor that
            // by scaling the quad rather than resampling the pixels.
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

                let ii = image_vertices.len() as u32;
                image_vertices.extend_from_slice(&[
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
                image_indices.extend_from_slice(&[ii, ii + 1, ii + 2, ii + 2, ii + 1, ii + 3]);
            }
        }

        // ---- Acquire surface texture ----
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            other => {
                error!("surface error: {other:?}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        // ---- BG pass ----
        let bg_vbuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bg_verts"),
                contents: bytemuck::cast_slice(&bg_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let bg_ibuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bg_idx"),
                contents: bytemuck::cast_slice(&bg_indices),
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
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: self.bg_alpha as f64 / 255.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.screen_size_bind_group, &[]);
            pass.set_vertex_buffer(0, bg_vbuf.slice(..));
            pass.set_index_buffer(bg_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..bg_indices.len() as u32, 0, 0..1);
        }

        // ---- FG pass ----
        if !fg_indices.is_empty() {
            let fg_vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fg_verts"),
                    contents: bytemuck::cast_slice(&fg_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let fg_ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fg_idx"),
                    contents: bytemuck::cast_slice(&fg_indices),
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
            pass.draw_indexed(0..fg_indices.len() as u32, 0, 0..1);
        }

        // ---- Image pass ----
        if !image_indices.is_empty() {
            let img_vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("img_verts"),
                    contents: bytemuck::cast_slice(&image_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let img_ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("img_idx"),
                    contents: bytemuck::cast_slice(&image_indices),
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
            pass.draw_indexed(0..image_indices.len() as u32, 0, 0..1);
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
    /// active tab uses `default_bg`, inactive tabs use a 50/50 blend of
    /// `default_bg` and `default_fg`.
    fn render_tab_bar(
        &mut self,
        font_system: &mut FontSystem,
        tabs: &[TabInfo],
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
    ) {
        if tabs.is_empty() {
            return;
        }
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let surface_w = self.surface_config.width as f32;

        let active_bg = crate::terminal::default_bg();
        let inactive_bg = blend(
            crate::terminal::default_bg(),
            crate::terminal::default_fg(),
            0.5,
        );

        // Full-width bar background (inactive colour as the base).
        let bar_bg = pack_color(&inactive_bg, 255);
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

        // Divide available width equally among tabs, capped to a
        // reasonable maximum so a single tab doesn't span the screen.
        let max_tab_w = cell_w * 30.0;
        let tab_w = (surface_w / tabs.len() as f32).min(max_tab_w);

        let label_fg = pack_color(&crate::terminal::default_fg(), 255);

        for (i, tab) in tabs.iter().enumerate() {
            let x0 = i as f32 * tab_w;

            // Active tab background highlight.
            if tab.active {
                let color = pack_color(&active_bg, 255);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [x0, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [x0 + tab_w, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [x0, cell_h],
                        color,
                    },
                    BgVertex {
                        pos: [x0 + tab_w, cell_h],
                        color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            // Thin separator between tabs.
            if i > 0 {
                let sep_w = 3.0_f32;
                let sep_color = pack_color(&blend(active_bg, inactive_bg, 0.5), self.bg_alpha);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [x0, 0.0],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [x0 + sep_w, 0.0],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [x0, cell_h],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [x0 + sep_w, cell_h],
                        color: sep_color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            // Label glyphs. Truncate to fit the tab width with a small
            // margin on each side.
            let margin = cell_w;
            let max_label_chars = ((tab_w - margin * 2.0) / cell_w).max(1.0) as usize;
            let label: String = tab.label.chars().take(max_label_chars).collect();

            let cells: Vec<smol_str::SmolStr> = label
                .chars()
                .map(|c| {
                    let mut buf = [0u8; 4];
                    smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf))
                })
                .collect();
            let attrs = vec![crate::terminal::CellAttrs::default(); cells.len()];
            let shaped = font_system.shape_row(&cells, &attrs);

            for sg in &shaped {
                let slot = match self.glyph_atlas.ensure_cached(
                    &self.queue,
                    font_system,
                    sg.font_index,
                    sg.glyph_id,
                    sg.cells_wide,
                    false,
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

                let gx = x0 + margin + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
                let gy = baseline - slot.bearing_y as f32 - sg.y_offset;
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
    }

    /// Paint the bottom-of-viewport search bar. The bar is a dark quad
    /// stretching across the viewport's last row, with a prompt + typed
    /// query + match counter shaped through the normal glyph atlas. A
    /// small caret marks the query's end so the user can see where their
    /// next keystroke will land.
    fn render_search_bar(
        &mut self,
        font_system: &mut FontSystem,
        terminal: &Terminal,
        y_offset: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg_vertices: &mut Vec<FgVertex>,
        fg_indices: &mut Vec<u32>,
    ) {
        let Some(search) = terminal.search_state() else {
            return;
        };

        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let cols = terminal.viewport.cols;
        let rows = terminal.viewport.rows;
        if rows == 0 || cols == 0 {
            return;
        }

        // Build the visible label. The counter only appears once there are
        // matches to count — an empty query draws just the prompt so the
        // user sees something immediately on `Ctrl+Shift+F`.
        let counter = if search.matches.is_empty() {
            if search.query.is_empty() {
                String::new()
            } else {
                "  (no match)".to_string()
            }
        } else {
            format!("  ({}/{})", search.active_idx + 1, search.matches.len())
        };
        let label = format!("Find: {}{}", search.query, counter);

        // Truncate to fit the viewport width. We measure by char count —
        // one cell per char is the same approximation we use throughout
        // the ASCII-dominant pieces of this code.
        let max_chars = cols as usize;
        let label_chars: Vec<char> = label.chars().take(max_chars).collect();

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
        let cells: Vec<smol_str::SmolStr> = label_chars
            .iter()
            .map(|c| {
                let mut buf = [0u8; 4];
                smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf))
            })
            .collect();
        let attrs = vec![crate::terminal::CellAttrs::default(); cells.len()];
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
            let gy = bar_y + baseline - slot.bearing_y as f32 - sg.y_offset;
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
        let attrs = vec![crate::terminal::CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
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
            let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
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

    /// Resolve "is the cursor visible right now and what does it look like"
    /// once per frame. Hidden cases — scrolled away from live or in the
    /// blink-off phase — collapse to [`CursorRenderState::Hidden`] so the
    /// per-cell loops don't have to know the rules.
    fn cursor_state(
        &self,
        terminal: &Terminal,
    ) -> CursorRenderState {
        if terminal.active.offset != 0 || !terminal.active.cursor_visible {
            return CursorRenderState::Hidden;
        }
        let style = terminal.cursor_style;
        if style.blink {
            // Square wave: half the period on, half off. `as_secs_f32` keeps
            // the math out of integer overflow territory for long sessions.
            let elapsed = self.started.elapsed().as_secs_f32();
            let half = CURSOR_BLINK_HALF_PERIOD.as_secs_f32();
            let phase = (elapsed / half) as u64;
            if phase & 1 == 1 {
                return CursorRenderState::Hidden;
            }
        }
        CursorRenderState::Visible {
            row: terminal.active.cursor.row,
            col: terminal.active.cursor.col,
            shape: style.shape,
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Some(data) = self.pipeline_cache.get_data()
            && let Some(path) = pipeline_cache_path()
        {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, data) {
                warn!("failed to save pipeline cache: {e}");
            }
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
    /// True iff the cursor is a visible block at this cell — both sides of
    /// the inversion (bg quad, fg glyph) consult this.
    fn is_block_at(
        self,
        row: u32,
        col: u32,
    ) -> bool {
        matches!(
            self,
            CursorRenderState::Visible {
                row: r,
                col: c,
                shape: CursorShape::Block,
            } if r == row && c == col,
        )
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
    terminal: &Terminal,
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

    const SUCCESS: [u8; 3] = [80, 200, 120];
    const FAILURE: [u8; 3] = [220, 80, 80];
    const RUNNING: [u8; 3] = [140, 140, 140];

    for row_idx in 0..terminal.viewport.rows {
        let row = terminal.visible_row(row_idx);
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
