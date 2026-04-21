//! Kitty graphics protocol parser and image decoder.
//!
//! Handles APC payloads of the form `G key=val,...;base64_payload`. Parses the
//! control keys, decodes the payload (base64 → optional zlib inflate → raw
//! pixels or PNG), and produces a [`DecodedImage`] ready for the atlas.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::DecodedImage;
use crate::decode_png;

// ---------------------------------------------------------------------------
// Parsed command
// ---------------------------------------------------------------------------

/// All key=value fields from a single kitty graphics APC escape.
#[derive(Debug, Clone)]
pub struct KittyCommand {
    /// `a` — action (default `T`).
    pub action: u8,
    /// `f` — pixel format: 24 (RGB), 32 (RGBA, default), 100 (PNG).
    pub format: u32,
    /// `t` — transmission medium: `d` direct, `f` file, `t` temp file.
    pub transmission: u8,
    /// `o` — compression: 0 none, `z` zlib.
    pub compression: u8,
    /// `i` — image id (1–4294967295, 0 = unset).
    pub image_id: u32,
    /// `I` — image number (client-assigned, terminal maps to an id).
    pub image_number: u32,
    /// `p` — placement id.
    pub placement_id: u32,
    /// `s` — source image width in pixels (required for raw formats).
    pub width: u32,
    /// `v` — source image height in pixels (required for raw formats).
    pub height: u32,
    /// `x` — left edge of the source rectangle (pixels).
    pub src_x: u32,
    /// `y` — top edge of the source rectangle (pixels).
    pub src_y: u32,
    /// `w` — width of the source rectangle (0 = full width).
    pub src_w: u32,
    /// `h` — height of the source rectangle (0 = full height).
    pub src_h: u32,
    /// `c` — display columns (0 = auto).
    pub columns: u32,
    /// `r` — display rows (0 = auto).
    pub rows: u32,
    /// `X` — pixel offset within the cell (horizontal).
    pub cell_x_offset: u32,
    /// `Y` — pixel offset within the cell (vertical).
    pub cell_y_offset: u32,
    /// `z` — z-index for stacking order.
    pub z_index: i32,
    /// `m` — more data: 1 = more chunks follow, 0 = final chunk.
    pub more: u8,
    /// `q` — suppress responses: 0 = normal, 1 = suppress OK, 2 = suppress all.
    pub quiet: u8,
    /// `C` — cursor movement: 0 = move cursor, 1 = don't move.
    pub no_move_cursor: bool,
    /// `d` — delete specifier character.
    pub delete: u8,
    /// Raw base64 payload (not yet decoded).
    pub payload: Vec<u8>,
}

