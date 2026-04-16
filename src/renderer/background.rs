//! Wallpaper-style background image painted behind terminal cells.
//!
//! Owns the GPU resources for one optional image: a single 2D texture, a
//! linear-clamped sampler, a 4-vertex full-window quad, and the bookkeeping
//! to swap the texture's pixels when an animated image advances frames.
//!
//! The `Renderer` draws this — when present — at the start of the bg pass,
//! before any cell quads. The cell loop then skips quads for cells whose bg
//! is the default colour, leaving the image visible through the "holes"
//! while explicitly-coloured SGR cells overpaint it.
//!
//! Single texture, swap-on-frame: animated GIFs re-upload the whole image
//! when `frame_at` returns a different index. That trades GPU memory (one
//! frame at a time) for upload bandwidth on every animation tick — fine
//! for typical wallpaper sizes.

use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use bytemuck::Pod;
use bytemuck::Zeroable;
use wgpu::util::DeviceExt;

use crate::image::DecodedImage;
use crate::image::decode_image;

/// One vertex of the background quad: position in pixels, UV into the
/// background texture (computed CPU-side for the chosen fit mode), and a
/// per-vertex copy of the RGB dim multiplier. The same `dim` value lands
/// on all four vertices so the shader can read it as a `flat` attribute
/// without needing a separate uniform buffer for a single float.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct BgImageVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub dim: f32,
}

pub(crate) struct Background {
    /// Path the image was loaded from. Compared against the new config on
    /// reload so we only re-decode when the path actually changes.
    path: PathBuf,
    image: DecodedImage,
    /// Wall-clock anchor for the animation. `frame_at(now - placed_at)`
    /// picks the current frame index; `Instant` keeps it monotonic across
    /// system clock changes.
    placed_at: Instant,
    /// Frame currently sitting in `texture`. Stays on `frame_advance` calls
    /// when the index hasn't changed so we skip the upload.
    uploaded_frame: usize,
    /// `background_opacity` config — RGB multiplier in `[0.0, 1.0]`.
    dim: f32,

    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,

    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,

    /// Last window size used to compute the fill UVs. UVs are recomputed
    /// when the window resizes (and on opacity / image swap).
    window_size: (u32, u32),
}

impl Background {
    /// Decode the image at `path` and build all GPU resources for it.
    /// Returns `None` if the file can't be read or the bytes don't decode
    /// into a supported format (PNG always; GIF behind the `ffmpeg` feature).
    pub(crate) fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        path: PathBuf,
        dim: f32,
        window_size: (u32, u32),
    ) -> Option<Self> {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                // Logged at error so the default env_logger level surfaces
                // it; a misspelled path or wrong permissions would
                // otherwise look like a silently-broken feature.
                error!("background image: failed to read {}: {e}", path.display());
                return None;
            }
        };
        let image = match decode_image(&bytes) {
            Some(img) => img,
            None => {
                error!(
                    "background image: failed to decode {} (only PNG and GIF are supported today; \
                     other formats need a follow-up)",
                    path.display()
                );
                return None;
            }
        };

        let texture = create_texture(device, image.width, image.height);
        upload_frame(
            queue,
            &texture,
            image.width,
            image.height,
            &image.frames[0].pixels,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bg_image_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg_image_bind_group"),
            layout,
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

        let vertices = fill_vertices(window_size, image.width, image.height, dim);
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bg_image_verts"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let indices: [u32; 6] = [0, 1, 2, 2, 1, 3];
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bg_image_idx"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Some(Self {
            path,
            image,
            placed_at: Instant::now(),
            uploaded_frame: 0,
            dim,
            texture,
            bind_group,
            vbuf,
            ibuf,
            window_size,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Update the dim multiplier. Cheap — just rewrites the vertex buffer
    /// (4 vertices × one float each); no GPU resource needs reallocating.
    pub(crate) fn set_dim(
        &mut self,
        queue: &wgpu::Queue,
        dim: f32,
    ) {
        if (self.dim - dim).abs() < f32::EPSILON {
            return;
        }
        self.dim = dim;
        let verts = fill_vertices(self.window_size, self.image.width, self.image.height, dim);
        queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));
    }

    /// Recompute fill UVs for a new window size. No-op if the size is
    /// unchanged.
    pub(crate) fn resize(
        &mut self,
        queue: &wgpu::Queue,
        window_size: (u32, u32),
    ) {
        if window_size == self.window_size {
            return;
        }
        self.window_size = window_size;
        let verts = fill_vertices(window_size, self.image.width, self.image.height, self.dim);
        queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));
    }

    /// Move to the frame appropriate for `now`. For static images this is
    /// always frame 0 — the first call uploads it, subsequent calls find
    /// `uploaded_frame == 0` and skip. For animated images, re-uploads
    /// when the frame index changes.
    pub(crate) fn frame_advance(
        &mut self,
        queue: &wgpu::Queue,
        now: Instant,
    ) {
        let elapsed = now.saturating_duration_since(self.placed_at);
        let frame_idx = self.image.frame_at(elapsed);
        if frame_idx == self.uploaded_frame {
            return;
        }
        self.uploaded_frame = frame_idx;
        upload_frame(
            queue,
            &self.texture,
            self.image.width,
            self.image.height,
            &self.image.frames[frame_idx].pixels,
        );
    }

    /// Are we drawing more than one frame on a loop? The render thread
    /// needs to know to wake up between frames so animations actually
    /// progress instead of stalling on input idleness.
    pub(crate) fn is_animated(&self) -> bool {
        self.image.is_animated()
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn vbuf(&self) -> &wgpu::Buffer {
        &self.vbuf
    }

    pub(crate) fn ibuf(&self) -> &wgpu::Buffer {
        &self.ibuf
    }
}

