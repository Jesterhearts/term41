use std::collections::HashMap;
use std::collections::HashSet;
use std::num::NonZeroU64;
use std::sync::Arc;

use palette::Srgb;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::font::FontSystem;
use crate::font::RasterizedGlyph;
use crate::sixel::SixelImage;
use crate::terminal::Terminal;
use crate::terminal::default_fg;

const ATLAS_SIZE: u32 = 1024;
const IMAGE_ATLAS_SIZE: u32 = 2048;
const IMAGE_ATLAS_LAYERS: u32 = 64;

/// Packed vertex for background quads: position + color.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BgVertex {
    pos: [f32; 2],
    color: u32,
}

/// Packed vertex for foreground (glyph) quads: position + UV + color.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FgVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: u32,
}

/// Packed vertex for image quads: position + UV + layer + transparency flag.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageVertex {
    pos: [f32; 2],
    /// xy = UV coords, z = atlas layer, w = 1.0 if image has transparent bg.
    uv_layer: [f32; 3],
}

/// Location of a glyph in the atlas texture.
#[derive(Clone, Copy)]
struct AtlasEntry {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    bearing_x: i32,
    bearing_y: i32,
}

/// Location of an image in the image atlas texture array.
#[derive(Clone, Copy)]
struct ImageAtlasEntry {
    layer: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Per-layer allocation state for row-based packing.
struct LayerState {
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
}

fn pack_color(
    c: &Srgb<u8>,
    alpha: u8,
) -> u32 {
    u32::from_be_bytes([c.red, c.green, c.blue, alpha])
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

    atlas_texture: wgpu::Texture,
    atlas_bind_group: wgpu::BindGroup,

    image_atlas_texture: wgpu::Texture,
    image_bind_group: wgpu::BindGroup,

    glyph_cache: HashMap<(usize, u16), AtlasEntry>,
    atlas_cursor_x: u32,
    atlas_cursor_y: u32,
    atlas_row_height: u32,
    bg_alpha: u8,

    // Image atlas state.
    image_layers: Vec<LayerState>,
    image_entries: HashMap<u64, ImageAtlasEntry>,
    uploaded_image_ids: HashSet<u64>,
}

impl Renderer {
    pub async fn new(
        window: Arc<Window>,
        font_system: &mut FontSystem,
        _terminal: &Terminal,
        opacity: f32,
    ) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window).expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("request adapter");

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("request device");

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
            present_mode: wgpu::PresentMode::Fifo,
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

