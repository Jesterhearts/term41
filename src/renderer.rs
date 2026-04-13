pub mod glyph_atlas;
pub mod image_atlas;
mod shelf;

use std::num::NonZeroU64;
use std::sync::Arc;

use palette::Srgb;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::font::FontSystem;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::image_atlas::IMAGE_ATLAS_SIZE;
use crate::renderer::image_atlas::ImageAtlas;
use crate::terminal::Terminal;
use crate::terminal::default_fg;

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
            cache: None,
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
            glyph_atlas,
            image_atlas,
            bg_alpha: (opacity.clamp(0.0, 1.0) * 255.0) as u8,
        };

        renderer.update_screen_size(size);

        // Pre-cache printable ASCII glyphs so the first few frames don't
        // rasterize them on demand.
        let ascii_chars: Vec<char> = (' '..='~').collect();
        let shaped = font_system.shape_row(&ascii_chars);
        for sg in &shaped {
            renderer.glyph_atlas.ensure_cached(
                &renderer.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
            );
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
                // Selected cells invert fg/bg — keeps selection visible
                // against any theme without adding a dedicated highlight
                // color or pipeline.
                let selected = terminal.is_cell_selected(row, col);
                let bg_cell = if selected {
                    &grid_row.fg[col as usize]
                } else {
                    &grid_row.bg[col as usize]
                };
                let bg_color = pack_color(bg_cell, self.bg_alpha);
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
                let slot = match self.glyph_atlas.ensure_cached(
                    &self.queue,
                    font_system,
                    sg.font_index,
                    sg.glyph_id,
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
                let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
                let gw = sw as f32;
                let gh = sh as f32;

                let fg_cell = if terminal.is_cell_selected(row, sg.col as u32) {
                    &grid_row.bg[sg.col as usize]
                } else {
                    &grid_row.fg[sg.col as usize]
                };
                let fg_color = pack_color(fg_cell, 255);
                let fi = fg_vertices.len() as u32;
                fg_vertices.extend_from_slice(&[
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color: fg_color,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: fg_color,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: fg_color,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
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

        for vis in terminal.visible_images() {
            let entry = match self
                .image_atlas
                .ensure_cached(&self.queue, vis.id, vis.image)
            {
                Some(e) => e,
                None => continue,
            };

            let base_x = vis.screen_col as f32 * cell_w;
            let base_y = vis.screen_row as f32 * cell_h;

            for tile in &entry.tiles {
                let a = &tile.alloc;
                let x = base_x + tile.src_x as f32;
                let y = base_y + tile.src_y as f32;
                let w = a.width as f32;
                let h = a.height as f32;

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
}
