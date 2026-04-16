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

/// ffmpeg read callback. Fills `buf` from the backing `ReaderState`'s
/// cursor and returns the byte count, [`AVERROR_EOF`] at end of input,
/// or `AVERROR(EIO)` on an unreadable cursor.
///
/// # Safety
///
/// Registered via `avio_alloc_context` with `opaque` set to
/// `&mut ReaderState`. ffmpeg's contract:
/// - `opaque` is returned verbatim as whatever pointer we passed in.
/// - `buf` points to at least `buf_size` writable bytes owned by ffmpeg.
/// - `buf_size` is non-negative.
///
/// The `ReaderState` is kept alive via a `Box` held in [`MemInput`]
/// whose field drop order tears down ffmpeg (which stops calling this
/// callback) before the box releases the allocation.
unsafe extern "C" fn read_packet(
    opaque: *mut c_void,
    buf: *mut u8,
    buf_size: c_int,
) -> c_int {
    // SAFETY: `opaque` is the `&mut *reader as *mut ReaderState`
    // stored in `open_in_memory`, and the backing `Box` outlives this
    // callback (see `MemInput::drop`). ffmpeg never aliases the opaque
    // pointer across threads for a given context.
    let state = unsafe { &mut *(opaque as *mut ReaderState) };
    // SAFETY: ffmpeg guarantees `buf` is valid for writes of `buf_size`
    // bytes for the duration of this call and isn't concurrently
    // accessed elsewhere.
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match state.cursor.read(slice) {
        Ok(0) => ffi::AVERROR_EOF,
        Ok(n) => n as c_int,
        Err(_) => ffi::AVERROR(libc::EIO),
    }
}

/// ffmpeg seek callback. Handles `SEEK_SET` / `SEEK_CUR` / `SEEK_END`
/// plus the ffmpeg-specific `AVSEEK_SIZE` query ("what's the total
/// length?"). Returns the new position or `-1` on failure.
///
/// # Safety
///
/// Same contract as [`read_packet`] — `opaque` is a valid
/// `&mut ReaderState` for the duration of the call.
unsafe extern "C" fn seek_packet(
    opaque: *mut c_void,
    offset: i64,
    whence: c_int,
) -> i64 {
    // SAFETY: see `read_packet` — `opaque` points to a live ReaderState
    // owned by the `MemInput` driving this decode.
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
        // SAFETY: `self.input` was constructed via `ManuallyDrop::new` in
        // `open_in_memory` and hasn't been dropped yet — `Drop::drop`
        // runs exactly once, so this is the only call to
        // `ManuallyDrop::drop`. After this line `self.input` must not
        // be used again, which holds because we don't touch it below.
        //
        // `avformat_close_input` (run via the wrapped Input's Drop)
        // leaves our AVIOContext alone thanks to the
        // `AVFMT_FLAG_CUSTOM_IO` we set — so the subsequent calls below
        // act on an AVIOContext that is still valid and owned by us.
        unsafe {
            ManuallyDrop::drop(&mut self.input);
        }

        if !self.io_ctx.is_null() {
            // SAFETY: `io_ctx` is the AVIOContext we allocated in
            // `open_in_memory` and no one else has freed it: ffmpeg-next
            // doesn't own custom-IO contexts. `av_freep(&mut buffer)`
            // frees whatever buffer the context currently holds (ffmpeg
            // may have reallocated it during open) and nulls the field,
            // which `avio_context_free` tolerates. Both functions are
            // safe to call with valid pointers we own.
            unsafe {
                let buffer_ptr = &mut (*self.io_ctx).buffer as *mut *mut u8;
                ffi::av_freep(buffer_ptr as *mut c_void);
                ffi::avio_context_free(&mut self.io_ctx);
            }
        }
        // `_reader` drops via normal field drop order, safely after
        // every ffmpeg callback has had its last chance to run.
    }
}

