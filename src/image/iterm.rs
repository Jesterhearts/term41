//! iTerm2 inline image protocol parser.
//!
//! Wire format (OSC 1337):
//!
//! ```text
//! ESC ] 1337 ; File = <args> : <base64 payload> BEL
//! ```
//!
//! Where `<args>` is a `;`-separated list of `key=value` pairs
//! (`name=…;width=10;inline=1`). Multi-part transmissions split the
//! payload across three sub-commands (iTerm 3.5+, for tmux pass-through):
//!
//! ```text
//! OSC 1337 ; MultipartFile = <args>    BEL
//! OSC 1337 ; FilePart = <base64 chunk> BEL
//! OSC 1337 ; FileEnd                   BEL
//! ```
//!
//! This module only parses and accumulates — placement onto the grid is
//! wired from `terminal.rs` alongside the other graphics protocols.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::image::DecodedImage;
use crate::image::decode_png;

/// A `width=` / `height=` value. iTerm2 distinguishes the unit by suffix:
/// bare digits = cells, `px` suffix = pixels, `%` suffix = viewport fraction,
/// literal `auto` (or anything unparseable) = use the intrinsic dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dimension {
    #[default]
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u32),
}

impl Dimension {
    /// Resolve to pixels on one axis. `fallback` is used for `Auto` — the
    /// caller passes the image's intrinsic pixel size there.
    pub fn to_pixels(
        self,
        cell: u32,
        viewport_px: u32,
        fallback: u32,
    ) -> u32 {
        match self {
            Dimension::Auto => fallback,
            Dimension::Cells(n) => n.saturating_mul(cell),
            Dimension::Pixels(n) => n,
            Dimension::Percent(n) => ((viewport_px as u64 * n as u64) / 100) as u32,
        }
    }
}

/// One `File=` or `MultipartFile=` header, optionally with a decoded payload.
#[derive(Debug, Clone)]
pub struct ItermCommand {
    /// Original filename, UTF-8 decoded from the base64 `name=` argument.
    pub name: Option<String>,
    pub width: Dimension,
    pub height: Dimension,
    pub preserve_aspect_ratio: bool,
    /// iTerm2 defaults `inline` to 0 ("download silently"). Terminals that
    /// can't offer a download UI use this flag to decide whether to render
    /// — most senders explicitly set `inline=1`.
    pub inline: bool,
    pub do_not_move_cursor: bool,
    /// Raw image bytes post-base64. Populated by `parse_file` and by
    /// [`ChunkedTransmission::finish`]; empty for a bare `MultipartFile=`.
    pub payload: Vec<u8>,
}

impl Default for ItermCommand {
    fn default() -> Self {
        Self {
            name: None,
            width: Dimension::Auto,
            height: Dimension::Auto,
            // The spec default is on: the sender rarely cares about the
            // distinction, but honouring it matches iTerm2/wezterm behaviour
            // when only one axis is given.
            preserve_aspect_ratio: true,
            inline: false,
            do_not_move_cursor: false,
            payload: Vec::new(),
        }
    }
}

/// Attempt to parse a `File=<args>:<base64>` OSC 1337 payload. Returns
/// `None` when the prefix doesn't match or base64 decode fails.
pub fn parse_file(rest: &[u8]) -> Option<ItermCommand> {
    let body = rest.strip_prefix(b"File=")?;
    let colon = body.iter().position(|&b| b == b':')?;
    let (args, tail) = body.split_at(colon);
    let b64 = &tail[1..];

    let mut cmd = parse_args(args);
    cmd.payload = decode_base64(b64)?;
    Some(cmd)
}

/// Attempt to parse a `MultipartFile=<args>` header. The payload remains
/// empty; the caller drives the chunk accumulator.
pub fn parse_multipart_start(rest: &[u8]) -> Option<ItermCommand> {
    let args = rest.strip_prefix(b"MultipartFile=")?;
    Some(parse_args(args))
}

/// Return the base64 body of a `FilePart=<base64>` payload, if the prefix
/// matches. Callers accumulate the returned slice for later decoding.
pub fn parse_file_part(rest: &[u8]) -> Option<&[u8]> {
    rest.strip_prefix(b"FilePart=")
}

/// True when `rest` is exactly `FileEnd` — the finalizer for a multi-part
/// transmission.
pub fn is_file_end(rest: &[u8]) -> bool {
    rest == b"FileEnd"
}

fn parse_args(args: &[u8]) -> ItermCommand {
    let mut cmd = ItermCommand::default();
    for kv in args.split(|&b| b == b';') {
        if kv.is_empty() {
            continue;
        }
        let Some((key, val)) = split_kv(kv) else {
            continue;
        };
        apply_arg(&mut cmd, key, val);
    }
    cmd
}

