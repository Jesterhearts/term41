//! Glyph atlas: lazily-created 512² texture pages packed with rasterized
//! glyphs, each backed by a [`ShelfPacker`] and a shared LRU cache
//! (`evictor::Lru`).
//!
//! Two separate resources constrain the atlas: currently-created pages (which
//! can fill up even with the cache well under capacity) and the cache's own
//! entry limit. On page exhaustion we explicitly `pop()` the LRU and free its
//! region in a retry loop. On cache overflow we let `insert()` handle eviction
//! and free the returned entry's packer region inline.

use std::num::NonZeroU64;
use std::num::NonZeroUsize;

use evictor::Lru;
use font41::FontSystem;
use font41::RasterizedGlyph;

use crate::renderer::shelf::Allocation;
use crate::renderer::shelf::ShelfPacker;

pub const ATLAS_SIZE: u32 = 512;
// Keep worst-case glyph texture memory equal to the old eager 2048² atlas
// while letting the common ASCII path start at a single 512² page.
const MAX_ATLAS_PAGES: usize = 16;
const CACHE_CAPACITY: usize = 16384;
const PADDING: u32 = 4;
const X_OFFSET: u32 = PADDING / 2;
const Y_OFFSET: u32 = PADDING / 2;

/// `(font_index, glyph_id, cells_wide, synthetic_bold, drcs_geometry)`. Cluster
/// span is part of the key because colour rasterisers size their output to the
/// cluster's visual footprint — the same `glyph_id` rendered at width 1
/// versus width 2 yields different bitmaps. The trailing bool distinguishes
/// the synthetic-bold variant of a colour glyph (dilated coverage) from
/// the unmodified raster, so the same glyph can live twice in the atlas
/// without colliding under the same key.
pub type GlyphKey = (usize, u16, u8, bool, Option<font41::DrcsGeometryClass>);

/// A cached glyph: its atlas region plus the font metrics needed to position
/// the quad. Empty glyphs (zero-size whitespace) carry no allocation.
#[derive(Clone, Copy)]
pub struct GlyphSlot {
    pub page_index: usize,
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

struct GlyphAtlasPage {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    _size_buffer: wgpu::Buffer,
    packer: ShelfPacker,
}

pub struct GlyphAtlas {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pages: Vec<GlyphAtlasPage>,
    cache: Lru<GlyphKey, GlyphSlot>,
    generation: u64,
}

impl GlyphAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
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

        Self {
            bind_group_layout,
            sampler,
            pages: Vec::new(),
            cache: Lru::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap()),
            generation: 0,
        }
    }

    /// Discard all cached glyphs. Called when the DPI scale factor changes
    /// so every glyph is re-rasterized at the new resolution.
    pub fn clear(&mut self) {
        self.cache = Lru::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap());
        self.pages.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn bind_group(
        &self,
        page_index: usize,
    ) -> Option<&wgpu::BindGroup> {
        self.pages.get(page_index).map(|page| &page.bind_group)
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
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_system: &FontSystem,
        font_index: usize,
        glyph_id: u16,
        cells_wide: u8,
        synthetic_bold: bool,
        drcs: Option<(font41::DrcsGeometryClass, font41::DrcsGlyphMap)>,
    ) -> Option<GlyphSlot> {
        // Outline glyphs never take the synthetic-bold path, so collapse the
        // key bit to avoid double-caching the same raster under two keys.
        let synthetic_bold = synthetic_bold && font_system.font_is_color(font_index);
        let key = (
            font_index,
            glyph_id,
            cells_wide,
            synthetic_bold,
            drcs.as_ref().map(|(g, _)| *g),
        );

        if let Some(slot) = self.cache.get(&key).copied() {
            return Some(slot);
        }

        let _drcs =
            drcs.map(|(geometry, glyphs)| font41::set_drcs_context(Some(geometry), Some(glyphs)));
        let mut glyph = font_system.rasterize_glyph(font_index, glyph_id, cells_wide as u32);
        if synthetic_bold {
            dilate_alpha(&mut glyph);
        }

        if glyph.width == 0 || glyph.height == 0 {
            let slot = GlyphSlot {
                page_index: 0,
                bearing_x: glyph.bearing_x,
                bearing_y: glyph.bearing_y,
                is_color: glyph.is_color,
                alloc: None,
            };
            if release_evicted(&mut self.pages, self.cache.insert(key, slot)) {
                self.generation = self.generation.wrapping_add(1);
            }
            return Some(slot);
        }

        if glyph.width + PADDING > ATLAS_SIZE || glyph.height + PADDING > ATLAS_SIZE {
            warn!(
                "glyph {glyph_id} too large for atlas page ({}x{})",
                glyph.width, glyph.height
            );
            return None;
        }

        let tile = loop {
            if let Some(tile) = allocate_glyph(self, device, glyph.width, glyph.height) {
                break tile;
            }
            // Allocated pages are full; free the LRU entry and retry. Give up
            // if the cache is empty, the page cap has been reached, and the
            // glyph still does not fit.
            if !evict_one(&mut self.cache, &mut self.pages) {
                warn!(
                    "glyph atlas exhausted before allocating glyph {glyph_id} ({}x{})",
                    glyph.width, glyph.height
                );
                return None;
            }
            self.generation = self.generation.wrapping_add(1);
        };

        upload_glyph(
            queue,
            &self.pages[tile.page_index].texture,
            &tile.alloc,
            &glyph,
        );

        let slot = GlyphSlot {
            page_index: tile.page_index,
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            is_color: glyph.is_color,
            alloc: Some(tile.alloc),
        };
        if release_evicted(&mut self.pages, self.cache.insert(key, slot)) {
            self.generation = self.generation.wrapping_add(1);
        }
        Some(slot)
    }
}