/// Open `data` as an ffmpeg input with a custom AVIOContext. Returns
/// `None` if any allocation or probe step fails.
///
/// All the unsafe machinery below assumes:
/// - ffmpeg's allocation functions (`av_malloc`, `avio_alloc_context`,
///   `avformat_alloc_context`) either return `NULL` on failure or a
///   freshly-allocated, uniquely-owned pointer on success.
/// - `avio_alloc_context` takes ownership of `io_buffer`.
/// - `avformat_open_input` on success transfers no ownership; on failure it
///   frees and nulls the AVFormatContext pointer we pass in.
/// - `Input::wrap` takes ownership of the AVFormatContext and arranges
///   `avformat_close_input` on drop.
/// - All cleanup paths free exactly what we still own at the failure point
///   (audited test `rejects_garbage_bytes` exercises the open failure branch
///   specifically).
fn open_in_memory(data: Vec<u8>) -> Option<MemInput> {
    // 4 KiB matches ffmpeg's own default probe buffer size; too small and
    // the demuxer will keep re-reading to grow it.
    const BUF_SIZE: usize = 4096;

    // SAFETY: `av_malloc` returns either a fresh allocation of at least
    // `BUF_SIZE` bytes or NULL. We check for NULL immediately.
    let io_buffer = unsafe { ffi::av_malloc(BUF_SIZE) };
    if io_buffer.is_null() {
        return None;
    }

    // Box the reader so its address is stable for the lifetime of the
    // AVIOContext — ffmpeg keeps our `opaque` pointer. The Box holds a
    // unique allocation with no aliases at this point.
    let mut reader = Box::new(ReaderState {
        cursor: Cursor::new(data),
    });
    let opaque = &mut *reader as *mut ReaderState as *mut c_void;

    // SAFETY: `io_buffer` is the valid av_malloc'd pointer from above,
    // `read_packet` / `seek_packet` match ffmpeg's callback ABI, and
    // `opaque` points to a live `ReaderState` owned by the `reader`
    // Box that stays alive in the returned `MemInput` (the
    // destructuring on error paths drops it after we've freed ffmpeg
    // state).
    let io_ctx = unsafe {
        ffi::avio_alloc_context(
            io_buffer as *mut u8,
            BUF_SIZE as c_int,
            0, // write_flag = 0 (read-only)
            opaque,
            Some(read_packet),
            None,
            Some(seek_packet),
        )
    };
    if io_ctx.is_null() {
        // SAFETY: `io_buffer` was never handed to avio_alloc_context
        // (it returned NULL), so ownership is still ours.
        unsafe { ffi::av_free(io_buffer) };
        return None;
    }

    // SAFETY: `avformat_alloc_context` either returns a fresh context
    // or NULL. No ownership transfer.
    let mut fmt_ctx = unsafe { ffi::avformat_alloc_context() };
    if fmt_ctx.is_null() {
        // SAFETY: AVIOContext now owns `io_buffer`; free via the
        // context's `buffer` field in case ffmpeg already swapped it,
        // then free the context itself. These are the only live
        // ffmpeg-owned resources in this branch.
        unsafe {
            let buffer_ptr = &mut (*io_ctx).buffer as *mut *mut u8;
            ffi::av_freep(buffer_ptr as *mut c_void);
            let mut io = io_ctx;
            ffi::avio_context_free(&mut io);
        }
        return None;
    }

    // SAFETY: `fmt_ctx` is a valid, uniquely-owned AVFormatContext we
    // just allocated; writing its `pb` and `flags` fields is sound.
    // Setting AVFMT_FLAG_CUSTOM_IO tells avformat_close_input not to
    // touch our AVIOContext, which is essential for the Drop ordering
    // in `MemInput`.
    unsafe {
        (*fmt_ctx).pb = io_ctx;
        (*fmt_ctx).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
    }

    // SAFETY: `fmt_ctx` is a valid owned pointer; the other three
    // arguments are explicitly documented to accept NULL for "auto
    // detect format / no options". On failure ffmpeg frees `fmt_ctx`
    // and nulls the pointer for us; on success ownership stays with
    // `fmt_ctx`.
    let ret = unsafe {
        ffi::avformat_open_input(&mut fmt_ctx, ptr::null(), ptr::null_mut(), ptr::null_mut())
    };
    if ret < 0 {
        // SAFETY: avformat_open_input has already freed fmt_ctx, so
        // the AVIOContext is the only ffmpeg resource we still own.
        unsafe {
            let buffer_ptr = &mut (*io_ctx).buffer as *mut *mut u8;
            ffi::av_freep(buffer_ptr as *mut c_void);
            let mut io = io_ctx;
            ffi::avio_context_free(&mut io);
        }
        return None;
    }

    // SAFETY: `fmt_ctx` is a fully-initialized, opened AVFormatContext
    // with exclusive ownership — the requirement stated on
    // `Input::wrap`. The returned `Input` will call
    // `avformat_close_input` on drop, which we sequence correctly in
    // `MemInput::drop` (close Input first, then free the AVIOContext).
    let input = unsafe { Input::wrap(fmt_ctx) };

    Some(MemInput {
        input: ManuallyDrop::new(input),
        io_ctx,
        _reader: reader,
    })
}