/// Bind group layout for the background pipeline: one texture + one
/// sampler. Carved out so `Renderer` can build the layout once and pass it
/// to `Background::load` whenever the image changes.
pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bg_image_bind_group_layout"),
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
    })
}

fn create_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bg_image_texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload_frame(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    pixels: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Compute the four vertices of a full-window quad whose UVs sample the
/// image in "fill" mode: scaled so the smaller window-vs-image ratio
/// matches, with the larger axis cropped equally on both sides. This is
/// the wallpaper-style "cover" behaviour every photo viewer uses by
/// default.
fn fill_vertices(
    window: (u32, u32),
    img_w: u32,
    img_h: u32,
    dim: f32,
) -> [BgImageVertex; 4] {
    let (ww, wh) = (window.0.max(1) as f32, window.1.max(1) as f32);
    let (iw, ih) = (img_w.max(1) as f32, img_h.max(1) as f32);
    // For "fill", crop the dimension whose ratio is *smaller* — the image
    // overshoots that axis when scaled to match the larger one. e.g. a
    // 16:9 image in a 4:3 window has window.w/img.w smaller, so width
    // wraps inside and height needs cropping.
    let scale = (ww / iw).max(wh / ih);
    let crop_w = (iw * scale - ww).max(0.0);
    let crop_h = (ih * scale - wh).max(0.0);
    // Crop is in screen pixels; convert to UV by dividing back through
    // scale*image-dim.
    let u_inset = (crop_w * 0.5) / (iw * scale);
    let v_inset = (crop_h * 0.5) / (ih * scale);
    let u0 = u_inset;
    let u1 = 1.0 - u_inset;
    let v0 = v_inset;
    let v1 = 1.0 - v_inset;

    [
        BgImageVertex {
            pos: [0.0, 0.0],
            uv: [u0, v0],
            dim,
        },
        BgImageVertex {
            pos: [ww, 0.0],
            uv: [u1, v0],
            dim,
        },
        BgImageVertex {
            pos: [0.0, wh],
            uv: [u0, v1],
            dim,
        },
        BgImageVertex {
            pos: [ww, wh],
            uv: [u1, v1],
            dim,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uvs(
        window: (u32, u32),
        img: (u32, u32),
    ) -> ([f32; 2], [f32; 2]) {
        let v = fill_vertices(window, img.0, img.1, 1.0);
        // Top-left and bottom-right in UV space.
        (v[0].uv, v[3].uv)
    }

    #[test]
    fn matching_aspect_uses_full_image() {
        let (tl, br) = uvs((800, 400), (200, 100));
        // Identical aspect → no cropping, UVs span [0, 1].
        assert!((tl[0] - 0.0).abs() < 1e-6);
        assert!((tl[1] - 0.0).abs() < 1e-6);
        assert!((br[0] - 1.0).abs() < 1e-6);
        assert!((br[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn wider_window_crops_image_height() {
        // Window 4:1, image 1:1 → image scales to fill the width and
        // crops vertically. UV.x stays full; UV.y centred crop.
        let (tl, br) = uvs((400, 100), (100, 100));
        assert!((tl[0] - 0.0).abs() < 1e-6);
        assert!((br[0] - 1.0).abs() < 1e-6);
        // Image scaled by 4 (to match width). Image height = 400px,
        // window height = 100px → crop 300px / 400px = 0.75 of UV
        // height total, 0.375 each side.
        assert!((tl[1] - 0.375).abs() < 1e-6);
        assert!((br[1] - 0.625).abs() < 1e-6);
    }

    #[test]
    fn taller_window_crops_image_width() {
        let (tl, br) = uvs((100, 400), (100, 100));
        assert!((tl[1] - 0.0).abs() < 1e-6);
        assert!((br[1] - 1.0).abs() < 1e-6);
        assert!((tl[0] - 0.375).abs() < 1e-6);
        assert!((br[0] - 0.625).abs() < 1e-6);
    }
}
