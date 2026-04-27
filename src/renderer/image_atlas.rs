//! Image atlas: lazily-created 2048² texture pages packed with images
//! (potentially tiled), each backed by a [`ShelfPacker`] and a shared LRU
//! cache.
//!
//! Images that exceed a page are split into tiles of at most
//! `IMAGE_ATLAS_SIZE` on each side; each tile is packed independently. The
//! renderer walks the cached tile list and emits one quad per tile.
//!
//! Eviction has two triggers: all allocated pages are full and the page limit
//! has been reached (handled by popping the LRU and returning each of its tile
//! regions to its page until the new allocation fits), and a full cache
//! (handled by `insert()` returning the evicted entry, whose tiles we then
//! free). The page cap keeps a single terminal process from pinning excessive
//! GPU texture memory during long inline-image or animated-image sessions.

use std::num::NonZeroUsize;

use evictor::Lru;
use image41::DecodedImage;

use crate::renderer::shelf::Allocation;
use crate::renderer::shelf::ShelfPacker;

pub const IMAGE_ATLAS_SIZE: u32 = 2048;
const MAX_ATLAS_PAGES: usize = 12;
const CACHE_CAPACITY: usize = 256;

/// A single rectangular tile of an image in the atlas.
///
/// Images smaller than [`IMAGE_ATLAS_SIZE`] on both axes produce exactly one
/// tile covering the whole image; larger images produce a grid.
#[derive(Clone, Copy)]
pub struct ImageTile {
    pub page_index: usize,
    pub alloc: Allocation,
    /// Offset of this tile's top-left within the source image (in pixels).
    pub src_x: u32,
    pub src_y: u32,
}

pub struct ImageEntry {
    pub tiles: Vec<ImageTile>,
}

struct ImageAtlasPage {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    packer: ShelfPacker,
}

pub struct ImageAtlas {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pages: Vec<ImageAtlasPage>,
    cache: Lru<u64, ImageEntry>,
}

impl ImageAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image_atlas_layout"),
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
            ],
        });

        Self {
            bind_group_layout,
            sampler,
            pages: Vec::new(),
            cache: Lru::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap()),
        }
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

    /// Look up a specific frame of an image in the cache, tiling and
    /// uploading on miss. Returns `None` only if a tile cannot be packed
    /// even after full eviction (impossible given that tiles are bounded
    /// by the atlas size) or the requested frame index is out of range.
    ///
    /// Static images pass `frame_index = 0`; animated images pass the
    /// current frame index, and each frame is packed independently under
    /// a composite `(image_id, frame_index)` cache key. This means a
    /// 20-frame animation occupies up to 20 atlas entries, and LRU
    /// eviction can rotate them like any other cached image.
    pub fn ensure_cached(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image_id: u64,
        frame_index: usize,
        image: &DecodedImage,
    ) -> Option<&ImageEntry> {
        let frame = image.frames.get(frame_index)?;
        let key = atlas_key(image_id, frame_index);
        if self.cache.contains_key(&key) {
            return self.cache.get(&key);
        }

        let regions = tile_regions(image.width, image.height, IMAGE_ATLAS_SIZE);
        let mut tiles: Vec<ImageTile> = Vec::with_capacity(regions.len());

        for region in &regions {
            let tile = loop {
                if let Some(tile) = allocate_tile(self, device, region) {
                    break tile;
                }
                if !evict_one(&mut self.cache, &mut self.pages) {
                    // All allocated pages are full, the page limit has been
                    // reached, and no cached entry exists to evict.
                    free_tiles(&mut self.pages, &tiles);
                    warn!(
                        "image {image_id} frame {frame_index} tile {}x{} does not fit in atlas",
                        region.width, region.height
                    );
                    return None;
                }
            };
            upload_tile(
                queue,
                &self.pages[tile.page_index].texture,
                &tile,
                image.width,
                &frame.pixels,
            );
            tiles.push(tile);
        }

        let evicted = self.cache.insert(key, ImageEntry { tiles });
        if let Some(entry) = evicted {
            free_tiles(&mut self.pages, &entry.tiles);
        }
        self.cache.get(&key)
    }
}