/// Split a `key=value` byte slice on the *first* `=`. Values may themselves
/// contain `=` (base64 padding of `name=`), so `splitn` semantics matter.
fn split_kv(kv: &[u8]) -> Option<(&[u8], &[u8])> {
    let eq = kv.iter().position(|&b| b == b'=')?;
    Some((&kv[..eq], &kv[eq + 1..]))
}

fn apply_arg(
    cmd: &mut ItermCommand,
    key: &[u8],
    val: &[u8],
) {
    match key {
        b"name" => {
            if let Ok(bytes) = BASE64.decode(val)
                && let Ok(s) = String::from_utf8(bytes)
            {
                cmd.name = Some(s);
            }
        }
        b"width" => cmd.width = parse_dimension(val),
        b"height" => cmd.height = parse_dimension(val),
        b"preserveAspectRatio" => cmd.preserve_aspect_ratio = val == b"1",
        b"inline" => cmd.inline = val == b"1",
        b"doNotMoveCursor" => cmd.do_not_move_cursor = val == b"1",
        // Silently ignore unknown / advisory keys (`size`, `type`, …).
        _ => {}
    }
}

fn parse_dimension(s: &[u8]) -> Dimension {
    if s == b"auto" {
        return Dimension::Auto;
    }
    if let Some(n_bytes) = s.strip_suffix(b"px")
        && let Some(n) = parse_u32(n_bytes)
    {
        return Dimension::Pixels(n);
    }
    if let Some(n_bytes) = s.strip_suffix(b"%")
        && let Some(n) = parse_u32(n_bytes)
    {
        return Dimension::Percent(n);
    }
    if let Some(n) = parse_u32(s) {
        return Dimension::Cells(n);
    }
    Dimension::Auto
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() || !s.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse().ok()
}

fn decode_base64(s: &[u8]) -> Option<Vec<u8>> {
    // Tolerate embedded whitespace — long payloads arriving under tmux
    // pass-through are sometimes wrapped.
    let filtered: Vec<u8> = s
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    BASE64.decode(&filtered).ok()
}

/// Decode an iTerm2 image payload into an [`DecodedImage`]. Only PNG is
/// supported today; other raster formats return `None` and the caller
/// silently drops the image.
pub fn decode_payload(data: &[u8]) -> Option<DecodedImage> {
    decode_png(data)
}

/// Accumulator for a `MultipartFile` → `FilePart*` → `FileEnd` sequence.
///
/// Base64 text is buffered as received and decoded once at `finish`: decoding
/// per-chunk would require chunks to sit on 4-char boundaries, which the spec
/// does not guarantee.
#[derive(Debug, Default)]
pub struct ChunkedTransmission {
    header: Option<ItermCommand>,
    b64_buf: Vec<u8>,
}

impl ChunkedTransmission {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new chunked transmission. Any in-progress state is discarded
    /// — matches iTerm2's behaviour when a sender restarts mid-stream.
    pub fn begin(
        &mut self,
        header: ItermCommand,
    ) {
        self.header = Some(header);
        self.b64_buf.clear();
    }

    /// Append a `FilePart=` chunk. Drops silently if no `MultipartFile` is
    /// in progress.
    pub fn push(
        &mut self,
        b64_chunk: &[u8],
    ) {
        if self.header.is_some() {
            self.b64_buf.extend_from_slice(b64_chunk);
        }
    }

