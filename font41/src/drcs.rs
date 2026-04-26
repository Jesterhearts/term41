use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::RasterizedGlyph;

/// Sentinel font index used for DRCS glyphs in shaped output.
pub const FONT_INDEX: usize = usize::MAX - 1;
/// Number of glyph slots in one DEC DRCS character set.
pub const GLYPHS_PER_SET: u16 = 128;
const PUA_BASE: u32 = 0xF0000;

/// Terminal geometry class used when selecting a DRCS glyph variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryClass {
    /// 80 columns by 24 lines.
    Col80Line24,
    /// 132 columns by 24 lines.
    Col132Line24,
    /// 80 columns by 36 lines.
    Col80Line36,
    /// 132 columns by 36 lines.
    Col132Line36,
    /// 80 columns by 48 lines.
    Col80Line48,
    /// 132 columns by 48 lines.
    Col132Line48,
}

/// One DRCS glyph definition decoded from the terminal protocol.
#[derive(Debug, Clone)]
pub struct GlyphDef {
    /// Glyph id within the loaded DRCS set.
    pub glyph_id: u16,
    /// Source glyph width in pixels.
    pub width: u8,
    /// Source glyph height in pixels.
    pub height: u8,
    /// Whether the glyph should scale to the full terminal cell.
    pub full_cell: bool,
    /// Source bitmap coverage, one byte per source pixel.
    pub pixels: Vec<u8>,
}

/// Shared map of DRCS glyph definitions keyed by geometry and glyph id.
pub type GlyphMap = Arc<HashMap<(GeometryClass, u16), GlyphDef>>;

thread_local! {
    static CURRENT_GEOMETRY: Cell<Option<GeometryClass>> = const { Cell::new(None) };
    static CURRENT_GLYPHS: RefCell<Option<GlyphMap>> = const { RefCell::new(None) };
}

/// Guard that restores the previous thread-local DRCS context on drop.
pub struct GeometryGuard {
    geometry: Option<GeometryClass>,
    glyphs: Option<GlyphMap>,
}

impl Drop for GeometryGuard {
    fn drop(&mut self) {
        CURRENT_GEOMETRY.set(self.geometry);
        CURRENT_GLYPHS.replace(self.glyphs.take());
    }
}

/// Install the DRCS context used by subsequent glyph rasterization on this
/// thread, returning a guard that restores the previous context.
pub fn set_context(
    geometry: Option<GeometryClass>,
    glyphs: Option<GlyphMap>,
) -> GeometryGuard {
    let previous_geometry = CURRENT_GEOMETRY.replace(geometry);
    let previous_glyphs = CURRENT_GLYPHS.replace(glyphs);
    GeometryGuard {
        geometry: previous_geometry,
        glyphs: previous_glyphs,
    }
}

/// Encode a DRCS glyph id as a private-use Unicode scalar.
pub fn encode_char(glyph_id: u16) -> Option<char> {
    char::from_u32(PUA_BASE + glyph_id as u32)
}

pub fn encode_single(cell: &str) -> Option<u16> {
    let glyph_id = private_use_glyph_id(cell)?;
    if active_glyph_exists(glyph_id) {
        Some(glyph_id)
    } else {
        None
    }
}

fn private_use_glyph_id(cell: &str) -> Option<u16> {
    let mut chars = cell.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let cp = ch as u32;
    if !(PUA_BASE..PUA_BASE + (u16::MAX as u32) + 1).contains(&cp) {
        return None;
    }
    Some((cp - PUA_BASE) as u16)
}

fn active_glyph_exists(glyph_id: u16) -> bool {
    let Some(geometry) = CURRENT_GEOMETRY.get() else {
        return false;
    };
    let glyphs = CURRENT_GLYPHS.with(|current| current.borrow().as_ref().cloned());
    let Some(glyphs) = glyphs else {
        return false;
    };
    resolve_glyph(&glyphs, geometry, glyph_id).is_some()
}

pub fn rasterize(
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
) -> RasterizedGlyph {
    let Some(geometry) = CURRENT_GEOMETRY.get() else {
        return empty();
    };
    let glyphs = CURRENT_GLYPHS.with(|current| current.borrow().as_ref().cloned());
    let Some(glyphs) = glyphs else {
        return empty();
    };
    let Some(glyph) = resolve_glyph(&glyphs, geometry, glyph_id) else {
        return empty();
    };

    let gw = glyph.width.max(1) as u32;
    let gh = glyph.height.max(1) as u32;
    let target_w = if glyph.full_cell {
        cell_width.max(gw)
    } else {
        gw.min(cell_width.max(1))
    };
    let target_h = if glyph.full_cell {
        cell_height.max(gh)
    } else {
        gh.min(cell_height.max(1))
    };

    let draw_w = target_w.min(cell_width.max(1));
    let draw_h = target_h.min(cell_height.max(1));
    let offset_x = ((cell_width.max(draw_w) - draw_w) / 2) as i32;
    let offset_y = ((cell_height.max(draw_h) - draw_h) / 2) as i32;
    let mut bitmap = vec![0u8; (draw_w * draw_h * 4) as usize];

    for y in 0..draw_h {
        let src_y = y * gh / draw_h;
        for x in 0..draw_w {
            let src_x = x * gw / draw_w;
            let src_idx = (src_y * gw + src_x) as usize;
            if glyph.pixels.get(src_idx).copied().unwrap_or(0) == 0 {
                continue;
            }
            let dst_idx = ((y * draw_w + x) * 4) as usize;
            bitmap[dst_idx + 3] = 255;
        }
    }

    RasterizedGlyph {
        bitmap,
        width: draw_w,
        height: draw_h,
        bearing_x: offset_x,
        bearing_y: cell_height as i32 - offset_y,
        is_color: false,
    }
}