        // ---- Glyph atlas ----
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph_atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(8),
                    },
                    count: None,
                },
            ],
        });

        let atlas_size_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atlas_size"),
            contents: bytemuck::cast_slice(&[ATLAS_SIZE as f32, ATLAS_SIZE as f32]),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas_bg"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: atlas_size_buffer.as_entire_binding(),
                },
            ],
        });

        // ---- Image atlas (2D texture array) ----
        let image_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image_atlas"),
            size: wgpu::Extent3d {
                width: IMAGE_ATLAS_SIZE,
                height: IMAGE_ATLAS_SIZE,
                depth_or_array_layers: IMAGE_ATLAS_LAYERS,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let image_atlas_view = image_atlas_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let image_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image_bg"),
            layout: &image_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&image_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&image_sampler),
                },
            ],
        });

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
            cache: None,
        });

        // ---- Foreground pipeline ----
        let fg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fg_pipeline_layout"),
            bind_group_layouts: &[Some(&screen_size_layout), Some(&atlas_layout)],
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
                        2 => Uint32
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
            cache: None,
        });

        // ---- Image pipeline ----
        let image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("image_pipeline_layout"),
                bind_group_layouts: &[Some(&screen_size_layout), Some(&image_layout)],
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
            cache: None,
        });

        let mut renderer = Self {
            device,
            queue,
            surface,
            surface_config,
            bg_pipeline,
            fg_pipeline,
            image_pipeline,
            screen_size_buffer,
            screen_size_bind_group,
            atlas_texture,
            atlas_bind_group,
            image_atlas_texture,
            image_bind_group,
            glyph_cache: HashMap::new(),
            atlas_cursor_x: 0,
            atlas_cursor_y: 0,
            atlas_row_height: 0,
            bg_alpha: (opacity.clamp(0.0, 1.0) * 255.0) as u8,
            image_layers: vec![LayerState {
                cursor_x: 0,
                cursor_y: 0,
                row_height: 0,
            }],
            image_entries: HashMap::new(),
            uploaded_image_ids: HashSet::new(),
        };

        renderer.update_screen_size(size);

        // Pre-cache printable ASCII glyphs.
        {
            let ascii_chars: Vec<char> = (' '..='~').collect();
            let shaped = font_system.shape_row(&ascii_chars);
            for sg in &shaped {
                renderer.ensure_glyph_cached(font_system, sg.font_index, sg.glyph_id);
            }
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

    /// Look up a glyph for a character. Uses char→glyph_index cache to skip
    /// shaping on subsequent frames. Only calls FontSystem on first encounter.
    /// Ensure a glyph is cached in the atlas. Rasterizes on first encounter.
    fn ensure_glyph_cached(
        &mut self,
        font_system: &FontSystem,
        font_index: usize,
        glyph_id: u16,
    ) -> Option<AtlasEntry> {
        let key = (font_index, glyph_id);

        if let Some(entry) = self.glyph_cache.get(&key).copied() {
            return Some(entry);
        }

        let glyph = font_system.rasterize_glyph(font_index, glyph_id);

        if glyph.width == 0 || glyph.height == 0 {
            let entry = AtlasEntry {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                bearing_x: glyph.bearing_x,
                bearing_y: glyph.bearing_y,
            };
            self.glyph_cache.insert(key, entry);
            return Some(entry);
        }

        if self.atlas_cursor_x + glyph.width > ATLAS_SIZE {
            self.atlas_cursor_x = 0;
            self.atlas_cursor_y += self.atlas_row_height;
            self.atlas_row_height = 0;
        }

        if self.atlas_cursor_y + glyph.height > ATLAS_SIZE {
            log::warn!("glyph atlas full, cannot cache glyph {glyph_id}");
            return None;
        }

        let entry = AtlasEntry {
            x: self.atlas_cursor_x,
            y: self.atlas_cursor_y,
            width: glyph.width,
            height: glyph.height,
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
        };

        upload_glyph(&self.queue, &self.atlas_texture, &entry, &glyph);

        self.atlas_cursor_x += glyph.width;
        self.atlas_row_height = self.atlas_row_height.max(glyph.height);
        self.glyph_cache.insert(key, entry);

        Some(entry)
    }

    /// Allocate space in the image atlas for an image of the given dimensions.
    fn allocate_image_slot(
        &mut self,
        width: u32,
        height: u32,
    ) -> Option<ImageAtlasEntry> {
        if width > IMAGE_ATLAS_SIZE || height > IMAGE_ATLAS_SIZE {
            log::warn!(
                "sixel image too large for atlas: {width}x{height} (max {IMAGE_ATLAS_SIZE})"
            );
            return None;
        }

        // Try to fit in an existing layer.
        for (layer_idx, layer) in self.image_layers.iter_mut().enumerate() {
            if layer.cursor_x + width > IMAGE_ATLAS_SIZE {
                layer.cursor_x = 0;
                layer.cursor_y += layer.row_height;
                layer.row_height = 0;
            }
            if layer.cursor_y + height <= IMAGE_ATLAS_SIZE {
                let entry = ImageAtlasEntry {
                    layer: layer_idx as u32,
                    x: layer.cursor_x,
                    y: layer.cursor_y,
                    width,
                    height,
                };
                layer.cursor_x += width;
                layer.row_height = layer.row_height.max(height);
                return Some(entry);
            }
        }

        // Need a new layer.
        if self.image_layers.len() as u32 >= IMAGE_ATLAS_LAYERS {
            log::warn!("image atlas full, all {IMAGE_ATLAS_LAYERS} layers used");
            return None;
        }

        let layer_idx = self.image_layers.len() as u32;
        self.image_layers.push(LayerState {
            cursor_x: width,
            cursor_y: 0,
            row_height: height,
        });

        Some(ImageAtlasEntry {
            layer: layer_idx,
            x: 0,
            y: 0,
            width,
            height,
        })
    }

    fn upload_image(
        &self,
        image: &SixelImage,
        entry: &ImageAtlasEntry,
    ) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.image_atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: entry.x,
                    y: entry.y,
                    z: entry.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        terminal: &Terminal,
    ) {
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();

        let mut bg_vertices: Vec<BgVertex> = Vec::new();
        let mut bg_indices: Vec<u32> = Vec::new();
        let mut fg_vertices: Vec<FgVertex> = Vec::new();
        let mut fg_indices: Vec<u32> = Vec::new();

        for row in 0..terminal.viewport.rows {
            let y = row as f32 * cell_h;

            // Background quads for the whole row.
            let grid_row = terminal.visible_row(row);
            for col in 0..terminal.viewport.cols {
                let x = col as f32 * cell_w;
                let bg_color = pack_color(&grid_row.bg[col as usize], self.bg_alpha);
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
            }

            // Shape the entire row for foreground glyphs — borrows &[char] directly.
            let shaped = font_system.shape_row(&grid_row.chars);

            for sg in &shaped {
                let entry = match self.ensure_glyph_cached(font_system, sg.font_index, sg.glyph_id)
                {
                    Some(e) => e,
                    None => continue,
                };

                if entry.width == 0 || entry.height == 0 {
                    continue;
                }

                let gx = sg.col as f32 * cell_w + entry.bearing_x as f32 + sg.x_offset;
                let gy = y + baseline - entry.bearing_y as f32 - sg.y_offset;
                let gw = entry.width as f32;
                let gh = entry.height as f32;

                let fg_color = pack_color(&grid_row.fg[sg.col as usize], 255);
                let fi = fg_vertices.len() as u32;
                fg_vertices.extend_from_slice(&[
                    FgVertex {
                        pos: [gx, gy],
                        uv: [entry.x as f32, entry.y as f32],
                        color: fg_color,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(entry.x + entry.width) as f32, entry.y as f32],
                        color: fg_color,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [entry.x as f32, (entry.y + entry.height) as f32],
                        color: fg_color,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [
                            (entry.x + entry.width) as f32,
                            (entry.y + entry.height) as f32,
                        ],
                        color: fg_color,
                    },
                ]);
                fg_indices.extend_from_slice(&[fi, fi + 1, fi + 2, fi + 2, fi + 1, fi + 3]);
            }
        }

        // Cursor: draw an inverted cell (only when viewing the live terminal).
        if terminal.active.offset == 0 {
            let cx = terminal.active.cursor.col as f32 * cell_w;
            let cy = terminal.active.cursor.row as f32 * cell_h;
            let cursor_color = pack_color(&default_fg(), 255);
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [cx, cy],
                    color: cursor_color,
                },
                BgVertex {
                    pos: [cx + cell_w, cy],
                    color: cursor_color,
                },
                BgVertex {
                    pos: [cx, cy + cell_h],
                    color: cursor_color,
                },
                BgVertex {
                    pos: [cx + cell_w, cy + cell_h],
                    color: cursor_color,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        // ---- Build image quads ----
        let mut image_vertices: Vec<ImageVertex> = Vec::new();
        let mut image_indices: Vec<u32> = Vec::new();

        let mut live_ids = HashSet::<u64>::new();
        for vis in terminal.visible_images() {
            live_ids.insert(vis.id);

            // Upload to atlas on first encounter.
            if !self.uploaded_image_ids.contains(&vis.id)
                && let Some(entry) = self.allocate_image_slot(vis.image.width, vis.image.height)
            {
                self.upload_image(vis.image, &entry);
                self.image_entries.insert(vis.id, entry);
                self.uploaded_image_ids.insert(vis.id);
            }

            if let Some(entry) = self.image_entries.get(&vis.id) {
                let x = vis.screen_col as f32 * cell_w;
                let y = vis.screen_row as f32 * cell_h;
                let w = vis.image.width as f32;
                let h = vis.image.height as f32;

                let u0 = entry.x as f32 / IMAGE_ATLAS_SIZE as f32;
                let v0 = entry.y as f32 / IMAGE_ATLAS_SIZE as f32;
                let u1 = (entry.x + entry.width) as f32 / IMAGE_ATLAS_SIZE as f32;
                let v1 = (entry.y + entry.height) as f32 / IMAGE_ATLAS_SIZE as f32;
                let layer = entry.layer as f32;

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

        // Clean up atlas entries for images no longer in the terminal.
        self.uploaded_image_ids.retain(|id| live_ids.contains(id));
        self.image_entries.retain(|id, _| live_ids.contains(id));

        // ---- Acquire surface texture ----
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            other => {
                log::error!("surface error: {other:?}");
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
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
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
            pass.set_bind_group(1, &self.image_bind_group, &[]);
            pass.set_vertex_buffer(0, img_vbuf.slice(..));
            pass.set_index_buffer(img_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..image_indices.len() as u32, 0, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
    }
}

fn upload_glyph(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    entry: &AtlasEntry,
    glyph: &RasterizedGlyph,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: entry.x,
                y: entry.y,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &glyph.bitmap,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(glyph.width),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: glyph.width,
            height: glyph.height,
            depth_or_array_layers: 1,
        },
    );
}
