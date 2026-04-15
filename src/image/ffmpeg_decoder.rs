//! Animated-GIF decoding via ffmpeg-next. Compiled in behind the `ffmpeg`
//! cargo feature so light builds (no libav* on the host) keep working.
//!
//! The decoder drives ffmpeg against the raw byte buffer via a custom
//! [`AVIOContext`] — no temp files, no disk I/O, no symlink races. The
//! flow is:
//!   1. Allocate an IO buffer with `av_malloc` and wrap it plus a
//!      `Cursor<Vec<u8>>` in an AVIOContext with read + seek callbacks.
//!   2. Point a fresh AVFormatContext at the AVIOContext, set
//!      `AVFMT_FLAG_CUSTOM_IO`, and call `avformat_open_input`. ffmpeg
//!      auto-detects the container via the probe buffer it fills via our read
//!      callback.
//!   3. Hand the opened context to ffmpeg-next's safe `Input` wrapper for the
//!      decode loop.
//!   4. Walk packets → `send_packet` → drain `receive_frame`, converting each
//!      frame through an `sws` scaler and recording the packet's duration as
//!      the frame's on-screen time.
//!
//! Teardown order matters: close `Input` first (leaves custom IO alone
//! thanks to the flag), then free the AVIOContext (and its internal
//! buffer, which ffmpeg may have reallocated), then drop the reader
//! state. [`MemInput`] enforces this via field order + a manual `Drop`.

use std::ffi::c_void;
use std::io::Cursor;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::mem::ManuallyDrop;
use std::os::raw::c_int;
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use ff::ffi;
use ff::format::Pixel;
use ff::format::context::Input;
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

fn ensure_init() -> bool {
    *INIT.get_or_init(|| ff::init().is_ok())
}

/// State handed to ffmpeg's read/seek callbacks via the AVIOContext
/// `opaque` pointer. Lives inside a `Box` on the heap so the pointer
/// stays stable while ffmpeg holds it.
struct ReaderState {
    cursor: Cursor<Vec<u8>>,
}

unsafe extern "C" fn read_packet(
    opaque: *mut c_void,
    buf: *mut u8,
    buf_size: c_int,
) -> c_int {
    let state = unsafe { &mut *(opaque as *mut ReaderState) };
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match state.cursor.read(slice) {
        Ok(0) => ffi::AVERROR_EOF,
        Ok(n) => n as c_int,
        Err(_) => ffi::AVERROR(libc::EIO),
    }
}

unsafe extern "C" fn seek_packet(
    opaque: *mut c_void,
    offset: i64,
    whence: c_int,
) -> i64 {
    let state = unsafe { &mut *(opaque as *mut ReaderState) };

    // AVSEEK_SIZE is an ffmpeg extension: "don't seek, just tell me the
    // total length." Report the buffer size without touching the cursor.
    if whence & ffi::AVSEEK_SIZE as c_int != 0 {
        return state.cursor.get_ref().len() as i64;
    }

    // Mask off AVSEEK_FORCE (0x20000) etc. — only the low 3 bits carry
    // the actual SEEK_SET / SEEK_CUR / SEEK_END selector.
    let pos = match whence & 0x7 {
        libc::SEEK_SET => SeekFrom::Start(offset as u64),
        libc::SEEK_CUR => SeekFrom::Current(offset),
        libc::SEEK_END => SeekFrom::End(offset),
        _ => return -1,
    };
    state.cursor.seek(pos).map(|p| p as i64).unwrap_or(-1)
}

/// A custom-IO-opened ffmpeg input plus the raw AVIOContext and reader
/// state that back it. Teardown order is Input → AVIOContext → reader.
struct MemInput {
    input: ManuallyDrop<Input>,
    io_ctx: *mut ffi::AVIOContext,
    _reader: Box<ReaderState>,
}

