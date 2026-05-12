#![allow(clippy::too_many_arguments)]

use log::debug;
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

use crate::FontSystem;
use crate::RasterizedGlyph;
use crate::bitmap;
use crate::colr;
use crate::drcs;
use crate::legacy;
use crate::loader;
use crate::loader::load_font_candidate;
use crate::loader::loaded_font_ref;
use crate::svg;

pub(crate) fn rasterize_glyph(
    font_system: &FontSystem,
    font_index: usize,
    glyph_index: u16,
    cells_wide: u32,
) -> RasterizedGlyph {
    // Legacy shapes (block elements, braille, sextants, SFLC 1/8 blocks)
    // bypass the font entirely -- they render as cell-filling solid tiles
    // so "pixel art" built from them tiles seamlessly across neighbouring
    // cells. See `legacy` for the full codepoint list.
    if font_index == legacy::FONT_INDEX {
        debug!("rasterizing legacy glyph {glyph_index} for cell span {cells_wide}");
        return legacy::rasterize(
            glyph_index,
            font_system.cell_width,
            font_system.cell_height,
            font_system.ascent,
            font_system.supersample,
        );
    }
    if font_index == drcs::FONT_INDEX {
        debug!("rasterizing DRCS glyph {glyph_index} for cell span {cells_wide}");
        return drcs::rasterize(glyph_index, font_system.cell_width, font_system.cell_height);
    }

    let font_faces = loader::font_faces();
    let Some(font_candidate) =
        load_font_candidate(font_index, &font_faces, &font_system.final_font)
    else {
        return empty_glyph();
    };
    let loaded = font_candidate.as_loaded();
    let scale = font_system.font_size / loaded.units_per_em;

    let Ok(font) = loaded_font_ref(loaded) else {
        return empty_glyph();
    };

    // Colour rasterisers receive the cell box scaled to the cluster's visual
    // span. The outline path doesn't read `cell_width` at all -- glyf
    // positioning derives entirely from the glyph's own bounding box and
    // `font_size` -- so it sees no behavioural change when a cluster covers
    // more than one cell.
    let target_w = font_system.cell_width * cells_wide.max(1);

    if let Some(glyph) = colr::rasterize_colr(&font, glyph_index, target_w, font_system.cell_height)
    {
        debug!("rasterized COLR glyph {glyph_index} for cell span {cells_wide}");
        return glyph;
    }
    if let Some(glyph) = svg::rasterize_svg(&font, glyph_index, target_w, font_system.cell_height) {
        debug!("rasterized SVG glyph {glyph_index} for cell span {cells_wide}");
        return glyph;
    }
    if let Some(glyph) =
        bitmap::rasterize_sbix(&font, glyph_index, target_w, font_system.cell_height)
    {
        debug!("rasterized sbix glyph {glyph_index} for cell span {cells_wide}");
        return glyph;
    }
    if let Some(glyph) =
        bitmap::rasterize_cbdt(&font, glyph_index, target_w, font_system.cell_height)
    {
        debug!("rasterized CBDT glyph {glyph_index} for cell span {cells_wide}");
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
        Ok(Some(Glyph::Simple(simple))) => {
            debug!("rasterizing outline glyph {glyph_index} for cell span {cells_wide}");
            rasterize_simple_glyph(&simple, scale, font_system.supersample)
        }
        Ok(Some(Glyph::Composite(composite))) => {
            debug!("rasterizing composite glyph {glyph_index} for cell span {cells_wide}");
            rasterize_composite_glyph(&composite, &loca, &glyf, scale, font_system.supersample)
        }
        _ => empty_glyph(),
    }
}

pub(crate) fn outline_glyph_bounds(
    font: &read_fonts::FontRef<'_>,
    glyph_id: u16,
    scale: f32,
) -> Option<[f32; 4]> {
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;
    let glyph = loca.get_glyf(GlyphId::new(glyph_id as u32), &glyf).ok()??;

    match glyph {
        Glyph::Simple(simple) => Some([
            simple.x_min() as f32 * scale,
            simple.y_min() as f32 * scale,
            simple.x_max() as f32 * scale,
            simple.y_max() as f32 * scale,
        ]),
        Glyph::Composite(composite) => {
            let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            composite_bounds(&composite, &loca, &glyf, scale, 0.0, 0.0, &mut bounds);
            (bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(bounds)
        }
    }
}

pub(crate) fn empty_glyph() -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: vec![],
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
        is_color: false,
    }
}

