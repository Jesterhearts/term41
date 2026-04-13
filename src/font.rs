mod bitmap;
mod colr;
mod svg;

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
use smol_str::SmolStr;

/// The embedded Fairfax HD font (ultimate fallback).
static FAIRFAX_HD: &[u8] = include_bytes!("../resources/fonts/FairfaxHD.ttf");

/// Rasterized glyph data ready for upload to a texture atlas. The bitmap is
/// always RGBA8 (4 bytes per pixel). Outline glyphs encode their coverage
/// into the alpha channel with `rgb = 0`; color glyphs (COLR, emoji bitmaps)
/// encode full colour and set `is_color = true` so the shader samples the
/// atlas directly instead of tinting by the fg colour.
pub struct RasterizedGlyph {
    pub bitmap: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub is_color: bool,
}

/// A shaped glyph with its position info, ready for rendering.
pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub font_index: usize,
    pub col: u16,
    /// Number of terminal cells this glyph occupies horizontally — derived
    /// from the column gap to the next shaped glyph (or the row end). For a
    /// ZWJ ligature the font collapses into a single glyph, this counts all
    /// the cells the ligated cluster claims, so colour rasterisers can scale
    /// the emoji to fit its visual footprint instead of squashing it into a
    /// single cell.
    pub cells_wide: u8,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// A loaded font with its shaping data and raw bytes.
