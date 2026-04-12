use std::collections::HashMap;
use std::sync::Arc;

use harfrust::Direction;
use harfrust::FontRef;
use harfrust::Script;
use harfrust::ShapePlan;
use harfrust::ShaperData;
use harfrust::UnicodeBuffer;
use raqote::DrawOptions;
use raqote::DrawTarget;
use raqote::PathBuilder;
use raqote::SolidSource;
use raqote::Source;
use read_fonts::TableProvider;
use read_fonts::tables::glyf::Anchor;
use read_fonts::tables::glyf::CompositeGlyph;
use read_fonts::tables::glyf::CurvePoint;
use read_fonts::tables::glyf::Glyph;
use read_fonts::tables::glyf::SimpleGlyph;
use read_fonts::tables::loca::Loca;
use read_fonts::types::GlyphId;

/// The embedded Fairfax HD font (ultimate fallback).
static FAIRFAX_HD: &[u8] = include_bytes!("../resources/fonts/FairfaxHD.ttf");

/// Rasterized glyph data ready for upload to a texture atlas.
pub struct RasterizedGlyph {
    pub bitmap: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
}

/// A shaped glyph with its position info, ready for rendering.
pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub font_index: usize,
    pub col: u16,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// A loaded font with its shaping data and raw bytes.
struct LoadedFont {
    data: Arc<Vec<u8>>,
    shaper_data: ShaperData,
    units_per_em: f32,
}

/// Key for the ShapePlan cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PlanKey {
    font_index: usize,
    direction: Direction,
    script: Script,
}

/// Font system: manages an ordered list of fonts with fallback and plan
/// caching.
pub struct FontSystem {
    fonts: Vec<LoadedFont>,
    plan_cache: HashMap<PlanKey, ShapePlan>,
    pub cell_width: u32,
    pub cell_height: u32,
    pub font_size: f32,
    ascent: f32,
}

impl FontSystem {
    pub fn new(
        fonts_config: Option<&str>,
        font_size: f32,
    ) -> Self {
        let mut fonts = Vec::new();

        if let Some(families_str) = fonts_config {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();

            for family_name in families_str.split(',').map(|s| s.trim()) {
                if family_name.is_empty() {
                    continue;
                }

                let family = match family_name.to_lowercase().as_str() {
                    "monospace" => fontdb::Family::Monospace,
                    "serif" => fontdb::Family::Serif,
                    "sans-serif" | "sans serif" => fontdb::Family::SansSerif,
                    _ => fontdb::Family::Name(family_name),
                };

                let query = fontdb::Query {
                    families: &[family],
                    ..Default::default()
                };

                if let Some(id) = db.query(&query) {
                    let loaded = db.with_face_data(id, |data, _face_index| load_font(data));
                    if let Some(Some(font)) = loaded {
                        log::info!("loaded font: {family_name}");
                        fonts.push(font);
                    }
                } else {
                    log::warn!("font not found: {family_name}");
                }
            }
        }

        // Always append embedded Fairfax HD as ultimate fallback.
        fonts.push(load_font(FAIRFAX_HD).expect("embedded font must load"));

        // Compute cell metrics from the first font.
        let primary = &fonts[0];
        let rf = read_fonts::FontRef::new(&primary.data).expect("parse font");
        let hhea = rf.hhea().expect("hhea table");
        let hmtx = rf.hmtx().expect("hmtx table");

        let scale = font_size / primary.units_per_em;
        let ascent = hhea.ascender().to_i16() as f32 * scale;
        let descent = hhea.descender().to_i16() as f32 * scale;
        let line_gap = hhea.line_gap().to_i16() as f32 * scale;
        let cell_height = (ascent - descent + line_gap).ceil() as u32;

        let m_advance = hmtx
            .advance(GlyphId::new(charmap_lookup(&rf, 'M')))
            .unwrap_or(0) as f32
            * scale;
        let cell_width = m_advance.ceil() as u32;

        Self {
            fonts,
            plan_cache: HashMap::new(),
            cell_width,
            cell_height,
            font_size,
            ascent,
        }
    }

    pub fn grid_size(
        &self,
        cols: u32,
        rows: u32,
    ) -> (u32, u32) {
        (cols * self.cell_width, rows * self.cell_height)
    }

    pub fn grid_dimensions(
        &self,
        pixel_width: u32,
        pixel_height: u32,
    ) -> (u32, u32) {
        let cols = (pixel_width / self.cell_width).max(1);
        let rows = (pixel_height / self.cell_height).max(1);
        (cols, rows)
    }

