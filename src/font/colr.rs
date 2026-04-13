//! COLR v1 paint-graph rasterisation on top of raqote.
//!
//! Walks the paint graph for a single base glyph, composes all accumulated
//! transforms eagerly onto path and gradient coordinates, and fills into a
//! pixel-space [`raqote::DrawTarget`]. Unsupported nodes (Var*, Composite,
//! Sweep gradients) log-and-skip — they won't crash the font fallback, but
//! emoji relying on them will show the nearest supported part instead.

use std::collections::HashSet;

use raqote::DrawOptions;
use raqote::DrawTarget;
use raqote::Gradient;
use raqote::GradientStop;
use raqote::Path;
use raqote::PathBuilder;
use raqote::SolidSource;
use raqote::Source;
use raqote::Spread;
use raqote::Transform;
use read_fonts::FontRef;
use read_fonts::TableProvider;
use read_fonts::tables::colr::Colr;
use read_fonts::tables::colr::Paint;
use read_fonts::tables::cpal::ColorRecord;
use read_fonts::tables::glyf::CompositeGlyph;
use read_fonts::tables::glyf::Glyph;
use read_fonts::tables::glyf::SimpleGlyph;
use read_fonts::tables::loca::Loca;
use read_fonts::types::GlyphId;

use super::RasterizedGlyph;

/// Rasterise a glyph via the font's COLR v1 paint graph. Returns `None` if
/// the font has no COLR table, no v1 base-glyph record for this id, or the
/// outline tables needed for `PaintGlyph` resolution are missing.
///
/// Scaling and baseline are derived from the **emoji font's own** metrics —
/// not the primary text font's — so a Noto Color Emoji glyph with its 1024
/// upem fits the cell regardless of the monospace font we're running. The
/// bitmap's baseline still lands near the centre of the cell so emoji sit
/// visually aligned with surrounding text.
pub fn rasterize_colr_v1(
    font: &FontRef<'_>,
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
) -> Option<RasterizedGlyph> {
    let colr = font.colr().ok()?;
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;
    let palette = load_palette(font)?;

    let (root_paint, root_id) = colr
        .v1_base_glyph(GlyphId::new(glyph_id as u32))
        .ok()
        .flatten()?;

    // Emoji font's own vertical metrics. We fit the emoji's natural line
    // height (ascent + |descent|) to the cell height so the glyph ends up
    // roughly the same size as monospace letters in the primary font.
    let hhea = font.hhea().ok()?;
    let ascent_units = hhea.ascender().to_i16() as f32;
    let descent_units = -hhea.descender().to_i16() as f32;
    let line_height_units = (ascent_units + descent_units).max(1.0);
    let scale = cell_height as f32 / line_height_units;
    let ascent_px = ascent_units * scale;

    // Bitmap size — match the cell with small padding for ink that slightly
    // exceeds the advance/ascent.
    let pad = 2;
    let width = cell_width as i32 + pad * 2;
    let height = cell_height as i32 + pad * 2;

    // Base transform: font units (y-up) → pixel coords (y-down). The origin
    // (baseline at x=0) lands at (pad, pad + ascent_px) so both ink above
    // and below the baseline fits within the allocated bitmap.
    let base = Transform::scale(scale, -scale)
        .then_translate(raqote::Vector::new(pad as f32, pad as f32 + ascent_px));

    let mut painter = Painter {
        colr: colr.clone(),
        palette,
        loca,
        glyf,
        dt: DrawTarget::new(width, height),
        transforms: vec![base],
        current_path: None,
        visited: HashSet::new(),
    };
    painter.visited.insert(root_id);
    painter.paint(&root_paint);

    let pixels = painter.dt.into_vec();
    Some(RasterizedGlyph {
        bitmap: premul_argb_to_rgba(&pixels),
        width: width as u32,
        height: height as u32,
        bearing_x: -pad,
        // bearing_y is measured from baseline upward; our base transform put
        // the baseline at (pad + ascent), so the top of the bitmap sits
        // `pad + ascent` above the baseline.
        bearing_y: (pad as f32 + ascent_px) as i32,
        is_color: true,
    })
}

