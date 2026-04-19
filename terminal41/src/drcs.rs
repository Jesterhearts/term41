use std::collections::HashMap;
use std::sync::Arc;

use font41::DRCS_GLYPHS_PER_SET;
use font41::DrcsGeometryClass;
use font41::DrcsGlyphDef;
use font41::DrcsGlyphMap;
use font41::encode_drcs_char;
use smol_str::SmolStr;

use crate::charset::CharacterSet;

pub const MAX_DRCS_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_DRCS_TOTAL_STORAGE_BYTES: usize = 256 * 1024;
pub const MAX_DRCS_GLYPHS_PER_LOAD: usize = 96;
const MAX_DRCS_BUFFERS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharsetSize {
    Cs94,
    Cs96,
}

#[derive(Debug, Clone)]
pub struct GlyphPattern {
    pub width: u8,
    pub height: u8,
    pub full_cell: bool,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
struct Variation {
    geometry: DrcsGeometryClass,
    glyphs: Vec<Option<GlyphPattern>>,
    storage_bytes: usize,
}

#[derive(Debug, Clone)]
struct Buffer {
    designator: Vec<u8>,
    charset_size: CharsetSize,
    variations: HashMap<DrcsGeometryClass, Variation>,
    serial: u64,
}

#[derive(Debug, Clone)]
pub struct Store {
    buffers: Vec<Option<Buffer>>,
    next_serial: u64,
    total_storage_bytes: usize,
    render_glyphs: DrcsGlyphMap,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            buffers: vec![None; MAX_DRCS_BUFFERS],
            next_serial: 1,
            total_storage_bytes: 0,
            render_glyphs: Arc::new(HashMap::new()),
        }
    }
}

impl Store {
    pub fn clear(&mut self) {
        self.buffers.fill(None);
        self.total_storage_bytes = 0;
        self.render_glyphs = Arc::new(HashMap::new());
    }

    pub fn render_glyphs(&self) -> DrcsGlyphMap {
        self.render_glyphs.clone()
    }

    pub fn lookup_designation(
        &self,
        intermediates: &[u8],
        final_byte: u8,
    ) -> Option<CharacterSet> {
        let (_, designator_intermediates) = intermediates.split_first()?;
        let designator = build_designator(designator_intermediates, final_byte)?;
        let mut best: Option<(u64, usize)> = None;
        for (idx, buffer) in self.buffers.iter().enumerate() {
            let Some(buffer) = buffer else {
                continue;
            };
            if buffer.designator != designator {
                continue;
            }
            if best.is_none_or(|(serial, _)| buffer.serial > serial) {
                best = Some((buffer.serial, idx));
            }
        }
        best.map(|(_, idx)| {
            let buffer = self.buffers[idx].as_ref().expect("matched buffer");
            CharacterSet::Drcs(idx as u8, buffer.charset_size)
        })
    }

    pub fn define(
        &mut self,
        params: &[u16],
        payload: &[u8],
    ) {
        if payload.len() > MAX_DRCS_PAYLOAD_BYTES {
            return;
        }
        let Some(load) = parse_load(params, payload) else {
            return;
        };
        let Some(buffer_idx) = resolve_buffer_index(load.font_number, &self.buffers) else {
            return;
        };

        let previous = self.buffers[buffer_idx].take();
        let mut buffer = previous.unwrap_or_else(|| Buffer {
            designator: load.designator.clone(),
            charset_size: load.charset_size,
            variations: HashMap::new(),
            serial: 0,
        });

        if buffer.designator != load.designator || buffer.charset_size != load.charset_size {
            self.total_storage_bytes = self.total_storage_bytes.saturating_sub(
                buffer
                    .variations
                    .values()
                    .map(|v| v.storage_bytes)
                    .sum::<usize>(),
            );
            buffer = Buffer {
                designator: load.designator.clone(),
                charset_size: load.charset_size,
                variations: HashMap::new(),
                serial: 0,
            };
        }

        apply_erase_control(
            &mut buffer,
            load.erase_control,
            load.geometry,
            &mut self.total_storage_bytes,
        );

        let variation = buffer
            .variations
            .entry(load.geometry)
            .or_insert_with(|| Variation {
                geometry: load.geometry,
                glyphs: vec![None; charset_len(load.charset_size)],
                storage_bytes: 0,
            });

        for (idx, glyph) in load.glyphs.into_iter().enumerate() {
            let slot = load.start_index + idx;
            if slot >= variation.glyphs.len() {
                break;
            }
            if let Some(existing) = variation.glyphs[slot].take() {
                let size = glyph_storage_bytes(&existing);
                variation.storage_bytes = variation.storage_bytes.saturating_sub(size);
                self.total_storage_bytes = self.total_storage_bytes.saturating_sub(size);
            }
            let size = glyph_storage_bytes(&glyph);
            if self.total_storage_bytes.saturating_add(size) > MAX_DRCS_TOTAL_STORAGE_BYTES {
                continue;
            }
            self.total_storage_bytes += size;
            variation.storage_bytes += size;
            variation.glyphs[slot] = Some(glyph);
        }

        buffer.serial = self.next_serial;
        self.next_serial += 1;
        self.buffers[buffer_idx] = Some(buffer);
        self.rebuild_render_glyphs();
    }