    pub fn baseline_offset(&self) -> f32 {
        self.ascent
    }

    /// Shape an entire terminal row with font fallback and plan caching.
    /// Takes `&[char]` directly from the terminal's SoA storage.
    pub fn shape_row(
        &mut self,
        chars: &[char],
    ) -> Vec<ShapedGlyph> {
        if chars.is_empty() {
            return vec![];
        }

        // Build the row string and byte-offset → column mapping.
        let mut row_text = String::new();
        let mut col_map: Vec<u16> = Vec::new();
        for (col, &ch) in chars.iter().enumerate() {
            let start = row_text.len();
            row_text.push(ch);
            let added = row_text.len() - start;
            for _ in 0..added {
                col_map.push(col as u16);
            }
        }

        // Track which columns still need a glyph (for fallback).
        let mut has_glyph = vec![false; chars.len()];
        let mut result: Vec<ShapedGlyph> = Vec::with_capacity(chars.len());

        for (font_idx, loaded) in self.fonts.iter().enumerate() {
            let font_ref = match FontRef::new(&loaded.data) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut buffer = UnicodeBuffer::new();
            buffer.push_str(&row_text);
            buffer.guess_segment_properties();

            let direction = buffer.direction();
            let script = buffer.script();

            let key = PlanKey {
                font_index: font_idx,
                direction,
                script,
            };

            // Get or create cached plan.
            self.plan_cache.entry(key).or_insert_with(|| {
                let shaper = loaded.shaper_data.shaper(&font_ref).build();

                ShapePlan::new(
                    &shaper,
                    direction,
                    Some(script),
                    buffer.language().as_ref(),
                    &[],
                )
            });
            let plan = &self.plan_cache[&key];

            let shaper = loaded.shaper_data.shaper(&font_ref).build();
            let output = shaper.shape_with_plan(plan, buffer, &[]);

            let infos = output.glyph_infos();
            let positions = output.glyph_positions();
            let scale = self.font_size / loaded.units_per_em;

            for (i, (info, pos)) in infos.iter().zip(positions.iter()).enumerate() {
                let cluster = info.cluster as usize;
                if cluster >= col_map.len() {
                    continue;
                }
                let col = col_map[cluster];

                // Skip if this column already has a glyph from a higher-priority font.
                if has_glyph[col as usize] {
                    continue;
                }

                let glyph_id = info.glyph_id as u16;
                // glyph_id 0 is .notdef — try next font.
                if glyph_id == 0 {
                    continue;
                }

                // Mark all columns consumed by this glyph. For ligatures
                // (e.g. `::` → single glyph), the cluster gap between
                // consecutive output glyphs tells us which input columns
                // were merged. Without this, fallback fonts would place
                // individual glyphs on top of the ligature.
                let end_byte = if i + 1 < infos.len() {
                    (infos[i + 1].cluster as usize).min(col_map.len())
                } else {
                    col_map.len()
                };
                for byte in cluster..end_byte {
                    if byte < col_map.len() {
                        has_glyph[col_map[byte] as usize] = true;
                    }
                }

                result.push(ShapedGlyph {
                    glyph_id,
                    font_index: font_idx,
                    col,
                    x_offset: pos.x_offset as f32 * scale,
                    y_offset: pos.y_offset as f32 * scale,
                });
            }

            // If all non-space columns are filled, stop trying fonts.
            let all_covered = has_glyph
                .iter()
                .enumerate()
                .all(|(i, &has)| has || chars[i] == ' ');
            if all_covered {
                break;
            }
        }

        result
    }

    /// Rasterize a glyph from a specific font in the chain.
    pub fn rasterize_glyph(
        &self,
        font_index: usize,
        glyph_index: u16,
    ) -> RasterizedGlyph {
        let loaded = &self.fonts[font_index];
        let scale = self.font_size / loaded.units_per_em;
        let rf = match read_fonts::FontRef::new(&loaded.data) {
            Ok(f) => f,
            Err(_) => return empty_glyph(),
        };
        let loca = match rf.loca(None) {
            Ok(l) => l,
            Err(_) => return empty_glyph(),
        };
        let glyf = match rf.glyf() {
            Ok(g) => g,
            Err(_) => return empty_glyph(),
        };

        let gid = GlyphId::new(glyph_index as u32);
        match loca.get_glyf(gid, &glyf) {
            Ok(Some(Glyph::Simple(simple))) => rasterize_simple_glyph(&simple, scale),
            Ok(Some(Glyph::Composite(composite))) => {
                rasterize_composite_glyph(&composite, &loca, &glyf, scale)
            }
            _ => empty_glyph(),
        }
    }
}

