//! Wallpaper-style background image painted behind terminal cells.
//!
//! Owns the GPU resources for one optional image: a single 2D texture, a
//! linear-clamped sampler, a 4-vertex full-window quad, and the bookkeeping
//! to swap the texture's pixels when an animated image advances frames.
//!
//! Static images (PNG) decode once at load time; the texture is written
//! and the job is done. Animated content (GIF, MP4, WebM, etc.) runs on
//! a dedicated decoder thread that produces frames through a bounded
//! `sync_channel`. The render thread drains the channel each cycle and
//! uploads the newest buffered frame — so memory stays bounded at the
//! channel capacity regardless of stream length. A 20-minute video uses
//! the same RAM as a 2-second GIF.
//!
//! The `Renderer` draws the background quad at the start of the bg pass,
//! before any cell quads. The cell loop then skips bg quads for cells
//! whose bg is the default colour, leaving the image visible through
//! those "holes" while explicitly-coloured SGR cells overpaint it.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Instant;

use bytemuck::Pod;
use bytemuck::Zeroable;
use image41::decode_image;
#[cfg(feature = "ffmpeg")]
use image41::ffmpeg_decoder::FrameReader;
use utils41::lerp;
use wgpu::util::DeviceExt;

/// How many decoded frames the decoder thread is allowed to get ahead of
/// the render thread. Pre-buffering absorbs render-thread hiccups
/// (GC pauses, scheduler jitter) without dropping playback; too-large a
/// buffer just pins more RAM for no visual gain. Four frames at 1080p
/// RGBA is ~32 MB always-resident — cheap compared to loading a whole
/// GIF/video into RAM, the failure mode this module exists to avoid.
const FRAME_BUFFER_CAPACITY: usize = 4;
const STARTUP_SNAPSHOT_PREFIX: &str = "startup_background_";

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
    width: u32,
    height: u32,
    source: BackgroundSource,
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

struct Frame {
    pixels: Vec<u8>,
    delay: std::time::Duration,
    width: u32,
    height: u32,
}

/// How `Background` gets its frames. Static images upload once at load
/// time; streaming sources own a decoder thread that produces frames on
/// demand through a bounded channel.
enum BackgroundSource {
    /// One-shot: the frame was uploaded at load time and `frame_advance`
    /// is a no-op. Covers PNGs and anything else that decodes to a
    /// single frame.
    Static,
    /// Multi-frame, decoded off-thread. `frame_advance` drains the
    /// receiver each render cycle, discarding all but the newest frame
    /// so a lagging render doesn't pile up a backlog.
    Streaming {
        rx: mpsc::Receiver<Frame>,
        last_frame_at: Instant,
        frame_delay: std::time::Duration,
        shutdown: Arc<AtomicBool>,
    },
}

impl Drop for Background {
    fn drop(&mut self) {
        // Closing the channel makes the decoder thread's `send` fail on
        // its next attempt, so it exits the loop cleanly. Joining here
        // ensures ffmpeg teardown happens before we return — leaking
        // decoder threads across config reloads would burn memory for
        // nothing.
        if let BackgroundSource::Streaming { shutdown, rx, .. } = &mut self.source {
            shutdown.store(true, std::sync::atomic::Ordering::Release);
            while rx.try_recv().is_ok() {}
        }
    }
}

