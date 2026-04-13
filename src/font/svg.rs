//! Rasterise emoji from the OpenType `SVG ` table via `resvg`.
//!
//! OpenType SVG documents are plain SVG (optionally gzip-compressed) whose
//! coordinate system matches the font's em grid. A single document often
//! carries several glyphs side-by-side in a `<defs>` block and tags each
//! one with `id="glyphN"`; we render only the matching subtree so nearby
//! glyphs don't leak into the cell. When the document contains no such id
//! (common for single-glyph documents) we render the whole tree.

use std::io::Read;

use flate2::read::GzDecoder;
use read_fonts::FontRef;
use read_fonts::TableProvider;
use read_fonts::types::GlyphId;
use resvg::tiny_skia::Pixmap;
use resvg::tiny_skia::Transform;
use resvg::usvg;

use super::RasterizedGlyph;

/// Rasterise a glyph via the font's SVG table.
pub fn rasterize_svg(
    font: &FontRef<'_>,
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
) -> Option<RasterizedGlyph> {
    let svg = font.svg().ok()?;
    let raw = svg
        .glyph_data(GlyphId::new(glyph_id as u32))
        .ok()
        .flatten()?;

    let decompressed = maybe_decompress(raw)?;
    let svg_bytes: &[u8] = decompressed.as_deref().unwrap_or(raw);

    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &opts).ok()?;

    let (scale, ascent_px) = font_scale(font, cell_height);
    let pad = 2u32;
    let id = format!("glyph{}", glyph_id);
    match tree.node_by_id(&id) {
        Some(node) => render_node(node, scale, ascent_px, cell_width, cell_height, pad),
        None => render_tree(&tree, scale, ascent_px, cell_width, cell_height, pad),
    }
}

/// Returns the decompressed bytes when the input is gzip, `Some(None)` when
/// the input is a plain SVG (caller uses the original slice), and `None` on
/// a gzip decode failure.
fn maybe_decompress(bytes: &[u8]) -> Option<Option<Vec<u8>>> {
    if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        let mut out = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut out).ok()?;
        Some(Some(out))
    } else {
        Some(None)
    }
}

fn render_node(
    node: &usvg::Node,
    scale: f32,
    ascent_px: f32,
    cell_width: u32,
    cell_height: u32,
    pad: u32,
) -> Option<RasterizedGlyph> {
    // `render_node` translates the node so its bounding box sits at pixmap
    // origin; we get to choose where in the pixmap that origin lands. Place
    // the scaled bbox centered horizontally and flush to the top-pad line —
    // matches the layout the other emoji rasterisers produce.
    let bbox = node.abs_layer_bounding_box()?;
    let scaled_w = (bbox.width() * scale).ceil() as u32;
    let scaled_h = (bbox.height() * scale).ceil() as u32;
    let out_w = scaled_w.max(cell_width) + 2 * pad;
    let out_h = scaled_h.max(cell_height) + 2 * pad;

    let x_off = pad as f32 + ((cell_width as f32 - scaled_w as f32) * 0.5).max(0.0);
    let y_off = pad as f32;

    let mut pixmap = Pixmap::new(out_w, out_h)?;
    let transform = Transform::from_translate(x_off, y_off).pre_scale(scale, scale);
    resvg::render_node(node, transform, &mut pixmap.as_mut());

    Some(finish(pixmap, out_w, out_h, ascent_px, pad))
}

fn render_tree(
    tree: &usvg::Tree,
    scale: f32,
    ascent_px: f32,
    cell_width: u32,
    cell_height: u32,
    pad: u32,
) -> Option<RasterizedGlyph> {
    let size = tree.size();
    let scaled_w = (size.width() * scale).ceil() as u32;
    let scaled_h = (size.height() * scale).ceil() as u32;
    let out_w = scaled_w.max(cell_width) + 2 * pad;
    let out_h = scaled_h.max(cell_height) + 2 * pad;

    let mut pixmap = Pixmap::new(out_w, out_h)?;
    let transform = Transform::from_translate(pad as f32, pad as f32).pre_scale(scale, scale);
    resvg::render(tree, transform, &mut pixmap.as_mut());

    Some(finish(pixmap, out_w, out_h, ascent_px, pad))
}

fn finish(
    pixmap: Pixmap,
    out_w: u32,
    out_h: u32,
    ascent_px: f32,
    pad: u32,
) -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: pixmap.data().to_vec(),
        width: out_w,
        height: out_h,
        bearing_x: -(pad as i32),
        // Baseline sits at pad+ascent_px below the pixmap top, so the top of
        // the bitmap is pad+ascent_px above the baseline — matches COLR and
        // sbix/CBDT.
        bearing_y: (pad as f32 + ascent_px) as i32,
        is_color: true,
    }
}

/// Derive the SVG → pixel scale and the ascent (in pixels) from the emoji
/// font's horizontal metrics. The OpenType SVG coord system is the font's
/// unit-per-em box, so fitting `ascender + |descender|` units into the cell
/// height lines the glyph up with surrounding text — same policy as the COLR
/// and bitmap paths.
fn font_scale(
    font: &FontRef<'_>,
    cell_height: u32,
) -> (f32, f32) {
    let Ok(hhea) = font.hhea() else {
        return (1.0, cell_height as f32);
    };
    let ascent_units = hhea.ascender().to_i16() as f32;
    let descent_units = -hhea.descender().to_i16() as f32;
    let line_h = (ascent_units + descent_units).max(1.0);
    let scale = cell_height as f32 / line_h;
    (scale, ascent_units * scale)
}
