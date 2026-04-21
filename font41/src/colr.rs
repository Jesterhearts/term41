//! COLR paint-graph rasterisation on top of raqote.
//!
//! COLRv0 glyphs are simple ordered layer lists. COLRv1 glyphs are paint
//! graphs; we walk those graphs for a single base glyph, compose accumulated
//! transforms eagerly onto path and gradient coordinates, and fill into a
//! pixel-space [`raqote::DrawTarget`]. Variation paint nodes render their
//! default-instance values; this keeps variable COLR fonts visible even
//! though term41 does not thread variation coordinates through font41 yet.

use std::collections::HashSet;

use raqote::BlendMode;
use raqote::DrawOptions;
use raqote::DrawTarget;
use raqote::Gradient;
use raqote::GradientStop;
use raqote::Image;
use raqote::Path;
use raqote::PathBuilder;
use raqote::SolidSource;
use raqote::Source;
use raqote::Spread;
use raqote::Transform;
use read_fonts::FontRef;
use read_fonts::TableProvider;
use read_fonts::tables::colr::ColorLine;
use read_fonts::tables::colr::Colr;
use read_fonts::tables::colr::CompositeMode;
use read_fonts::tables::colr::Extend;
use read_fonts::tables::colr::Paint;
use read_fonts::tables::colr::VarColorLine;
use read_fonts::tables::cpal::ColorRecord;
use read_fonts::tables::glyf::Glyph;
use read_fonts::tables::glyf::SimpleGlyph;
use read_fonts::tables::loca::Loca;
use read_fonts::types::GlyphId;

use super::RasterizedGlyph;

/// Rasterise a glyph via the font's COLR table. Returns `None` if the font
/// has no COLR table, no base-glyph record for this id, or the outline tables
/// needed for layer/`PaintGlyph` resolution are missing.
///
/// Scaling and baseline are derived from the **emoji font's own** metrics —
/// not the primary text font's — so a Noto Color Emoji glyph with its 1024
/// upem fits the cell regardless of the monospace font we're running. The
/// bitmap's baseline still lands near the centre of the cell so emoji sit
/// visually aligned with surrounding text.
pub fn rasterize_colr(
    font: &FontRef<'_>,
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
) -> Option<RasterizedGlyph> {
    let colr = font.colr().ok()?;
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;
    let palette = load_palette(font)?;
    let layout = colr_layout(font, cell_width, cell_height)?;

    rasterize_colr_v1(
        colr.clone(),
        palette.clone(),
        loca.clone(),
        glyf.clone(),
        glyph_id,
        &layout,
    )
    .or_else(|| rasterize_colr_v0(colr, palette, loca, glyf, glyph_id, &layout))
}

struct ColrLayout {
    width: i32,
    height: i32,
    pad: i32,
    ascent_px_for_bearing: f32,
    base_transform: Transform,
}

fn colr_layout(
    font: &FontRef<'_>,
    cell_width: u32,
    cell_height: u32,
) -> Option<ColrLayout> {
    // Emoji font's own horizontal metrics. We fit the emoji's natural line
    // height (ascent + |descent|) to `cell_height`, and additionally cap the
    // scale so the rendered ink fits `cell_width`. `ascent_units` is the
    // closer proxy for visual ink extent than `line_height_units` — most
    // emoji occupy the above-baseline region and the descender area stays
    // empty — so width/height centering uses the ascent box rather than the
    // em box.
    let hhea = font.hhea().ok()?;
    let ascent_units = hhea.ascender().to_i16() as f32;
    let descent_units = -hhea.descender().to_i16() as f32;
    let line_height_units = (ascent_units + descent_units).max(1.0);
    let scale_h = cell_height as f32 / line_height_units;
    let scale_w = cell_width as f32 / ascent_units.max(1.0);
    let scale = scale_h.min(scale_w);
    // `bearing_y` describes where the **bitmap** sits relative to the line
    // baseline — the renderer doesn't know we shrank the artwork. Use the
    // un-shrunk ascent so the bitmap rectangle still aligns with the cell
    // box; the in-bitmap centering below moves only the artwork inside it.
    let ascent_px_for_bearing = ascent_units * scale_h;

    // Bitmap size — match the cell with small padding for ink that slightly
    // exceeds the advance/ascent.
    let pad = 2;
    let width = cell_width as i32 + pad * 2;
    let height = cell_height as i32 + pad * 2;

    // Base transform: font units (y-up) → pixel coords (y-down). Center the
    // approximate ink box (ascent-sized, square) inside the cell. The em
    // box would leave an unused descender strip that biases the glyph
    // upward; the ascent box is a better fit for where the ink actually is.
    let ink_box = ascent_units * scale;
    let x_origin = pad as f32 + ((cell_width as f32 - ink_box) * 0.5).max(0.0);
    let y_origin = pad as f32 + ((cell_height as f32 - ink_box) * 0.5).max(0.0) + ink_box;
    let base_transform =
        Transform::scale(scale, -scale).then_translate(raqote::Vector::new(x_origin, y_origin));
    Some(ColrLayout {
        width,
        height,
        pad,
        ascent_px_for_bearing,
        base_transform,
    })
}