impl Default for KittyCommand {
    fn default() -> Self {
        Self {
            action: b'T',
            format: 32,
            transmission: b'd',
            compression: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            width: 0,
            height: 0,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            columns: 0,
            rows: 0,
            cell_x_offset: 0,
            cell_y_offset: 0,
            z_index: 0,
            more: 0,
            quiet: 0,
            no_move_cursor: false,
            delete: 0,
            payload: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse an APC payload into a `KittyCommand`.
///
/// Expected format: `G<key>=<val>,<key>=<val>,...;<base64 payload>`
/// The leading `G` must already have been verified by the caller.
pub fn parse_command(payload: &[u8]) -> KittyCommand {
    let mut cmd = KittyCommand::default();

    // Find semicolon separating control data from base64 payload.
    let (control, data) = match payload.iter().position(|&b| b == b';') {
        Some(pos) => (&payload[..pos], &payload[pos + 1..]),
        None => (payload, &[] as &[u8]),
    };

    cmd.payload = data.to_vec();

    // Parse comma-separated key=value pairs.
    for kv in control.split(|&b| b == b',') {
        if let Some(eq) = kv.iter().position(|&b| b == b'=') {
            let key = &kv[..eq];
            let val = &kv[eq + 1..];
            if key.len() == 1 {
                apply_key(&mut cmd, key[0], val);
            }
        }
    }

    cmd
}

fn parse_u32(val: &[u8]) -> u32 {
    let mut n: u32 = 0;
    for &b in val {
        if b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as u32);
        } else {
            break;
        }
    }
    n
}

fn parse_i32(val: &[u8]) -> i32 {
    if val.first() == Some(&b'-') {
        -(parse_u32(&val[1..]) as i32)
    } else {
        parse_u32(val) as i32
    }
}

fn apply_key(
    cmd: &mut KittyCommand,
    key: u8,
    val: &[u8],
) {
    match key {
        b'a' => {
            if let Some(&v) = val.first() {
                cmd.action = v;
            }
        }
        b'f' => cmd.format = parse_u32(val),
        b't' => {
            if let Some(&v) = val.first() {
                cmd.transmission = v;
            }
        }
        b'o' => {
            if let Some(&v) = val.first() {
                cmd.compression = v;
            }
        }
        b'i' => cmd.image_id = parse_u32(val),
        b'I' => cmd.image_number = parse_u32(val),
        b'p' => cmd.placement_id = parse_u32(val),
        b's' => cmd.width = parse_u32(val),
        b'v' => cmd.height = parse_u32(val),
        b'x' => cmd.src_x = parse_u32(val),
        b'y' => cmd.src_y = parse_u32(val),
        b'w' => cmd.src_w = parse_u32(val),
        b'h' => cmd.src_h = parse_u32(val),
        b'c' => cmd.columns = parse_u32(val),
        b'r' => cmd.rows = parse_u32(val),
        b'X' => cmd.cell_x_offset = parse_u32(val),
        b'Y' => cmd.cell_y_offset = parse_u32(val),
        b'z' => cmd.z_index = parse_i32(val),
        b'm' => cmd.more = parse_u32(val) as u8,
        b'q' => cmd.quiet = parse_u32(val) as u8,
        b'C' => cmd.no_move_cursor = parse_u32(val) == 1,
        b'd' => {
            if let Some(&v) = val.first() {
                cmd.delete = v;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Payload decoding
// ---------------------------------------------------------------------------

/// Decode a base64 payload, optionally decompress, then interpret as pixels.
///
/// Returns `None` on any decoding failure (bad base64, corrupt PNG, wrong
/// dimensions, etc.).
pub fn decode_payload(
    cmd: &KittyCommand,
    raw_b64: &[u8],
) -> Option<DecodedImage> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;

    let decoded = engine.decode(raw_b64).ok()?;

    let pixels = if cmd.compression == b'z' {
        let mut inflated = Vec::new();
        flate2::read::ZlibDecoder::new(&decoded[..])
            .read_to_end(&mut inflated)
            .ok()?;
        inflated
    } else {
        decoded
    };

    match cmd.format {
        100 => decode_png(&pixels),
        24 => decode_rgb(&pixels, cmd.width, cmd.height),
        _ => decode_rgba(&pixels, cmd.width, cmd.height),
    }
}

/// Load image data from a file path (base64-encoded in the payload).
pub fn decode_file_payload(
    cmd: &KittyCommand,
    raw_b64: &[u8],
    delete: bool,
) -> Option<DecodedImage> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;

    let path_bytes = engine.decode(raw_b64).ok()?;
    let path_str = std::str::from_utf8(&path_bytes).ok()?;
    let path = Path::new(path_str);

    // Security: for temp files, only allow paths containing the marker and
    // residing under known temp directories.
    if delete {
        let canonical = path.to_str().unwrap_or("");
        let is_temp = canonical.starts_with("/tmp/")
            || canonical.starts_with("/dev/shm/")
            || canonical.starts_with(std::env::temp_dir().to_str().unwrap_or("/tmp/"));
        if !is_temp || !canonical.contains("tty-graphics-protocol") {
            return None;
        }
    }

    let file_data = std::fs::read(path).ok()?;

    if delete {
        let _ = std::fs::remove_file(path);
    }

    let pixels = if cmd.compression == b'z' {
        let mut inflated = Vec::new();
        flate2::read::ZlibDecoder::new(&file_data[..])
            .read_to_end(&mut inflated)
            .ok()?;
        inflated
    } else {
        file_data
    };

    match cmd.format {
        100 => decode_png(&pixels),
        24 => decode_rgb(&pixels, cmd.width, cmd.height),
        _ => decode_rgba(&pixels, cmd.width, cmd.height),
    }
}

fn decode_rgb(
    data: &[u8],
    width: u32,
    height: u32,
) -> Option<DecodedImage> {
    let expected = width as usize * height as usize * 3;
    if width == 0 || height == 0 || data.len() < expected {
        return None;
    }

    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for chunk in data[..expected].chunks_exact(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }

    Some(DecodedImage::single_frame(width, height, rgba))
}

fn decode_rgba(
    data: &[u8],
    width: u32,
    height: u32,
) -> Option<DecodedImage> {
    let expected = width as usize * height as usize * 4;
    if width == 0 || height == 0 || data.len() < expected {
        return None;
    }

    Some(DecodedImage::single_frame(
        width,
        height,
        data[..expected].to_vec(),
    ))
}

/// Apply source-rectangle cropping to a decoded image.
pub fn crop_source_rect(
    image: DecodedImage,
    cmd: &KittyCommand,
) -> DecodedImage {
    let src_x = cmd.src_x.min(image.width);
    let src_y = cmd.src_y.min(image.height);
    let src_w = if cmd.src_w == 0 {
        image.width - src_x
    } else {
        cmd.src_w.min(image.width - src_x)
    };
    let src_h = if cmd.src_h == 0 {
        image.height - src_y
    } else {
        cmd.src_h.min(image.height - src_y)
    };

    if src_x == 0 && src_y == 0 && src_w == image.width && src_h == image.height {
        return image;
    }

    // Cropping is only sensible for static single-frame images; the kitty
    // protocol doesn't transmit animations, so this code never needs to
    // track cropping across a frame sequence.
    let src_pixels = &image.frames[0].pixels;
    let mut pixels = Vec::with_capacity(src_w as usize * src_h as usize * 4);
    for row in src_y..src_y + src_h {
        let start = (row as usize * image.width as usize + src_x as usize) * 4;
        let end = start + src_w as usize * 4;
        pixels.extend_from_slice(&src_pixels[start..end]);
    }

    DecodedImage::single_frame(src_w, src_h, pixels)
}

// ---------------------------------------------------------------------------
// Kitty image store
// ---------------------------------------------------------------------------

/// Stores transmitted images that have not yet been placed (or that can be
/// placed multiple times via `a=p`).
#[derive(Debug, Default)]
pub struct KittyImageStore {
    /// Images keyed by their kitty image id.
    images: HashMap<u32, DecodedImage>,
    /// Maps client-assigned image numbers to terminal-assigned image ids.
    number_to_id: HashMap<u32, u32>,
    /// Next auto-assigned image id (when client sends `I=` without `i=`).
    next_id: u32,
}

impl KittyImageStore {
    /// Create an empty image store with ids auto-assigned from 1 upward.
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            number_to_id: HashMap::new(),
            next_id: 1,
        }
    }

    /// Resolve or assign an image id from a command's `i=` / `I=` keys.
    pub fn resolve_id(
        &mut self,
        cmd: &KittyCommand,
    ) -> u32 {
        if cmd.image_id != 0 {
            return cmd.image_id;
        }
        if cmd.image_number != 0
            && let Some(&id) = self.number_to_id.get(&cmd.image_number)
        {
            return id;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        if cmd.image_number != 0 {
            self.number_to_id.insert(cmd.image_number, id);
        }
        id
    }

    /// Store or replace a decoded image under `id`.
    pub fn store(
        &mut self,
        id: u32,
        image: DecodedImage,
    ) {
        self.images.insert(id, image);
    }

    /// Return the image stored under `id`, if any.
    pub fn get(
        &self,
        id: u32,
    ) -> Option<&DecodedImage> {
        self.images.get(&id)
    }

    /// Remove one image id and any image-number aliases that resolve to it.
    pub fn remove(
        &mut self,
        id: u32,
    ) {
        self.images.remove(&id);
        self.number_to_id.retain(|_, v| *v != id);
    }

    /// Drop all stored images and aliases.
    pub fn clear(&mut self) {
        self.images.clear();
        self.number_to_id.clear();
    }

    /// Remove images by id range [lo, hi].
    pub fn remove_range(
        &mut self,
        lo: u32,
        hi: u32,
    ) {
        self.images.retain(|&id, _| id < lo || id > hi);
        self.number_to_id.retain(|_, v| *v < lo || *v > hi);
    }
}

// ---------------------------------------------------------------------------
// Chunked transmission accumulator
// ---------------------------------------------------------------------------

/// Accumulates chunks for a multi-part kitty graphics transmission.
#[derive(Debug, Default)]
pub struct ChunkedTransmission {
    /// The command from the first chunk (carries the control keys).
    pub cmd: Option<KittyCommand>,
    /// Accumulated base64 payload across chunks.
    pub payload: Vec<u8>,
}

impl ChunkedTransmission {
    /// Create an empty chunk accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a command. Returns `Some(merged_command)` when the final chunk
    /// arrives (`m=0`), or `None` while accumulating (`m=1`).
    pub fn feed(
        &mut self,
        cmd: KittyCommand,
    ) -> Option<KittyCommand> {
        let more = cmd.more;
        self.payload.extend_from_slice(&cmd.payload);

        if self.cmd.is_none() {
            self.cmd = Some(cmd);
        }

        if let Some(ref mut stored) = self.cmd {
            stored.more = more;
        }

        if more == 1 {
            return None;
        }

        let mut final_cmd = self.cmd.take().unwrap();
        final_cmd.payload = std::mem::take(&mut self.payload);
        final_cmd.more = 0;
        Some(final_cmd)
    }
}

// ---------------------------------------------------------------------------
// Response formatting
// ---------------------------------------------------------------------------

/// Format a kitty graphics protocol response.
///
/// Response format: `ESC _ G i=<id>;OK ESC \` or `ESC _ G i=<id>;error ESC \`
pub fn format_response(
    image_id: u32,
    ok: bool,
    message: &str,
) -> Vec<u8> {
    use std::fmt::Write;
    let mut resp = String::new();
    let _ = write!(resp, "\x1b_Gi={image_id};");
    if ok {
        resp.push_str("OK");
    } else {
        resp.push_str(message);
    }
    resp.push_str("\x1b\\");
    resp.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_transmit_display_png() {
        let cmd = parse_command(b"a=T,f=100,s=200,v=150,i=42;AAAA");
        assert_eq!(cmd.action, b'T');
        assert_eq!(cmd.format, 100);
        assert_eq!(cmd.width, 200);
        assert_eq!(cmd.height, 150);
        assert_eq!(cmd.image_id, 42);
        assert_eq!(cmd.payload, b"AAAA");
    }

    #[test]
    fn parse_defaults() {
        let cmd = parse_command(b";AAAA");
        assert_eq!(cmd.action, b'T');
        assert_eq!(cmd.format, 32);
        assert_eq!(cmd.transmission, b'd');
        assert_eq!(cmd.payload, b"AAAA");
    }

    #[test]
    fn parse_delete_command() {
        let cmd = parse_command(b"a=d,d=i,i=7");
        assert_eq!(cmd.action, b'd');
        assert_eq!(cmd.delete, b'i');
        assert_eq!(cmd.image_id, 7);
    }

    #[test]
    fn parse_negative_z_index() {
        let cmd = parse_command(b"z=-1,i=1;");
        assert_eq!(cmd.z_index, -1);
    }

    #[test]
    fn chunked_accumulation() {
        let mut chunked = ChunkedTransmission::new();

        let c1 = KittyCommand {
            action: b'T',
            format: 100,
            image_id: 5,
            more: 1,
            payload: b"AAAA".to_vec(),
            ..Default::default()
        };
        assert!(chunked.feed(c1).is_none());

        let c2 = KittyCommand {
            more: 1,
            payload: b"BBBB".to_vec(),
            ..Default::default()
        };
        assert!(chunked.feed(c2).is_none());

        let c3 = KittyCommand {
            more: 0,
            payload: b"CCCC".to_vec(),
            ..Default::default()
        };
        let final_cmd = chunked.feed(c3).unwrap();
        assert_eq!(final_cmd.action, b'T');
        assert_eq!(final_cmd.format, 100);
        assert_eq!(final_cmd.image_id, 5);
        assert_eq!(final_cmd.payload, b"AAAABBBBCCCC");
    }

    #[test]
    fn decode_rgba_pixels() {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;

        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
        ];
        let b64 = engine.encode(&pixels);

        let cmd = KittyCommand {
            format: 32,
            width: 2,
            height: 1,
            ..Default::default()
        };
        let image = decode_payload(&cmd, b64.as_bytes()).unwrap();
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 1);
        assert_eq!(image.frames[0].pixels, pixels);
    }

    #[test]
    fn decode_rgb_pixels() {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;

        let rgb: Vec<u8> = vec![255, 0, 0, 0, 255, 0];
        let b64 = engine.encode(&rgb);

        let cmd = KittyCommand {
            format: 24,
            width: 2,
            height: 1,
            ..Default::default()
        };
        let image = decode_payload(&cmd, b64.as_bytes()).unwrap();
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 1);
        assert_eq!(image.frames[0].pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn crop_identity() {
        let image = DecodedImage::single_frame(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let cmd = KittyCommand::default();
        let cropped = crop_source_rect(image.clone(), &cmd);
        assert_eq!(cropped.frames[0].pixels, image.frames[0].pixels);
    }

    #[test]
    fn response_ok() {
        let resp = format_response(42, true, "");
        assert_eq!(resp, b"\x1b_Gi=42;OK\x1b\\");
    }

    #[test]
    fn response_error() {
        let resp = format_response(1, false, "EINVAL");
        assert_eq!(resp, b"\x1b_Gi=1;EINVAL\x1b\\");
    }
}