impl Background {
    /// Decode or open the image at `path` and build all GPU resources
    /// for it. Returns `None` if the file can't be read or the bytes
    /// don't decode into a supported format (PNG always; GIF behind
    /// the `ffmpeg` feature). Animated content (GIF, future video)
    /// spawns a decoder thread so only a bounded number of frames sit
    /// in RAM regardless of the stream's duration.
    pub(crate) fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        path: PathBuf,
        dim: f32,
        window_size: (u32, u32),
        startup_snapshot_size: (u32, u32),
    ) -> Option<Self> {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                error!("background image: failed to read {}: {e}", path.display());
                return None;
            }
        };

        // Format sniff: animated paths go to the streaming decoder;
        // everything else decodes to one frame up front.
        let is_animated = is_animated_format(&bytes);

        if is_animated {
            return Self::load_streaming(
                device,
                queue,
                layout,
                path,
                bytes,
                dim,
                window_size,
                startup_snapshot_size,
            );
        }

        let image = match decode_image(&bytes) {
            Some(img) => img,
            None => {
                error!(
                    "background image: failed to decode {} (supported: PNG, GIF, MP4, WebM, MKV, \
                     MOV, AVI — GIF/video require the ffmpeg feature)",
                    path.display()
                );
                return None;
            }
        };
        let first = image.frames.first()?.clone();
        Some(Self::build(
            device,
            queue,
            layout,
            path,
            Frame {
                pixels: first.pixels,
                delay: first.delay,
                width: image.width,
                height: image.height,
            },
            BackgroundSource::Static,
            dim,
            window_size,
            startup_snapshot_size,
        ))
    }

    #[cfg(feature = "ffmpeg")]
    fn load_streaming(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        path: PathBuf,
        bytes: Vec<u8>,
        dim: f32,
        window_size: (u32, u32),
        startup_snapshot_size: (u32, u32),
    ) -> Option<Self> {
        // Decoder thread model: the `FrameReader` is opened, used, and
        // dropped entirely on the decoder thread — it never crosses a
        // thread boundary. Only `Send`-safe bytes (`Vec<u8>`, `u32`)
        // flow back to the render thread, so we don't need an
        // `unsafe impl Send` on ffmpeg state that might rely on
        // thread-local quirks we can't see from the outside.
        //
        // Startup flow:
        //   1. Spawn thread with raw bytes + a `meta` oneshot channel.
        //   2. Thread opens FrameReader, pulls frame 1, ships `(width, height,
        //      frame_1_pixels)` through `meta`.
        //   3. Main thread blocks on `meta.recv` (fast — ffmpeg init
        //      + 1-frame decode is ~5-20 ms), then builds GPU state
        //      with the first frame uploaded synchronously.
        //   4. Thread continues in its steady-state loop, shipping subsequent frames
        //      through `frame_tx`.

        let (frame_tx, frame_rx) = mpsc::sync_channel::<Frame>(FRAME_BUFFER_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_ = shutdown.clone();
        let path_for_thread = path.clone();
        std::thread::Builder::new()
            .name("bg-decoder".into())
            .spawn(move || decoder_thread(bytes, frame_tx, &path_for_thread, shutdown_))
            .ok()?;

        let meta = frame_rx.recv().ok()?;
        let delay = meta.delay;

        Some(Self::build(
            device,
            queue,
            layout,
            path,
            meta,
            BackgroundSource::Streaming {
                rx: frame_rx,
                last_frame_at: Instant::now(),
                frame_delay: delay,
                shutdown,
            },
            dim,
            window_size,
            startup_snapshot_size,
        ))
    }

    #[cfg(not(feature = "ffmpeg"))]
    fn load_streaming(
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _layout: &wgpu::BindGroupLayout,
        path: PathBuf,
        _bytes: Vec<u8>,
        _dim: f32,
        _window_size: (u32, u32),
        _startup_snapshot_size: (u32, u32),
    ) -> Option<Self> {
        error!(
            "background image: {} looks animated but ffmpeg support isn't compiled in",
            path.display()
        );
        None
    }

    fn build(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        path: PathBuf,
        first: Frame,
        source: BackgroundSource,
        dim: f32,
        window_size: (u32, u32),
        startup_snapshot_size: (u32, u32),
    ) -> Self {
        save_startup_snapshot(&first, dim, startup_snapshot_size);
        let texture = create_texture(device, first.width, first.height);
        upload_frame(queue, &texture, first.width, first.height, &first.pixels);

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

        let vertices = fill_vertices(window_size, first.width, first.height, dim);
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

        Self {
            path,
            width: first.width,
            height: first.height,
            source,
            dim,
            texture,
            bind_group,
            vbuf,
            ibuf,
            window_size,
        }
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
        let verts = fill_vertices(self.window_size, self.width, self.height, dim);
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
        let verts = fill_vertices(window_size, self.width, self.height, self.dim);
        queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));
    }

    /// Swap in whatever frame the decoder thread has most recently
    /// produced. For static sources this is a no-op (the single frame
    /// was uploaded at load time). For streaming sources, drains the
    /// channel and uploads the newest buffered frame — older ones are
    /// discarded so a lagging render doesn't chase history.
    pub(crate) fn frame_advance(
        &mut self,
        queue: &wgpu::Queue,
    ) {
        let BackgroundSource::Streaming {
            rx,
            last_frame_at,
            frame_delay,
            ..
        } = &mut self.source
        else {
            return;
        };

        let mut total_delay = *frame_delay;
        let mut new_frame = None;
        while last_frame_at.elapsed() >= total_delay {
            if let Ok(frame) = rx.try_recv() {
                total_delay += frame.delay;
                new_frame = Some(frame);
            } else {
                break;
            }
        }

        if let Some(frame) = new_frame {
            *last_frame_at = Instant::now();
            upload_frame(
                queue,
                &self.texture,
                frame.width,
                frame.height,
                &frame.pixels,
            );
            *frame_delay = frame.delay;
        }
    }

    /// Are we drawing more than one frame on a loop? The render thread
    /// needs to know to wake up between frames so animations actually
    /// progress instead of stalling on input idleness.
    pub(crate) fn is_animated(&self) -> bool {
        matches!(self.source, BackgroundSource::Streaming { .. })
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

pub(crate) fn startup_snapshot_path() -> Option<PathBuf> {
    let dir = crate::renderer::term41_data_dir()?;
    Some(dir.join(format!("{STARTUP_SNAPSHOT_PREFIX}.png")))
}

fn save_startup_snapshot(
    frame: &Frame,
    dim: f32,
    window_size: (u32, u32),
) {
    let Some(path) = startup_snapshot_path() else {
        return;
    };
    let rgba =
        render_startup_snapshot_rgba(window_size, frame.width, frame.height, &frame.pixels, dim);
    if rgba.is_empty() {
        return;
    }

    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        warn!("startup background snapshot: failed to create data dir: {err}");
        return;
    }

    let Ok(mut file) = atomic_write_file::AtomicWriteFile::options().open(&path) else {
        warn!(
            "startup background snapshot: failed to open {} for atomic write",
            path.display()
        );
        return;
    };

    if let Err(err) = write_png_rgba(&mut file, window_size.0, window_size.1, &rgba) {
        warn!(
            "startup background snapshot: failed to write {}: {err}",
            path.display()
        );
        return;
    }
    if let Err(err) = file.commit() {
        warn!(
            "startup background snapshot: failed to commit {}: {err}",
            path.display()
        );
    }
}