fn allocate_tile(
    atlas: &mut ImageAtlas,
    device: &wgpu::Device,
    region: &TileRegion,
) -> Option<ImageTile> {
    if let Some(tile) = allocate_in_existing_pages(atlas, region) {
        return Some(tile);
    }
    if atlas.pages.len() >= MAX_ATLAS_PAGES {
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
        .allocate(region.width, region.height)
        .map(|alloc| ImageTile {
            page_index,
            alloc,
            src_x: region.src_x,
            src_y: region.src_y,
        })
}

fn allocate_in_existing_pages(
    atlas: &mut ImageAtlas,
    region: &TileRegion,
) -> Option<ImageTile> {
    for (page_index, page) in atlas.pages.iter_mut().enumerate() {
        if let Some(alloc) = page.packer.allocate(region.width, region.height) {
            return Some(ImageTile {
                page_index,
                alloc,
                src_x: region.src_x,
                src_y: region.src_y,
            });
        }
    }
    None
}

fn create_page(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    page_index: usize,
) -> ImageAtlasPage {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("image_atlas_page"),
        size: wgpu::Extent3d {
            width: IMAGE_ATLAS_SIZE,
            height: IMAGE_ATLAS_SIZE,
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
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("image_atlas_page_bg"),
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
        ],
    });

    tracing::debug!(page_index, "created image atlas page");

    ImageAtlasPage {
        texture,
        bind_group,
        packer: ShelfPacker::new(IMAGE_ATLAS_SIZE),
    }
}

/// Pack `(image_id, frame_index)` into the atlas's `u64` cache key. The
/// low 16 bits hold the frame index (max 65k frames — far beyond any
/// realistic animation); the rest holds the image id. The layout keeps
/// static images (frame 0) from colliding with animated images since the
/// shift puts every image at a different base offset.
fn atlas_key(
    image_id: u64,
    frame_index: usize,
) -> u64 {
    (image_id << 16) | (frame_index as u64 & 0xFFFF)
}

/// A sub-rectangle of a source image that fits within one atlas tile.
#[derive(Clone, Copy)]
struct TileRegion {
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
}

/// Split an image into tiles no larger than `max` on each side. Images that
/// already fit produce a single tile covering the whole image.
fn tile_regions(
    image_width: u32,
    image_height: u32,
    max: u32,
) -> Vec<TileRegion> {
    let mut tiles = Vec::new();
    let mut y = 0;
    while y < image_height {
        let h = (image_height - y).min(max);
        let mut x = 0;
        while x < image_width {
            let w = (image_width - x).min(max);
            tiles.push(TileRegion {
                src_x: x,
                src_y: y,
                width: w,
                height: h,
            });
            x += w;
        }
        y += h;
    }
    tiles
}

fn evict_one(
    cache: &mut Lru<u64, ImageEntry>,
    pages: &mut [ImageAtlasPage],
) -> bool {
    match cache.pop() {
        Some((_, entry)) => {
            free_tiles(pages, &entry.tiles);
            true
        }
        None => false,
    }
}

fn free_tiles(
    pages: &mut [ImageAtlasPage],
    tiles: &[ImageTile],
) {
    for tile in tiles {
        if let Some(page) = pages.get_mut(tile.page_index) {
            page.packer.free(&tile.alloc);
        }
    }
}

fn upload_tile(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    tile: &ImageTile,
    image_width: u32,
    pixels: &[u8],
) {
    // Point write_texture at the sub-rectangle by offsetting into the source
    // buffer; `bytes_per_row` stays at the full image stride so wgpu walks
    // the right distance between rows.
    let bytes_per_pixel = 4;
    let row_stride = image_width * bytes_per_pixel;
    let offset = (tile.src_y * image_width + tile.src_x) as usize * bytes_per_pixel as usize;

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: tile.alloc.x,
                y: tile.alloc.y,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &pixels[offset..],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row_stride),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: tile.alloc.width,
            height: tile.alloc.height,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_image_produces_single_tile() {
        let tiles = tile_regions(100, 50, 2048);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].width, 100);
        assert_eq!(tiles[0].height, 50);
        assert_eq!((tiles[0].src_x, tiles[0].src_y), (0, 0));
    }

    #[test]
    fn exact_multiple_tiles_evenly() {
        let tiles = tile_regions(4096, 2048, 2048);
        assert_eq!(tiles.len(), 2);
        assert_eq!((tiles[0].src_x, tiles[0].width), (0, 2048));
        assert_eq!((tiles[1].src_x, tiles[1].width), (2048, 2048));
    }

    #[test]
    fn non_multiple_produces_remainder_tiles() {
        let tiles = tile_regions(3000, 2500, 2048);
        // 3000 -> 2048 + 952; 2500 -> 2048 + 452; total 4 tiles.
        assert_eq!(tiles.len(), 4);
        assert!(tiles.iter().any(|t| t.width == 952 && t.height == 2048));
        assert!(tiles.iter().any(|t| t.width == 952 && t.height == 452));
        assert!(tiles.iter().any(|t| t.width == 2048 && t.height == 452));
    }

    #[test]
    fn tiles_cover_whole_image_without_overlap() {
        let tiles = tile_regions(5000, 3000, 2048);
        let total: u64 = tiles.iter().map(|t| t.width as u64 * t.height as u64).sum();
        assert_eq!(total, 5000u64 * 3000u64);
    }
}
