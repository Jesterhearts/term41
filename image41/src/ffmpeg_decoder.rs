//! GIF and video decoding via ffmpeg-next. Compiled in behind the `ffmpeg`
//! cargo feature so light builds (no libav* on the host) keep working.
//!
//! The decoder drives ffmpeg against the raw byte buffer via a custom
//! `AVIOContext` — no temp files, no disk I/O, no symlink races. The
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
//! state. The internal `memory_input::MemoryInput` wrapper keeps that unsafe
//! adapter separate from the decode pipeline.

use std::sync::OnceLock;
use std::time::Duration;

use ff::format::Pixel;
use ff::media::Type;
use ff::software::scaling::Context as SwsContext;
use ff::software::scaling::Flags as SwsFlags;
use ff::util::frame::video::Video as VideoFrame;
use ffmpeg_next as ff;

use crate::DecodedImage;
use crate::Frame;

/// One-time ffmpeg runtime init. Subsequent calls are no-ops; wrap in a
/// `OnceLock` so we don't race on first use.
static INIT: OnceLock<bool> = OnceLock::new();

fn ensure_init() -> bool {
    *INIT.get_or_init(|| ff::init().is_ok())
}

mod memory_input {
    use std::ffi::c_void;
    use std::fmt;
    use std::io::Cursor;
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;
    use std::mem::ManuallyDrop;
    use std::ops::Deref;
    use std::ops::DerefMut;
    use std::os::raw::c_int;
    use std::ptr;
    use std::ptr::NonNull;

    use ff::ffi;
    use ff::format::context::Input as FormatInput;

    use super::ff;

    /// A custom-IO-opened ffmpeg input.
    ///
    /// This is the only safe type exposed by the in-memory input adapter.
    /// It owns the ffmpeg format context, the custom AVIO context, and the
    /// Rust reader state used by ffmpeg's callbacks. Drop order is
    /// format input → AVIO context → reader state.
    pub(super) struct MemoryInput {
        input: ManuallyDrop<FormatInput>,
        _io: AvioContext,
        _reader: Box<ReaderState>,
    }

    pub(super) enum OpenError {
        IoBufferAlloc,
        AvioContextAlloc,
        FormatContextAlloc,
        OpenInput(i32),
    }

    impl fmt::Display for OpenError {
        fn fmt(
            &self,
            f: &mut fmt::Formatter<'_>,
        ) -> fmt::Result {
            match self {
                Self::IoBufferAlloc => f.write_str("failed to allocate AVIO buffer"),
                Self::AvioContextAlloc => f.write_str("failed to allocate AVIO context"),
                Self::FormatContextAlloc => f.write_str("failed to allocate AVFormatContext"),
                Self::OpenInput(code) => {
                    write!(f, "avformat_open_input failed with ffmpeg error {code}")
                }
            }
        }
    }

    /// Open `data` as an ffmpeg input with custom in-memory IO.
    pub(super) fn open(data: Vec<u8>) -> Result<MemoryInput, OpenError> {
        let (io_ctx, reader) = open_io_context(data)?;
        let fmt_ctx = match open_format_context(io_ctx.as_ptr()) {
            Ok(fmt_ctx) => fmt_ctx,
            Err(err) => {
                // `MemoryInput` has not been assembled yet, so the local
                // AVIO wrapper owns cleanup for the context created above.
                drop(io_ctx);
                return Err(err);
            }
        };

        // SAFETY: `fmt_ctx` is a fully-opened `AVFormatContext` returned
        // by `avformat_open_input`. Ownership is transferred to the
        // ffmpeg wrapper, which will close it on drop.
        let input = unsafe { FormatInput::wrap(fmt_ctx) };

        Ok(MemoryInput {
            input: ManuallyDrop::new(input),
            _io: io_ctx,
            _reader: reader,
        })
    }

    impl Deref for MemoryInput {
        type Target = FormatInput;

        fn deref(&self) -> &Self::Target {
            &self.input
        }
    }

