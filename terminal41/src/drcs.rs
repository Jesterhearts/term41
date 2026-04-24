use std::collections::HashMap;
use std::sync::Arc;

use font41::DRCS_GLYPHS_PER_SET;
use font41::DrcsGeometryClass;
use font41::DrcsGlyphDef;
use font41::DrcsGlyphMap;
use font41::encode_drcs_char;
use smol_str::SmolStr;

use crate::charset::CharacterSet;
use crate::feature::TerminalLimits;

pub const MAX_DRCS_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_DRCS_TOTAL_STORAGE_BYTES: usize = 256 * 1024;
pub const MAX_DRCS_GLYPHS_PER_LOAD: usize = 96;
const MAX_DRCS_BUFFERS: usize = 256;

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
pub struct DrcsStore {
    buffers: Vec<Option<Buffer>>,
    next_serial: u64,
    total_storage_bytes: usize,
    render_glyphs: DrcsGlyphMap,
}

impl Default for DrcsStore {
    fn default() -> Self {
        Self {
            buffers: vec![None; MAX_DRCS_BUFFERS],
            next_serial: 1,
            total_storage_bytes: 0,
            render_glyphs: Arc::new(HashMap::new()),
        }
    }
}

impl DrcsStore {
    pub fn clear(&mut self) {
        self.buffers.fill(None);
        self.total_storage_bytes = 0;
        self.render_glyphs = Arc::new(HashMap::new());
    }

    pub fn render_glyphs(&self) -> DrcsGlyphMap {
        self.render_glyphs.clone()
    }

    pub fn designation_for_buffer(
        &self,
        buffer_id: u8,
    ) -> Option<&[u8]> {
        self.buffers
            .get(buffer_id as usize)
            .and_then(|buffer| buffer.as_ref())
            .map(|buffer| buffer.designator.as_slice())
    }