struct Painter<'a> {
    colr: Colr<'a>,
    palette: Vec<[u8; 4]>,
    loca: Loca<'a>,
    glyf: read_fonts::tables::glyf::Glyf<'a>,
    dt: DrawTarget,
    transforms: Vec<Transform>,
    /// Path set by a `PaintGlyph` so a subsequent fill knows what to draw.
    current_path: Option<Path>,
    /// Paint ids we've already walked — cycle guard for `PaintColrGlyph`.
    visited: HashSet<usize>,
}

impl<'a> Painter<'a> {
    fn current_transform(&self) -> Transform {
        *self.transforms.last().expect("transform stack is empty")
    }

    fn push_transform(
        &mut self,
        t: Transform,
    ) {
        let current = self.current_transform();
        // The new transform applies in the coordinate space the *child*
        // paints see; composing as `t.then(&current)` means child coords go
        // through `t` first and then the outer transform — matches the COLR
        // spec's inside-out composition order.
        self.transforms.push(t.then(&current));
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }

    fn paint(
        &mut self,
        paint: &Paint<'_>,
    ) {
        match paint {
            Paint::ColrLayers(p) => {
                let first = p.first_layer_index() as usize;
                let count = p.num_layers() as usize;
                for i in 0..count {
                    let Ok((sub, id)) = self.colr.v1_layer(first + i) else {
                        continue;
                    };
                    if !self.visited.insert(id) {
                        continue;
                    }
                    self.paint(&sub);
                    self.visited.remove(&id);
                }
            }
            Paint::Solid(p) => {
                if let Some(path) = self.current_path.clone() {
                    let rgba = apply_alpha(
                        palette_color(&self.palette, p.palette_index()),
                        p.alpha().to_f32(),
                    );
                    self.fill_solid(&path, rgba);
                }
            }
            Paint::LinearGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                let stops = color_line_stops(&line, &self.palette);
                if stops.is_empty() {
                    return;
                }
                let tfm = self.current_transform();
                let p0 = tfm.transform_point(raqote::Point::new(
                    p.x0().to_i16() as f32,
                    p.y0().to_i16() as f32,
                ));
                let p1 = tfm.transform_point(raqote::Point::new(
                    p.x1().to_i16() as f32,
                    p.y1().to_i16() as f32,
                ));
                let grad = Gradient { stops };
                let src = Source::new_linear_gradient(
                    grad,
                    raqote::Point::new(p0.x, p0.y),
                    raqote::Point::new(p1.x, p1.y),
                    extend_spread(line.extend()),
                );
                self.dt.fill(&path, &src, &DrawOptions::default());
            }
            Paint::RadialGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                let stops = color_line_stops(&line, &self.palette);
                if stops.is_empty() {
                    return;
                }
                let tfm = self.current_transform();
                let c0 = tfm.transform_point(raqote::Point::new(
                    p.x0().to_i16() as f32,
                    p.y0().to_i16() as f32,
                ));
                let c1 = tfm.transform_point(raqote::Point::new(
                    p.x1().to_i16() as f32,
                    p.y1().to_i16() as f32,
                ));
                // Radii live in design units; apply the transform's scale
                // component so the rendered extent matches the geometry.
                let scale_est = (tfm.m11.abs() + tfm.m22.abs()) * 0.5;
                let r0 = p.radius0().to_u16() as f32 * scale_est;
                let r1 = p.radius1().to_u16() as f32 * scale_est;
                let grad = Gradient { stops };
                let src = Source::new_two_circle_radial_gradient(
                    grad,
                    raqote::Point::new(c0.x, c0.y),
                    r0,
                    raqote::Point::new(c1.x, c1.y),
                    r1,
                    extend_spread(line.extend()),
                );
                self.dt.fill(&path, &src, &DrawOptions::default());
            }
            Paint::Glyph(p) => {
                let Some(path) = self.extract_outline(p.glyph_id().to_u16()) else {
                    return;
                };
                let prev = self.current_path.replace(path);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.current_path = prev;
            }
            Paint::ColrGlyph(p) => {
                let gid = GlyphId::new(p.glyph_id().to_u16() as u32);
                let Ok(Some((sub, id))) = self.colr.v1_base_glyph(gid) else {
                    return;
                };
                if !self.visited.insert(id) {
                    return;
                }
                self.paint(&sub);
                self.visited.remove(&id);
            }
            Paint::Transform(p) => {
                let Ok(a) = p.transform() else { return };
                let t = Transform::new(
                    a.xx().to_f32(),
                    a.yx().to_f32(),
                    a.xy().to_f32(),
                    a.yy().to_f32(),
                    a.dx().to_f32(),
                    a.dy().to_f32(),
                );
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::Translate(p) => {
                let t = Transform::translation(p.dx().to_i16() as f32, p.dy().to_i16() as f32);
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::Scale(p) => {
                let t = Transform::scale(p.scale_x().to_f32(), p.scale_y().to_f32());
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::ScaleAroundCenter(p) => {
                let cx = p.center_x().to_i16() as f32;
                let cy = p.center_y().to_i16() as f32;
                let t = Transform::translation(-cx, -cy)
                    .then_scale(p.scale_x().to_f32(), p.scale_y().to_f32())
                    .then_translate(raqote::Vector::new(cx, cy));
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::ScaleUniform(p) => {
                let s = p.scale().to_f32();
                let t = Transform::scale(s, s);
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::ScaleUniformAroundCenter(p) => {
                let s = p.scale().to_f32();
                let cx = p.center_x().to_i16() as f32;
                let cy = p.center_y().to_i16() as f32;
                let t = Transform::translation(-cx, -cy)
                    .then_scale(s, s)
                    .then_translate(raqote::Vector::new(cx, cy));
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::Rotate(p) => {
                // COLR angles are fractions of 180° (so 1.0 == 180°).
                let radians = p.angle().to_f32() * std::f32::consts::PI;
                self.push_transform(rotation(radians));
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::RotateAroundCenter(p) => {
                let radians = p.angle().to_f32() * std::f32::consts::PI;
                let cx = p.center_x().to_i16() as f32;
                let cy = p.center_y().to_i16() as f32;
                let t = Transform::translation(-cx, -cy)
                    .then(&rotation(radians))
                    .then_translate(raqote::Vector::new(cx, cy));
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::Skew(p) => {
                let xk = p.x_skew_angle().to_f32() * std::f32::consts::PI;
                let yk = p.y_skew_angle().to_f32() * std::f32::consts::PI;
                let t = skew(xk, yk);
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            Paint::SkewAroundCenter(p) => {
                let xk = p.x_skew_angle().to_f32() * std::f32::consts::PI;
                let yk = p.y_skew_angle().to_f32() * std::f32::consts::PI;
                let cx = p.center_x().to_i16() as f32;
                let cy = p.center_y().to_i16() as f32;
                let t = Transform::translation(-cx, -cy)
                    .then(&skew(xk, yk))
                    .then_translate(raqote::Vector::new(cx, cy));
                self.push_transform(t);
                if let Ok(sub) = p.paint() {
                    self.paint(&sub);
                }
                self.pop_transform();
            }
            // Variable-font variants: paint without the delta adjustments.
            // Most emoji fonts don't ship COLR variations, and for those that
            // do we render the base (no deltas) instance — visually close
            // enough as a fallback.
            Paint::VarSolid(p) => {
                if let Some(path) = self.current_path.clone() {
                    let rgba = apply_alpha(
                        palette_color(&self.palette, p.palette_index()),
                        p.alpha().to_f32(),
                    );
                    self.fill_solid(&path, rgba);
                }
            }
            // Unsupported: sweep gradients, composite blend modes, all
            // remaining Var* nodes. Log once per glyph so we can spot it
            // when investigating missing artwork.
            other => {
                debug!("COLR paint format {} unsupported — skipped", other.format());
            }
        }
    }

    fn fill_solid(
        &mut self,
        path: &Path,
        rgba: [u8; 4],
    ) {
        let src = Source::Solid(SolidSource::from_unpremultiplied_argb(
            rgba[3], rgba[0], rgba[1], rgba[2],
        ));
        self.dt.fill(path, &src, &DrawOptions::default());
    }

    fn extract_outline(
        &self,
        glyph_id: u16,
    ) -> Option<Path> {
        let gid = GlyphId::new(glyph_id as u32);
        let glyph = self.loca.get_glyf(gid, &self.glyf).ok().flatten()?;
        let mut pb = PathBuilder::new();
        match glyph {
            Glyph::Simple(simple) => {
                add_simple_outline(&mut pb, &simple);
            }
            Glyph::Composite(composite) => {
                add_composite_outline(&mut pb, &composite, &self.loca, &self.glyf, 0.0, 0.0);
            }
        }
        let raw = pb.finish();
        // Transform the raw outline (in font units, y-up) into the current
        // pixel-space coordinate system. raqote fills with the DrawTarget's
        // identity transform, so we bake our transform into the geometry.
        Some(raw.transform(&self.current_transform()))
    }
}

fn add_simple_outline(
    pb: &mut PathBuilder,
    simple: &SimpleGlyph,
) {
    use read_fonts::tables::glyf::CurvePoint;
    let points: Vec<CurvePoint> = simple.points().collect();
    let contour_ends: Vec<usize> = simple
        .end_pts_of_contours()
        .iter()
        .map(|e| e.get() as usize)
        .collect();

    let mut start = 0;
    for &end in &contour_ends {
        add_contour(pb, &points[start..=end]);
        start = end + 1;
    }
}

fn add_contour(
    pb: &mut PathBuilder,
    contour: &[read_fonts::tables::glyf::CurvePoint],
) {
    if contour.is_empty() {
        return;
    }
    // Expand implicit on-curve points between consecutive off-curve points.
    let mut expanded: Vec<(f32, f32, bool)> = Vec::with_capacity(contour.len());
    for p in contour {
        let px = p.x as f32;
        let py = p.y as f32;
        if !expanded.is_empty() && !p.on_curve {
            let last = expanded.last().copied().unwrap();
            if !last.2 {
                expanded.push(((last.0 + px) * 0.5, (last.1 + py) * 0.5, true));
            }
        }
        expanded.push((px, py, p.on_curve));
    }
    if expanded.is_empty() {
        return;
    }
    let first = expanded[0];
    let last = *expanded.last().unwrap();
    if !last.2 && !first.2 {
        expanded.push(((last.0 + first.0) * 0.5, (last.1 + first.1) * 0.5, true));
    }

    let start_idx = expanded.iter().position(|p| p.2).unwrap_or(0);
    let n = expanded.len();
    let (sx, sy, _) = expanded[start_idx];
    pb.move_to(sx, sy);

    let mut i = 1;
    while i < n {
        let idx = (start_idx + i) % n;
        let (px, py, on) = expanded[idx];
        if on {
            pb.line_to(px, py);
            i += 1;
        } else {
            let nidx = (start_idx + i + 1) % n;
            let (nx, ny, _) = expanded[nidx];
            pb.quad_to(px, py, nx, ny);
            i += 2;
        }
    }
    pb.close();
}

fn add_composite_outline(
    pb: &mut PathBuilder,
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    _parent_dx: f32,
    _parent_dy: f32,
) {
    // Composite outlines are rare inside COLR paint graphs. We expand one
    // level of composition — simple glyph components get added directly;
    // further-nested composites recurse. We ignore component transforms
    // other than translation for brevity — matches what most emoji need.
    use read_fonts::tables::glyf::Anchor;
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let Ok(Some(glyph)) = loca.get_glyf(gid, glyf) else {
            continue;
        };
        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32, y as f32),
            _ => (0.0, 0.0),
        };
        let _ = (dx, dy); // translation baked into font units — left as TODO
        match glyph {
            Glyph::Simple(simple) => add_simple_outline(pb, &simple),
            Glyph::Composite(inner) => {
                add_composite_outline(pb, &inner, loca, glyf, 0.0, 0.0);
            }
        }
    }
}

fn load_palette(font: &FontRef) -> Option<Vec<[u8; 4]>> {
    let cpal = font.cpal().ok()?;
    let records = cpal.color_records_array()?.ok()?;
    // Take the first palette (index 0). CPAL supports multiple palettes for
    // light/dark theme variants but we don't thread a theme choice through
    // yet.
    let num_entries = cpal.num_palette_entries() as usize;
    let palette_start = cpal
        .color_record_indices()
        .first()
        .map(|idx| idx.get() as usize)
        .unwrap_or(0);
    let end = (palette_start + num_entries).min(records.len());
    let entries: Vec<[u8; 4]> = records[palette_start..end]
        .iter()
        .map(|r: &ColorRecord| [r.red(), r.green(), r.blue(), r.alpha()])
        .collect();
    Some(entries)
}

fn palette_color(
    palette: &[[u8; 4]],
    index: u16,
) -> [u8; 4] {
    // 0xFFFF is the spec-defined "use fg color" sentinel. We don't have a fg
    // colour for emoji (they're self-coloured), so fall back to opaque black.
    if index == 0xFFFF {
        return [0, 0, 0, 0xFF];
    }
    palette
        .get(index as usize)
        .copied()
        .unwrap_or([0, 0, 0, 0xFF])
}

fn apply_alpha(
    mut rgba: [u8; 4],
    multiplier: f32,
) -> [u8; 4] {
    let a = (rgba[3] as f32 * multiplier).clamp(0.0, 255.0);
    rgba[3] = a as u8;
    rgba
}

fn color_line_stops(
    line: &read_fonts::tables::colr::ColorLine,
    palette: &[[u8; 4]],
) -> Vec<GradientStop> {
    line.color_stops()
        .iter()
        .map(|stop| {
            let rgba = apply_alpha(
                palette_color(palette, stop.palette_index()),
                stop.alpha().to_f32(),
            );
            GradientStop {
                position: stop.stop_offset().to_f32(),
                color: raqote::Color::new(rgba[3], rgba[0], rgba[1], rgba[2]),
            }
        })
        .collect()
}

fn extend_spread(extend: read_fonts::tables::colr::Extend) -> Spread {
    use read_fonts::tables::colr::Extend as E;
    match extend {
        E::Pad => Spread::Pad,
        E::Repeat => Spread::Repeat,
        E::Reflect => Spread::Reflect,
        _ => Spread::Pad,
    }
}

fn skew(
    xk: f32,
    yk: f32,
) -> Transform {
    // COLR Skew is `x' = x + tan(-xSkew)*y`, `y' = tan(ySkew)*x + y`. Our
    // angles came in as radians.
    Transform::new(1.0, yk.tan(), -xk.tan(), 1.0, 0.0, 0.0)
}

fn rotation(radians: f32) -> Transform {
    let (s, c) = radians.sin_cos();
    Transform::new(c, s, -s, c, 0.0, 0.0)
}

/// Convert raqote's `DrawTarget` output (premultiplied ARGB in platform byte
/// order) into the RGBA8 byte order the atlas expects.
fn premul_argb_to_rgba(pixels: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for &p in pixels {
        // Platform-order u32: bits 24-31 = A, 16-23 = R, 8-15 = G, 0-7 = B
        let a = ((p >> 24) & 0xFF) as u8;
        let r = ((p >> 16) & 0xFF) as u8;
        let g = ((p >> 8) & 0xFF) as u8;
        let b = (p & 0xFF) as u8;
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}
