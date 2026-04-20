//! Rasterise emoji from PNG-backed glyph bitmap tables.
//!
//! Covers two OpenType bitmap tables:
//!
//! - `sbix` (Apple Color Emoji and anything else that follows the Apple
//!   convention — one or more strikes, each keyed by pixels-per-em, with the
//!   glyph payload stored as a standalone PNG).
//! - `CBDT`/`CBLC` (Noto Color Emoji's older revisions, Windows emoji — a
//!   location table points into a data table, with PNG payloads living in
//!   `BitmapDataFormat::Png` records).
//!
//! Monochrome EBDT/EBLC is intentionally unsupported — legacy Windows-style
//! embedded bitmaps almost never show up in fonts a terminal would load, and
//! the outline fallback already renders those glyphs from `glyf`.

use png::Transformations;
use read_fonts::FontRef;
use read_fonts::TableProvider;
use read_fonts::tables::bitmap::BitmapContent;
use read_fonts::tables::bitmap::BitmapDataFormat;
use read_fonts::tables::bitmap::BitmapSize;
use read_fonts::tables::cblc::Cblc;
use read_fonts::tables::sbix::Sbix;
use read_fonts::types::GlyphId;
use read_fonts::types::Tag;
use resvg::tiny_skia::FilterQuality;
use resvg::tiny_skia::Pixmap;
use resvg::tiny_skia::PixmapPaint;
use resvg::tiny_skia::Transform;

use super::RasterizedGlyph;

/// Rasterise a glyph via the font's `sbix` table. Returns `None` when the
/// font lacks `sbix`, the glyph has no entry, or the payload is not in a
/// format we decode (we accept PNG only — JPEG/TIFF are spec-legal but vanish
/// in practice, and no widely distributed emoji font ships them).
pub fn rasterize_sbix(
    font: &FontRef<'_>,
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
) -> Option<RasterizedGlyph> {
    let sbix = font.sbix().ok()?;
    let strike = pick_sbix_strike(&sbix, cell_height)?;
    let glyph_data = strike
        .glyph_data(GlyphId::new(glyph_id as u32))
        .ok()
        .flatten()?;
    if glyph_data.graphic_type() != Tag::new(b"png ") {
        return None;
    }
    let pixmap = decode_png(glyph_data.data())?;
    Some(place_emoji(font, pixmap, cell_width, cell_height))
}

/// Rasterise a glyph via the font's `CBDT`/`CBLC` tables. Falls through to
/// `None` on any unsupported format — only PNG payloads are decoded, which
/// covers every real-world color emoji font with CBDT.
pub fn rasterize_cbdt(
    font: &FontRef<'_>,
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
) -> Option<RasterizedGlyph> {
    let cblc = font.cblc().ok()?;
    let cbdt = font.cbdt().ok()?;
    let size = pick_cblc_size(&cblc, cell_height)?;
    let location = size
        .location(cblc.offset_data(), GlyphId::new(glyph_id as u32))
        .ok()?;
    if location.is_empty() {
        return None;
    }
    let bitmap_data = cbdt.data(&location).ok()?;
    let bytes = match &bitmap_data.content {
        BitmapContent::Data(BitmapDataFormat::Png, bytes) => *bytes,
        // Raw bitmap and composite formats are spec-legal for CBDT but
        // Noto/Apple/Microsoft all ship PNG. Skip — the outline fallback
        // will produce something monochrome if anything.
        _ => return None,
    };
    let pixmap = decode_png(bytes)?;
    Some(place_emoji(font, pixmap, cell_width, cell_height))
}

/// Pick the sbix strike closest to `cell_height` without going below it. Apple
/// Color Emoji ships ppem steps like 32/64/96/128/160; for a 24 px cell we
/// want the 32 strike (downscaling is cheaper than upscaling), and for a 200
/// px cell we fall back to the largest strike available rather than scaling
/// the smallest one up.
fn pick_sbix_strike<'a>(
    sbix: &Sbix<'a>,
    cell_height: u32,
) -> Option<read_fonts::tables::sbix::Strike<'a>> {
    let strikes = sbix.strikes();
    let n = strikes.len();
    if n == 0 {
        return None;
    }

    let mut best_above: Option<(u16, usize)> = None;
    let mut best_below: Option<(u16, usize)> = None;
    for i in 0..n {
        let Ok(strike) = strikes.get(i) else { continue };
        let ppem = strike.ppem();
        if ppem as u32 >= cell_height {
            if best_above.is_none_or(|(p, _)| ppem < p) {
                best_above = Some((ppem, i));
            }
        } else if best_below.is_none_or(|(p, _)| ppem > p) {
            best_below = Some((ppem, i));
        }
    }

    let idx = best_above.or(best_below)?.1;
    strikes.get(idx).ok()
}