    pub fn charset_for_designator(
        &self,
        designator: &[u8],
    ) -> Option<CharacterSet> {
        let mut best: Option<(u64, CharacterSet)> = None;
        for (idx, buffer) in self.buffers.iter().enumerate() {
            let Some(buffer) = buffer else {
                continue;
            };
            if buffer.designator != designator {
                continue;
            }
            let charset = CharacterSet::Drcs(idx as u8, buffer.charset_size);
            if best.is_none_or(|(serial, _)| buffer.serial > serial) {
                best = Some((buffer.serial, charset));
            }
        }
        best.map(|(_, charset)| charset)
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
        limits: TerminalLimits,
    ) {
        if payload.len() > limits.drcs_payload_bytes {
            warn!("DRCS load payload too large, ignoring");
            return;
        }
        let Some(load) = parse_load(params, payload) else {
            warn!("Failed to parse DRCS load, ignoring");
            return;
        };
        let Some(buffer_idx) = resolve_buffer_index(&load.designator, &self.buffers) else {
            warn!("Failed to resolve DRCS buffer index, ignoring");
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
            if self.total_storage_bytes.saturating_add(size) > limits.drcs_storage_bytes {
                warn!("DRCS store is full, cannot load more glyphs");
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
    designator: &[u8],
    buffers: &[Option<Buffer>],
) -> Option<usize> {
    buffers
        .iter()
        .position(|buffer| {
            buffer
                .as_ref()
                .is_some_and(|buffer| buffer.designator == designator)
        })
        .or_else(|| buffers.iter().position(Option::is_none))
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
    let _font_number = params.first().copied().unwrap_or(0);
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
        // VT soft-font downloads commonly use PCN=0 to mean "start at the
        // first printable slot" for 94-character sets. Rejecting that causes
        // the whole load to be discarded, which then leaves any subsequent
        // sampler output rendering as raw ASCII.
        CharsetSize::Cs94 => match pcn {
            0 | 1 => Some(0),
            2..=94 => Some((pcn - 1) as usize),
            _ => None,
        },
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

#[cfg(test)]
mod integration_tests {
    use super::MAX_DRCS_PAYLOAD_BYTES;
    use crate::test_support::TestTerm;

    #[test]
    fn decdld_loads_and_designates_soft_charset() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;1;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @!");

        let expected = font41::encode_drcs_char(0).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn decdld_accepts_pcn_zero_for_94_character_sets() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;0;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @!");

        let expected = font41::encode_drcs_char(0).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn decdld_supports_space_intermediate_designation() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;0;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @!");

        let expected = font41::encode_drcs_char(0).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn bundled_selftest_drcs_script_renders_soft_glyphs() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let script = include_str!("../../selftest41/resources/icon.drcs")
            .replace('\u{0090}', "\x1bP")
            .replace('\u{009c}', "\x1b\\");
        term.process(script.as_bytes());

        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_ne!(actual, '!');
        assert!((actual as u32) >= 0xF0000);
    }

    #[test]
    fn decdld_94_charset_maps_colon_to_its_own_glyph_slot() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;26;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @:");

        let expected = font41::encode_drcs_char((b':' - b'!') as u16).unwrap();
        let actual = term.visible_row(0).cells[0].chars().next().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn vtrex_cactus_snippet_writes_soft_glyphs_into_two_rows() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;55;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;87;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @");
        term.process(b"\x1b[10;30Hw\x08\x1bMW");

        let lower = term.visible_row(9).cells[29].chars().next().unwrap();
        let upper = term.visible_row(8).cells[29].chars().next().unwrap();
        assert_eq!(
            lower,
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        assert_eq!(
            upper,
            font41::encode_drcs_char((b'W' - b'!') as u16).unwrap()
        );
    }

    #[test]
    fn vtrex_trex_snippet_writes_soft_glyphs_into_two_rows() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;15;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;26;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;28;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bP1;64;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1b( @");
        term.process(b"\x1b[7;8H:<\x08\x08\x0b/`");

        let top_left = term.visible_row(6).cells[7].chars().next().unwrap();
        let top_right = term.visible_row(6).cells[8].chars().next().unwrap();
        let bottom_left = term.visible_row(7).cells[7].chars().next().unwrap();
        let bottom_right = term.visible_row(7).cells[8].chars().next().unwrap();
        assert_eq!(
            top_left,
            font41::encode_drcs_char((b':' - b'!') as u16).unwrap()
        );
        assert_eq!(
            top_right,
            font41::encode_drcs_char((b'<' - b'!') as u16).unwrap()
        );
        assert_eq!(
            bottom_left,
            font41::encode_drcs_char((b'/' - b'!') as u16).unwrap()
        );
        assert_eq!(
            bottom_right,
            font41::encode_drcs_char((b'`' - b'!') as u16).unwrap()
        );
    }

    #[test]
    fn vtrex_soft_font_load_contains_trex_and_cactus_glyph_defs() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        for pcn in [15u16, 26, 28, 55, 64, 65, 78, 87] {
            term.process(format!("\x1bP1;{pcn};1;6;0;2;16;0{{ @~~~~~~\x1b\\").as_bytes());
        }
        let glyphs = term.drcs_render_glyphs();
        let geometry = font41::DrcsGeometryClass::Col80Line24;

        for byte in [b':', b'<', b'/', b'`', b'w', b'W', b'n', b'a'] {
            let glyph_id = byte as u16 - b'!' as u16;
            assert!(
                glyphs.contains_key(&(geometry, glyph_id)),
                "missing DRCS glyph for byte {byte:?} -> id {glyph_id}"
            );
        }
    }

    #[test]
    fn vtrex_trex_and_cactus_drcs_glyphs_rasterize_non_empty() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        for pcn in [15u16, 26, 28, 55, 64, 65, 78, 87] {
            term.process(format!("\x1bP1;{pcn};1;6;0;2;16;0{{ @~~~~~~\x1b\\").as_bytes());
        }

        let mut font_system = font41::FontSystem::new(None, 16.0, 1);
        let _guard = font41::set_drcs_context(
            Some(font41::DrcsGeometryClass::Col80Line24),
            Some(term.drcs_render_glyphs()),
        );

        for byte in [b':', b'<', b'/', b'`', b'w', b'W', b'n', b'a'] {
            let glyph_id = byte as u16 - b'!' as u16;
            let cell = font41::encode_drcs_char(glyph_id).unwrap().to_string();
            let shaped = font_system.shape_row(
                &[smol_str::SmolStr::new(cell)],
                &[font41::attrs::CellAttrs::default()],
            );
            let raster = font_system.rasterize_glyph(shaped[0].font_index, shaped[0].glyph_id, 1);
            assert!(
                raster.width > 0 && raster.height > 0 && !raster.bitmap.is_empty(),
                "empty raster for byte {byte:?} -> id {glyph_id}"
            );
        }
    }

    #[test]
    fn vtrex_page_composition_copies_cactus_and_trex_to_visible_page() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        for pcn in [15u16, 26, 28, 55, 64, 65, 78, 87] {
            term.process(format!("\x1bP1;{pcn};1;6;0;2;16;0{{ @~~~~~~\x1b\\").as_bytes());
        }

        term.process(b"\x1b[?64l");
        term.process(b"\x1b[2 P\x1b( @");
        term.process(b"\x1b[10;30Hw\x08\x1bMW");
        let page2 = crate::screen::page_viewport(&term.active, &term.viewport, 2).unwrap();
        assert_eq!(
            term.active.grid.rows[page2.top + 9].cells[29]
                .chars()
                .next()
                .unwrap(),
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        term.process(b"\x1b[1;1;10;30;2;1;1;3$v");
        let page3 = crate::screen::page_viewport(&term.active, &term.viewport, 3).unwrap();
        assert_eq!(
            term.active.grid.rows[page3.top + 9].cells[29]
                .chars()
                .next()
                .unwrap(),
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        term.process(b"\x1b[3 P\x1b[7;8H:<\x08\x08\x0b/`");
        assert_eq!(
            term.active.grid.rows[page3.top + 6].cells[7]
                .chars()
                .next()
                .unwrap(),
            font41::encode_drcs_char((b':' - b'!') as u16).unwrap()
        );
        term.process(b"\x1b[1 P\x1b[1;1;10;30;3;1;1;1$v");

        let cactus_lower = term.visible_row(9).cells[29].chars().next().unwrap();
        let cactus_upper = term.visible_row(8).cells[29].chars().next().unwrap();
        let trex_top_left = term.visible_row(6).cells[7].chars().next().unwrap();
        let trex_top_right = term.visible_row(6).cells[8].chars().next().unwrap();
        let trex_bottom_left = term.visible_row(7).cells[7].chars().next().unwrap();
        let trex_bottom_right = term.visible_row(7).cells[8].chars().next().unwrap();

        assert_eq!(
            cactus_lower,
            font41::encode_drcs_char((b'w' - b'!') as u16).unwrap()
        );
        assert_eq!(
            cactus_upper,
            font41::encode_drcs_char((b'W' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_top_left,
            font41::encode_drcs_char((b':' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_top_right,
            font41::encode_drcs_char((b'<' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_bottom_left,
            font41::encode_drcs_char((b'/' - b'!') as u16).unwrap()
        );
        assert_eq!(
            trex_bottom_right,
            font41::encode_drcs_char((b'`' - b'!') as u16).unwrap()
        );
    }

    #[test]
    fn ris_clears_loaded_soft_charsets() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1bP1;1;1;6;0;2;16;0{ @~~~~~~\x1b\\");
        term.process(b"\x1bc");
        term.process(b"\x1b( @!");
        assert_eq!(term.visible_row(0).cells[0].as_str(), "!");
    }

    #[test]
    fn oversized_drcs_payload_is_discarded() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let mut seq = b"\x1bP1;1;1;6;0;2;16;0{ @".to_vec();
        seq.extend(std::iter::repeat_n(b'~', MAX_DRCS_PAYLOAD_BYTES + 32));
        seq.extend_from_slice(b"\x1b\\");
        term.process(&seq);
        term.process(b"\x1b( @!");
        assert_eq!(term.visible_row(0).cells[0].as_str(), "!");
    }
}