fn resolve_glyph(
    glyphs: &GlyphMap,
    geometry: GeometryClass,
    glyph_id: u16,
) -> Option<&GlyphDef> {
    glyphs
        .get(&(geometry, glyph_id))
        .or_else(|| fallback_glyph(glyphs, geometry, glyph_id))
}

fn fallback_glyph(
    glyphs: &GlyphMap,
    requested: GeometryClass,
    glyph_id: u16,
) -> Option<&GlyphDef> {
    let requested_cols = geometry_cols(requested);
    let requested_lines = geometry_lines(requested);
    glyphs
        .iter()
        .filter(|((geometry, id), _)| *id == glyph_id && geometry_cols(*geometry) == requested_cols)
        .min_by_key(|((geometry, _), _)| {
            let candidate_lines = geometry_lines(*geometry);
            requested_lines.abs_diff(candidate_lines)
        })
        .map(|(_, glyph)| glyph)
        .or_else(|| {
            glyphs
                .iter()
                .filter(|((_, id), _)| *id == glyph_id)
                .min_by_key(|((geometry, _), _)| {
                    let col_penalty = if geometry_cols(*geometry) == requested_cols {
                        0
                    } else {
                        1000
                    };
                    col_penalty + requested_lines.abs_diff(geometry_lines(*geometry))
                })
                .map(|(_, glyph)| glyph)
        })
}

fn geometry_cols(geometry: GeometryClass) -> u32 {
    match geometry {
        GeometryClass::Col80Line24 | GeometryClass::Col80Line36 | GeometryClass::Col80Line48 => 80,
        GeometryClass::Col132Line24 | GeometryClass::Col132Line36 | GeometryClass::Col132Line48 => {
            132
        }
    }
}

fn geometry_lines(geometry: GeometryClass) -> u32 {
    match geometry {
        GeometryClass::Col80Line24 | GeometryClass::Col132Line24 => 24,
        GeometryClass::Col80Line36 | GeometryClass::Col132Line36 => 36,
        GeometryClass::Col80Line48 | GeometryClass::Col132Line48 => 48,
    }
}

fn empty() -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: vec![],
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
        is_color: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_pixel_glyph(glyph_id: u16) -> GlyphDef {
        GlyphDef {
            glyph_id,
            width: 1,
            height: 1,
            full_cell: false,
            pixels: vec![1],
        }
    }

    #[test]
    fn encode_single_only_claims_active_drcs_glyphs() {
        let glyph_id = 1;
        let ch = encode_char(glyph_id).unwrap();

        assert_eq!(private_use_glyph_id(&ch.to_string()), Some(glyph_id));
        assert_eq!(encode_single(&ch.to_string()), None);

        let glyphs: GlyphMap = Arc::new(HashMap::from([(
            (GeometryClass::Col80Line24, glyph_id),
            single_pixel_glyph(glyph_id),
        )]));
        let _guard = set_context(Some(GeometryClass::Col80Line24), Some(glyphs));

        assert_eq!(encode_single(&ch.to_string()), Some(glyph_id));
    }

    #[test]
    fn encode_single_does_not_swallow_undefined_nerd_font_pua() {
        assert_eq!(encode_single("\u{f0001}"), None);
    }

    #[test]
    fn rasterize_falls_back_to_same_column_geometry() {
        let glyph_id = 7;
        let glyphs: GlyphMap = Arc::new(HashMap::from([(
            (GeometryClass::Col80Line24, glyph_id),
            single_pixel_glyph(glyph_id),
        )]));
        let _guard = set_context(Some(GeometryClass::Col80Line48), Some(glyphs));
        let raster = rasterize(glyph_id, 10, 8);
        assert!(raster.width > 0);
        assert!(raster.height > 0);
        assert!(raster.bitmap.iter().any(|&b| b != 0));
    }

    #[test]
    fn rasterize_prefers_matching_column_family() {
        let glyph_id = 9;
        let glyphs: GlyphMap = Arc::new(HashMap::from([
            (
                (GeometryClass::Col80Line24, glyph_id),
                GlyphDef {
                    pixels: vec![1],
                    ..single_pixel_glyph(glyph_id)
                },
            ),
            (
                (GeometryClass::Col132Line48, glyph_id),
                GlyphDef {
                    pixels: vec![0],
                    ..single_pixel_glyph(glyph_id)
                },
            ),
        ]));
        let _guard = set_context(Some(GeometryClass::Col80Line48), Some(glyphs));
        let raster = rasterize(glyph_id, 10, 8);
        assert!(raster.bitmap.iter().any(|&b| b != 0));
    }
}