    fn rebuild_render_glyphs(&mut self) {
        let mut glyphs = HashMap::new();
        for (buffer_idx, buffer) in self.buffers.iter().enumerate() {
            let Some(buffer) = buffer else {
                continue;
            };
            for variation in buffer.variations.values() {
                for (table_idx, glyph) in variation.glyphs.iter().enumerate() {
                    let Some(glyph) = glyph else {
                        continue;
                    };
                    glyphs.insert(
                        (
                            variation.geometry,
                            glyph_id(buffer_idx as u8, table_idx as u16),
                        ),
                        DrcsGlyphDef {
                            glyph_id: glyph_id(buffer_idx as u8, table_idx as u16),
                            width: glyph.width,
                            height: glyph.height,
                            full_cell: glyph.full_cell,
                            pixels: glyph.pixels.clone(),
                        },
                    );
                }
            }
        }
        self.render_glyphs = Arc::new(glyphs);
    }
}

#[derive(Debug)]
struct LoadRequest {
    font_number: u16,
    start_index: usize,
    erase_control: u16,
    geometry: DrcsGeometryClass,
    charset_size: CharsetSize,
    designator: Vec<u8>,
    glyphs: Vec<GlyphPattern>,
}

pub fn translate_byte(
    buffer_id: u8,
    charset_size: CharsetSize,
    byte: u8,
) -> Option<SmolStr> {
    let table_index = match charset_size {
        CharsetSize::Cs94 => {
            if !(0x21..=0x7E).contains(&byte) {
                return None;
            }
            byte - 0x21
        }
        CharsetSize::Cs96 => {
            if !(0x20..=0x7F).contains(&byte) {
                return None;
            }
            byte - 0x20
        }
    };
    let ch = encode_drcs_char(glyph_id(buffer_id, table_index as u16))?;
    Some(SmolStr::new(ch.encode_utf8(&mut [0u8; 4]) as &str))
}

fn glyph_id(
    buffer_id: u8,
    table_index: u16,
) -> u16 {
    buffer_id as u16 * DRCS_GLYPHS_PER_SET + table_index
}

fn resolve_buffer_index(
    font_number: u16,
    buffers: &[Option<Buffer>],
) -> Option<usize> {
    match font_number {
        0 => buffers.iter().position(Option::is_none).or(Some(0)),
        1 | 2 | 3 => Some((font_number - 1) as usize).filter(|idx| *idx < buffers.len()),
        _ => None,
    }
}

fn apply_erase_control(
    buffer: &mut Buffer,
    erase_control: u16,
    geometry: DrcsGeometryClass,
    total_storage_bytes: &mut usize,
) {
    match erase_control {
        0 => {
            if let Some(variation) = buffer.variations.remove(&geometry) {
                *total_storage_bytes = total_storage_bytes.saturating_sub(variation.storage_bytes);
            }
        }
        2 => {
            let removed = buffer
                .variations
                .values()
                .map(|v| v.storage_bytes)
                .sum::<usize>();
            *total_storage_bytes = total_storage_bytes.saturating_sub(removed);
            buffer.variations.clear();
        }
        _ => {}
    }
}