fn render_startup_snapshot_rgba(
    window_size: (u32, u32),
    img_w: u32,
    img_h: u32,
    pixels: &[u8],
    dim: f32,
) -> Vec<u8> {
    let out_w = window_size.0.max(1) as usize;
    let out_h = window_size.1.max(1) as usize;
    let src_w = img_w.max(1) as usize;
    let src_h = img_h.max(1) as usize;
    let expected = src_w.saturating_mul(src_h).saturating_mul(4);
    if pixels.len() < expected {
        return Vec::new();
    }

    let scale = (out_w as f32 / src_w as f32).max(out_h as f32 / src_h as f32);
    let scaled_w = src_w as f32 * scale;
    let scaled_h = src_h as f32 * scale;
    let crop_x = ((scaled_w - out_w as f32) * 0.5).max(0.0);
    let crop_y = ((scaled_h - out_h as f32) * 0.5).max(0.0);
    let dim = dim.clamp(0.0, 1.0);

    let mut out = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        let src_y = ((y as f32 + crop_y) / scale).clamp(0.0, (src_h - 1) as f32);
        for x in 0..out_w {
            let src_x = ((x as f32 + crop_x) / scale).clamp(0.0, (src_w - 1) as f32);
            let rgba = sample_bilinear_rgba(pixels, src_w, src_h, src_x, src_y);
            let dst = (y * out_w + x) * 4;
            out[dst] = (rgba[0] as f32 * dim).round() as u8;
            out[dst + 1] = (rgba[1] as f32 * dim).round() as u8;
            out[dst + 2] = (rgba[2] as f32 * dim).round() as u8;
            out[dst + 3] = rgba[3];
        }
    }
    out
}

fn sample_bilinear_rgba(
    pixels: &[u8],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
) -> [u8; 4] {
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let c00 = rgba_at(pixels, width, x0, y0);
    let c10 = rgba_at(pixels, width, x1, y0);
    let c01 = rgba_at(pixels, width, x0, y1);
    let c11 = rgba_at(pixels, width, x1, y1);
    let mut out = [0u8; 4];
    for channel in 0..4 {
        let top = lerp(c00[channel] as f32, c10[channel] as f32, tx);
        let bottom = lerp(c01[channel] as f32, c11[channel] as f32, tx);
        out[channel] = lerp(top, bottom, ty).round() as u8;
    }
    out
}

