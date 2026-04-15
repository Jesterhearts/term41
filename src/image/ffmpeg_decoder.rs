//! Animated-GIF decoding via ffmpeg-next. Compiled in behind the `ffmpeg`
//! cargo feature so light builds (no libav* on the host) keep working.
//!
//! The flow is:
//!   1. Stash the in-memory bytes to a temp file — ffmpeg-next's easiest entry
//!      point is `format::input(path)` and wiring a custom `AVIOContext` for
//!      in-memory reads is comparatively hairy.
//!   2. Open the input, find the best video stream (GIFs expose a single video
//!      stream), pull out its time base.
//!   3. Build an `sws` scaler that converts the decoder's native pixel format
//!      into RGBA8 at the source dimensions.
//!   4. Loop packets → send_packet → drain `receive_frame`, converting each
//!      frame and recording the packet's duration as the frame's on-screen
//!      time.
//!
//! GIF-specific quirks handled by ffmpeg automatically:
//!   - Frame disposal methods (keep/background/previous): the decoder returns
//!     fully-composited frames, so we never see partial updates.
//!   - Transparent index: surfaces as RGBA alpha through the scaler.

use std::fs;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ff::format::Pixel;
use ff::media::Type;
use ff::software::scaling::Context as SwsContext;
use ff::software::scaling::Flags as SwsFlags;
use ff::util::frame::video::Video as VideoFrame;
use ffmpeg_next as ff;

use crate::image::DecodedImage;
use crate::image::Frame;

/// One-time ffmpeg runtime init. Subsequent calls are no-ops; wrap in a
/// `OnceLock` so we don't race on first use.
static INIT: OnceLock<bool> = OnceLock::new();

/// Monotonic counter used to disambiguate temp-file names across calls
/// that happen inside the same nanosecond (rare, but cheap insurance).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn ensure_init() -> bool {
    *INIT.get_or_init(|| ff::init().is_ok())
}

/// Decode an animated GIF byte buffer into a [`DecodedImage`]. Returns
/// `None` when ffmpeg init fails, the decoder can't open the input, or
/// no frames decode cleanly.
pub fn decode(data: &[u8]) -> Option<DecodedImage> {
    if !ensure_init() {
        warn!("ffmpeg init failed; cannot decode GIF");
        return None;
    }

    let tmp_path = std::env::temp_dir().join(format!(
        "term41-gif-{}-{}.gif",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    if let Err(e) = fs::write(&tmp_path, data) {
        warn!("gif temp write failed: {e}");
        return None;
    }
    let result = decode_from_path(&tmp_path);
    let _ = fs::remove_file(&tmp_path);
    result
}

fn decode_from_path(path: &std::path::Path) -> Option<DecodedImage> {
    let mut ictx = ff::format::input(path).ok()?;

    let (stream_index, time_base, mut decoder) = {
        let stream = ictx.streams().best(Type::Video)?;
        let idx = stream.index();
        let tb = stream.time_base();
        let ctx = ff::codec::context::Context::from_parameters(stream.parameters()).ok()?;
        let dec = ctx.decoder().video().ok()?;
        (idx, tb, dec)
    };

    let width = decoder.width();
    let height = decoder.height();
    if width == 0 || height == 0 {
        return None;
    }

    let mut scaler = SwsContext::get(
        decoder.format(),
        width,
        height,
        Pixel::RGBA,
        width,
        height,
        SwsFlags::BILINEAR,
    )
    .ok()?;

    let mut frames: Vec<Frame> = Vec::new();

    // For GIF, exactly one packet produces one frame, so capturing the
    // per-packet duration before send_packet and handing it to the drain
    // pass gives each decoded frame its correct presentation delay.
    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        let packet_duration = packet.duration();
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        drain_decoder(
            &mut decoder,
            &mut scaler,
            packet_duration,
            time_base,
            width,
            height,
            &mut frames,
        );
    }

    let _ = decoder.send_eof();
    drain_decoder(
        &mut decoder,
        &mut scaler,
        0,
        time_base,
        width,
        height,
        &mut frames,
    );

    if frames.is_empty() {
        return None;
    }

    Some(DecodedImage {
        width,
        height,
        frames,
    })
}

fn drain_decoder(
    decoder: &mut ff::decoder::Video,
    scaler: &mut SwsContext,
    packet_duration: i64,
    time_base: ff::Rational,
    width: u32,
    height: u32,
    frames: &mut Vec<Frame>,
) {
    let mut frame = VideoFrame::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let mut rgba = VideoFrame::empty();
        if scaler.run(&frame, &mut rgba).is_err() {
            continue;
        }

        // Tightly-packed rows: drop ffmpeg's stride padding so the image
        // atlas's flat `width * 4`-per-row assumption holds.
        let stride = rgba.stride(0);
        let row_bytes = (width as usize) * 4;
        let src = rgba.data(0);
        let mut pixels = Vec::with_capacity(row_bytes * height as usize);
        for y in 0..height as usize {
            let row_start = y * stride;
            pixels.extend_from_slice(&src[row_start..row_start + row_bytes]);
        }

        let delay = duration_from_timebase(packet_duration, time_base);
        frames.push(Frame { pixels, delay });
    }
}