fn parse_load(
    params: &[u16],
    payload: &[u8],
) -> Option<LoadRequest> {
    let font_number = params.first().copied().unwrap_or(0);
    let pcn = params.get(1).copied().unwrap_or(0);
    let erase_control = params.get(2).copied().unwrap_or(0);
    let pcmw = params.get(3).copied().unwrap_or(0);
    let pss = params.get(4).copied().unwrap_or(0);
    let pt = params.get(5).copied().unwrap_or(0);
    let pcmh = params.get(6).copied().unwrap_or(0);
    let pcss = params.get(7).copied().unwrap_or(0);

    let charset_size = match pcss {
        0 => CharsetSize::Cs94,
        1 => CharsetSize::Cs96,
        _ => return None,
    };
    let start_index = start_index(charset_size, pcn)?;
    let geometry = parse_geometry(pss)?;
    let (designator, bitmap_bytes) = split_designator(payload)?;
    let (default_w, default_h) = default_dimensions(geometry);
    let glyph_width = if pcmw == 0 {
        default_w
    } else {
        pcmw.min(16) as u8
    };
    let glyph_height = if pcmh == 0 {
        default_h
    } else {
        pcmh.min(16) as u8
    };
    let full_cell = pt == 2;
    let mut glyphs = vec![];

    for pattern in bitmap_bytes.split(|&b| b == b';') {
        if pattern.is_empty() {
            continue;
        }
        if glyphs.len() >= MAX_DRCS_GLYPHS_PER_LOAD {
            break;
        }
        glyphs.push(parse_pattern(
            pattern,
            glyph_width,
            glyph_height,
            full_cell,
        )?);
    }
    if glyphs.is_empty() {
        return None;
    }

    Some(LoadRequest {
        font_number,
        start_index,
        erase_control,
        geometry,
        charset_size,
        designator,
        glyphs,
    })
}

fn split_designator(payload: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut idx = 0;
    while idx < payload.len() && idx < 2 && (0x20..=0x2F).contains(&payload[idx]) {
        idx += 1;
    }
    if idx >= payload.len() || !(0x30..=0x7E).contains(&payload[idx]) {
        return None;
    }
    idx += 1;
    Some((payload[..idx].to_vec(), &payload[idx..]))
}

fn build_designator(
    intermediates: &[u8],
    final_byte: u8,
) -> Option<Vec<u8>> {
    if intermediates.len() > 2 {
        return None;
    }
    if intermediates.iter().any(|b| !(0x20..=0x2F).contains(b)) {
        return None;
    }
    if !(0x30..=0x7E).contains(&final_byte) {
        return None;
    }
    let mut designator = intermediates.to_vec();
    designator.push(final_byte);
    Some(designator)
}

fn parse_geometry(pss: u16) -> Option<DrcsGeometryClass> {
    match pss {
        0 | 1 => Some(DrcsGeometryClass::Col80Line24),
        2 => Some(DrcsGeometryClass::Col132Line24),
        11 => Some(DrcsGeometryClass::Col80Line36),
        12 => Some(DrcsGeometryClass::Col132Line36),
        21 => Some(DrcsGeometryClass::Col80Line48),
        22 => Some(DrcsGeometryClass::Col132Line48),
        _ => None,
    }
}

fn default_dimensions(geometry: DrcsGeometryClass) -> (u8, u8) {
    match geometry {
        DrcsGeometryClass::Col80Line24 => (10, 16),
        DrcsGeometryClass::Col132Line24 => (6, 16),
        DrcsGeometryClass::Col80Line36 => (10, 10),
        DrcsGeometryClass::Col132Line36 => (6, 10),
        DrcsGeometryClass::Col80Line48 => (10, 8),
        DrcsGeometryClass::Col132Line48 => (6, 8),
    }
}

fn charset_len(size: CharsetSize) -> usize {
    match size {
        CharsetSize::Cs94 => 95,
        CharsetSize::Cs96 => 96,
    }
}

fn start_index(
    charset_size: CharsetSize,
    pcn: u16,
) -> Option<usize> {
    match charset_size {
        CharsetSize::Cs94 => {
            if !(1..=94).contains(&pcn) {
                return None;
            }
            Some((pcn - 1) as usize)
        }
        CharsetSize::Cs96 => (pcn <= 95).then_some(pcn as usize),
    }
}

fn parse_pattern(
    pattern: &[u8],
    width: u8,
    height: u8,
    full_cell: bool,
) -> Option<GlyphPattern> {
    let width = width.max(1);
    let height = height.max(1);
    let mut pixels = vec![0u8; width as usize * height as usize];
    for (group, chunk) in pattern.split(|&b| b == b'/').enumerate() {
        for (x, &sixel) in chunk.iter().enumerate() {
            if x >= width as usize || !(b'?'..=b'~').contains(&sixel) {
                continue;
            }
            let bits = sixel - b'?';
            for bit in 0..6 {
                let y = group * 6 + bit;
                if y >= height as usize {
                    break;
                }
                if bits & (1 << bit) != 0 {
                    pixels[y * width as usize + x] = 1;
                }
            }
        }
    }
    Some(GlyphPattern {
        width,
        height,
        full_cell,
        pixels,
    })
}

fn glyph_storage_bytes(glyph: &GlyphPattern) -> usize {
    glyph.pixels.len()
}
