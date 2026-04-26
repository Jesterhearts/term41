//! Kitty graphics protocol parser and image decoder.
//!
//! Handles APC payloads of the form `G key=val,...;base64_payload`. Parses the
//! control keys, decodes the payload (base64 → optional zlib inflate → raw
//! pixels or encoded images), and produces a [`DecodedImage`] ready for the
//! atlas.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;

use crate::DecodedImage;
use crate::decode_png_or_jpeg;

// ---------------------------------------------------------------------------
// Parsed command
// ---------------------------------------------------------------------------

/// All key=value fields from a single kitty graphics APC escape.
#[derive(Debug, Clone)]
pub struct KittyCommand {
    /// `a` — action (default `t`).
    pub action: u8,
    /// `f` — pixel format: 24 (RGB), 32 (RGBA, default), 100 (PNG by spec;
    /// term41 also accepts JPEG bytes as a compatibility extension).
    pub format: u32,
    /// `t` — transmission medium: `d` direct, `f` file, `t` temp file.
    /// The kitty protocol also defines `s` for shared memory; term41 parses
    /// the value but intentionally rejects it in the terminal layer.
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
    /// `S` — bytes to read from file payloads (0 = to end).
    pub data_size: u32,
    /// `O` — byte offset to read from file payloads.
    pub data_offset: u32,
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
    /// `U` — create a virtual placement for Unicode placeholders.
    pub virtual_placement: bool,
    /// `P` — parent image id for relative placement.
    pub parent_image_id: u32,
    /// `Q` — parent placement id for relative placement.
    pub parent_placement_id: u32,
    /// `H` — horizontal cell offset from the parent placement.
    pub relative_col_offset: i32,
    /// `V` — vertical cell offset from the parent placement.
    pub relative_row_offset: i32,
    /// `d` — delete specifier character.
    pub delete: u8,
    /// Raw base64 payload (not yet decoded).
    pub payload: Vec<u8>,
}

impl Default for KittyCommand {
    fn default() -> Self {
        Self {
            action: b't',
            format: 32,
            transmission: b'd',
            compression: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            width: 0,
            height: 0,
            data_size: 0,
            data_offset: 0,
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
            virtual_placement: false,
            parent_image_id: 0,
            parent_placement_id: 0,
            relative_col_offset: 0,
            relative_row_offset: 0,
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
        b'S' => cmd.data_size = parse_u32(val),
        b'O' => cmd.data_offset = parse_u32(val),
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
        b'U' => cmd.virtual_placement = parse_u32(val) == 1,
        b'P' => cmd.parent_image_id = parse_u32(val),
        b'Q' => cmd.parent_placement_id = parse_u32(val),
        b'H' => cmd.relative_col_offset = parse_i32(val),
        b'V' => cmd.relative_row_offset = parse_i32(val),
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

    decode_data(cmd, decoded)
}

/// Load image data from a file path (base64-encoded in the payload).
pub fn decode_file_payload(
    cmd: &KittyCommand,
    raw_b64: &[u8],
    delete: bool,
    max_bytes: usize,
) -> Option<DecodedImage> {
    let path = file_payload_path(raw_b64)?;
    decode_file_payload_from_path(cmd, &path, delete, max_bytes)
}

pub fn file_payload_path(raw_b64: &[u8]) -> Option<PathBuf> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;

    let path_bytes = engine.decode(raw_b64).ok()?;
    let path_str = std::str::from_utf8(&path_bytes).ok()?;
    Some(PathBuf::from(path_str))
}

pub fn file_payload_path_allowed(
    path: &Path,
    delete: bool,
) -> bool {
    if !delete {
        return true;
    }

    // Security: for temp files, only allow paths containing the marker and
    // residing under known temp directories.
    let canonical = path.to_str().unwrap_or("");
    let is_temp = canonical.starts_with("/tmp/")
        || canonical.starts_with("/dev/shm/")
        || canonical.starts_with(std::env::temp_dir().to_str().unwrap_or("/tmp/"));
    is_temp && canonical.contains("tty-graphics-protocol")
}

pub fn decode_file_payload_from_path(
    cmd: &KittyCommand,
    path: &Path,
    delete: bool,
    max_bytes: usize,
) -> Option<DecodedImage> {
    if !file_payload_path_allowed(path, delete) {
        return None;
    }

    let file_data = read_ranged_file(path, cmd, max_bytes)?;

    if delete {
        let _ = std::fs::remove_file(path);
    }

    decode_data(cmd, file_data)
}

fn decode_data(
    cmd: &KittyCommand,
    data: Vec<u8>,
) -> Option<DecodedImage> {
    let pixels = if cmd.compression == b'z' {
        let mut inflated = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..])
            .read_to_end(&mut inflated)
            .ok()?;
        inflated
    } else {
        data
    };

    match cmd.format {
        100 => decode_png_or_jpeg(&pixels),
        24 => decode_rgb(&pixels, cmd.width, cmd.height),
        _ => decode_rgba(&pixels, cmd.width, cmd.height),
    }
}