fn rasterize_colr_v1<'a>(
    colr: Colr<'a>,
    palette: Vec<[u8; 4]>,
    loca: Loca<'a>,
    glyf: read_fonts::tables::glyf::Glyf<'a>,
    glyph_id: u16,
    layout: &ColrLayout,
) -> Option<RasterizedGlyph> {
    let (root_paint, root_id) = colr
        .v1_base_glyph(GlyphId::new(glyph_id as u32))
        .ok()
        .flatten()?;

    let mut painter = Painter {
        colr,
        palette,
        loca,
        glyf,
        dt: DrawTarget::new(layout.width, layout.height),
        width: layout.width,
        height: layout.height,
        transforms: vec![layout.base_transform],
        current_path: None,
        visited: HashSet::new(),
    };
    painter.visited.insert(root_id);
    painter.paint(&root_paint);

    let pixels = painter.dt.into_vec();
    Some(RasterizedGlyph {
        bitmap: premul_argb_to_rgba(&pixels),
        width: layout.width as u32,
        height: layout.height as u32,
        bearing_x: -layout.pad,
        bearing_y: (layout.pad as f32 + layout.ascent_px_for_bearing) as i32,
        is_color: true,
    })
}

fn rasterize_colr_v0<'a>(
    colr: Colr<'a>,
    palette: Vec<[u8; 4]>,
    loca: Loca<'a>,
    glyf: read_fonts::tables::glyf::Glyf<'a>,
    glyph_id: u16,
    layout: &ColrLayout,
) -> Option<RasterizedGlyph> {
    let layers = colr
        .v0_base_glyph(GlyphId::new(glyph_id as u32))
        .ok()
        .flatten()?;
    let mut painter = Painter {
        colr: colr.clone(),
        palette,
        loca,
        glyf,
        dt: DrawTarget::new(layout.width, layout.height),
        width: layout.width,
        height: layout.height,
        transforms: vec![layout.base_transform],
        current_path: None,
        visited: HashSet::new(),
    };
    for layer_index in layers {
        let Ok((layer_glyph, palette_index)) = colr.v0_layer(layer_index) else {
            continue;
        };
        painter.paint_colr_v0_layer(layer_glyph.to_u16(), palette_index);
    }

    let pixels = painter.dt.into_vec();
    Some(RasterizedGlyph {
        bitmap: premul_argb_to_rgba(&pixels),
        width: layout.width as u32,
        height: layout.height as u32,
        bearing_x: -layout.pad,
        bearing_y: (layout.pad as f32 + layout.ascent_px_for_bearing) as i32,
        is_color: true,
    })
}