fn empty_glyph() -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: vec![],
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
    }
}

fn load_font(data: &[u8]) -> Option<LoadedFont> {
    let data = Arc::new(data.to_vec());
    let font_ref = FontRef::new(&data).ok()?;
    let shaper_data = ShaperData::new(&font_ref);
    let rf = read_fonts::FontRef::new(&data).ok()?;
    let head = rf.head().ok()?;
    let units_per_em = head.units_per_em() as f32;

    Some(LoadedFont {
        data,
        shaper_data,
        units_per_em,
    })
}

fn charmap_lookup(
    font: &read_fonts::FontRef,
    ch: char,
) -> u32 {
    let cmap = match font.cmap() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for record in cmap.encoding_records() {
        if let Ok(subtable) = record.subtable(cmap.offset_data())
            && let Some(gid) = subtable.map_codepoint(ch)
        {
            return gid.to_u32();
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Rasterization via raqote
// ---------------------------------------------------------------------------

fn rasterize_simple_glyph(
    simple: &SimpleGlyph,
    scale: f32,
) -> RasterizedGlyph {
    let x_min = (simple.x_min() as f32 * scale).floor();
    let y_max = (simple.y_max() as f32 * scale).ceil();
    let width = ((simple.x_max() as f32 * scale).ceil() - x_min) as i32 + 2;
    let height = (y_max - (simple.y_min() as f32 * scale).floor()) as i32 + 2;

    if width <= 0 || height <= 0 {
        return empty_glyph();
    }

    let path = build_path(simple, scale, x_min, y_max);
    let mut dt = DrawTarget::new(width, height);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    let pixels = dt.get_data();
    let mut bitmap = vec![0u8; (width * height) as usize];
    for (i, &pixel) in pixels.iter().enumerate() {
        bitmap[i] = (pixel >> 24) as u8;
    }

    RasterizedGlyph {
        bitmap,
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
    }
}

/// Recursively collect the bounding box of a composite glyph, flattening
/// nested composites so that leaf simple-glyph components at any depth are
/// included.
fn composite_bounds(
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
    parent_dx: f32,
    parent_dy: f32,
    bounds: &mut [f32; 4], // [x_min, y_min, x_max, y_max]
) {
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let glyph = match loca.get_glyf(gid, glyf) {
            Ok(Some(g)) => g,
            _ => continue,
        };

        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32 * scale + parent_dx, y as f32 * scale + parent_dy),
            _ => (parent_dx, parent_dy),
        };

        match glyph {
            Glyph::Simple(simple) => {
                bounds[0] = bounds[0].min(simple.x_min() as f32 * scale + dx);
                bounds[1] = bounds[1].min(simple.y_min() as f32 * scale + dy);
                bounds[2] = bounds[2].max(simple.x_max() as f32 * scale + dx);
                bounds[3] = bounds[3].max(simple.y_max() as f32 * scale + dy);
            }
            Glyph::Composite(inner) => {
                composite_bounds(&inner, loca, glyf, scale, dx, dy, bounds);
            }
        }
    }
}

/// Recursively add all simple-glyph leaf components of a composite to a path,
/// accumulating offsets through any nesting depth.
fn composite_to_path(
    pb: &mut PathBuilder,
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
    x_min: f32,
    y_max: f32,
    parent_dx: f32,
    parent_dy: f32,
) {
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let glyph = match loca.get_glyf(gid, glyf) {
            Ok(Some(g)) => g,
            _ => continue,
        };

        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32 * scale + parent_dx, y as f32 * scale + parent_dy),
            _ => (parent_dx, parent_dy),
        };

        match glyph {
            Glyph::Simple(simple) => {
                add_simple_glyph_to_path(pb, &simple, scale, x_min, y_max, dx, dy);
            }
            Glyph::Composite(inner) => {
                composite_to_path(pb, &inner, loca, glyf, scale, x_min, y_max, dx, dy);
            }
        }
    }
}