fn read_ranged_file(
    path: &Path,
    cmd: &KittyCommand,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }

    let data_len = metadata.len();
    let start = cmd.data_offset as u64;
    if start > data_len {
        return None;
    }

    let requested_len = if cmd.data_size == 0 {
        data_len - start
    } else {
        u64::from(cmd.data_size).min(data_len - start)
    };
    if requested_len > max_bytes as u64 {
        return None;
    }

    file.seek(SeekFrom::Start(start)).ok()?;
    let mut data = Vec::with_capacity(requested_len as usize);
    file.take(requested_len).read_to_end(&mut data).ok()?;
    Some(data)
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
#[derive(Debug)]
pub struct KittyImageStore {
    /// Images keyed by their kitty image id.
    images: HashMap<u32, DecodedImage>,
    /// Maps client-assigned image numbers to terminal-assigned image ids.
    number_to_id: HashMap<u32, u32>,
    /// Next auto-assigned image id (when client sends `I=` without `i=`).
    next_id: u32,
    /// Decoded image bytes currently retained in `images`.
    used_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyStoreError {
    StorageLimitExceeded,
}

impl Default for KittyImageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KittyImageStore {
    /// Create an empty image store with ids auto-assigned from 1 upward.
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            number_to_id: HashMap::new(),
            next_id: 1,
            used_bytes: 0,
        }
    }

    /// Resolve an existing image id from `i=` / `I=` keys.
    pub fn resolve_existing_id(
        &self,
        cmd: &KittyCommand,
    ) -> Option<u32> {
        if cmd.image_id != 0 || cmd.image_number == 0 {
            return Some(cmd.image_id);
        }
        self.number_to_id.get(&cmd.image_number).copied()
    }

    /// Resolve or assign the id for a new image transmission.
    pub fn resolve_transmission_id(
        &mut self,
        cmd: &KittyCommand,
    ) -> u32 {
        if cmd.image_id != 0 {
            return cmd.image_id;
        }
        if cmd.image_number == 0 {
            return 0;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.number_to_id.insert(cmd.image_number, id);
        id
    }

    /// Store or replace a decoded image under `id`.
    pub fn store(
        &mut self,
        id: u32,
        image: DecodedImage,
        max_storage_bytes: usize,
    ) -> Result<(), KittyStoreError> {
        let previous_bytes = self.images.get(&id).map_or(0, image_storage_bytes);
        let image_bytes = image_storage_bytes(&image);
        let projected = self
            .used_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(image_bytes);
        if projected > max_storage_bytes {
            return Err(KittyStoreError::StorageLimitExceeded);
        }
        self.used_bytes = projected;
        self.images.insert(id, image);
        Ok(())
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
        if let Some(image) = self.images.remove(&id) {
            self.used_bytes = self.used_bytes.saturating_sub(image_storage_bytes(&image));
        }
        self.number_to_id.retain(|_, v| *v != id);
    }

    /// Drop all stored images and aliases.
    pub fn clear(&mut self) {
        self.images.clear();
        self.number_to_id.clear();
        self.used_bytes = 0;
    }

    /// Remove images by id range [lo, hi].
    pub fn remove_range(
        &mut self,
        lo: u32,
        hi: u32,
    ) {
        self.images.retain(|&id, image| {
            let keep = id < lo || id > hi;
            if !keep {
                self.used_bytes = self.used_bytes.saturating_sub(image_storage_bytes(image));
            }
            keep
        });
        self.number_to_id.retain(|_, v| *v < lo || *v > hi);
    }
}

