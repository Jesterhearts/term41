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
use read_fonts::tables::glyf::CurvePoint;
use read_fonts::tables::glyf::Glyph;
use read_fonts::tables::glyf::SimpleGlyph;
use read_fonts::types::GlyphId;

/// The embedded Fairfax HD font.
static FAIRFAX_HD: &[u8] = include_bytes!("../resources/fonts/FairfaxHD.ttf");

/// Rasterized glyph data ready for upload to a texture atlas.
pub struct RasterizedGlyph {
    pub bitmap: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
}

/// Font system: handles text shaping (harfrust) and rasterization (custom).
pub struct FontSystem {
    shaper_data: ShaperData,
    pub cell_width: u32,
    pub cell_height: u32,
    pub font_size: f32,
    ascent: f32,
    units_per_em: f32,
}

impl FontSystem {
    pub fn new() -> Self {
        let font_size = 24.0;

        let font_ref = FontRef::new(FAIRFAX_HD).expect("parse font");
        let shaper_data = ShaperData::new(&font_ref);

        // Read font metrics from tables.
        let rf = read_fonts::FontRef::new(FAIRFAX_HD).expect("parse font for metrics");
        let head = rf.head().expect("head table");
        let hhea = rf.hhea().expect("hhea table");
        let hmtx = rf.hmtx().expect("hmtx table");

        let units_per_em = head.units_per_em() as f32;
        let scale = font_size / units_per_em;

        let ascent = hhea.ascender().to_i16() as f32 * scale;
        let descent = hhea.descender().to_i16() as f32 * scale;
        let line_gap = hhea.line_gap().to_i16() as f32 * scale;
        let cell_height = (ascent - descent + line_gap).ceil() as u32;

        // Use advance width of glyph for 'M' as cell width (monospace).
        let m_advance = hmtx
            .advance(GlyphId::new(charmap_lookup(&rf, 'M')))
            .unwrap_or(0) as f32
            * scale;
        let cell_width = m_advance.ceil() as u32;

        Self {
            shaper_data,
            cell_width,
            cell_height,
            font_size,
            ascent,
            units_per_em,
        }
    }

    /// Pixel dimensions for a grid of the given size.
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

    /// How many columns and rows fit in the given pixel dimensions.
    pub fn grid_dimensions(
        &self,
        pixel_width: u32,
        pixel_height: u32,
    ) -> (u16, u16) {
        let cols = (pixel_width / self.cell_width).max(1) as u16;
        let rows = (pixel_height / self.cell_height).max(1) as u16;
        (cols, rows)
    }

    /// The vertical offset from the top of the cell to the baseline.
    pub fn baseline_offset(&self) -> f32 {
        self.ascent
    }

    /// Shape a single character through harfrust, returning the glyph index.
    pub fn shape_char(
        &self,
        ch: char,
    ) -> u16 {
        let font_ref = FontRef::new(FAIRFAX_HD).expect("parse font");
        let shaper = self.shaper_data.shaper(&font_ref).build();

        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(&ch.to_string());
        buffer.set_direction(Direction::LeftToRight);

        let output = shaper.shape(buffer, &[]);
        let info = output.glyph_infos();
        if info.is_empty() {
            0
        } else {
            info[0].glyph_id as u16
        }
    }

    /// Rasterize a glyph by its index at the configured font size.
    pub fn rasterize_glyph(
        &self,
        glyph_index: u16,
    ) -> RasterizedGlyph {
        let scale = self.font_size / self.units_per_em;
        let rf = read_fonts::FontRef::new(FAIRFAX_HD).expect("parse font");
        let loca = rf.loca(None).expect("loca table");
        let glyf = rf.glyf().expect("glyf table");

        let gid = GlyphId::new(glyph_index as u32);
        let glyph = match loca.get_glyf(gid, &glyf) {
            Ok(Some(g)) => g,
            _ => {
                return RasterizedGlyph {
                    bitmap: vec![],
                    width: 0,
                    height: 0,
                    bearing_x: 0,
                    bearing_y: 0,
                };
            }
        };

        match glyph {
            Glyph::Simple(simple) => rasterize_simple_glyph(&simple, scale),
            Glyph::Composite(_) => {
                // Composite glyphs not yet supported — return empty.
                RasterizedGlyph {
                    bitmap: vec![],
                    width: 0,
                    height: 0,
                    bearing_x: 0,
                    bearing_y: 0,
                }
            }
        }
    }