/// Decode an animated GIF byte buffer into a [`DecodedImage`]. Returns
/// `None` when ffmpeg init fails, the probe rejects the input, or no
/// frames decode cleanly. Loads *every* frame into memory up front —
/// used only by inline-image protocols, where the content is small and
/// the per-frame storage is bounded by the protocol itself. Backgrounds
/// use [`FrameReader`] directly instead so a multi-GB video doesn't
/// need to fit in RAM.
pub fn decode(data: &[u8]) -> Option<DecodedImage> {
    let mut reader = FrameReader::open(data.to_vec())?;
    let width = reader.width;
    let height = reader.height;
    let mut frames: Vec<Frame> = Vec::new();
    while let Some((pixels, delay)) = reader.next_frame() {
        frames.push(Frame { pixels, delay });
    }
    if frames.is_empty() {
        return None;
    }
    Some(DecodedImage {
        width,
        height,
        frames,
    })
}

/// Pull-based video-stream reader. Owns the ffmpeg state (input,
/// decoder, scaler) for one input and hands out decoded RGBA frames on
/// demand. Build with [`FrameReader::open`], pull with [`next_frame`]
/// (EOF-terminating) or [`next_frame_looping`] (seeks back to start on
/// EOF for endless playback).
///
/// Designed for the background decoder thread: keep memory use bounded
/// regardless of stream length. A 30-second 1080p video holds one
/// frame in `pending_rgba` plus whatever the GPU upload path has
/// buffered — not the whole movie.
pub struct FrameReader {
    pub width: u32,
    pub height: u32,
    mem: MemInput,
    stream_index: usize,
    time_base: ff::Rational,
    decoder: ff::decoder::Video,
    scaler: SwsContext,
    /// Reusable holding cell for the scaled RGBA output so we don't
    /// reallocate a fresh `VideoFrame` every pull.
    pending_rgba: VideoFrame,
    /// Whether the demuxer has hit EOF and the decoder has been flushed.
    /// `next_frame` returns `None` once the decoder has drained all its
    /// remaining frames past EOF; `next_frame_looping` observes the EOF
    /// and seeks to the start before calling again.
    finished: bool,
}

impl FrameReader {
    /// Open `data` as a streaming input. Returns `None` if ffmpeg init
    /// fails, the bytes don't probe as a recognised format, or the
    /// codec can't be initialised.
    pub fn open(data: Vec<u8>) -> Option<Self> {
        if !ensure_init() {
            warn!("ffmpeg init failed; cannot decode");
            return None;
        }
        let mem = open_in_memory(data)?;
        Self::init(mem)
    }

