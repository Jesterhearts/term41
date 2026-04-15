//! Image atlas: a 2D texture array packed with images (potentially tiled),
//! backed by a [`ShelfPacker`] spanning every layer and an LRU cache.
//!
//! Images that exceed the per-layer size are split into tiles of at most
//! `IMAGE_ATLAS_SIZE` on each side; each tile is packed independently. The
//! renderer walks the cached tile list and emits one quad per tile.
//!
//! `evictor` has no eviction callback, so eviction is driven explicitly: on
//! allocation failure this module pops the LRU entry and returns every tile
//! region to the packer before retrying.

use std::num::NonZeroUsize;

use evictor::Lru;

use crate::renderer::shelf::Allocation;
use crate::renderer::shelf::ShelfPacker;
use crate::sixel::DecodedImage;

pub const IMAGE_ATLAS_SIZE: u32 = 2048;
pub const IMAGE_ATLAS_LAYERS: u32 = 64;
const CACHE_CAPACITY: usize = 256;

/// A single rectangular tile of an image in the atlas.
///
/// Images smaller than [`IMAGE_ATLAS_SIZE`] on both axes produce exactly one
/// tile covering the whole image; larger images produce a grid.
#[derive(Clone, Copy)]
pub struct ImageTile {
    pub alloc: Allocation,
    /// Offset of this tile's top-left within the source image (in pixels).
    pub src_x: u32,
    pub src_y: u32,
}

pub struct ImageEntry {
    pub tiles: Vec<ImageTile>,
}

pub struct ImageAtlas {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    bind_group_layout: wgpu::BindGroupLayout,
    cache: Lru<u64, ImageEntry>,
    packer: ShelfPacker,
}

impl ImageAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
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

        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image_atlas_bg"),
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
            ],
        });

        Self {
            texture,
            bind_group,
            bind_group_layout,
            cache: Lru::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap()),
            packer: ShelfPacker::new(IMAGE_ATLAS_SIZE, IMAGE_ATLAS_LAYERS),
        }
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    /// Look up an image in the cache, tiling and uploading on miss. Returns
    /// `None` only if a tile cannot be packed even after full eviction
    /// (impossible given that tiles are bounded by the atlas size).
    pub fn ensure_cached(
        &mut self,
        queue: &wgpu::Queue,
        id: u64,
        image: &DecodedImage,
    ) -> Option<&ImageEntry> {
        if self.cache.contains_key(&id) {
            return self.cache.get(&id);
        }

        let regions = tile_regions(image.width, image.height, IMAGE_ATLAS_SIZE);
        let mut tiles: Vec<ImageTile> = Vec::with_capacity(regions.len());

        for region in &regions {
            let tile = loop {
                if let Some(alloc) = self.packer.allocate(region.width, region.height) {
                    break ImageTile {
                        alloc,
                        src_x: region.src_x,
                        src_y: region.src_y,
                    };
                }
                if !evict_one(&mut self.cache, &mut self.packer) {
                    // Atlas genuinely cannot fit this tile — undo partial
                    // work so the packer is left consistent.
                    for prior in &tiles {
                        self.packer.free(&prior.alloc);
                    }
                    warn!(
                        "image {id} tile {}x{} does not fit in atlas",
                        region.width, region.height
                    );
                    return None;
                }
            };
            upload_tile(queue, &self.texture, &tile, image);
            tiles.push(tile);
        }

        make_room_in_cache(&mut self.cache, &mut self.packer);
        self.cache.insert(id, ImageEntry { tiles });
        self.cache.get(&id)
    }
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
    packer: &mut ShelfPacker,
) -> bool {
    match cache.pop() {
        Some((_, entry)) => {
            for tile in &entry.tiles {
                packer.free(&tile.alloc);
            }
            true
        }
        None => false,
    }
}

fn make_room_in_cache(
    cache: &mut Lru<u64, ImageEntry>,
    packer: &mut ShelfPacker,
) {
    while cache.len() >= cache.capacity() {
        if !evict_one(cache, packer) {
            break;
        }
    }
}

fn upload_tile(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    tile: &ImageTile,
    image: &DecodedImage,
) {
    // Point write_texture at the sub-rectangle by offsetting into the source
    // buffer; `bytes_per_row` stays at the full image stride so wgpu walks
    // the right distance between rows.
    let bytes_per_pixel = 4;
    let row_stride = image.width * bytes_per_pixel;
    let offset = (tile.src_y * image.width + tile.src_x) as usize * bytes_per_pixel as usize;

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: tile.alloc.x,
                y: tile.alloc.y,
                z: tile.alloc.layer,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &image.pixels[offset..],
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