    /// Convenience: shape and rasterize a character in one step.
    pub fn rasterize_char(
        &self,
        ch: char,
    ) -> (u16, RasterizedGlyph) {
        let glyph_index = self.shape_char(ch);
        let glyph = self.rasterize_glyph(glyph_index);
        (glyph_index, glyph)
    }
}

// ---------------------------------------------------------------------------
// Character map lookup
// ---------------------------------------------------------------------------

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
    // Snap bounding box to pixel grid.
    let x_min = (simple.x_min() as f32 * scale).floor();
    let y_min = (simple.y_min() as f32 * scale).floor();
    let x_max = (simple.x_max() as f32 * scale).ceil();
    let y_max = (simple.y_max() as f32 * scale).ceil();

    // Pixel dimensions with 1px padding.
    let width = (x_max - x_min) as i32 + 2;
    let height = (y_max - y_min) as i32 + 2;

    if width <= 0 || height <= 0 {
        return RasterizedGlyph {
            bitmap: vec![],
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
        };
    }

    // Build a raqote path from the TrueType contours.
    let path = build_path(simple, scale, x_min, y_max);

    // Rasterize with raqote.
    let mut dt = DrawTarget::new(width, height);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    // Extract alpha channel from raqote's ARGB output.
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

/// Build a raqote Path from TrueType glyph contours.
///
/// Translates from font coordinates (Y-up) to bitmap coordinates (Y-down),
/// offset so the glyph sits within the bitmap with 1px padding.
fn build_path(
    simple: &SimpleGlyph,
    scale: f32,
    x_min: f32,
    y_max: f32,
) -> raqote::Path {
    let points: Vec<CurvePoint> = simple.points().collect();
    let contour_ends: Vec<usize> = simple
        .end_pts_of_contours()
        .iter()
        .map(|e| e.get() as usize)
        .collect();

    let mut pb = PathBuilder::new();
    let mut contour_start = 0;

    for &contour_end in &contour_ends {
        let contour = &points[contour_start..=contour_end];
        add_contour_to_path(&mut pb, contour, scale, x_min, y_max);
        contour_start = contour_end + 1;
    }

    pb.finish()
}

/// Convert a single TrueType contour into raqote path commands.
fn add_contour_to_path(
    pb: &mut PathBuilder,
    contour: &[CurvePoint],
    scale: f32,
    x_min: f32,
    y_max: f32,
) {
    if contour.is_empty() {
        return;
    }

    // Transform a font-coordinate point to bitmap coordinates.
    let tx = |p: &CurvePoint| -> (f32, f32) {
        let x = p.x as f32 * scale - x_min + 1.0;
        let y = y_max - p.y as f32 * scale + 1.0;
        (x, y)
    };

    // TrueType contours: on-curve points are endpoints, off-curve points are
    // quadratic Bézier control points. Two consecutive off-curve points imply
    // an on-curve point at their midpoint.

    // Build expanded point list with implicit on-curve midpoints inserted.
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

    // Handle wrap-around between last and first points.
    let (fx, fy, first_on) = expanded[0];
    let (lx, ly, last_on) = *expanded.last().unwrap();
    if !last_on && !first_on {
        expanded.push(((lx + fx) / 2.0, (ly + fy) / 2.0, true));
    }

    // Find the first on-curve point to start from.
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
            // Quadratic Bézier: off-curve control point → next on-curve endpoint.
            let next_idx = (start_idx + i + 1) % n;
            let (nx, ny, _) = expanded[next_idx];
            pb.quad_to(px, py, nx, ny);
            i += 2;
        }
    }

    pb.close();
}