impl Drop for MemInput {
    fn drop(&mut self) {
        unsafe {
            // Close the format context first. With AVFMT_FLAG_CUSTOM_IO
            // set, avformat_close_input leaves the AVIOContext alone and
            // we're still on the hook for it.
            ManuallyDrop::drop(&mut self.input);

            if !self.io_ctx.is_null() {
                // ffmpeg may have reallocated the probe buffer during
                // open — free whatever the context points at now rather
                // than the original av_malloc'd pointer.
                let buffer_ptr = &mut (*self.io_ctx).buffer as *mut *mut u8;
                ffi::av_freep(buffer_ptr as *mut c_void);
                ffi::avio_context_free(&mut self.io_ctx);
            }
            // `_reader` drops last via normal field drop order, safely
            // after every ffmpeg callback has had a chance to run.
        }
    }
}

/// Open `data` as an ffmpeg input with a custom AVIOContext. Returns
/// `None` if any allocation or probe step fails.
fn open_in_memory(data: Vec<u8>) -> Option<MemInput> {
    // 4 KiB matches ffmpeg's own default probe buffer size; too small and
    // the demuxer will keep re-reading to grow it.
    const BUF_SIZE: usize = 4096;

    unsafe {
        let io_buffer = ffi::av_malloc(BUF_SIZE);
        if io_buffer.is_null() {
            return None;
        }

        // Box the reader so its address is stable for the lifetime of
        // the AVIOContext — ffmpeg keeps our `opaque` pointer.
        let mut reader = Box::new(ReaderState {
            cursor: Cursor::new(data),
        });
        let opaque = &mut *reader as *mut ReaderState as *mut c_void;

        let io_ctx = ffi::avio_alloc_context(
            io_buffer as *mut u8,
            BUF_SIZE as c_int,
            0, // write_flag = 0 (read-only)
            opaque,
            Some(read_packet),
            None,
            Some(seek_packet),
        );
        if io_ctx.is_null() {
            ffi::av_free(io_buffer);
            return None;
        }

        let mut fmt_ctx = ffi::avformat_alloc_context();
        if fmt_ctx.is_null() {
            let buffer_ptr = &mut (*io_ctx).buffer as *mut *mut u8;
            ffi::av_freep(buffer_ptr as *mut c_void);
            let mut io = io_ctx;
            ffi::avio_context_free(&mut io);
            return None;
        }

        (*fmt_ctx).pb = io_ctx;
        (*fmt_ctx).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;

        let ret =
            ffi::avformat_open_input(&mut fmt_ctx, ptr::null(), ptr::null_mut(), ptr::null_mut());
        if ret < 0 {
            // avformat_open_input frees fmt_ctx on failure and sets it
            // to NULL, but our io_ctx is unaffected.
            let buffer_ptr = &mut (*io_ctx).buffer as *mut *mut u8;
            ffi::av_freep(buffer_ptr as *mut c_void);
            let mut io = io_ctx;
            ffi::avio_context_free(&mut io);
            return None;
        }

        Some(MemInput {
            input: ManuallyDrop::new(Input::wrap(fmt_ctx)),
            io_ctx,
            _reader: reader,
        })
    }
}

/// Decode an animated GIF byte buffer into a [`DecodedImage`]. Returns
/// `None` when ffmpeg init fails, the probe rejects the input, or no
/// frames decode cleanly.
pub fn decode(data: &[u8]) -> Option<DecodedImage> {
    if !ensure_init() {
        warn!("ffmpeg init failed; cannot decode GIF");
        return None;
    }

    let mut mem = open_in_memory(data.to_vec())?;
    decode_frames(&mut mem.input)
}

fn decode_frames(ictx: &mut Input) -> Option<DecodedImage> {
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

    // For GIF, each packet decodes to exactly one frame, so capturing
    // the packet duration before `send_packet` and handing it to the
    // drain pass gives each decoded frame the correct presentation delay.
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

    #[test]
    fn rejects_garbage_bytes() {
        // Not any known container → open_in_memory should bail cleanly.
        // Importantly this checks no leaks / UB in the error-path cleanup.
        assert!(decode(b"this is not a valid image file").is_none());
    }

    #[test]
    fn rejects_empty_bytes() {
        assert!(decode(&[]).is_none());
    }
}
