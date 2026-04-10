use harfrust::{Direction, FontRef, ShaperData, UnicodeBuffer};
use read_fonts::TableProvider;
use read_fonts::tables::glyf::{CurvePoint, Glyph, SimpleGlyph};
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
// Outline extraction: TrueType contours → line segments
// ---------------------------------------------------------------------------

struct Segment {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

fn extract_segments(
    simple: &SimpleGlyph,
    scale: f32,
) -> Vec<Segment> {
    let points: Vec<CurvePoint> = simple.points().collect();
    let contour_ends: Vec<usize> = simple
        .end_pts_of_contours()
        .iter()
        .map(|e| e.get() as usize)
        .collect();

    let mut segments = Vec::new();
    let mut contour_start = 0;

    for &contour_end in &contour_ends {
        let contour = &points[contour_start..=contour_end];
        flatten_contour(contour, scale, &mut segments);
        contour_start = contour_end + 1;
    }

    segments
}

fn flatten_contour(
    contour: &[CurvePoint],
    scale: f32,
    segments: &mut Vec<Segment>,
) {
    if contour.is_empty() {
        return;
    }

    // TrueType contours: on-curve points are endpoints, off-curve points are
    // quadratic Bézier control points. Two consecutive off-curve points imply
    // an on-curve point at their midpoint.

    // Build an expanded point list with implicit on-curve points inserted.
    let mut expanded: Vec<(f32, f32, bool)> = Vec::new();
    for i in 0..contour.len() {
        let p = contour[i];
        let px = p.x as f32 * scale;
        let py = p.y as f32 * scale;

        if !expanded.is_empty() && !p.on_curve {
            let (_, _, prev_on) = *expanded.last().unwrap();
            if !prev_on {
                // Two consecutive off-curve: insert implicit on-curve midpoint.
                let (lx, ly, _) = *expanded.last().unwrap();
                expanded.push(((lx + px) / 2.0, (ly + py) / 2.0, true));
            }
        }
        expanded.push((px, py, p.on_curve));
    }

    if expanded.is_empty() {
        return;
    }

    // Handle wrap-around: the contour is closed, so we may need an implicit
    // midpoint between the last and first points.
    let (fx, fy, first_on) = expanded[0];
    let (lx, ly, last_on) = *expanded.last().unwrap();
    if !last_on && !first_on {
        expanded.push(((lx + fx) / 2.0, (ly + fy) / 2.0, true));
    }

    // Find the first on-curve point to start from.
    let start_idx = expanded.iter().position(|p| p.2).unwrap_or(0);
    let n = expanded.len();

    let mut cur_x = expanded[start_idx].0;
    let mut cur_y = expanded[start_idx].1;
    let mut i = 1;

    while i < n {
        let idx = (start_idx + i) % n;
        let (px, py, on_curve) = expanded[idx];

        if on_curve {
            // Line segment.
            segments.push(Segment {
                x0: cur_x,
                y0: cur_y,
                x1: px,
                y1: py,
            });
            cur_x = px;
            cur_y = py;
            i += 1;
        } else {
            // Quadratic Bézier: current point → off-curve → next on-curve.
            let next_idx = (start_idx + i + 1) % n;
            let (nx, ny, _) = expanded[next_idx];
            flatten_quad(cur_x, cur_y, px, py, nx, ny, segments);
            cur_x = nx;
            cur_y = ny;
            i += 2;
        }
    }

    // Close the contour.
    let (sx, sy, _) = expanded[start_idx];
    if (cur_x - sx).abs() > 0.01 || (cur_y - sy).abs() > 0.01 {
        segments.push(Segment {
            x0: cur_x,
            y0: cur_y,
            x1: sx,
            y1: sy,
        });
    }
}

/// Flatten a quadratic Bézier curve into line segments.
fn flatten_quad(
    x0: f32,
    y0: f32,
    cx: f32,
    cy: f32,
    x1: f32,
    y1: f32,
    segments: &mut Vec<Segment>,
) {
    // Adaptive subdivision: split until flat enough.
    const TOLERANCE: f32 = 0.25;

    // Flatness test: distance from control point to midpoint of the line.
    let mx = (x0 + x1) * 0.5;
    let my = (y0 + y1) * 0.5;
    let dx = cx - mx;
    let dy = cy - my;

    if dx * dx + dy * dy <= TOLERANCE * TOLERANCE {
        segments.push(Segment { x0, y0, x1, y1 });
        return;
    }

    // De Casteljau subdivision at t=0.5.
    let mid_cx0 = (x0 + cx) * 0.5;
    let mid_cy0 = (y0 + cy) * 0.5;
    let mid_cx1 = (cx + x1) * 0.5;
    let mid_cy1 = (cy + y1) * 0.5;
    let mid_x = (mid_cx0 + mid_cx1) * 0.5;
    let mid_y = (mid_cy0 + mid_cy1) * 0.5;

    flatten_quad(x0, y0, mid_cx0, mid_cy0, mid_x, mid_y, segments);
    flatten_quad(mid_x, mid_y, mid_cx1, mid_cy1, x1, y1, segments);
}

// ---------------------------------------------------------------------------
// Coverage-based rasterizer
// ---------------------------------------------------------------------------

fn rasterize_simple_glyph(
    simple: &SimpleGlyph,
    scale: f32,
) -> RasterizedGlyph {
    // Snap bounding box to pixel grid so the rasterized outline aligns with
    // the same integer coordinates the renderer will use for placement.
    let x_min = (simple.x_min() as f32 * scale).floor();
    let y_min = (simple.y_min() as f32 * scale).floor();
    let x_max = (simple.x_max() as f32 * scale).ceil();
    let y_max = (simple.y_max() as f32 * scale).ceil();

    // Pixel dimensions with 1px padding.
    let width = (x_max - x_min) as u32 + 2;
    let height = (y_max - y_min) as u32 + 2;

    if width == 0 || height == 0 {
        return RasterizedGlyph {
            bitmap: vec![],
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
        };
    }

    let segments = extract_segments(simple, scale);

    // Coverage buffer: one f32 per pixel, accumulates signed area contributions.
    let mut coverage = vec![0.0f32; (width * height) as usize];

    // For each segment, compute per-pixel coverage contributions.
    // x_min/y_max are already snapped to integers, so the outline lands on
    // pixel boundaries consistently with the bearing values below.
    for seg in &segments {
        // Transform to bitmap coordinates (flip Y: font Y-up → bitmap Y-down).
        let sx0 = seg.x0 - x_min + 1.0;
        let sy0 = (y_max - seg.y0) + 1.0;
        let sx1 = seg.x1 - x_min + 1.0;
        let sy1 = (y_max - seg.y1) + 1.0;

        rasterize_edge(&mut coverage, width, height, sx0, sy0, sx1, sy1);
    }

    // Accumulate coverage left-to-right and convert to alpha.
    let mut bitmap = vec![0u8; (width * height) as usize];
    for y in 0..height {
        let row_start = (y * width) as usize;
        let mut accum = 0.0f32;
        for x in 0..width {
            accum += coverage[row_start + x as usize];
            let alpha = accum.abs().min(1.0);
            bitmap[row_start + x as usize] = (alpha * 255.0) as u8;
        }
    }

    RasterizedGlyph {
        bitmap,
        width,
        height,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
    }
}

/// Rasterize a single edge into the coverage buffer using signed area.
fn rasterize_edge(
    coverage: &mut [f32],
    width: u32,
    height: u32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
) {
    // Skip horizontal edges — they don't contribute to coverage.
    if (y1 - y0).abs() < 1e-6 {
        return;
    }

    // Ensure we iterate top-to-bottom (increasing y in bitmap space).
    let (x0, y0, x1, y1, dir) = if y0 <= y1 {
        (x0, y0, x1, y1, 1.0f32)
    } else {
        (x1, y1, x0, y0, -1.0f32)
    };

    let dxdy = (x1 - x0) / (y1 - y0);
    let row_start = (y0.floor() as i32).max(0) as u32;
    let row_end = (y1.ceil() as i32).min(height as i32) as u32;

    for row in row_start..row_end {
        let ry = row as f32;

        // Clamp the edge to this row's vertical span [ry, ry+1].
        let ey0 = y0.max(ry);
        let ey1 = y1.min(ry + 1.0);
        if ey1 <= ey0 {
            continue;
        }

        let dy = ey1 - ey0;

        // X coordinates at the clamped y positions.
        let ex0 = x0 + (ey0 - y0) * dxdy;
        let ex1 = x0 + (ey1 - y0) * dxdy;

        // The x range this edge fragment covers within the row.
        let x_mid = (ex0 + ex1) * 0.5;

        let col = x_mid.floor() as i32;
        if col >= 0 && (col as u32) < width {
            let idx = row * width + col as u32;
            coverage[idx as usize] += dir * dy;
        }
    }
}