    impl DerefMut for MemoryInput {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.input
        }
    }

    impl Drop for MemoryInput {
        fn drop(&mut self) {
            // SAFETY: `input` is constructed exactly once in `open` and
            // `MemoryInput::drop` runs exactly once. Dropping the input
            // first closes the `AVFormatContext`; because
            // `AVFMT_FLAG_CUSTOM_IO` is set, that close leaves our custom
            // `AVIOContext` for the `AvioContext` field to free next.
            unsafe {
                ManuallyDrop::drop(&mut self.input);
            }
        }
    }

    /// State handed to ffmpeg's read/seek callbacks via the AVIOContext
    /// `opaque` pointer. Lives inside a `Box` on the heap so the pointer
    /// stays stable while ffmpeg holds it.
    struct ReaderState {
        cursor: Cursor<Vec<u8>>,
    }

    struct AvioContext {
        ptr: NonNull<ffi::AVIOContext>,
    }

    impl AvioContext {
        fn as_ptr(&self) -> *mut ffi::AVIOContext {
            self.ptr.as_ptr()
        }
    }

    impl Drop for AvioContext {
        fn drop(&mut self) {
            // SAFETY: `ptr` is a live AVIOContext created by
            // `avio_alloc_context` and uniquely owned by this wrapper.
            // ffmpeg may reallocate `buffer` during probing, so free the
            // current field value before freeing the context itself.
            unsafe {
                free_avio_context(self.ptr.as_ptr());
            }
        }
    }

    /// ffmpeg read callback. Fills `buf` from the backing
    /// `ReaderState`'s cursor and returns the byte count,
    /// [`AVERROR_EOF`] at end of input, or `AVERROR(EIO)` on an
    /// unreadable cursor.
    ///
    /// # Safety
    ///
    /// Registered via `avio_alloc_context` with `opaque` set to
    /// `&mut ReaderState`. ffmpeg's contract:
    /// - `opaque` is returned verbatim as whatever pointer we passed in.
    /// - `buf` points to at least `buf_size` writable bytes owned by ffmpeg.
    /// - `buf_size` is non-negative.
    ///
    /// The `ReaderState` is kept alive inside `MemoryInput`, whose drop
    /// order closes ffmpeg before releasing the reader allocation.
    unsafe extern "C" fn read_packet(
        opaque: *mut c_void,
        buf: *mut u8,
        buf_size: c_int,
    ) -> c_int {
        if opaque.is_null() || buf_size < 0 {
            return ffi::AVERROR(libc::EINVAL);
        }
        if buf_size == 0 {
            return 0;
        }
        if buf.is_null() {
            return ffi::AVERROR(libc::EINVAL);
        }

        // SAFETY: `opaque` is the pointer to the boxed `ReaderState`
        // passed to `avio_alloc_context` by `open_io_context`; the box
        // outlives all callback calls because it is owned by
        // `MemoryInput` and dropped after the ffmpeg input and AVIO
        // context.
        let state = unsafe { &mut *(opaque as *mut ReaderState) };
        // SAFETY: ffmpeg provides a writable buffer of `buf_size` bytes
        // for the duration of this callback. Local guards above reject a
        // null buffer and negative sizes before constructing the slice.
        let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
        match state.cursor.read(slice) {
            Ok(0) => ffi::AVERROR_EOF,
            Ok(n) => n as c_int,
            Err(_) => ffi::AVERROR(libc::EIO),
        }
    }

    /// ffmpeg seek callback. Handles `SEEK_SET` / `SEEK_CUR` /
    /// `SEEK_END` plus the ffmpeg-specific `AVSEEK_SIZE` query.
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
        if opaque.is_null() {
            return -1;
        }

        // SAFETY: see `read_packet`; this callback is registered with
        // the same boxed `ReaderState` pointer and cannot outlive the
        // owning `MemoryInput`.
        let state = unsafe { &mut *(opaque as *mut ReaderState) };

        if whence & ffi::AVSEEK_SIZE as c_int != 0 {
            return state.cursor.get_ref().len() as i64;
        }

        let pos = match whence & 0x7 {
            libc::SEEK_SET => SeekFrom::Start(offset as u64),
            libc::SEEK_CUR => SeekFrom::Current(offset),
            libc::SEEK_END => SeekFrom::End(offset),
            _ => return -1,
        };
        state.cursor.seek(pos).map(|p| p as i64).unwrap_or(-1)
    }

    fn open_io_context(data: Vec<u8>) -> Result<(AvioContext, Box<ReaderState>), OpenError> {
        // 4 KiB matches ffmpeg's own default probe buffer size; too small
        // and the demuxer will keep re-reading to grow it.
        const BUF_SIZE: usize = 4096;

        // SAFETY: `av_malloc` returns either a fresh allocation of at
        // least `BUF_SIZE` bytes or NULL. We check for NULL immediately.
        let io_buffer = unsafe { ffi::av_malloc(BUF_SIZE) };
        if io_buffer.is_null() {
            return Err(OpenError::IoBufferAlloc);
        }

        let mut reader = Box::new(ReaderState {
            cursor: Cursor::new(data),
        });
        let opaque = &mut *reader as *mut ReaderState as *mut c_void;

        // SAFETY: `io_buffer` is the valid av_malloc'd pointer from
        // above, `read_packet` / `seek_packet` match ffmpeg's callback
        // ABI, and `opaque` points to a boxed `ReaderState` that is
        // returned with the AVIO context and then stored in `MemoryInput`.
        let io_ctx = unsafe {
            ffi::avio_alloc_context(
                io_buffer as *mut u8,
                BUF_SIZE as c_int,
                0,
                opaque,
                Some(read_packet),
                None,
                Some(seek_packet),
            )
        };
        let ptr = match NonNull::new(io_ctx) {
            Some(ptr) => ptr,
            None => {
                // SAFETY: `io_buffer` was not accepted by
                // `avio_alloc_context`, so ownership is still local.
                unsafe { ffi::av_free(io_buffer) };
                return Err(OpenError::AvioContextAlloc);
            }
        };

        Ok((AvioContext { ptr }, reader))
    }

    fn open_format_context(
        io_ctx: *mut ffi::AVIOContext
    ) -> Result<*mut ffi::AVFormatContext, OpenError> {
        // SAFETY: `avformat_alloc_context` either returns a fresh context
        // or NULL. No ownership transfer occurs on success.
        let mut fmt_ctx = unsafe { ffi::avformat_alloc_context() };
        if fmt_ctx.is_null() {
            return Err(OpenError::FormatContextAlloc);
        }

        // SAFETY: `fmt_ctx` is a valid, uniquely-owned AVFormatContext
        // and `io_ctx` is the live AVIOContext created for this input.
        // Setting `AVFMT_FLAG_CUSTOM_IO` keeps ffmpeg from freeing our
        // AVIOContext when the format context closes.
        unsafe {
            (*fmt_ctx).pb = io_ctx;
            (*fmt_ctx).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
        }

        // SAFETY: `fmt_ctx` is a valid owned pointer. Passing NULL for
        // path, format, and options asks ffmpeg to auto-probe through the
        // custom IO context. On failure ffmpeg frees and nulls `fmt_ctx`;
        // on success ownership stays with the caller.
        let ret = unsafe {
            ffi::avformat_open_input(&mut fmt_ctx, ptr::null(), ptr::null_mut(), ptr::null_mut())
        };
        if ret < 0 {
            return Err(OpenError::OpenInput(ret));
        }

        Ok(fmt_ctx)
    }

    /// Free an AVIOContext allocated by `avio_alloc_context`.
    ///
    /// # Safety
    ///
    /// `io_ctx` must be non-null, uniquely owned, allocated by
    /// `avio_alloc_context`, and not freed by an `AVFormatContext`.
    unsafe fn free_avio_context(io_ctx: *mut ffi::AVIOContext) {
        // SAFETY: caller guarantees `io_ctx` is a valid custom
        // AVIOContext we own. `av_freep` accepts a pointer to the buffer
        // field and nulls it after free; `avio_context_free` then frees
        // the context and nulls the local pointer.
        unsafe {
            let buffer_ptr = &mut (*io_ctx).buffer as *mut *mut u8;
            ffi::av_freep(buffer_ptr as *mut c_void);
            let mut io = io_ctx;
            ffi::avio_context_free(&mut io);
        }
    }
}