fn image_storage_bytes(image: &DecodedImage) -> usize {
    image
        .frames
        .iter()
        .map(|frame| frame.pixels.len())
        .sum::<usize>()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkedTransmissionError {
    PayloadLimitExceeded,
}

impl ChunkedTransmission {
    /// Create an empty chunk accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Abort any in-progress multi-part upload.
    pub fn clear(&mut self) {
        self.cmd = None;
        self.payload.clear();
    }

    /// Feed a command. Returns `Some(merged_command)` when the final chunk
    /// arrives (`m=0`), or `None` while accumulating (`m=1`).
    pub fn feed(
        &mut self,
        cmd: KittyCommand,
        max_payload_bytes: usize,
    ) -> Result<Option<KittyCommand>, ChunkedTransmissionError> {
        let more = cmd.more;
        if self.payload.len().saturating_add(cmd.payload.len()) > max_payload_bytes {
            self.clear();
            return Err(ChunkedTransmissionError::PayloadLimitExceeded);
        }
        self.payload.extend_from_slice(&cmd.payload);

        if self.cmd.is_none() {
            self.cmd = Some(cmd);
        }

        if let Some(ref mut stored) = self.cmd {
            stored.more = more;
        }

        if more == 1 {
            return Ok(None);
        }

        let mut final_cmd = self.cmd.take().unwrap();
        final_cmd.payload = std::mem::take(&mut self.payload);
        final_cmd.more = 0;
        Ok(Some(final_cmd))
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
        assert_eq!(cmd.action, b't');
        assert_eq!(cmd.format, 32);
        assert_eq!(cmd.transmission, b'd');
        assert_eq!(cmd.payload, b"AAAA");
    }

    #[test]
    fn parse_file_range_and_relative_placement_keys() {
        let cmd = parse_command(b"a=p,i=5,p=9,S=123,O=7,U=1,P=3,Q=4,H=-2,V=6;");
        assert_eq!(cmd.action, b'p');
        assert_eq!(cmd.image_id, 5);
        assert_eq!(cmd.placement_id, 9);
        assert_eq!(cmd.data_size, 123);
        assert_eq!(cmd.data_offset, 7);
        assert!(cmd.virtual_placement);
        assert_eq!(cmd.parent_image_id, 3);
        assert_eq!(cmd.parent_placement_id, 4);
        assert_eq!(cmd.relative_col_offset, -2);
        assert_eq!(cmd.relative_row_offset, 6);
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
        assert!(chunked.feed(c1, usize::MAX).unwrap().is_none());

        let c2 = KittyCommand {
            more: 1,
            payload: b"BBBB".to_vec(),
            ..Default::default()
        };
        assert!(chunked.feed(c2, usize::MAX).unwrap().is_none());

        let c3 = KittyCommand {
            more: 0,
            payload: b"CCCC".to_vec(),
            ..Default::default()
        };
        let final_cmd = chunked.feed(c3, usize::MAX).unwrap().unwrap();
        assert_eq!(final_cmd.action, b'T');
        assert_eq!(final_cmd.format, 100);
        assert_eq!(final_cmd.image_id, 5);
        assert_eq!(final_cmd.payload, b"AAAABBBBCCCC");
    }

    #[test]
    fn chunked_accumulation_enforces_payload_limit() {
        let mut chunked = ChunkedTransmission::new();
        let first = KittyCommand {
            more: 1,
            payload: b"AAAA".to_vec(),
            ..Default::default()
        };
        assert!(chunked.feed(first, 8).unwrap().is_none());

        let second = KittyCommand {
            more: 0,
            payload: b"BBBBB".to_vec(),
            ..Default::default()
        };
        assert!(chunked.feed(second, 8).is_err());
        assert!(chunked.cmd.is_none());
        assert!(chunked.payload.is_empty());
    }

    #[test]
    fn image_number_transmissions_allocate_fresh_ids() {
        let mut store = KittyImageStore::new();
        let cmd = KittyCommand {
            image_number: 13,
            ..Default::default()
        };

        let first = store.resolve_transmission_id(&cmd);
        let second = store.resolve_transmission_id(&cmd);

        assert_ne!(first, second);
        assert_eq!(store.resolve_existing_id(&cmd), Some(second));
    }

    #[test]
    fn default_image_store_auto_assigns_nonzero_ids() {
        let mut store = KittyImageStore::default();
        let cmd = KittyCommand {
            image_number: 13,
            ..Default::default()
        };

        assert_ne!(store.resolve_transmission_id(&cmd), 0);
    }

    #[test]
    fn image_store_enforces_decoded_storage_limit() {
        let mut store = KittyImageStore::new();
        let image = DecodedImage::single_frame(2, 1, vec![0; 8]);

        assert!(store.store(1, image.clone(), 7).is_err());
        assert!(store.get(1).is_none());
        assert!(store.store(1, image, 8).is_ok());
        assert!(store.get(1).is_some());
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
    fn decode_png_format_accepts_jpeg_payload() {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;

        let jpeg = engine
            .decode(crate::SMALL_JPEG_B64)
            .expect("valid JPEG fixture");
        let b64 = engine.encode(jpeg);

        let cmd = KittyCommand {
            format: 100,
            ..Default::default()
        };
        let image = decode_payload(&cmd, b64.as_bytes()).unwrap();
        assert_eq!((image.width, image.height), (15, 7));
        assert_eq!(image.frames[0].pixels.len(), 15 * 7 * 4);
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