fn rasterize_simple_glyph(
    simple: &SimpleGlyph,
    scale: f32,
    supersample: u32,
) -> RasterizedGlyph {
    // 1x bounds -- these define the output size and bearing so the glyph
    // lands at the same position it would without supersampling.
    let x_min = (simple.x_min() as f32 * scale).floor();
    let y_max = (simple.y_max() as f32 * scale).ceil();
    let width = ((simple.x_max() as f32 * scale).ceil() - x_min) as i32 + 2;
    let height = (y_max - (simple.y_min() as f32 * scale).floor()) as i32 + 2;

    if width <= 0 || height <= 0 {
        return empty_glyph();
    }

    // Rasterize at supersampled resolution then downsample.
    let ss = supersample as i32;
    let ss_scale = scale * ss as f32;
    let ss_w = width * ss;
    let ss_h = height * ss;
    let path = build_path(
        simple,
        ss_scale,
        x_min * ss as f32,
        y_max * ss as f32,
        STEM_DARKEN_SS_PX,
    );
    let mut dt = DrawTarget::new(ss_w, ss_h);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    RasterizedGlyph {
        bitmap: downsample_alpha(dt.get_data(), ss_w, ss_h, width, height, ss),
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
        is_color: false,
    }
}

/// Box-filter downsample a supersampled raqote alpha buffer into an RGBA8
/// bitmap whose colour channels are zero. Each output pixel averages the
/// `ss x ss` block of source pixels that map to it, giving the glyph
/// `ss^2` levels of sub-pixel coverage -- noticeably smoother stems and
/// curves than the binary on/off coverage at 1x.
///
/// `pixels` is the raqote `DrawTarget::get_data()` buffer -- premultiplied
/// ARGB in platform byte order, `ss_w x ss_h` pixels. Output is
/// `out_w x out_h` RGBA8 with `rgb = 0`.
fn downsample_alpha(
    pixels: &[u32],
    ss_w: i32,
    ss_h: i32,
    out_w: i32,
    out_h: i32,
    ss: i32,
) -> Vec<u8> {
    let pixels_u8: Vec<u8> = pixels.iter().flat_map(|&p| p.to_le_bytes()).collect();
    downsample_alpha_u8(&pixels_u8, ss_w, ss_h, out_w, out_h, ss)
}

pub(crate) fn downsample_alpha_u8(
    pixels: &[u8],
    ss_w: i32,
    ss_h: i32,
    out_w: i32,
    out_h: i32,
    ss: i32,
) -> Vec<u8> {
    let mut bitmap = vec![0u8; (out_w * out_h * 4) as usize];
    let area = (ss * ss) as u32;
    for y in 0..out_h {
        for x in 0..out_w {
            let mut alpha_sum = 0u32;
            for dy in 0..ss {
                for dx in 0..ss {
                    let sx = x * ss + dx;
                    let sy = y * ss + dy;
                    if sx < ss_w && sy < ss_h {
                        let idx = (sy * ss_w + sx) as usize * 4;
                        alpha_sum += pixels[idx + 3] as u32;
                    }
                }
            }
            let idx = (y * out_w + x) as usize * 4;
            bitmap[idx + 3] = (alpha_sum / area) as u8;
        }
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
    bounds: &mut [f32; 4],
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
    embolden: f32,
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
                add_simple_glyph_to_path(pb, &simple, scale, x_min, y_max, dx, dy, embolden);
            }
            Glyph::Composite(inner) => {
                composite_to_path(
                    pb, &inner, loca, glyf, scale, x_min, y_max, dx, dy, embolden,
                );
            }
        }
    }
}