fn rgba_at(
    pixels: &[u8],
    width: usize,
    x: usize,
    y: usize,
) -> [u8; 4] {
    let idx = (y * width + x) * 4;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

fn write_png_rgba(
    writer: &mut atomic_write_file::AtomicWriteFile,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    let mut encoder = png::Encoder::new(&mut *writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png = encoder.write_header().map_err(std::io::Error::other)?;
    png.write_image_data(rgba).map_err(std::io::Error::other)?;
    png.finish().map_err(std::io::Error::other)?;
    Ok(())
}

/// Decoder thread body. Owns the `FrameReader` for its entire lifetime —
/// never hands it out, never moves it. Ships only `Vec<u8>` pixels and
/// `u32` dimensions back to the render thread. Structure:
///
///   1. `FrameReader::open(bytes)` — ffmpeg setup on this thread.
///   2. Pull frame 1 for the initial-state handshake.
///   3. Send `StreamMeta` → unblocks main thread's `load_streaming`.
///   4. Sleep frame 1's presentation delay, then loop: pull next frame, send
///      via `frame_tx`, sleep its delay. `next_frame_looping` seeks to start on
///      EOF so the stream plays forever.
///
/// Any failure (open fails, no decodable frames, decoder unrecoverable)
/// sends `None` on `meta_tx` if we haven't already ACKed meta, and
/// exits. The thread's exit on unrecoverable errors is the user-visible
/// behaviour of "the background froze on its last frame"; better than
/// panicking on the background thread.
#[cfg(feature = "ffmpeg")]
fn decoder_thread(
    bytes: Vec<u8>,
    frame_tx: mpsc::SyncSender<Frame>,
    path_for_log: &Path,
    shutdown: Arc<AtomicBool>,
) {
    let Some(mut reader) = FrameReader::open(bytes) else {
        return;
    };

    let width = reader.width;
    let height = reader.height;

    loop {
        let (pixels, delay) = match reader.next_frame_looping() {
            Some(f) => f,
            None => {
                warn!(
                    "background image: decoder exiting for {} (stream unrecoverable)",
                    path_for_log.display()
                );
                return;
            }
        };

        // `send` blocks when the channel is full — that's the backpressure
        // that keeps us a bounded number of frames ahead of the renderer.
        // If the channel closed the receiver dropped; exit cleanly.
        if frame_tx
            .send(Frame {
                pixels,
                delay,
                width,
                height,
            })
            .is_err()
        {
            return;
        }

        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
    }
}

/// Sniff whether the bytes represent a format we treat as animated (and
/// therefore route through the streaming decoder). Covers GIF and the
/// common video containers ffmpeg supports. Unknown or non-matching
/// formats fall back to the static path — `decode_image` will either
/// handle them or report a clean "unsupported format" error there.
fn is_animated_format(bytes: &[u8]) -> bool {
    let Some(kind) = infer::get(bytes) else {
        return false;
    };
    matches!(
        kind.mime_type(),
        "image/gif"
            | "video/mp4"
            | "video/webm"
            | "video/x-matroska"
            | "video/quicktime"
            | "video/x-msvideo"
    )
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
        (v[0].uv, v[3].uv)
    }

    #[test]
    fn matching_aspect_uses_full_image() {
        let (tl, br) = uvs((800, 400), (200, 100));
        assert!((tl[0] - 0.0).abs() < 1e-6);
        assert!((tl[1] - 0.0).abs() < 1e-6);
        assert!((br[0] - 1.0).abs() < 1e-6);
        assert!((br[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn wider_window_crops_image_height() {
        let (tl, br) = uvs((400, 100), (100, 100));
        assert!((tl[0] - 0.0).abs() < 1e-6);
        assert!((br[0] - 1.0).abs() < 1e-6);
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

    #[test]
    fn sniffs_gif_as_animated() {
        // GIF magic: "GIF89a" / "GIF87a".
        assert!(is_animated_format(b"GIF89a\x00\x00"));
        assert!(is_animated_format(b"GIF87a\x00\x00"));
    }

    #[test]
    fn sniffs_video_as_animated() {
        // Minimal ftyp box header for MP4/MOV.
        let mut ftyp = vec![0u8; 12];
        ftyp[3] = 12; // box size
        ftyp[4..8].copy_from_slice(b"ftyp");
        ftyp[8..12].copy_from_slice(b"isom");
        assert!(is_animated_format(&ftyp));

        // WebM/MKV: EBML header magic 0x1A45DFA3.
        let ebml = [0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0];
        assert!(is_animated_format(&ebml));
    }

    #[test]
    fn png_not_treated_as_animated() {
        let png_magic = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert!(!is_animated_format(&png_magic));
    }

    #[test]
    fn startup_snapshot_applies_dim_without_touching_alpha() {
        let rgba = render_startup_snapshot_rgba((1, 1), 1, 1, &[200, 100, 50, 128], 0.5);
        assert_eq!(rgba, vec![100, 50, 25, 128]);
    }

    #[test]
    fn startup_snapshot_uses_cover_crop() {
        let pixels = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let rgba = render_startup_snapshot_rgba((1, 2), 2, 2, &pixels, 1.0);
        assert_eq!(rgba.len(), 8);
        assert_ne!(&rgba[0..4], &pixels[0..4]);
        assert_ne!(&rgba[4..8], &pixels[12..16]);
    }
}