/// Same strike-picking policy as sbix, but for the `BitmapSize` records in
/// the CBLC table. We key off `ppem_y` since it drives vertical scaling and
/// most strikes are square.
fn pick_cblc_size<'a>(
    cblc: &Cblc<'a>,
    cell_height: u32,
) -> Option<&'a BitmapSize> {
    let sizes = cblc.bitmap_sizes();
    let mut best_above: Option<(u8, &BitmapSize)> = None;
    let mut best_below: Option<(u8, &BitmapSize)> = None;
    for size in sizes {
        let ppem = size.ppem_y();
        if ppem as u32 >= cell_height {
            if best_above.is_none_or(|(p, _)| ppem < p) {
                best_above = Some((ppem, size));
            }
        } else if best_below.is_none_or(|(p, _)| ppem > p) {
            best_below = Some((ppem, size));
        }
    }
    best_above.or(best_below).map(|(_, s)| s)
}

/// Decode PNG bytes into a `tiny_skia::Pixmap`. We route through the `png`
/// crate rather than tiny_skia's own `decode_png` because we want to drive
/// the 8-bit + RGBA transformation explicitly — real-world emoji PNGs show up
/// in every flavor (RGB, palette, 16-bit) and `Transformations::ALPHA |
/// EXPAND | STRIP_16` normalises them all into 8-bit RGBA in one pass.
fn decode_png(bytes: &[u8]) -> Option<Pixmap> {
    // png::Decoder wants Read + Seek; a slice is only Read, so we wrap it.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        Transformations::ALPHA | Transformations::EXPAND | Transformations::STRIP_16,
    );
    let mut reader = decoder.read_info().ok()?;

    let mut rgba = vec![0u8; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut rgba).ok()?;
    let width = frame.width;
    let height = frame.height;
    rgba.truncate(frame.buffer_size());

    // png gives us straight (non-premultiplied) RGBA. The atlas (and shader)
    // expect premultiplied RGBA on the color path — matches what raqote
    // produces for the COLR path — so multiply the color channels by alpha
    // before handing the buffer to tiny_skia.
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
    }

    Pixmap::from_vec(rgba, resvg::tiny_skia::IntSize::from_wh(width, height)?)
}

/// Place the decoded emoji pixmap inside a cell-sized bitmap. Scale is
/// derived from the emoji font's own `hhea` metrics so the bitmap lands at
/// the same visual size as COLR-rasterised emoji — see the parallel block in
/// `colr::rasterize_colr_v1`.
fn place_emoji(
    font: &FontRef<'_>,
    src: Pixmap,
    cell_width: u32,
    cell_height: u32,
) -> RasterizedGlyph {
    let pad = 2u32;
    let out_w = cell_width + 2 * pad;
    let out_h = cell_height + 2 * pad;

    // Pick the source → pixel scale. Start from the font-metric ratio that
    // fits the emoji's natural line height into `cell_height`, then cap it
    // by the width budget so a width=1 cluster doesn't overflow into the
    // neighbouring cell. The height fallback (no `hhea`) keeps the emoji at
    // cell height — better than collapsing.
    let (mut scale, ascent_px) = match font.hhea() {
        Ok(hhea) => {
            let ascent_units = hhea.ascender().to_i16() as f32;
            let descent_units = -hhea.descender().to_i16() as f32;
            let line_h = (ascent_units + descent_units).max(1.0);
            let upem = font
                .head()
                .ok()
                .map(|head| head.units_per_em())
                .unwrap_or(cell_width as u16) as f32;
            let font_scale = (cell_height as f32 / line_h).min(cell_width as f32 / upem);
            let src_to_pixel =
                font_scale * (upem / src.width() as f32).max(line_h / src.height() as f32);
            (src_to_pixel, ascent_units * font_scale)
        }
        Err(_) => {
            let scale = cell_height as f32 / src.height() as f32;
            (scale, cell_height as f32)
        }
    };
    let natural_w = src.width() as f32 * scale;
    if natural_w > cell_width as f32 {
        scale *= cell_width as f32 / natural_w;
    }

    let scaled_w = (src.width() as f32 * scale).round() as u32;
    let scaled_h = (src.height() as f32 * scale).round() as u32;

    let x_off = pad as f32 + ((cell_width as f32 - scaled_w as f32) * 0.5).max(0.0);
    let y_off = pad as f32 + ((cell_height as f32 - scaled_h as f32) * 0.5).max(0.0);

    let mut dst = match Pixmap::new(out_w, out_h) {
        Some(p) => p,
        None => {
            // Should only hit for zero-sized cells. Return a degenerate
            // bitmap rather than panicking — the atlas handles width=0.
            return RasterizedGlyph {
                bitmap: vec![],
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                is_color: true,
            };
        }
    };

    dst.draw_pixmap(
        x_off as i32,
        y_off as i32,
        src.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..Default::default()
        },
        Transform::from_scale(scale, scale),
        None,
    );

    RasterizedGlyph {
        bitmap: dst.data().to_vec(),
        width: out_w,
        height: out_h,
        bearing_x: -(pad as i32),
        bearing_y: ascent_px as i32,
        is_color: true,
    }
}