fn rasterize_composite_glyph(
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
) -> RasterizedGlyph {
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    composite_bounds(composite, loca, glyf, scale, 0.0, 0.0, &mut bounds);
    let [x_min_all, y_min_all, x_max_all, y_max_all] = bounds;

    if x_min_all >= x_max_all || y_min_all >= y_max_all {
        return empty_glyph();
    }

    let x_min = x_min_all.floor();
    let y_max = y_max_all.ceil();
    let width = (x_max_all.ceil() - x_min) as i32 + 2;
    let height = (y_max - y_min_all.floor()) as i32 + 2;

    if width <= 0 || height <= 0 {
        return empty_glyph();
    }

    let mut pb = PathBuilder::new();
    composite_to_path(
        &mut pb, composite, loca, glyf, scale, x_min, y_max, 0.0, 0.0,
    );

    let path = pb.finish();
    let mut dt = DrawTarget::new(width, height);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    let pixels = dt.get_data();
    let mut bitmap = vec![0u8; (width * height) as usize];
    for (i, &pixel) in pixels.iter().enumerate() {
        bitmap[i] = (pixel >> 24) as u8;
    }

    RasterizedGlyph {
        bitmap,
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
    }
}

fn add_simple_glyph_to_path(
    pb: &mut PathBuilder,
    simple: &SimpleGlyph,
    scale: f32,
    x_min: f32,
    y_max: f32,
    dx: f32,
    dy: f32,
) {
    let points: Vec<CurvePoint> = simple.points().collect();
    let contour_ends: Vec<usize> = simple
        .end_pts_of_contours()
        .iter()
        .map(|e| e.get() as usize)
        .collect();

    let mut contour_start = 0;
    for &contour_end in &contour_ends {
        let contour = &points[contour_start..=contour_end];
        add_contour_to_path_with_offset(pb, contour, scale, x_min, y_max, dx, dy);
        contour_start = contour_end + 1;
    }
}

fn build_path(
    simple: &SimpleGlyph,
    scale: f32,
    x_min: f32,
    y_max: f32,
) -> raqote::Path {
    let mut pb = PathBuilder::new();
    add_simple_glyph_to_path(&mut pb, simple, scale, x_min, y_max, 0.0, 0.0);
    pb.finish()
}

fn add_contour_to_path_with_offset(
    pb: &mut PathBuilder,
    contour: &[CurvePoint],
    scale: f32,
    x_min: f32,
    y_max: f32,
    dx: f32,
    dy: f32,
) {
    if contour.is_empty() {
        return;
    }

    let tx = |p: &CurvePoint| -> (f32, f32) {
        let x = p.x as f32 * scale + dx - x_min + 1.0;
        let y = y_max - (p.y as f32 * scale + dy) + 1.0;
        (x, y)
    };

    let mut expanded: Vec<(f32, f32, bool)> = Vec::new();
    for p in contour {
        let (px, py) = tx(p);
        if !expanded.is_empty() && !p.on_curve {
            let (_, _, prev_on) = *expanded.last().unwrap();
            if !prev_on {
                let (lx, ly, _) = *expanded.last().unwrap();
                expanded.push(((lx + px) / 2.0, (ly + py) / 2.0, true));
            }
        }
        expanded.push((px, py, p.on_curve));
    }

    if expanded.is_empty() {
        return;
    }

    let (fx, fy, first_on) = expanded[0];
    let (lx, ly, last_on) = *expanded.last().unwrap();
    if !last_on && !first_on {
        expanded.push(((lx + fx) / 2.0, (ly + fy) / 2.0, true));
    }

    let start_idx = expanded.iter().position(|p| p.2).unwrap_or(0);
    let n = expanded.len();
    let (sx, sy, _) = expanded[start_idx];
    pb.move_to(sx, sy);

    let mut i = 1;
    while i < n {
        let idx = (start_idx + i) % n;
        let (px, py, on_curve) = expanded[idx];
        if on_curve {
            pb.line_to(px, py);
            i += 1;
        } else {
            let next_idx = (start_idx + i + 1) % n;
            let (nx, ny, _) = expanded[next_idx];
            pb.quad_to(px, py, nx, ny);
            i += 2;
        }
    }
    pb.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All shaped glyphs for visible characters must rasterize to non-empty
    /// bitmaps, including ligature replacement glyphs that may be nested
    /// composites (composite referencing another composite).
    #[test]
    fn shaped_glyphs_rasterize() {
        let mut fs = FontSystem::new(None, 18.0);

        for text in [":: ", "a::b ", "Hello "] {
            let chars: Vec<char> = text.chars().collect();
            let shaped = fs.shape_row(&chars);
            for sg in &shaped {
                let ch = chars[sg.col as usize];
                if ch == ' ' {
                    continue;
                }
                let raster = fs.rasterize_glyph(sg.font_index, sg.glyph_id);
                assert!(
                    raster.width > 0 && raster.height > 0,
                    "glyph {} for '{ch}' at col {} in {text:?} must rasterize, got {}x{}",
                    sg.glyph_id,
                    sg.col,
                    raster.width,
                    raster.height,
                );
            }
        }
    }
}