    /// Finalize the accumulator into a command with a decoded payload.
    /// Returns `None` when no transmission was in progress or the
    /// accumulated base64 is malformed.
    pub fn finish(&mut self) -> Option<ItermCommand> {
        let mut cmd = self.header.take()?;
        let buf = std::mem::take(&mut self.b64_buf);
        cmd.payload = decode_base64(&buf)?;
        Some(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 red PNG (8-bit RGBA), base64-encoded. Built by hand to keep the
    /// test fixture self-contained — any corruption would make `decode_png`
    /// return `None`, which is exactly what we're checking against.
    const RED_PIXEL_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/\
         iZk9HQAAAABJRU5ErkJggg==";

    #[test]
    fn parse_file_extracts_inline_flag_and_payload() {
        let mut bytes = b"File=inline=1:".to_vec();
        bytes.extend_from_slice(RED_PIXEL_PNG_B64.as_bytes());
        let cmd = parse_file(&bytes).expect("parse ok");
        assert!(cmd.inline);
        assert!(!cmd.payload.is_empty());
    }

    #[test]
    fn parse_file_rejects_missing_prefix() {
        let res = parse_file(b"Multipart=foo:AAAA");
        assert!(res.is_none());
    }

    #[test]
    fn parse_file_rejects_missing_colon() {
        let res = parse_file(b"File=inline=1");
        assert!(res.is_none());
    }

    #[test]
    fn parse_file_decodes_base64_name() {
        // "hello" -> "aGVsbG8="
        let mut bytes = b"File=name=aGVsbG8=;inline=1:".to_vec();
        bytes.extend_from_slice(RED_PIXEL_PNG_B64.as_bytes());
        let cmd = parse_file(&bytes).expect("parse ok");
        assert_eq!(cmd.name.as_deref(), Some("hello"));
        assert!(cmd.inline);
    }

    #[test]
    fn parse_file_reads_dimensions() {
        let mut bytes = b"File=width=10;height=5px;inline=1:".to_vec();
        bytes.extend_from_slice(RED_PIXEL_PNG_B64.as_bytes());
        let cmd = parse_file(&bytes).expect("parse ok");
        assert_eq!(cmd.width, Dimension::Cells(10));
        assert_eq!(cmd.height, Dimension::Pixels(5));
    }

    #[test]
    fn parse_dimension_handles_percent_and_auto() {
        assert_eq!(parse_dimension(b"auto"), Dimension::Auto);
        assert_eq!(parse_dimension(b"50%"), Dimension::Percent(50));
        assert_eq!(parse_dimension(b"42px"), Dimension::Pixels(42));
        assert_eq!(parse_dimension(b"7"), Dimension::Cells(7));
        // Garbage silently falls back to Auto so a malformed sender doesn't
        // knock the whole command out.
        assert_eq!(parse_dimension(b"wat"), Dimension::Auto);
    }

    #[test]
    fn parse_file_ignores_unknown_keys() {
        let mut bytes = b"File=type=image/png;size=1234;inline=1:".to_vec();
        bytes.extend_from_slice(RED_PIXEL_PNG_B64.as_bytes());
        let cmd = parse_file(&bytes).expect("parse ok");
        assert!(cmd.inline);
    }

    #[test]
    fn default_preserves_aspect_ratio() {
        let cmd = ItermCommand::default();
        assert!(cmd.preserve_aspect_ratio);
        assert!(!cmd.inline);
    }

    #[test]
    fn dimension_resolves_to_pixels() {
        // cell=10, viewport=1000
        assert_eq!(Dimension::Auto.to_pixels(10, 1000, 80), 80);
        assert_eq!(Dimension::Cells(5).to_pixels(10, 1000, 80), 50);
        assert_eq!(Dimension::Pixels(200).to_pixels(10, 1000, 80), 200);
        assert_eq!(Dimension::Percent(25).to_pixels(10, 1000, 80), 250);
    }

    #[test]
    fn multipart_round_trip_accumulates_payload() {
        let header =
            parse_multipart_start(b"MultipartFile=inline=1;width=10").expect("header parses");
        let mut chunks = ChunkedTransmission::new();
        chunks.begin(header);

        // Split the base64 at an arbitrary non-aligned boundary to prove
        // cross-chunk concatenation works before decode.
        let (a, b) = RED_PIXEL_PNG_B64.split_at(37);
        chunks.push(parse_file_part(&[b"FilePart=", a.as_bytes()].concat()).unwrap());
        chunks.push(parse_file_part(&[b"FilePart=", b.as_bytes()].concat()).unwrap());
        assert!(is_file_end(b"FileEnd"));

        let cmd = chunks.finish().expect("finish ok");
        assert!(cmd.inline);
        assert_eq!(cmd.width, Dimension::Cells(10));
        // The payload is the decoded PNG file — not empty and begins with
        // the PNG magic number so we know we reassembled the right bytes.
        assert_eq!(
            &cmd.payload[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
    }

    #[test]
    fn multipart_finish_without_begin_returns_none() {
        let mut chunks = ChunkedTransmission::new();
        chunks.push(b"anything");
        assert!(chunks.finish().is_none());
    }

    #[test]
    fn file_part_requires_prefix() {
        assert_eq!(parse_file_part(b"FilePart=abcd"), Some(b"abcd" as &[u8]));
        assert!(parse_file_part(b"FileEnd").is_none());
    }

    #[test]
    fn decode_payload_accepts_png() {
        let bytes = BASE64.decode(RED_PIXEL_PNG_B64).unwrap();
        let img = decode_payload(&bytes).expect("decode ok");
        assert_eq!((img.width, img.height), (1, 1));
    }

    #[test]
    fn decode_payload_rejects_garbage() {
        assert!(decode_payload(b"not an image").is_none());
    }
}