#[derive(Clone, Copy)]
struct GlyphTile {
    page_index: usize,
    alloc: Allocation,
}

fn allocate_glyph(
    atlas: &mut GlyphAtlas,
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> Option<GlyphTile> {
    let width = width + PADDING;
    let height = height + PADDING;

    if let Some(tile) = allocate_in_existing_pages(atlas, width, height) {
        return Some(tile);
    }
    if width > ATLAS_SIZE || height > ATLAS_SIZE || atlas.pages.len() >= MAX_ATLAS_PAGES {
        return None;
    }

    let page_index = atlas.pages.len();
    atlas.pages.push(create_page(
        device,
        &atlas.bind_group_layout,
        &atlas.sampler,
        page_index,
    ));
    atlas.pages[page_index]
        .packer
        .allocate(width, height)
        .map(|alloc| GlyphTile { page_index, alloc })
}

fn allocate_in_existing_pages(
    atlas: &mut GlyphAtlas,
    width: u32,
    height: u32,
) -> Option<GlyphTile> {
    for (page_index, page) in atlas.pages.iter_mut().enumerate() {
        if let Some(alloc) = page.packer.allocate(width, height) {
            return Some(GlyphTile { page_index, alloc });
        }
    }
    None
}

fn create_page(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    page_index: usize,
) -> GlyphAtlasPage {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph_atlas_page"),
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
    let size_buffer = wgpu::util::DeviceExt::create_buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("glyph_atlas_size"),
            contents: bytemuck::cast_slice(&[ATLAS_SIZE as f32, ATLAS_SIZE as f32]),
            usage: wgpu::BufferUsages::UNIFORM,
        },
    );

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("glyph_atlas_page_bg"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: size_buffer.as_entire_binding(),
            },
        ],
    });

    tracing::debug!(page_index, "created glyph atlas page");

    GlyphAtlasPage {
        texture,
        bind_group,
        _size_buffer: size_buffer,
        packer: ShelfPacker::new(ATLAS_SIZE),
    }
}

fn evict_one(
    cache: &mut Lru<GlyphKey, GlyphSlot>,
    pages: &mut [GlyphAtlasPage],
) -> bool {
    match cache.pop() {
        Some((_, slot)) => {
            free_slot(pages, &slot);
            true
        }
        None => false,
    }
}

/// Return the packer region owned by a cache entry that `insert()` just
/// evicted, if any. Empty-glyph slots carry no allocation.
fn release_evicted(
    pages: &mut [GlyphAtlasPage],
    evicted: Option<GlyphSlot>,
) -> bool {
    if let Some(slot) = evicted {
        free_slot(pages, &slot);
        true
    } else {
        false
    }
}

fn free_slot(
    pages: &mut [GlyphAtlasPage],
    slot: &GlyphSlot,
) {
    if let Some(alloc) = slot.alloc
        && let Some(page) = pages.get_mut(slot.page_index)
    {
        page.packer.free(&alloc);
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
