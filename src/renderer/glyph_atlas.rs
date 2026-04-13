//! Glyph atlas: a single 2D texture packed with rasterized glyphs, backed by
//! a [`ShelfPacker`] and an LRU cache (`evictor::Lru`).
//!
//! `evictor` has no eviction callback, so eviction is driven explicitly: on
//! allocation failure we `pop()` the LRU entry and return its region to the
//! packer, and before insert we pop again if the cache is at capacity.

use std::num::NonZeroU64;
use std::num::NonZeroUsize;

use evictor::Lru;

use crate::font::FontSystem;
use crate::font::RasterizedGlyph;
use crate::renderer::shelf::Allocation;
use crate::renderer::shelf::ShelfPacker;

pub const ATLAS_SIZE: u32 = 1024;
const CACHE_CAPACITY: usize = 2048;

pub type GlyphKey = (usize, u16);

/// A cached glyph: its atlas region plus the font metrics needed to position
/// the quad. Empty glyphs (zero-size whitespace) carry no allocation.
#[derive(Clone, Copy)]
pub struct GlyphSlot {
    pub bearing_x: i32,
    pub bearing_y: i32,
    /// `None` for empty glyphs, which never consume atlas space.
    alloc: Option<Allocation>,
}

impl GlyphSlot {
    pub fn is_empty(&self) -> bool {
        self.alloc.is_none()
    }

    pub fn x(&self) -> u32 {
        self.alloc.map_or(0, |a| a.x)
    }

    pub fn y(&self) -> u32 {
        self.alloc.map_or(0, |a| a.y)
    }

    pub fn width(&self) -> u32 {
        self.alloc.map_or(0, |a| a.width)
    }

    pub fn height(&self) -> u32 {
        self.alloc.map_or(0, |a| a.height)
    }
}

pub struct GlyphAtlas {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    bind_group_layout: wgpu::BindGroupLayout,
    cache: Lru<GlyphKey, GlyphSlot>,
    packer: ShelfPacker,
}

impl GlyphAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
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

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glyph_atlas_layout"),
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

        let size_buffer = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("glyph_atlas_size"),
                contents: bytemuck::cast_slice(&[ATLAS_SIZE as f32, ATLAS_SIZE as f32]),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyph_atlas_bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: size_buffer.as_entire_binding(),
                },
            ],
        });

        Self {
            texture,
            bind_group,
            bind_group_layout,
            cache: Lru::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap()),
            packer: ShelfPacker::new(ATLAS_SIZE, 1),
        }
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    /// Look up a glyph in the cache or rasterize and upload on miss. Returns
    /// `None` only if the glyph is larger than the atlas itself.
    pub fn ensure_cached(
        &mut self,
        queue: &wgpu::Queue,
        font_system: &FontSystem,
        font_index: usize,
        glyph_id: u16,
    ) -> Option<GlyphSlot> {
        let key = (font_index, glyph_id);

        if let Some(slot) = self.cache.get(&key).copied() {
            return Some(slot);
        }

        let glyph = font_system.rasterize_glyph(font_index, glyph_id);

        if glyph.width == 0 || glyph.height == 0 {
            let slot = GlyphSlot {
                bearing_x: glyph.bearing_x,
                bearing_y: glyph.bearing_y,
                alloc: None,
            };
            make_room_in_cache(&mut self.cache, &mut self.packer);
            self.cache.insert(key, slot);
            return Some(slot);
        }

        let alloc = loop {
            if let Some(a) = self.packer.allocate(glyph.width, glyph.height) {
                break a;
            }
            // Atlas is full; free the LRU entry and retry. Give up if the
            // cache is empty — the glyph simply cannot fit.
            if !evict_one(&mut self.cache, &mut self.packer) {
                log::warn!(
                    "glyph {glyph_id} too large for atlas ({}x{})",
                    glyph.width,
                    glyph.height
                );
                return None;
            }
        };

        upload_glyph(queue, &self.texture, &alloc, &glyph);

        let slot = GlyphSlot {
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            alloc: Some(alloc),
        };
        make_room_in_cache(&mut self.cache, &mut self.packer);
        self.cache.insert(key, slot);
        Some(slot)
    }
}

fn evict_one(
    cache: &mut Lru<GlyphKey, GlyphSlot>,
    packer: &mut ShelfPacker,
) -> bool {
    match cache.pop() {
        Some((_, slot)) => {
            if let Some(alloc) = slot.alloc {
                packer.free(&alloc);
            }
            true
        }
        None => false,
    }
}

/// Make room for one insert. Evictor drops evicted entries silently, so we
/// pop ourselves and free the atlas region before calling `insert`.
fn make_room_in_cache(
    cache: &mut Lru<GlyphKey, GlyphSlot>,
    packer: &mut ShelfPacker,
) {
    while cache.len() >= cache.capacity() {
        if !evict_one(cache, packer) {
            break;
        }
    }
}

fn upload_glyph(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    alloc: &Allocation,
    glyph: &RasterizedGlyph,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: alloc.x,
                y: alloc.y,
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
