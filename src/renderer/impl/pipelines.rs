use wgpu::TextureFormat;

use super::BgVertex;
use super::FgVertex;
use super::IMAGE_DEPTH_FORMAT;
use super::ImageVertex;
#[cfg(feature = "vulkan")]
use super::pipeline_cache_path;
use crate::renderer::background::BgImageVertex;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::image_atlas::ImageAtlas;

pub(super) struct FgPipeline(pub(super) wgpu::RenderPipeline);
pub(super) struct BgPipeline(pub(super) wgpu::RenderPipeline);
pub(super) struct ImagePipeline(pub(super) wgpu::RenderPipeline);
pub(super) struct BgImagePipeline(pub(super) wgpu::RenderPipeline);
pub(super) struct LayerPipeline(pub(super) wgpu::RenderPipeline);

pub(super) fn build_pipeline_for_format(
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
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/bg.wgsl").into()),
    });
    let fg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fg_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fg.wgsl").into()),
    });
    let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("image_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/image.wgsl").into()),
    });
    let layer_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("layer_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/layer.wgsl").into()),
    });
    let bg_image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bg_image_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/bg_image.wgsl").into()),
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
                    2 => Float32,
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
        depth_stencil: Some(wgpu::DepthStencilState {
            format: IMAGE_DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
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
                    2 => Float32,
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