/// Convert a PTS / duration in stream-timebase units to a wall-clock
/// [`Duration`]. GIFs commonly use timebase 1/100 (each unit = 10 ms).
/// Degenerate or unset durations fall back to 100 ms — the same fallback
/// most browsers use for GIFs that forget to specify a frame delay.
fn duration_from_timebase(
    units: i64,
    tb: ff::Rational,
) -> Duration {
    let num = tb.numerator() as i64;
    let den = tb.denominator() as i64;
    if units <= 0 || num <= 0 || den <= 0 {
        return Duration::from_millis(100);
    }
    // Stay in i128 so the intermediate product doesn't overflow on long
    // durations — (units * num * 1e9) can exceed i64 range even for
    // modest GIFs.
    let nanos = (units as i128 * num as i128 * 1_000_000_000) / den as i128;
    if nanos < 0 {
        return Duration::from_millis(100);
    }
    Duration::from_nanos(nanos.min(u64::MAX as i128) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_duration_falls_back_to_default() {
        let tb = ff::Rational::new(1, 100);
        assert_eq!(duration_from_timebase(0, tb), Duration::from_millis(100));
    }

    #[test]
    fn rational_conversion_matches_expected() {
        // GIF standard: 10 units at 1/100 = 100ms.
        let tb = ff::Rational::new(1, 100);
        assert_eq!(duration_from_timebase(10, tb), Duration::from_millis(100));
        // 1/1000 timebase, 250 units = 250ms.
        let tb = ff::Rational::new(1, 1000);
        assert_eq!(duration_from_timebase(250, tb), Duration::from_millis(250));
    }

    #[test]
    fn broken_timebase_falls_back() {
        let tb = ff::Rational::new(0, 0);
        assert_eq!(duration_from_timebase(5, tb), Duration::from_millis(100));
    }

    /// Handcrafted 2-frame 2×1 GIF: red frame then blue frame, each with a
    /// 100 ms delay. Keeps the decoder end-to-end exercised without shipping
    /// a binary test fixture.
    const TWO_FRAME_GIF_B64: &str =
        "R0lGODlhAgABAIAAAP8AAAAA/yH/\
         C05FVFNDQVBFMi4wAwEAAAAh+QQACgAAACwAAAAAAgABAAACAgQBACH5BAAKAAAALAAAAAACAAEAAAICTAEAOw==";

    #[test]
    fn decodes_two_frame_gif_with_delays() {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(TWO_FRAME_GIF_B64)
            .unwrap();
        let img = decode(&bytes).expect("gif decodes");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.frames.len(), 2);
        // Each frame's GCE delay was 10 centiseconds = 100ms.
        for f in &img.frames {
            assert_eq!(f.delay, Duration::from_millis(100));
            // 2 pixels × 4 bytes (RGBA).
            assert_eq!(f.pixels.len(), 8);
        }
    }
}