fn rasterize_composite_glyph(
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
    supersample: u32,
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

    // Rasterize at supersampled resolution then downsample.
    let ss = supersample as i32;
    let ss_scale = scale * ss as f32;
    let ss_w = width * ss;
    let ss_h = height * ss;

    let mut pb = PathBuilder::new();
    composite_to_path(
        &mut pb,
        composite,
        loca,
        glyf,
        ss_scale,
        x_min * ss as f32,
        y_max * ss as f32,
        0.0,
        0.0,
        STEM_DARKEN_SS_PX,
    );

    let path = pb.finish();
    let mut dt = DrawTarget::new(ss_w, ss_h);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    RasterizedGlyph {
        bitmap: downsample_alpha(dt.get_data(), ss_w, ss_h, width, height, ss),
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
    embolden: f32,
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
        add_contour_to_path_with_offset(pb, contour, scale, x_min, y_max, dx, dy, embolden);
        contour_start = contour_end + 1;
    }
}

fn build_path(
    simple: &SimpleGlyph,
    scale: f32,
    x_min: f32,
    y_max: f32,
    embolden: f32,
) -> raqote::Path {
    let mut pb = PathBuilder::new();
    add_simple_glyph_to_path(&mut pb, simple, scale, x_min, y_max, 0.0, 0.0, embolden);
    pb.finish()
}

/// Stem darkening amount in supersample pixels. Each contour point is moved
/// outward along its vertex normal by this amount, uniformly thickening
/// stems. 0.4 supersample px = 0.2 display pixels at SUPERSAMPLE=2.
const STEM_DARKEN_SS_PX: f32 = 0.4;

/// Move each point in a closed contour outward along its vertex normal by
/// `amount` pixels. Outer contours expand; inner contours (holes) shrink --
/// both actions increase the filled area, thickening stems uniformly.
///
/// The vertex normal at each point is the average of the two adjacent edge
/// normals, normalised. At sharp corners (where the averaged normal is very
/// short), the offset is capped at `2 * amount` to prevent miter spikes.
fn embolden_contour(
    points: &mut [(f32, f32, bool)],
    amount: f32,
) {
    let n = points.len();
    if n < 3 || amount <= 0.0 {
        return;
    }

    // Signed area via the shoelace formula. In screen-space (Y-down) a
    // positive result means CW winding (inner/hole contour), negative means
    // CCW (outer contour).
    let mut area2 = 0.0f32;
    for i in 0..n {
        let j = (i + 1) % n;
        area2 += points[i].0 * points[j].1 - points[j].0 * points[i].1;
    }

    let sign = if area2 >= 0.0 { -amount } else { amount };
    let max_offset = 2.0 * amount;

    let offsets: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let prev = if i == 0 { n - 1 } else { i - 1 };
            let next = (i + 1) % n;

            let (px, py, _) = points[prev];
            let (cx, cy, _) = points[i];
            let (nx, ny, _) = points[next];

            let (e1x, e1y) = (cx - px, cy - py);
            let (e2x, e2y) = (nx - cx, ny - cy);

            let len1 = (e1x * e1x + e1y * e1y).sqrt().max(1e-6);
            let len2 = (e2x * e2x + e2y * e2y).sqrt().max(1e-6);
            let (n1x, n1y) = (-e1y / len1, e1x / len1);
            let (n2x, n2y) = (-e2y / len2, e2x / len2);

            let (ax, ay) = (n1x + n2x, n1y + n2y);
            let alen = (ax * ax + ay * ay).sqrt().max(1e-6);
            let offset = (sign / alen).clamp(-max_offset, max_offset);
            (ax * offset, ay * offset)
        })
        .collect();

    for (i, &(dx, dy)) in offsets.iter().enumerate() {
        points[i].0 += dx;
        points[i].1 += dy;
    }
}

fn add_contour_to_path_with_offset(
    pb: &mut PathBuilder,
    contour: &[CurvePoint],
    scale: f32,
    x_min: f32,
    y_max: f32,
    dx: f32,
    dy: f32,
    embolden: f32,
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

    embolden_contour(&mut expanded, embolden);

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
