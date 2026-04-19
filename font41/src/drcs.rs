use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::RasterizedGlyph;

pub const FONT_INDEX: usize = usize::MAX - 1;
pub const GLYPHS_PER_SET: u16 = 128;
const PUA_BASE: u32 = 0xF0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryClass {
    Col80Line24,
    Col132Line24,
    Col80Line36,
    Col132Line36,
    Col80Line48,
    Col132Line48,
}

#[derive(Debug, Clone)]
pub struct GlyphDef {
    pub glyph_id: u16,
    pub width: u8,
    pub height: u8,
    pub full_cell: bool,
    pub pixels: Vec<u8>,
}

pub type GlyphMap = Arc<HashMap<(GeometryClass, u16), GlyphDef>>;

thread_local! {
    static CURRENT_GEOMETRY: Cell<Option<GeometryClass>> = const { Cell::new(None) };
    static CURRENT_GLYPHS: RefCell<Option<GlyphMap>> = const { RefCell::new(None) };
}

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

pub fn encode_char(glyph_id: u16) -> Option<char> {
    char::from_u32(PUA_BASE + glyph_id as u32)
}

pub fn encode_single(cell: &str) -> Option<u16> {
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
    let Some(glyph) = glyphs.get(&(geometry, glyph_id)) else {
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