use memory_input::MemoryInput;

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
        frames: frames.into(),
    })
}

/// Pull-based video-stream reader. Owns the ffmpeg state (input,
/// decoder, scaler) for one input and hands out decoded RGBA frames on
/// demand. Build with [`FrameReader::open`], pull with
/// [`FrameReader::next_frame`] (EOF-terminating) or
/// [`FrameReader::next_frame_looping`] (seeks back to start on
/// EOF for endless playback).
///
/// Designed for the background decoder thread: keep memory use bounded
/// regardless of stream length. A 30-second 1080p video holds one
/// frame in `pending_rgba` plus whatever the GPU upload path has
/// buffered — not the whole movie.
pub struct FrameReader {
    /// Width of decoded frames in pixels.
    pub width: u32,
    /// Height of decoded frames in pixels.
    pub height: u32,
    mem: MemoryInput,
    stream_index: usize,
    time_base: ff::Rational,
    decoder: ff::decoder::Video,
    /// Lazily created on the first decoded frame. Codec parameters can
    /// report `Pixel::None` for video codecs whose format isn't known
    /// until after the first frame is decoded (H.264, VP9, etc.); the
    /// decoded frame always carries the correct format.
    scaler: Option<SwsContext>,
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
        let mem = match memory_input::open(data) {
            Ok(mem) => mem,
            Err(err) => {
                warn!("ffmpeg in-memory input open failed: {err}");
                return None;
            }
        };
        Self::init(mem)
    }

    fn init(mem: MemoryInput) -> Option<Self> {
        let (stream_index, time_base, decoder) = {
            let stream = mem.streams().best(Type::Video)?;
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
        Some(Self {
            width,
            height,
            mem,
            stream_index,
            time_base,
            decoder,
            scaler: None,
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
                let mut iter = self.mem.packets();
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
        let ok = self.mem.seek(0, ..).is_ok();
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
        // Lazily create the scaler from the first decoded frame's actual
        // pixel format. Codec parameters can report None or a mismatched
        // format for video codecs (H.264, VP9, …); using the decoded
        // frame's format avoids the swscale assertion that fires when the
        // configured source format doesn't match what the frame carries.
        if self.scaler.is_none() {
            self.scaler = SwsContext::get(
                frame.format(),
                self.width,
                self.height,
                Pixel::RGBA,
                self.width,
                self.height,
                SwsFlags::BILINEAR,
            )
            .ok();
        }
        let scaler = self.scaler.as_mut()?;
        if scaler.run(&frame, &mut self.pending_rgba).is_err() {
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
        for f in img.frames.iter() {
            assert_eq!(f.delay, Duration::from_millis(100));
            // 2 pixels × 4 bytes (RGBA).
            assert_eq!(f.pixels.len(), 8);
        }
    }

    #[test]
    fn rejects_garbage_bytes() {
        // Not any known container → memory input open should bail cleanly.
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