struct LoadedFont {
    data: Arc<Vec<u8>>,
    shaper_data: ShaperData,
    units_per_em: f32,
    /// True if the font carries colour glyph tables (COLR, CBDT, sbix, or
    /// SVG). Used by shape_row to prefer colour fonts for emoji clusters
    /// over text fonts that might also have a monochrome outline for the
    /// same codepoint.
    is_color: bool,
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
                        info!("loaded font: {family_name}");
                        fonts.push(font);
                    }
                } else {
                    warn!("font not found: {family_name}");
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
    /// Takes `&[SmolStr]` directly from the terminal's SoA storage — each
    /// cell is one grapheme cluster.
    ///
    /// Emoji clusters (ending in VS16 or whose first codepoint falls in a
    /// known emoji-presentation range) are shaped in a dedicated first pass
    /// that only accepts glyphs from colour fonts. A text font with a
    /// monochrome outline of `U+2764 HEAVY BLACK HEART` would otherwise win
    /// the race against a colour-emoji font and render a narrow lifeless
    /// glyph in place of the emoji the user pasted.
    pub fn shape_row(
        &mut self,
        cells: &[SmolStr],
    ) -> Vec<ShapedGlyph> {
        if cells.is_empty() {
            return vec![];
        }

        // Build the row string and byte-offset → column mapping.
        let mut row_text = String::new();
        let mut col_map: Vec<u16> = Vec::new();
        for (col, cell) in cells.iter().enumerate() {
            let start = row_text.len();
            row_text.push_str(cell);
            let added = row_text.len() - start;
            for _ in 0..added {
                col_map.push(col as u16);
            }
        }

        let wants_color: Vec<bool> = cells.iter().map(|c| cluster_prefers_color(c)).collect();

        // Track which columns still need a glyph (for fallback).
        let mut has_glyph = vec![false; cells.len()];
        let mut result: Vec<ShapedGlyph> = Vec::with_capacity(cells.len());

        // Two passes: first only accept a glyph when the font's "colour"
        // classification matches the cluster's preference. Second pass lets
        // any remaining uncovered cell take whichever font has a glyph.
        for pass in 0..2 {
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

                    if has_glyph[col as usize] {
                        continue;
                    }

                    let glyph_id = info.glyph_id as u16;
                    if glyph_id == 0 {
                        continue;
                    }

                    // Pass 0: require font type to match cluster preference.
                    if pass == 0 && wants_color[col as usize] != loaded.is_color {
                        continue;
                    }

                    // Mark all columns consumed by this glyph (handles
                    // ligatures and multi-codepoint clusters).
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
                        // Refined once shaping completes — see `assign_cells_wide`.
                        cells_wide: 1,
                        x_offset: pos.x_offset as f32 * scale,
                        y_offset: pos.y_offset as f32 * scale,
                    });
                }

                let all_covered = has_glyph
                    .iter()
                    .enumerate()
                    .all(|(i, &has)| has || cells[i] == " " || cells[i].is_empty());
                if all_covered {
                    assign_cells_wide(&mut result, cells.len() as u16);
                    return result;
                }
            }
        }

        assign_cells_wide(&mut result, cells.len() as u16);
        result
    }

    /// Rasterize a glyph from a specific font in the chain.
    ///
    /// Probes color-glyph tables in the FreeType/HarfBuzz-preferred order —
    /// SVG → COLR v1 → sbix → CBDT — then falls back to `glyf` outlines.
    /// Each color rasteriser derives its own scaling from the emoji font's
    /// metrics, so a Noto Color Emoji glyph (1024 upem) fits the cell
    /// regardless of the primary monospace font's unit system.
    pub fn rasterize_glyph(
        &self,
        font_index: usize,
        glyph_index: u16,
        cells_wide: u32,
    ) -> RasterizedGlyph {
        let loaded = &self.fonts[font_index];
        let scale = self.font_size / loaded.units_per_em;

        let Ok(font) = read_fonts::FontRef::new(&loaded.data) else {
            return empty_glyph();
        };

        // Colour rasterisers receive the cell box scaled to the cluster's
        // visual span. The outline path doesn't read `cell_width` at all —
        // glyf positioning derives entirely from the glyph's own bounding
        // box and `font_size` — so it sees no behavioural change when a
        // cluster covers more than one cell.
        let target_w = self.cell_width * cells_wide.max(1);

        if let Some(glyph) = svg::rasterize_svg(&font, glyph_index, target_w, self.cell_height) {
            return glyph;
        }
        if let Some(glyph) = colr::rasterize_colr_v1(&font, glyph_index, target_w, self.cell_height)
        {
            return glyph;
        }
        if let Some(glyph) = bitmap::rasterize_sbix(&font, glyph_index, target_w, self.cell_height)
        {
            return glyph;
        }
        if let Some(glyph) = bitmap::rasterize_cbdt(&font, glyph_index, target_w, self.cell_height)
        {
            return glyph;
        }

        let loca = match font.loca(None) {
            Ok(l) => l,
            Err(_) => return empty_glyph(),
        };
        let glyf = match font.glyf() {
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

/// Walk the shaped output left-to-right and stamp each glyph with the
/// number of terminal cells it occupies. The span is the gap between this
/// glyph's column and the next *strictly later* glyph's column (or the row
/// end). Glyphs that share a column — base + combining marks, or one
/// cluster that produced multiple glyphs — all get the same span, which is
/// what colour rasterisers need to size emoji to their visual footprint.
fn assign_cells_wide(
    result: &mut [ShapedGlyph],
    n_cells: u16,
) {
    result.sort_by(|a, b| {
        a.col.cmp(&b.col).then(
            a.x_offset
                .partial_cmp(&b.x_offset)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    for i in 0..result.len() {
        let here = result[i].col;
        let next_after = result[i + 1..]
            .iter()
            .find(|s| s.col > here)
            .map(|s| s.col)
            .unwrap_or(n_cells);
        let span = next_after.saturating_sub(here).max(1);
        result[i].cells_wide = span.min(u8::MAX as u16) as u8;
    }
}

/// Heuristic: true when a cluster is likely meant to render as a colour
/// emoji. Covers the two common routes: explicit `VS16` selector, and
/// default-emoji-presentation codepoints in the main emoji blocks. Keeps
/// CJK and ordinary symbols (which `unicode-width` also reports as wide)
/// out of the colour path.
fn cluster_prefers_color(cell: &str) -> bool {
    if cell.ends_with('\u{FE0F}') {
        return true;
    }
    cell.chars().any(is_default_emoji_codepoint)
}

fn is_default_emoji_codepoint(c: char) -> bool {
    let cp = c as u32;
    // Misc Symbols / Dingbats / Transport blocks that are all emoji-by-default.
    matches!(
        cp,
        0x1F300..=0x1F5FF
            | 0x1F600..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F700..=0x1F77F
            | 0x1F780..=0x1F7FF
            | 0x1F800..=0x1F8FF
            | 0x1F900..=0x1F9FF
            | 0x1FA00..=0x1FA6F
            | 0x1FA70..=0x1FAFF
            | 0x2600..=0x26FF
            | 0x2700..=0x27BF
            | 0x1F000..=0x1F0FF
            | 0x1F100..=0x1F1FF
            | 0x1F200..=0x1F2FF
    )
}

fn empty_glyph() -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: vec![],
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
        is_color: false,
    }
}

fn load_font(data: &[u8]) -> Option<LoadedFont> {
    let data = Arc::new(data.to_vec());
    let font_ref = FontRef::new(&data).ok()?;
    let shaper_data = ShaperData::new(&font_ref);
    let rf = read_fonts::FontRef::new(&data).ok()?;
    let head = rf.head().ok()?;
    let units_per_em = head.units_per_em() as f32;
    // Probe for colour glyph tables. If any is present, we treat this as a
    // colour font and let it win the font-selection race for emoji clusters.
    let is_color = rf.colr().is_ok() || rf.cbdt().is_ok() || rf.sbix().is_ok() || rf.svg().is_ok();

    Some(LoadedFont {
        data,
        shaper_data,
        units_per_em,
        is_color,
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

    RasterizedGlyph {
        bitmap: alpha_mask_to_rgba(dt.get_data(), width, height),
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
        is_color: false,
    }
}

/// Convert a raqote `DrawTarget` buffer (premultiplied ARGB in platform byte
/// order) into an RGBA8 bitmap whose colour channels are zero. The atlas
/// texture is RGBA, but outline glyphs only need coverage in the alpha
/// channel — the shader picks up `.a` in the alpha-path branch and the
/// colour path is not taken for `is_color = false` glyphs.
fn alpha_mask_to_rgba(
    pixels: &[u32],
    width: i32,
    height: i32,
) -> Vec<u8> {
    let mut bitmap = vec![0u8; (width * height * 4) as usize];
    for (i, &pixel) in pixels.iter().enumerate() {
        // raqote stores ARGB in u32; the alpha byte is the top 8 bits.
        let alpha = (pixel >> 24) as u8;
        bitmap[i * 4 + 3] = alpha;
    }
    bitmap
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

    RasterizedGlyph {
        bitmap: alpha_mask_to_rgba(dt.get_data(), width, height),
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
        is_color: false,
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
            let cells: Vec<SmolStr> = text
                .chars()
                .map(|c| {
                    let mut buf = [0u8; 4];
                    SmolStr::new_inline(c.encode_utf8(&mut buf))
                })
                .collect();
            let shaped = fs.shape_row(&cells);
            for sg in &shaped {
                let cell = &cells[sg.col as usize];
                if cell == " " {
                    continue;
                }
                let raster = fs.rasterize_glyph(sg.font_index, sg.glyph_id, sg.cells_wide as u32);
                assert!(
                    raster.width > 0 && raster.height > 0,
                    "glyph {} for {cell:?} at col {} in {text:?} must rasterize, got {}x{}",
                    sg.glyph_id,
                    sg.col,
                    raster.width,
                    raster.height,
                );
            }
        }
    }
}
