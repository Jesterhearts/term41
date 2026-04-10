use std::sync::Arc;

use harfrust::Direction;
use harfrust::FontRef;
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

/// A loaded font with its shaping data and raw bytes.
struct LoadedFont {
    data: Arc<Vec<u8>>,
    shaper_data: ShaperData,
    units_per_em: f32,
}

/// Font system: manages an ordered list of fonts with fallback.
pub struct FontSystem {
    fonts: Vec<LoadedFont>,
    pub cell_width: u32,
    pub cell_height: u32,
    pub font_size: f32,
    ascent: f32,
}

impl FontSystem {
    pub fn new(fonts_config: Option<&str>) -> Self {
        let font_size = 24.0;
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
            cell_width,
            cell_height,
            font_size,
            ascent,
        }
    }

    pub fn grid_size(
        &self,
        cols: u16,
        rows: u16,
    ) -> (u32, u32) {
        (
            cols as u32 * self.cell_width,
            rows as u32 * self.cell_height,
        )
    }

    pub fn grid_dimensions(
        &self,
        pixel_width: u32,
        pixel_height: u32,
    ) -> (u16, u16) {
        let cols = (pixel_width / self.cell_width).max(1) as u16;
        let rows = (pixel_height / self.cell_height).max(1) as u16;
        (cols, rows)
    }

    pub fn baseline_offset(&self) -> f32 {
        self.ascent
    }

    /// Shape a character with font fallback.
    /// Returns `(font_index, glyph_index)` from the first font that has
    /// coverage.
    pub fn shape_char_with_fallback(
        &self,
        ch: char,
    ) -> (usize, u16) {
        let s = ch.to_string();
        for (font_idx, loaded) in self.fonts.iter().enumerate() {
            let font_ref = match FontRef::new(&loaded.data) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let shaper = loaded.shaper_data.shaper(&font_ref).build();

            let mut buffer = UnicodeBuffer::new();
            buffer.push_str(&s);
            buffer.set_direction(Direction::LeftToRight);

            let output = shaper.shape(buffer, &[]);
            let info = output.glyph_infos();
            if !info.is_empty() {
                let glyph_id = info[0].glyph_id as u16;
                if glyph_id != 0 {
                    return (font_idx, glyph_id);
                }
            }
        }
        (self.fonts.len() - 1, 0)
    }

    /// Rasterize a glyph from a specific font in the chain.
    /// Supports both simple and composite glyphs.
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
        if let Ok(subtable) = record.subtable(cmap.offset_data()) {
            if let Some(gid) = subtable.map_codepoint(ch) {
                return gid.to_u32();
            }
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

fn rasterize_composite_glyph(
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
) -> RasterizedGlyph {
    // Collect all simple glyph outlines from components into one path.
    let mut pb = PathBuilder::new();
    let mut x_min_all = f32::MAX;
    let mut y_min_all = f32::MAX;
    let mut x_max_all = f32::MIN;
    let mut y_max_all = f32::MIN;

    // First pass: compute the combined bounding box.
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let glyph = match loca.get_glyf(gid, glyf) {
            Ok(Some(g)) => g,
            _ => continue,
        };

        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32 * scale, y as f32 * scale),
            _ => (0.0, 0.0),
        };

        match glyph {
            Glyph::Simple(simple) => {
                let sx_min = simple.x_min() as f32 * scale + dx;
                let sy_min = simple.y_min() as f32 * scale + dy;
                let sx_max = simple.x_max() as f32 * scale + dx;
                let sy_max = simple.y_max() as f32 * scale + dy;
                x_min_all = x_min_all.min(sx_min);
                y_min_all = y_min_all.min(sy_min);
                x_max_all = x_max_all.max(sx_max);
                y_max_all = y_max_all.max(sy_max);
            }
            // Nested composites — skip for now.
            _ => {}
        }
    }

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

    // Second pass: build the combined path with component offsets.
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let glyph = match loca.get_glyf(gid, glyf) {
            Ok(Some(Glyph::Simple(simple))) => simple,
            _ => continue,
        };

        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32 * scale, y as f32 * scale),
            _ => (0.0, 0.0),
        };

        add_simple_glyph_to_path(&mut pb, &glyph, scale, x_min, y_max, dx, dy);
    }

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

/// Add a simple glyph's contours to an existing PathBuilder with an offset.
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
