//! Image decoders for in-band graphics protocols (sixel, kitty, iterm).

pub mod iterm;
pub mod kitty;
pub mod sixel;

#[cfg(feature = "ffmpeg")]
pub(crate) mod ffmpeg_decoder;

use std::time::Duration;

/// A single frame of a decoded image. Static images carry exactly one
/// frame with `delay = Duration::ZERO`; animated images (GIFs today) carry
/// multiple frames with per-frame presentation delays.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
    /// How long this frame should be on screen before advancing to the next.
    pub delay: Duration,
}

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
}

impl DecodedImage {
    /// Build a one-frame image from a raw RGBA buffer. Used by the PNG and
    /// raw-pixel codepaths that have no concept of animation.
    pub fn single_frame(
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            width,
            height,
            frames: vec![Frame {
                pixels,
                delay: Duration::ZERO,
            }],
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// Total length of one animation cycle. `Duration::ZERO` for static
    /// images and for degenerate animated images where every frame has
    /// zero delay (treated as "don't animate").
    pub fn cycle_duration(&self) -> Duration {
        self.frames.iter().map(|f| f.delay).sum()
    }

    /// Frame index to show at `elapsed` time since placement. Wraps around
    /// the cycle for infinite loops; static images always return 0.
    pub fn frame_at(
        &self,
        elapsed: Duration,
    ) -> usize {
        if self.frames.len() <= 1 {
            return 0;
        }
        let cycle = self.cycle_duration();
        if cycle.is_zero() {
            return 0;
        }
        // Phase modulo the cycle, computed in nanoseconds to stay within
        // the arithmetic range of `Duration`.
        let phase_nanos = elapsed.as_nanos() % cycle.as_nanos();
        let phase = Duration::from_nanos(phase_nanos as u64);

        let mut acc = Duration::ZERO;
        for (i, f) in self.frames.iter().enumerate() {
            acc += f.delay;
            if phase < acc {
                return i;
            }
        }
        self.frames.len() - 1
    }
}

/// Decode an arbitrary image payload. Sniffs the format via the `infer`
/// crate and dispatches to the right decoder. Returns `None` on unknown
/// formats, malformed data, or formats whose decoder isn't compiled in.
///
/// PNG is always available. GIF requires the `ffmpeg` cargo feature.
pub fn decode_image(data: &[u8]) -> Option<DecodedImage> {
    let kind = infer::get(data)?;
    match kind.mime_type() {
        "image/png" => decode_png(data),
        #[cfg(feature = "ffmpeg")]
        "image/gif" => ffmpeg_decoder::decode(data),
        _ => None,
    }
}

/// Decode an 8-bit PNG into an RGBA [`DecodedImage`]. Returns `None` on any
/// decode failure — unsupported bit depth (16-bit), indexed colour, or
/// malformed data. Shared between kitty (`f=100`) and iterm2 payloads, both
/// of which carry raw PNG bytes.
pub fn decode_png(data: &[u8]) -> Option<DecodedImage> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().ok()?;

    let info = reader.info();
    let width = info.width;
    let height = info.height;
    let color_type = info.color_type;
    let bit_depth = info.bit_depth;

    if bit_depth != png::BitDepth::Eight {
        return None;
    }

    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let raw = &buf[..frame.buffer_size()];

    let pixels = match color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in raw.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in raw.chunks_exact(2) {
                let g = chunk[0];
                let a = chunk[1];
                rgba.extend_from_slice(&[g, g, g, a]);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for &g in raw {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
            rgba
        }
        png::ColorType::Indexed => {
            // Indexed PNG needs palette expansion — rare for in-band graphics.
            return None;
        }
    };

    Some(DecodedImage::single_frame(width, height, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(delay_ms: u64) -> Frame {
        Frame {
            pixels: Vec::new(),
            delay: Duration::from_millis(delay_ms),
        }
    }

    #[test]
    fn static_image_always_returns_first_frame() {
        let img = DecodedImage::single_frame(1, 1, vec![0, 0, 0, 255]);
        assert!(!img.is_animated());
        assert_eq!(img.frame_at(Duration::from_secs(1_000)), 0);
        assert_eq!(img.cycle_duration(), Duration::ZERO);
    }

    #[test]
    fn frame_at_picks_correct_frame_within_cycle() {
        let img = DecodedImage {
            width: 1,
            height: 1,
            frames: vec![frame(100), frame(200), frame(50)],
        };
        // Cycle = 350ms. Frame boundaries at [0..100), [100..300), [300..350).
        assert_eq!(img.frame_at(Duration::from_millis(0)), 0);
        assert_eq!(img.frame_at(Duration::from_millis(99)), 0);
        assert_eq!(img.frame_at(Duration::from_millis(100)), 1);
        assert_eq!(img.frame_at(Duration::from_millis(299)), 1);
        assert_eq!(img.frame_at(Duration::from_millis(300)), 2);
        assert_eq!(img.frame_at(Duration::from_millis(349)), 2);
    }

    #[test]
    fn frame_at_wraps_around_for_long_elapsed() {
        let img = DecodedImage {
            width: 1,
            height: 1,
            frames: vec![frame(100), frame(100)],
        };
        // Cycle = 200ms. Elapsed 450ms → phase 50ms → frame 0.
        assert_eq!(img.frame_at(Duration::from_millis(450)), 0);
        // Elapsed 550ms → phase 150ms → frame 1.
        assert_eq!(img.frame_at(Duration::from_millis(550)), 1);
    }

    #[test]
    fn zero_delay_cycle_collapses_to_first_frame() {
        let img = DecodedImage {
            width: 1,
            height: 1,
            frames: vec![frame(0), frame(0)],
        };
        // Every frame having zero delay would divide-by-zero if we tried
        // modulo. `frame_at` has to be defensive and pick a stable frame.
        assert_eq!(img.frame_at(Duration::from_millis(5)), 0);
    }
}