struct Painter<'a> {
    colr: Colr<'a>,
    palette: Vec<[u8; 4]>,
    loca: Loca<'a>,
    glyf: read_fonts::tables::glyf::Glyf<'a>,
    dt: DrawTarget,
    width: i32,
    height: i32,
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
            Paint::VarSolid(p) => {
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
                self.fill_linear_gradient(
                    &path,
                    color_line_stops(&line, &self.palette),
                    line.extend(),
                    p.x0().to_i16() as f32,
                    p.y0().to_i16() as f32,
                    p.x1().to_i16() as f32,
                    p.y1().to_i16() as f32,
                );
            }
            Paint::VarLinearGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                self.fill_linear_gradient(
                    &path,
                    var_color_line_stops(&line, &self.palette),
                    line.extend(),
                    p.x0().to_i16() as f32,
                    p.y0().to_i16() as f32,
                    p.x1().to_i16() as f32,
                    p.y1().to_i16() as f32,
                );
            }
            Paint::RadialGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                self.fill_radial_gradient(
                    &path,
                    color_line_stops(&line, &self.palette),
                    line.extend(),
                    p.x0().to_i16() as f32,
                    p.y0().to_i16() as f32,
                    p.radius0().to_u16() as f32,
                    p.x1().to_i16() as f32,
                    p.y1().to_i16() as f32,
                    p.radius1().to_u16() as f32,
                );
            }
            Paint::VarRadialGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                self.fill_radial_gradient(
                    &path,
                    var_color_line_stops(&line, &self.palette),
                    line.extend(),
                    p.x0().to_i16() as f32,
                    p.y0().to_i16() as f32,
                    p.radius0().to_u16() as f32,
                    p.x1().to_i16() as f32,
                    p.y1().to_i16() as f32,
                    p.radius1().to_u16() as f32,
                );
            }
            Paint::SweepGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                self.fill_sweep_gradient(
                    &path,
                    color_line_stops(&line, &self.palette),
                    line.extend(),
                    p.center_x().to_i16() as f32,
                    p.center_y().to_i16() as f32,
                    p.start_angle().to_f32(),
                    p.end_angle().to_f32(),
                );
            }
            Paint::VarSweepGradient(p) => {
                let path = match &self.current_path {
                    Some(p) => p.clone(),
                    None => return,
                };
                let Ok(line) = p.color_line() else { return };
                self.fill_sweep_gradient(
                    &path,
                    var_color_line_stops(&line, &self.palette),
                    line.extend(),
                    p.center_x().to_i16() as f32,
                    p.center_y().to_i16() as f32,
                    p.start_angle().to_f32(),
                    p.end_angle().to_f32(),
                );
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
                let t = affine_transform(
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
            Paint::VarTransform(p) => {
                let Ok(a) = p.transform() else { return };
                let t = affine_transform(
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
            Paint::VarTranslate(p) => {
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
            Paint::VarScale(p) => {
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
            Paint::VarScaleAroundCenter(p) => {
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
            Paint::VarScaleUniform(p) => {
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
            Paint::VarScaleUniformAroundCenter(p) => {
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
            Paint::VarRotate(p) => {
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
            Paint::VarRotateAroundCenter(p) => {
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
            Paint::VarSkew(p) => {
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
            Paint::VarSkewAroundCenter(p) => {
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
            Paint::Composite(p) => {
                let Ok(backdrop) = p.backdrop_paint() else {
                    return;
                };
                let Ok(source) = p.source_paint() else {
                    return;
                };
                self.paint_composite(&backdrop, &source, p.composite_mode());
            }
        }
    }

    fn paint_colr_v0_layer(
        &mut self,
        glyph_id: u16,
        palette_index: u16,
    ) {
        let Some(path) = self.extract_outline(glyph_id) else {
            return;
        };
        self.fill_solid(&path, palette_color(&self.palette, palette_index));
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

    fn fill_linear_gradient(
        &mut self,
        path: &Path,
        stops: Vec<ColrStop>,
        extend: Extend,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let tfm = self.current_transform();
        let p0 = tfm.transform_point(raqote::Point::new(x0, y0));
        let p1 = tfm.transform_point(raqote::Point::new(x1, y1));
        let grad = Gradient {
            stops: raqote_gradient_stops(&stops),
        };
        let src = Source::new_linear_gradient(
            grad,
            raqote::Point::new(p0.x, p0.y),
            raqote::Point::new(p1.x, p1.y),
            extend_spread(extend),
        );
        self.dt.fill(path, &src, &DrawOptions::default());
    }

    fn fill_radial_gradient(
        &mut self,
        path: &Path,
        stops: Vec<ColrStop>,
        extend: Extend,
        x0: f32,
        y0: f32,
        radius0: f32,
        x1: f32,
        y1: f32,
        radius1: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let tfm = self.current_transform();
        let c0 = tfm.transform_point(raqote::Point::new(x0, y0));
        let c1 = tfm.transform_point(raqote::Point::new(x1, y1));
        // Raqote's two-circle radial gradient accepts scalar radii. COLR
        // permits arbitrary transforms; use the mean axis scale as the same
        // pragmatic approximation the previous implementation used.
        let scale_est = (tfm.m11.hypot(tfm.m12) + tfm.m21.hypot(tfm.m22)) * 0.5;
        let grad = Gradient {
            stops: raqote_gradient_stops(&stops),
        };
        let src = Source::new_two_circle_radial_gradient(
            grad,
            raqote::Point::new(c0.x, c0.y),
            radius0 * scale_est,
            raqote::Point::new(c1.x, c1.y),
            radius1 * scale_est,
            extend_spread(extend),
        );
        self.dt.fill(path, &src, &DrawOptions::default());
    }

    fn fill_sweep_gradient(
        &mut self,
        path: &Path,
        stops: Vec<ColrStop>,
        extend: Extend,
        center_x: f32,
        center_y: f32,
        start_angle: f32,
        end_angle: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let mut mask = DrawTarget::new(self.width, self.height);
        mask.fill(
            path,
            &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
            &DrawOptions {
                blend_mode: BlendMode::Src,
                ..DrawOptions::default()
            },
        );

        let tfm = self.current_transform();
        let center = tfm.transform_point(raqote::Point::new(center_x, center_y));
        let start = start_angle * std::f32::consts::PI;
        let end = end_angle * std::f32::consts::PI;
        let mask_pixels = mask.get_data();
        let dst = self.dt.get_data_mut();
        for y in 0..self.height {
            for x in 0..self.width {
                let idx = (y * self.width + x) as usize;
                let coverage = ((mask_pixels[idx] >> 24) & 0xFF) as u8;
                if coverage == 0 {
                    continue;
                }
                let px = x as f32 + 0.5 - center.x;
                let py = center.y - (y as f32 + 0.5);
                let angle = py.atan2(px);
                let t = sweep_offset(angle, start, end, extend);
                let rgba = sample_color_line(&stops, t);
                let src = premul_rgba_to_argb(apply_alpha(rgba, coverage as f32 / 255.0));
                dst[idx] = src_over(src, dst[idx]);
            }
        }
    }

    fn paint_composite(
        &mut self,
        backdrop: &Paint<'_>,
        source: &Paint<'_>,
        mode: CompositeMode,
    ) {
        let mut composed = self.paint_to_layer(backdrop);
        let source = self.paint_to_layer(source);
        let source_data = source.into_vec();
        let image = Image {
            width: self.width,
            height: self.height,
            data: &source_data,
        };
        composed.draw_image_at(
            0.0,
            0.0,
            &image,
            &DrawOptions {
                blend_mode: composite_blend_mode(mode),
                ..DrawOptions::default()
            },
        );

        let composed_data = composed.into_vec();
        let image = Image {
            width: self.width,
            height: self.height,
            data: &composed_data,
        };
        self.dt
            .draw_image_at(0.0, 0.0, &image, &DrawOptions::default());
    }

    fn paint_to_layer(
        &mut self,
        paint: &Paint<'_>,
    ) -> DrawTarget {
        let parent = std::mem::replace(&mut self.dt, DrawTarget::new(self.width, self.height));
        self.paint(paint);
        std::mem::replace(&mut self.dt, parent)
    }

    fn extract_outline(
        &self,
        glyph_id: u16,
    ) -> Option<Path> {
        let gid = GlyphId::new(glyph_id as u32);
        let glyph = self.loca.get_glyf(gid, &self.glyf).ok().flatten()?;
        let mut pb = PathBuilder::new();
        add_glyph_outline(
            &mut pb,
            &glyph,
            &self.loca,
            &self.glyf,
            Transform::identity(),
            0,
        );
        let raw = pb.finish();
        // Transform the raw outline (in font units, y-up) into the current
        // pixel-space coordinate system. raqote fills with the DrawTarget's
        // identity transform, so we bake our transform into the geometry.
        Some(raw.transform(&self.current_transform()))
    }
}

#[derive(Clone, Copy)]
struct OutlinePoint {
    x: f32,
    y: f32,
    on_curve: bool,
}

fn add_glyph_outline(
    pb: &mut PathBuilder,
    glyph: &Glyph<'_>,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    transform: Transform,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    match glyph {
        Glyph::Simple(simple) => add_simple_outline(pb, simple, transform),
        Glyph::Composite(composite) => {
            for comp in composite.components() {
                let gid = GlyphId::new(comp.glyph.to_u32());
                let Ok(Some(glyph)) = loca.get_glyf(gid, glyf) else {
                    continue;
                };
                let component = component_transform(&comp);
                add_glyph_outline(
                    pb,
                    &glyph,
                    loca,
                    glyf,
                    component.then(&transform),
                    depth + 1,
                );
            }
        }
    }
}

fn add_simple_outline(
    pb: &mut PathBuilder,
    simple: &SimpleGlyph,
    transform: Transform,
) {
    let points: Vec<OutlinePoint> = simple
        .points()
        .map(|p| {
            let point = transform.transform_point(raqote::Point::new(p.x as f32, p.y as f32));
            OutlinePoint {
                x: point.x,
                y: point.y,
                on_curve: p.on_curve,
            }
        })
        .collect();
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
    contour: &[OutlinePoint],
) {
    if contour.is_empty() {
        return;
    }
    // Expand implicit on-curve points between consecutive off-curve points.
    let mut expanded: Vec<(f32, f32, bool)> = Vec::with_capacity(contour.len());
    for p in contour {
        let px = p.x;
        let py = p.y;
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

fn component_transform(comp: &read_fonts::tables::glyf::Component) -> Transform {
    let (dx, dy) = match comp.anchor {
        read_fonts::tables::glyf::Anchor::Offset { x, y } => (x as f32, y as f32),
        read_fonts::tables::glyf::Anchor::Point { .. } => (0.0, 0.0),
    };
    Transform::new(
        comp.transform.xx.to_f32(),
        comp.transform.yx.to_f32(),
        comp.transform.xy.to_f32(),
        comp.transform.yy.to_f32(),
        dx,
        dy,
    )
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

#[derive(Clone, Copy)]
struct ColrStop {
    position: f32,
    rgba: [u8; 4],
}

fn color_line_stops(
    line: &ColorLine,
    palette: &[[u8; 4]],
) -> Vec<ColrStop> {
    line.color_stops()
        .iter()
        .map(|stop| {
            let rgba = apply_alpha(
                palette_color(palette, stop.palette_index()),
                stop.alpha().to_f32(),
            );
            ColrStop {
                position: stop.stop_offset().to_f32(),
                rgba,
            }
        })
        .collect()
}

fn var_color_line_stops(
    line: &VarColorLine,
    palette: &[[u8; 4]],
) -> Vec<ColrStop> {
    line.color_stops()
        .iter()
        .map(|stop| {
            let rgba = apply_alpha(
                palette_color(palette, stop.palette_index()),
                stop.alpha().to_f32(),
            );
            ColrStop {
                position: stop.stop_offset().to_f32(),
                rgba,
            }
        })
        .collect()
}

fn raqote_gradient_stops(stops: &[ColrStop]) -> Vec<GradientStop> {
    stops
        .iter()
        .map(|stop| GradientStop {
            position: stop.position,
            color: raqote::Color::new(stop.rgba[3], stop.rgba[0], stop.rgba[1], stop.rgba[2]),
        })
        .collect()
}

fn extend_spread(extend: Extend) -> Spread {
    use Extend as E;
    match extend {
        E::Pad => Spread::Pad,
        E::Repeat => Spread::Repeat,
        E::Reflect => Spread::Reflect,
        _ => Spread::Pad,
    }
}

fn sweep_offset(
    angle: f32,
    start: f32,
    end: f32,
    extend: Extend,
) -> f32 {
    let span = end - start;
    if span.abs() < f32::EPSILON {
        return 1.0;
    }
    let raw = (angle - start) / span;
    match extend {
        Extend::Pad => raw.clamp(0.0, 1.0),
        Extend::Repeat => raw.rem_euclid(1.0),
        Extend::Reflect => {
            let reflected = raw.rem_euclid(2.0);
            if reflected <= 1.0 {
                reflected
            } else {
                2.0 - reflected
            }
        }
        _ => raw.clamp(0.0, 1.0),
    }
}

fn sample_color_line(
    stops: &[ColrStop],
    offset: f32,
) -> [u8; 4] {
    if stops.is_empty() {
        return [0, 0, 0, 0];
    }
    let mut prev = stops[0];
    if offset <= prev.position {
        return prev.rgba;
    }
    for &next in &stops[1..] {
        if offset <= next.position {
            let span = (next.position - prev.position).max(f32::EPSILON);
            let t = ((offset - prev.position) / span).clamp(0.0, 1.0);
            return lerp_rgba(prev.rgba, next.rgba, t);
        }
        prev = next;
    }
    prev.rgba
}

fn lerp_rgba(
    a: [u8; 4],
    b: [u8; 4],
    t: f32,
) -> [u8; 4] {
    [
        lerp_u8(a[0], b[0], t),
        lerp_u8(a[1], b[1], t),
        lerp_u8(a[2], b[2], t),
        lerp_u8(a[3], b[3], t),
    ]
}

fn lerp_u8(
    a: u8,
    b: u8,
    t: f32,
) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn premul_rgba_to_argb(rgba: [u8; 4]) -> u32 {
    let a = rgba[3] as u32;
    let r = (rgba[0] as u32 * a + 127) / 255;
    let g = (rgba[1] as u32 * a + 127) / 255;
    let b = (rgba[2] as u32 * a + 127) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

fn src_over(
    src: u32,
    dst: u32,
) -> u32 {
    let sa = (src >> 24) & 0xFF;
    let inv_sa = 255 - sa;
    let da = (dst >> 24) & 0xFF;
    let sr = (src >> 16) & 0xFF;
    let sg = (src >> 8) & 0xFF;
    let sb = src & 0xFF;
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;
    let a = sa + (da * inv_sa + 127) / 255;
    let r = sr + (dr * inv_sa + 127) / 255;
    let g = sg + (dg * inv_sa + 127) / 255;
    let b = sb + (db * inv_sa + 127) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

fn composite_blend_mode(mode: CompositeMode) -> BlendMode {
    match mode {
        CompositeMode::Clear => BlendMode::Clear,
        CompositeMode::Src => BlendMode::Src,
        CompositeMode::Dest => BlendMode::Dst,
        CompositeMode::SrcOver => BlendMode::SrcOver,
        CompositeMode::DestOver => BlendMode::DstOver,
        CompositeMode::SrcIn => BlendMode::SrcIn,
        CompositeMode::DestIn => BlendMode::DstIn,
        CompositeMode::SrcOut => BlendMode::SrcOut,
        CompositeMode::DestOut => BlendMode::DstOut,
        CompositeMode::SrcAtop => BlendMode::SrcAtop,
        CompositeMode::DestAtop => BlendMode::DstAtop,
        CompositeMode::Xor => BlendMode::Xor,
        CompositeMode::Plus => BlendMode::Add,
        CompositeMode::Screen => BlendMode::Screen,
        CompositeMode::Overlay => BlendMode::Overlay,
        CompositeMode::Darken => BlendMode::Darken,
        CompositeMode::Lighten => BlendMode::Lighten,
        CompositeMode::ColorDodge => BlendMode::ColorDodge,
        CompositeMode::ColorBurn => BlendMode::ColorBurn,
        CompositeMode::HardLight => BlendMode::HardLight,
        CompositeMode::SoftLight => BlendMode::SoftLight,
        CompositeMode::Difference => BlendMode::Difference,
        CompositeMode::Exclusion => BlendMode::Exclusion,
        CompositeMode::Multiply => BlendMode::Multiply,
        CompositeMode::HslHue => BlendMode::Hue,
        CompositeMode::HslSaturation => BlendMode::Saturation,
        CompositeMode::HslColor => BlendMode::Color,
        CompositeMode::HslLuminosity => BlendMode::Luminosity,
        CompositeMode::Unknown => BlendMode::SrcOver,
    }
}

fn affine_transform(
    xx: f32,
    yx: f32,
    xy: f32,
    yy: f32,
    dx: f32,
    dy: f32,
) -> Transform {
    Transform::new(xx, yx, xy, yy, dx, dy)
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