    fn init(mem: MemInput) -> Option<Self> {
        let (stream_index, time_base, decoder) = {
            let stream = mem.input.streams().best(Type::Video)?;
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
        let scaler = SwsContext::get(
            decoder.format(),
            width,
            height,
            Pixel::RGBA,
            width,
            height,
            SwsFlags::BILINEAR,
        )
        .ok()?;
        Some(Self {
            width,
            height,
            mem,
            stream_index,
            time_base,
            decoder,
            scaler,
            pending_rgba: VideoFrame::empty(),
            finished: false,
        })
    }

    /// Pull the next decoded RGBA frame and its on-screen presentation
    /// delay. Returns `None` at end of stream; subsequent calls keep
    /// returning `None` until `seek_to_start` rewinds.
    pub fn next_frame(&mut self) -> Option<(Vec<u8>, Duration)> {
        if self.finished {
            return None;
        }
        // First, drain whatever the decoder has buffered from prior
        // packets. This is common for B-frame-reordering codecs; GIFs
        // won't have any pending frames here but the loop is cheap.
        if let Some(frame) = self.pop_ready_frame(0) {
            return Some(frame);
        }

        // Otherwise, read packets until the decoder produces a frame or
        // the demuxer hits EOF.
        loop {
            let packet = {
                let mut iter = self.mem.input.packets();
                loop {
                    match iter.next() {
                        Some((stream, packet)) => {
                            if stream.index() == self.stream_index {
                                break Some(packet);
                            }
                        }
                        None => break None,
                    }
                }
            };
            match packet {
                Some(pkt) => {
                    let duration = pkt.duration();
                    if self.decoder.send_packet(&pkt).is_err() {
                        // Bad packet; keep trying with the next one.
                        continue;
                    }
                    if let Some(frame) = self.pop_ready_frame(duration) {
                        return Some(frame);
                    }
                }
                None => {
                    // Demuxer is out of packets. Flush the decoder and
                    // drain any remaining frames (B-frame tail); once
                    // that's empty we're truly done until a seek.
                    let _ = self.decoder.send_eof();
                    let tail = self.pop_ready_frame(0);
                    if tail.is_none() {
                        self.finished = true;
                    }
                    return tail;
                }
            }
        }
    }

    /// Pull the next frame, seeking back to the start of the stream on
    /// EOF. Used by the background decoder thread for endless loops —
    /// GIFs and videos both cycle via this path.
    pub fn next_frame_looping(&mut self) -> Option<(Vec<u8>, Duration)> {
        if let Some(frame) = self.next_frame() {
            return Some(frame);
        }
        if !self.seek_to_start() {
            return None;
        }
        self.next_frame()
    }

    /// Rewind to the first frame of the stream. Flushes the decoder so
    /// stale B-frame references are dropped, then tells the demuxer to
    /// seek to timestamp 0. Returns `false` when the demuxer refuses
    /// the seek — rare for well-formed files, but possible with weird
    /// mid-stream-only formats.
    pub fn seek_to_start(&mut self) -> bool {
        self.decoder.flush();
        let ok = self.mem.input.seek(0, ..).is_ok();
        if ok {
            self.finished = false;
        }
        ok
    }

    /// Drain one frame from the decoder's output queue if ready. Packs
    /// the scaled RGBA into a freshly-owned `Vec<u8>` and returns it
    /// along with the presentation delay derived from the source
    /// packet's duration (fallback to 100 ms for GIFs that forgot).
    fn pop_ready_frame(
        &mut self,
        packet_duration: i64,
    ) -> Option<(Vec<u8>, Duration)> {
        let mut frame = VideoFrame::empty();
        if self.decoder.receive_frame(&mut frame).is_err() {
            return None;
        }
        if self.scaler.run(&frame, &mut self.pending_rgba).is_err() {
            return None;
        }
        let stride = self.pending_rgba.stride(0);
        let row_bytes = (self.width as usize) * 4;
        let src = self.pending_rgba.data(0);
        let mut pixels = Vec::with_capacity(row_bytes * self.height as usize);
        for y in 0..self.height as usize {
            let row_start = y * stride;
            pixels.extend_from_slice(&src[row_start..row_start + row_bytes]);
        }
        let delay = duration_from_timebase(packet_duration, self.time_base);
        Some((pixels, delay))
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

    #[test]
    fn frame_reader_next_frame_returns_none_at_eof() {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(TWO_FRAME_GIF_B64)
            .unwrap();
        let mut reader = FrameReader::open(bytes).expect("open");
        assert!(reader.next_frame().is_some(), "frame 1");
        assert!(reader.next_frame().is_some(), "frame 2");
        assert!(reader.next_frame().is_none(), "EOF after 2 frames");
        // Repeated calls stay at EOF until a seek.
        assert!(reader.next_frame().is_none(), "still EOF");
    }

    #[test]
    fn frame_reader_next_frame_looping_wraps_around() {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(TWO_FRAME_GIF_B64)
            .unwrap();
        let mut reader = FrameReader::open(bytes).expect("open");
        // Pull a full cycle plus a third frame — the third should be
        // frame 1 of the next loop, not None.
        let f1 = reader.next_frame_looping().expect("frame 1");
        let f2 = reader.next_frame_looping().expect("frame 2");
        let f3 = reader.next_frame_looping().expect("frame 1 of loop 2");
        assert_eq!(f1.0.len(), 8, "2×1 RGBA");
        assert_eq!(f2.0.len(), 8);
        assert_eq!(f3.0.len(), 8);
        // Loop restart should land on frame 1 again — same pixel data as f1.
        assert_eq!(f1.0, f3.0, "first-frame content matches across loops");
    }
}
