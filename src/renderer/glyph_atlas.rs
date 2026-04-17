//! Glyph atlas: a single 2D texture packed with rasterized glyphs, backed by
//! a [`ShelfPacker`] and an LRU cache (`evictor::Lru`).
//!
//! Two separate resources constrain the atlas: the 1024² packer (which can
//! fill up even with the cache well under capacity) and the cache's own
//! entry limit. On packer exhaustion we explicitly `pop()` the LRU and
//! free its region in a retry loop. On cache overflow we let `insert()`
//! handle eviction and free the returned entry's packer region inline.

use std::num::NonZeroU64;
use std::num::NonZeroUsize;

use evictor::Lru;
use font41::FontSystem;
use font41::RasterizedGlyph;

use crate::renderer::shelf::Allocation;
use crate::renderer::shelf::ShelfPacker;

pub const ATLAS_SIZE: u32 = 1024;
const CACHE_CAPACITY: usize = 2048;
const PADDING: u32 = 4;
const X_OFFSET: u32 = PADDING / 2;
const Y_OFFSET: u32 = PADDING / 2;

/// `(font_index, glyph_id, cells_wide, synthetic_bold)`. Cluster span is
/// part of the key because colour rasterisers size their output to the
/// cluster's visual footprint — the same `glyph_id` rendered at width 1
/// versus width 2 yields different bitmaps. The trailing bool distinguishes
/// the synthetic-bold variant of a colour glyph (dilated coverage) from
/// the unmodified raster, so the same glyph can live twice in the atlas
/// without colliding under the same key.
pub type GlyphKey = (usize, u16, u8, bool);

/// A cached glyph: its atlas region plus the font metrics needed to position
/// the quad. Empty glyphs (zero-size whitespace) carry no allocation.
#[derive(Clone, Copy)]
pub struct GlyphSlot {
    pub bearing_x: i32,
    pub bearing_y: i32,
    /// True for color glyphs (COLR, emoji bitmaps, …) — the shader samples
    /// the atlas RGBA directly instead of tinting by the fg color.
    pub is_color: bool,
    /// `None` for empty glyphs, which never consume atlas space.
    alloc: Option<Allocation>,
}

impl GlyphSlot {
    pub fn is_empty(&self) -> bool {
        self.alloc.is_none()
    }

    pub fn x(&self) -> u32 {
        self.alloc.map_or(0, |a| a.x + X_OFFSET)
    }

    pub fn y(&self) -> u32 {
        self.alloc.map_or(0, |a| a.y + Y_OFFSET)
    }

    pub fn width(&self) -> u32 {
        self.alloc.map_or(0, |a| a.width - PADDING)
    }

    pub fn height(&self) -> u32 {
        self.alloc.map_or(0, |a| a.height - PADDING)
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
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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

    /// Discard all cached glyphs. Called when the DPI scale factor changes
    /// so every glyph is re-rasterized at the new resolution.
    pub fn clear(&mut self) {
        self.cache = Lru::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap());
        self.packer = ShelfPacker::new(ATLAS_SIZE, 1);
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    /// Look up a glyph in the cache or rasterize and upload on miss. Returns
    /// `None` only if the glyph is larger than the atlas itself.
    ///
    /// `synthetic_bold` is only honoured when the underlying face is a colour
    /// font — stems on outline fonts would smear unpleasantly under a blind
    /// bitmap dilation, so the caller's request is silently dropped there.
    pub fn ensure_cached(
        &mut self,
        queue: &wgpu::Queue,
        font_system: &FontSystem,
        font_index: usize,
        glyph_id: u16,
        cells_wide: u8,
        synthetic_bold: bool,
    ) -> Option<GlyphSlot> {
        // Outline glyphs never take the synthetic-bold path, so collapse the
        // key bit to avoid double-caching the same raster under two keys.
        let synthetic_bold = synthetic_bold && font_system.font_is_color(font_index);
        let key = (font_index, glyph_id, cells_wide, synthetic_bold);

        if let Some(slot) = self.cache.get(&key).copied() {
            return Some(slot);
        }

        let mut glyph = font_system.rasterize_glyph(font_index, glyph_id, cells_wide as u32);
        if synthetic_bold {
            dilate_alpha(&mut glyph);
        }

        if glyph.width == 0 || glyph.height == 0 {
            let slot = GlyphSlot {
                bearing_x: glyph.bearing_x,
                bearing_y: glyph.bearing_y,
                is_color: glyph.is_color,
                alloc: None,
            };
            release_evicted(&mut self.packer, self.cache.insert(key, slot));
            return Some(slot);
        }

        let alloc = loop {
            if let Some(a) = self
                .packer
                .allocate(glyph.width + PADDING, glyph.height + PADDING)
            {
                break a;
            }
            // Atlas is full; free the LRU entry and retry. Give up if the
            // cache is empty — the glyph simply cannot fit.
            if !evict_one(&mut self.cache, &mut self.packer) {
                warn!(
                    "glyph {glyph_id} too large for atlas ({}x{})",
                    glyph.width, glyph.height
                );
                return None;
            }
        };

        upload_glyph(queue, &self.texture, &alloc, &glyph);

        let slot = GlyphSlot {
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            is_color: glyph.is_color,
            alloc: Some(alloc),
        };
        release_evicted(&mut self.packer, self.cache.insert(key, slot));
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

/// Return the packer region owned by a cache entry that `insert()` just
/// evicted, if any. Empty-glyph slots carry no allocation.
fn release_evicted(
    packer: &mut ShelfPacker,
    evicted: Option<GlyphSlot>,
) {
    if let Some(slot) = evicted
        && let Some(alloc) = slot.alloc
    {
        packer.free(&alloc);
    }
}

/// Horizontally dilate an RGBA glyph's coverage by one pixel to fake bold
/// weight. Colour rasters that look like `[r,g,b,a]` per pixel get each
/// channel's value max'd with its left/right neighbour, which thickens
/// strokes without disturbing hue. The result is intentionally crude —
/// outline paths should come from a real bold font when one exists; this
/// is only the fallback for COLR/CBDT/sbix glyphs (usually emoji or icon
/// fonts) where a real bold variant almost never ships.
fn dilate_alpha(glyph: &mut RasterizedGlyph) {
    let w = glyph.width as usize;
    let h = glyph.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let src = glyph.bitmap.clone();
    let dst = &mut glyph.bitmap;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            for c in 0..4 {
                let here = src[i + c];
                let left = if x > 0 { src[i - 4 + c] } else { 0 };
                let right = if x + 1 < w { src[i + 4 + c] } else { 0 };
                dst[i + c] = here.max(left).max(right);
            }
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
                x: alloc.x + X_OFFSET,
                y: alloc.y + Y_OFFSET,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &glyph.bitmap,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(glyph.width * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: glyph.width,
            height: glyph.height,
            depth_or_array_layers: 1,
        },
    );
}
