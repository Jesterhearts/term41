//! Custom rasteriser for block drawing, braille, and legacy-computing shapes.
//!
//! Covers:
//! - Block Elements (U+2580–U+259F) — halves, eighths, quadrants, shades
//! - Braille Patterns (U+2800–U+28FF) — all 256 8-dot combinations
//! - Light box-drawing junctions used by DEC Special Graphics
//! - Symbols for Legacy Computing sextants (U+1FB00–U+1FB3B)
//! - SFLC smooth-mosaic wedge/triangle shapes (U+1FB3C–U+1FB67)
//! - SFLC block diagonal quarter/three-quarter fills (U+1FB68–U+1FB6F)
//! - SFLC one-eighth blocks and partial fills (U+1FB70–U+1FB80,
//!   U+1FB82–U+1FB8F)
//! - SFLC shade combos, checkerboards, hatching, diagonal splits, and
//!   triangular medium shade (U+1FB90–U+1FB9F)
//! - SFLC diagonal box-drawing lines and plus/cross (U+1FBA0–U+1FBAF)
//!
//! These codepoints are the building blocks of "pixel art" in the terminal,
//! where users expect adjacent filled cells to abut without visible gaps and
//! dots inside a cell to tile flush with neighbouring cells. Font-supplied
//! glyphs for the same characters typically render dots as circles, reserve
//! empty margins around each cell, or anti-alias the pixel boundaries — all
//! fine for isolated use, ruinous when you're using them as drawing
//! primitives. We side-step the font entirely and fill the exact shapes
//! each codepoint represents.
//!
//! Output contract matches the outline path: alpha coverage in the RGBA
//! bitmap's A byte (RGB = 0), `is_color = false`, bitmap sized to
//! `cell_width × cell_height`, `bearing_x = 0`, `bearing_y = ascent` so the
//! bitmap's top edge sits flush with the cell top.
//!
//! Wiring: [`super::FontSystem::shape_row`] runs a pre-pass that emits a
//! synthetic [`super::ShapedGlyph`] with `font_index = FONT_INDEX` for any
//! single-codepoint cell whose char is in our supported set.
//! [`super::FontSystem::rasterize_glyph`] spots the sentinel and forwards to
//! [`rasterize`]; the atlas key naturally partitions legacy glyphs from
//! font glyphs since `FONT_INDEX` can never collide with a fontdb slot.

use super::RasterizedGlyph;
use crate::downsample_alpha_u8;

/// Sentinel `font_index` marking a shaped glyph that should be rasterised by
/// [`rasterize`] instead of going through a loaded font. Set to `usize::MAX`
/// so it can never collide with a real index in `FONTS`.
pub const FONT_INDEX: usize = usize::MAX;

/// Encode `cell` to a legacy glyph id if it's exactly one codepoint we
/// handle. Multi-codepoint clusters (e.g. a legacy char followed by a
/// variation selector) fall through to normal font shaping so the user's
/// VS intent is preserved.
pub fn encode_single(cell: &str) -> Option<u16> {
    let mut chars = cell.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    encode(c)
}

fn encode(c: char) -> Option<u16> {
    let cp = c as u32;
    match cp {
        0x2500 | 0x2502 | 0x250C | 0x2510 | 0x2514 | 0x2518 | 0x251C | 0x2524 | 0x252C | 0x2534
        | 0x253C => Some(cp as u16),
        0x2580..=0x259F | 0x2800..=0x28FF => Some(cp as u16),
        // SFLC ranges fit into u16 after subtracting 0x10000 (max is 0xFBAF,
        // disjoint from both BMP ranges above). 0x1FB81 is excluded because
        // its dithered "horizontal one eighth block-1358" pattern isn't a
        // pure rectangle and we'd rather defer to whatever the font provides.
        0x1FB00..=0x1FB80 | 0x1FB82..=0x1FBAF => Some((cp - 0x10000) as u16),
        _ => None,
    }
}

fn decode(glyph_id: u16) -> u32 {
    // Range disambiguation: the SFLC shapes encode to 0xFB00..=0xFBAF,
    // disjoint from 0x2580..=0x259F and 0x2800..=0x28FF. A plain threshold
    // check recovers the original plane.
    let v = glyph_id as u32;
    if v >= 0xFB00 { 0x10000 + v } else { v }
}

/// Rasterise a single legacy glyph. `glyph_id` must have come from
/// [`encode_single`]; `cell_width`/`cell_height` are the live (DPI-adjusted)
/// cell dimensions; `ascent_px` is the cell ascent so the bitmap's top edge
/// aligns with the cell's interior top.
pub fn rasterize(
    glyph_id: u16,
    cell_width: u32,
    cell_height: u32,
    ascent_px: f32,
    supersample: u32,
) -> RasterizedGlyph {
    if cell_width == 0 || cell_height == 0 {
        return empty();
    }

    let w = (cell_width * supersample) as usize;
    let h = (cell_height * supersample) as usize;
    let mut alpha = vec![0u8; w * h];

    match decode(glyph_id) {
        cp @ (0x2500 | 0x2502 | 0x250C | 0x2510 | 0x2514 | 0x2518 | 0x251C | 0x2524 | 0x252C
        | 0x2534 | 0x253C) => draw_box_drawing(cp, &mut alpha, w, h),
        cp @ 0x2580..=0x259F => draw_block_element(cp, &mut alpha, w, h),
        cp @ 0x2800..=0x28FF => draw_braille(cp, &mut alpha, w, h),
        cp @ 0x1FB00..=0x1FB3B => draw_sextant(cp, &mut alpha, w, h),
        cp @ 0x1FB3C..=0x1FB67 => draw_wedge(cp, &mut alpha, w, h),
        cp @ 0x1FB68..=0x1FB6F => draw_block_diagonal(cp, &mut alpha, w, h),
        cp @ 0x1FB70..=0x1FB8F => draw_sflc_block(cp, &mut alpha, w, h),
        cp @ 0x1FB90..=0x1FB9F => draw_shade_pattern(cp, &mut alpha, w, h),
        cp @ 0x1FBA0..=0x1FBAF => draw_diagonal_lines(cp, &mut alpha, w, h),
        _ => {}
    }

    // Expand the single-channel alpha buffer to RGBA with RGB=0. The shader
    // reads `.a` on the non-colour path and tints by the cell's foreground
    // colour, matching the outline-glyph contract.
    let mut bitmap = vec![0u8; w * h * 4];
    for (i, a) in alpha.into_iter().enumerate() {
        bitmap[i * 4 + 3] = a;
    }

    let bitmap = downsample_alpha_u8(
        &bitmap,
        w as i32,
        h as i32,
        cell_width as i32,
        cell_height as i32,
        supersample as i32,
    );

    RasterizedGlyph {
        bitmap,
        width: cell_width,
        height: cell_height,
        bearing_x: 0,
        bearing_y: ascent_px.round() as i32,
        is_color: false,
    }
}

fn empty() -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: vec![],
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
        is_color: false,
    }
}

// --- Fill primitives -------------------------------------------------------

fn line_thickness(
    w: usize,
    h: usize,
) -> usize {
    (w.min(h) / 8).max(1)
}

fn center_span(
    total: usize,
    thickness: usize,
) -> (usize, usize) {
    let start = total.saturating_sub(thickness) / 2;
    (start, (start + thickness).min(total))
}

fn draw_box_drawing(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let thickness = line_thickness(w, h);
    let (vx0, vx1) = center_span(w, thickness);
    let (hy0, hy1) = center_span(h, thickness);

    let draw_h_left = |alpha: &mut [u8]| fill_rect(alpha, w, h, 0, hy0, vx1, hy1);
    let draw_h_right = |alpha: &mut [u8]| fill_rect(alpha, w, h, vx0, hy0, w, hy1);
    let draw_h_full = |alpha: &mut [u8]| fill_rect(alpha, w, h, 0, hy0, w, hy1);
    let draw_v_top = |alpha: &mut [u8]| fill_rect(alpha, w, h, vx0, 0, vx1, hy1);
    let draw_v_bottom = |alpha: &mut [u8]| fill_rect(alpha, w, h, vx0, hy0, vx1, h);
    let draw_v_full = |alpha: &mut [u8]| fill_rect(alpha, w, h, vx0, 0, vx1, h);

    match cp {
        0x2500 => draw_h_full(alpha),
        0x2502 => draw_v_full(alpha),
        0x250C => {
            draw_h_right(alpha);
            draw_v_bottom(alpha);
        }
        0x2510 => {
            draw_h_left(alpha);
            draw_v_bottom(alpha);
        }
        0x2514 => {
            draw_h_right(alpha);
            draw_v_top(alpha);
        }
        0x2518 => {
            draw_h_left(alpha);
            draw_v_top(alpha);
        }
        0x251C => {
            draw_h_right(alpha);
            draw_v_full(alpha);
        }
        0x2524 => {
            draw_h_left(alpha);
            draw_v_full(alpha);
        }
        0x252C => {
            draw_h_full(alpha);
            draw_v_bottom(alpha);
        }
        0x2534 => {
            draw_h_full(alpha);
            draw_v_top(alpha);
        }
        0x253C => {
            draw_h_full(alpha);
            draw_v_full(alpha);
        }
        _ => {}
    }
}

fn fill_rect(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) {
    fill_rect_with(alpha, w, h, x0, y0, x1, y1, 0xFF);
}

fn fill_rect_with(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    value: u8,
) {
    let x1 = x1.min(w);
    let y1 = y1.min(h);
    for y in y0..y1 {
        let row = y * w;
        for x in x0..x1 {
            alpha[row + x] = value;
        }
    }
}

/// Pixel boundary for `n`-eighths of `total`, rounded to the nearest integer.
/// Using one rounding rule everywhere means "upper 4/8" and "lower 4/8" meet
/// at the same pixel for any cell dimension — no one-pixel seam when
/// abutting blocks stack.
fn eighths(
    n: usize,
    total: usize,
) -> usize {
    (n * total + 4) / 8
}

/// Signed area of the triangle edge (a→b) evaluated at point p.
/// Positive when p is to the left of a→b, negative to the right.
fn edge_fn(
    a: (f32, f32),
    b: (f32, f32),
    p: (f32, f32),
) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// Fill a triangle defined by three vertices. Uses an edge-function test
/// at each pixel centre — O(w×h) per call, which is fine for the small
/// cell sizes we deal with.
fn fill_tri(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    v0: (f32, f32),
    v1: (f32, f32),
    v2: (f32, f32),
) {
    fill_tri_with(alpha, w, h, v0, v1, v2, 0xFF);
}

fn fill_tri_with(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    v0: (f32, f32),
    v1: (f32, f32),
    v2: (f32, f32),
    value: u8,
) {
    for py in 0..h {
        let cy = py as f32 + 0.5;
        for px in 0..w {
            let cx = px as f32 + 0.5;
            let d0 = edge_fn(v0, v1, (cx, cy));
            let d1 = edge_fn(v1, v2, (cx, cy));
            let d2 = edge_fn(v2, v0, (cx, cy));
            // Inside when all edge functions share the same sign (handles
            // both CW and CCW winding). Boundary pixels (d=0) are included.
            let has_neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
            let has_pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
            if !(has_neg && has_pos) {
                alpha[py * w + px] = value;
            }
        }
    }
}

/// Fill a convex quadrilateral by decomposing into two triangles.
fn fill_quad(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    v0: (f32, f32),
    v1: (f32, f32),
    v2: (f32, f32),
    v3: (f32, f32),
) {
    fill_tri(alpha, w, h, v0, v1, v2);
    fill_tri(alpha, w, h, v0, v2, v3);
}

/// Fill the triangle, then invert the entire alpha buffer so that the
/// triangle region becomes empty and everything else becomes filled.
/// The caller must ensure the buffer is zeroed before this call.
fn complement_tri(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    v0: (f32, f32),
    v1: (f32, f32),
    v2: (f32, f32),
) {
    fill_tri(alpha, w, h, v0, v1, v2);
    for a in alpha.iter_mut() {
        *a = 0xFF - *a;
    }
}

/// Squared distance from point `p` to the line segment `a`→`b`.
fn dist_to_segment_sq(
    p: (f32, f32),
    a: (f32, f32),
    b: (f32, f32),
) -> f32 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-6 {
        let ex = p.0 - a.0;
        let ey = p.1 - a.1;
        return ex * ex + ey * ey;
    }
    let t = ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let ex = p.0 - (a.0 + t * dx);
    let ey = p.1 - (a.1 + t * dy);
    ex * ex + ey * ey
}

/// Draw a thick line segment from `a` to `b`. Each pixel whose centre
/// falls within `thickness / 2` of the segment is set to 0xFF.
fn stroke_line(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    a: (f32, f32),
    b: (f32, f32),
    thickness: f32,
) {
    let half_t_sq = (thickness / 2.0) * (thickness / 2.0);
    for py in 0..h {
        let cy = py as f32 + 0.5;
        for px in 0..w {
            let cx = px as f32 + 0.5;
            if dist_to_segment_sq((cx, cy), a, b) <= half_t_sq {
                alpha[py * w + px] = 0xFF;
            }
        }
    }
}

// --- Block Elements (U+2580–U+259F) ---------------------------------------

fn draw_block_element(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    match cp {
        0x2580 => fill_rect(alpha, w, h, 0, 0, w, eighths(4, h)), // upper half
        0x2581 => fill_rect(alpha, w, h, 0, h - eighths(1, h), w, h), // lower 1/8
        0x2582 => fill_rect(alpha, w, h, 0, h - eighths(2, h), w, h), // lower 2/8
        0x2583 => fill_rect(alpha, w, h, 0, h - eighths(3, h), w, h), // lower 3/8
        0x2584 => fill_rect(alpha, w, h, 0, h - eighths(4, h), w, h), // lower half
        0x2585 => fill_rect(alpha, w, h, 0, h - eighths(5, h), w, h), // lower 5/8
        0x2586 => fill_rect(alpha, w, h, 0, h - eighths(6, h), w, h), // lower 6/8
        0x2587 => fill_rect(alpha, w, h, 0, h - eighths(7, h), w, h), // lower 7/8
        0x2588 => fill_rect(alpha, w, h, 0, 0, w, h),             // full block
        0x2589 => fill_rect(alpha, w, h, 0, 0, eighths(7, w), h), // left 7/8
        0x258A => fill_rect(alpha, w, h, 0, 0, eighths(6, w), h), // left 6/8
        0x258B => fill_rect(alpha, w, h, 0, 0, eighths(5, w), h), // left 5/8
        0x258C => fill_rect(alpha, w, h, 0, 0, eighths(4, w), h), // left half
        0x258D => fill_rect(alpha, w, h, 0, 0, eighths(3, w), h), // left 3/8
        0x258E => fill_rect(alpha, w, h, 0, 0, eighths(2, w), h), // left 2/8
        0x258F => fill_rect(alpha, w, h, 0, 0, eighths(1, w), h), // left 1/8
        0x2590 => fill_rect(alpha, w, h, w - eighths(4, w), 0, w, h), // right half
        // Shades render as a flat sub-255 alpha value. Adjacent shaded
        // cells then abut without any dither seam, and the shader tints
        // them with the foreground colour at the chosen density. A dot-
        // pattern would produce moiré artefacts at small cell sizes.
        0x2591 => fill_rect_with(alpha, w, h, 0, 0, w, h, 0x40), // light 25%
        0x2592 => fill_rect_with(alpha, w, h, 0, 0, w, h, 0x80), // medium 50%
        0x2593 => fill_rect_with(alpha, w, h, 0, 0, w, h, 0xC0), // dark 75%
        0x2594 => fill_rect(alpha, w, h, 0, 0, w, eighths(1, h)), // upper 1/8
        0x2595 => fill_rect(alpha, w, h, w - eighths(1, w), 0, w, h), // right 1/8
        // Quadrants — bit layout: UL=8, UR=4, LL=2, LR=1.
        0x2596 => draw_quadrants(alpha, w, h, 0b0010), // ▖ LL
        0x2597 => draw_quadrants(alpha, w, h, 0b0001), // ▗ LR
        0x2598 => draw_quadrants(alpha, w, h, 0b1000), // ▘ UL
        0x2599 => draw_quadrants(alpha, w, h, 0b1011), // ▙ UL+LL+LR
        0x259A => draw_quadrants(alpha, w, h, 0b1001), // ▚ UL+LR
        0x259B => draw_quadrants(alpha, w, h, 0b1110), // ▛ UL+UR+LL
        0x259C => draw_quadrants(alpha, w, h, 0b1101), // ▜ UL+UR+LR
        0x259D => draw_quadrants(alpha, w, h, 0b0100), // ▝ UR
        0x259E => draw_quadrants(alpha, w, h, 0b0110), // ▞ UR+LL
        0x259F => draw_quadrants(alpha, w, h, 0b0111), // ▟ UR+LL+LR
        _ => {}
    }
}

fn draw_quadrants(
    alpha: &mut [u8],
    w: usize,
    h: usize,
    mask: u8,
) {
    // Each half-boundary rounds toward its own side so the quadrant's
    // outer edge matches the corresponding block-element half on that
    // side. UL/UR's bottom edge matches ▀ (upper half ends at
    // `eighths(4, h)`); LL/LR's top edge matches ▄ (lower half starts at
    // `h - eighths(4, h)`). Same for x: UL/LL's right edge matches ▌,
    // UR/LR's left edge matches ▐. For odd cell dimensions the two
    // halves overlap by one pixel at the midline — harmless (all four
    // quadrants filled still yields a solid full block) and it keeps
    // the outer edges seam-free against all half-block neighbours.
    let mx_top = eighths(4, w);
    let mx_bot = w - eighths(4, w);
    let my_top = eighths(4, h);
    let my_bot = h - eighths(4, h);
    if mask & 0b1000 != 0 {
        // UL
        fill_rect(alpha, w, h, 0, 0, mx_top, my_top);
    }
    if mask & 0b0100 != 0 {
        // UR
        fill_rect(alpha, w, h, mx_bot, 0, w, my_top);
    }
    if mask & 0b0010 != 0 {
        // LL
        fill_rect(alpha, w, h, 0, my_bot, mx_top, h);
    }
    if mask & 0b0001 != 0 {
        // LR
        fill_rect(alpha, w, h, mx_bot, my_bot, w, h);
    }
}

// --- Braille (U+2800–U+28FF) ----------------------------------------------

fn draw_braille(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let bits = (cp - 0x2800) as u8;
    // 2 columns × 4 rows. Dot numbering:
    //   1 4
    //   2 5
    //   3 6
    //   7 8
    // Each dot fills its entire tile so a fully-set braille char
    // (⣿, U+28FF) covers the cell solidly and tiles with its neighbours.
    // Column boundaries use the half-block split (rounds up on the left,
    // down on the right) so a full-cell braille butts seamlessly against
    // ▌/▐ on either side. Row boundaries are the natural four-way split;
    // there is no "quarter" block element to align with, so floor-
    // division is fine and adjacent braille cells match exactly.
    let cols = [[0usize, eighths(4, w)], [w - eighths(4, w), w]];
    let cy = [0, h / 4, h / 2, 3 * h / 4, h];
    // (bit, col, row) for each dot.
    let dots: [(u8, usize, usize); 8] = [
        (0, 0, 0), // dot 1
        (1, 0, 1), // dot 2
        (2, 0, 2), // dot 3
        (3, 1, 0), // dot 4
        (4, 1, 1), // dot 5
        (5, 1, 2), // dot 6
        (6, 0, 3), // dot 7
        (7, 1, 3), // dot 8
    ];
    for (bit, col, row) in dots {
        if bits & (1 << bit) != 0 {
            fill_rect(
                alpha,
                w,
                h,
                cols[col][0],
                cy[row],
                cols[col][1],
                cy[row + 1],
            );
        }
    }
}

// --- Sextants (U+1FB00–U+1FB3B) -------------------------------------------

/// Return the 6-bit sextant mask for a codepoint in U+1FB00..=U+1FB3B.
/// The block enumerates the 60 non-trivial subsets of {1..6} in numeric
/// order; subsets 0 (empty → space), 21 (left half → U+258C),
/// 42 (right half → U+2590), and 63 (full → U+2588) are already encoded
/// elsewhere and get skipped in the sextant sequence.
fn sextant_subset(cp: u32) -> u8 {
    let idx = (cp - 0x1FB00) as u8;
    let mut count = 0u8;
    let mut value = 1u8;
    while value < 63 {
        if value != 21 && value != 42 {
            if count == idx {
                return value;
            }
            count += 1;
        }
        value += 1;
    }
    0
}

fn draw_sextant(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let mask = sextant_subset(cp);
    // x-midline uses the half-block split: left column rounds up to
    // `eighths(4, w)` (matching ▌), right column starts at the complement
    // `w - eighths(4, w)` (matching ▐). On odd widths the two columns
    // overlap by one pixel, which is exactly what we need for a sextant
    // cell to sit flush against its block-element neighbours on both
    // sides. y boundaries are thirds (no block-element equivalent), so
    // plain floor-division is fine and adjacent sextants stack identically.
    let cols = [[0usize, eighths(4, w)], [w - eighths(4, w), w]];
    let ys = [0, h / 3, 2 * h / 3, h];
    // Positions → bit index:
    //   1 2     bits 0 1
    //   3 4           2 3
    //   5 6           4 5
    for row in 0..3 {
        #[allow(clippy::needless_range_loop)]
        for col in 0..2 {
            let bit = row * 2 + col;
            if mask & (1 << bit) != 0 {
                fill_rect(
                    alpha,
                    w,
                    h,
                    cols[col][0],
                    ys[row],
                    cols[col][1],
                    ys[row + 1],
                );
            }
        }
    }
}

// --- SFLC 1/8 blocks (U+1FB70–U+1FB80, U+1FB82–U+1FB8F) -------------------

fn draw_sflc_block(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    match cp {
        // VERTICAL ONE EIGHTH BLOCK-N (N=2..=7): column N of 8 from the
        // left. N=1 and N=8 are U+258F and U+2595 respectively.
        0x1FB70 => fill_rect(alpha, w, h, eighths(1, w), 0, eighths(2, w), h),
        0x1FB71 => fill_rect(alpha, w, h, eighths(2, w), 0, eighths(3, w), h),
        0x1FB72 => fill_rect(alpha, w, h, eighths(3, w), 0, eighths(4, w), h),
        0x1FB73 => fill_rect(alpha, w, h, eighths(4, w), 0, eighths(5, w), h),
        0x1FB74 => fill_rect(alpha, w, h, eighths(5, w), 0, eighths(6, w), h),
        0x1FB75 => fill_rect(alpha, w, h, eighths(6, w), 0, eighths(7, w), h),
        // HORIZONTAL ONE EIGHTH BLOCK-N (N=2..=7): row N of 8 from the top.
        // N=1 is U+2594, N=8 is U+2581.
        0x1FB76 => fill_rect(alpha, w, h, 0, eighths(1, h), w, eighths(2, h)),
        0x1FB77 => fill_rect(alpha, w, h, 0, eighths(2, h), w, eighths(3, h)),
        0x1FB78 => fill_rect(alpha, w, h, 0, eighths(3, h), w, eighths(4, h)),
        0x1FB79 => fill_rect(alpha, w, h, 0, eighths(4, h), w, eighths(5, h)),
        0x1FB7A => fill_rect(alpha, w, h, 0, eighths(5, h), w, eighths(6, h)),
        0x1FB7B => fill_rect(alpha, w, h, 0, eighths(6, h), w, eighths(7, h)),
        // L-shaped corner frames: a 1/8-wide column abutting a 1/8-tall row.
        0x1FB7C => {
            // LEFT AND LOWER
            fill_rect(alpha, w, h, 0, 0, eighths(1, w), h);
            fill_rect(alpha, w, h, 0, h - eighths(1, h), w, h);
        }
        0x1FB7D => {
            // LEFT AND UPPER
            fill_rect(alpha, w, h, 0, 0, eighths(1, w), h);
            fill_rect(alpha, w, h, 0, 0, w, eighths(1, h));
        }
        0x1FB7E => {
            // RIGHT AND UPPER
            fill_rect(alpha, w, h, w - eighths(1, w), 0, w, h);
            fill_rect(alpha, w, h, 0, 0, w, eighths(1, h));
        }
        0x1FB7F => {
            // RIGHT AND LOWER
            fill_rect(alpha, w, h, w - eighths(1, w), 0, w, h);
            fill_rect(alpha, w, h, 0, h - eighths(1, h), w, h);
        }
        // Top + bottom 1/8 stripes (framing a hollow middle).
        0x1FB80 => {
            fill_rect(alpha, w, h, 0, 0, w, eighths(1, h));
            fill_rect(alpha, w, h, 0, h - eighths(1, h), w, h);
        }
        // UPPER N/8 BLOCK (N=2,3,5,6,7). N=1 is U+2594, N=4 is U+2580, N=8
        // is U+2588. The block numbers the fractions in quarters/eighths
        // informally; we just fill the top N/8.
        0x1FB82 => fill_rect(alpha, w, h, 0, 0, w, eighths(2, h)),
        0x1FB83 => fill_rect(alpha, w, h, 0, 0, w, eighths(3, h)),
        0x1FB84 => fill_rect(alpha, w, h, 0, 0, w, eighths(5, h)),
        0x1FB85 => fill_rect(alpha, w, h, 0, 0, w, eighths(6, h)),
        0x1FB86 => fill_rect(alpha, w, h, 0, 0, w, eighths(7, h)),
        // RIGHT N/8 BLOCK (N=2,3,5,6,7). N=1 is U+2595, N=4 is U+2590.
        0x1FB87 => fill_rect(alpha, w, h, w - eighths(2, w), 0, w, h),
        0x1FB88 => fill_rect(alpha, w, h, w - eighths(3, w), 0, w, h),
        0x1FB89 => fill_rect(alpha, w, h, w - eighths(5, w), 0, w, h),
        0x1FB8A => fill_rect(alpha, w, h, w - eighths(6, w), 0, w, h),
        0x1FB8B => fill_rect(alpha, w, h, w - eighths(7, w), 0, w, h),
        // Shaded halves: same geometry as 0x258C/0x2590/0x2580/0x2584 but
        // at medium (50%) density. The shader tints by fg colour so these
        // appear as a half-cell rectangle at half intensity.
        0x1FB8C => fill_rect_with(alpha, w, h, 0, 0, eighths(4, w), h, 0x80),
        0x1FB8D => fill_rect_with(alpha, w, h, w - eighths(4, w), 0, w, h, 0x80),
        0x1FB8E => fill_rect_with(alpha, w, h, 0, 0, w, eighths(4, h), 0x80),
        0x1FB8F => fill_rect_with(alpha, w, h, 0, h - eighths(4, h), w, h, 0x80),
        _ => {}
    }
}

// --- Smooth-mosaic wedge shapes (U+1FB3C–U+1FB67) -------------------------
//
// 44 characters: 20 direct triangles (5 per corner), 20 complement shapes
// (full cell minus a triangle from the opposite corner), and 4 diagonal-band
// trapezoids. Reference grid uses the sextant 2×3 intersections:
//   x: 0, w/2, w    y: 0, h/3, 2h/3, h

fn draw_wedge(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let wf = w as f32;
    let hf = h as f32;
    let xm = wf / 2.0;
    let y1 = hf / 3.0;
    let y2 = 2.0 * hf / 3.0;

    match cp {
        // Lower-left triangles — diagonal from left edge to bottom edge,
        // filled below and left.
        0x1FB3C => fill_tri(alpha, w, h, (0.0, y2), (0.0, hf), (xm, hf)),
        0x1FB3D => fill_tri(alpha, w, h, (0.0, y2), (0.0, hf), (wf, hf)),
        0x1FB3E => fill_tri(alpha, w, h, (0.0, y1), (0.0, hf), (xm, hf)),
        0x1FB3F => fill_tri(alpha, w, h, (0.0, y1), (0.0, hf), (wf, hf)),
        0x1FB40 => fill_tri(alpha, w, h, (0.0, 0.0), (0.0, hf), (xm, hf)),
        // Lower-left complements (full cell minus upper-left triangles).
        0x1FB41 => complement_tri(alpha, w, h, (0.0, 0.0), (xm, 0.0), (0.0, y1)),
        0x1FB42 => complement_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (0.0, y1)),
        0x1FB43 => complement_tri(alpha, w, h, (0.0, 0.0), (xm, 0.0), (0.0, y2)),
        0x1FB44 => complement_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (0.0, y2)),
        0x1FB45 => complement_tri(alpha, w, h, (0.0, 0.0), (xm, 0.0), (0.0, hf)),
        // Lower-left diagonal band (trapezoid).
        0x1FB46 => fill_quad(alpha, w, h, (0.0, y2), (wf, y1), (wf, hf), (0.0, hf)),

        // Lower-right triangles.
        0x1FB47 => fill_tri(alpha, w, h, (xm, hf), (wf, y2), (wf, hf)),
        0x1FB48 => fill_tri(alpha, w, h, (0.0, hf), (wf, y2), (wf, hf)),
        0x1FB49 => fill_tri(alpha, w, h, (xm, hf), (wf, y1), (wf, hf)),
        0x1FB4A => fill_tri(alpha, w, h, (0.0, hf), (wf, y1), (wf, hf)),
        0x1FB4B => fill_tri(alpha, w, h, (xm, hf), (wf, 0.0), (wf, hf)),
        // Lower-right complements (full cell minus upper-right triangles).
        0x1FB4C => complement_tri(alpha, w, h, (xm, 0.0), (wf, 0.0), (wf, y1)),
        0x1FB4D => complement_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, y1)),
        0x1FB4E => complement_tri(alpha, w, h, (xm, 0.0), (wf, 0.0), (wf, y2)),
        0x1FB4F => complement_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, y2)),
        0x1FB50 => complement_tri(alpha, w, h, (xm, 0.0), (wf, 0.0), (wf, hf)),
        // Lower-right diagonal band.
        0x1FB51 => fill_quad(alpha, w, h, (0.0, y1), (wf, y2), (wf, hf), (0.0, hf)),

        // Upper-right complements (full cell minus lower-left triangles).
        0x1FB52 => complement_tri(alpha, w, h, (0.0, y2), (0.0, hf), (xm, hf)),
        0x1FB53 => complement_tri(alpha, w, h, (0.0, y2), (0.0, hf), (wf, hf)),
        0x1FB54 => complement_tri(alpha, w, h, (0.0, y1), (0.0, hf), (xm, hf)),
        0x1FB55 => complement_tri(alpha, w, h, (0.0, y1), (0.0, hf), (wf, hf)),
        0x1FB56 => complement_tri(alpha, w, h, (0.0, 0.0), (0.0, hf), (xm, hf)),

        // Upper-left triangles — diagonal from left edge to top edge.
        0x1FB57 => fill_tri(alpha, w, h, (0.0, 0.0), (xm, 0.0), (0.0, y1)),
        0x1FB58 => fill_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (0.0, y1)),
        0x1FB59 => fill_tri(alpha, w, h, (0.0, 0.0), (xm, 0.0), (0.0, y2)),
        0x1FB5A => fill_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (0.0, y2)),
        0x1FB5B => fill_tri(alpha, w, h, (0.0, 0.0), (xm, 0.0), (0.0, hf)),
        // Upper-left diagonal band.
        0x1FB5C => fill_quad(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, y1), (0.0, y2)),

        // Upper-left complements (full cell minus lower-right triangles).
        0x1FB5D => complement_tri(alpha, w, h, (xm, hf), (wf, y2), (wf, hf)),
        0x1FB5E => complement_tri(alpha, w, h, (0.0, hf), (wf, y2), (wf, hf)),
        0x1FB5F => complement_tri(alpha, w, h, (xm, hf), (wf, y1), (wf, hf)),
        0x1FB60 => complement_tri(alpha, w, h, (0.0, hf), (wf, y1), (wf, hf)),
        0x1FB61 => complement_tri(alpha, w, h, (xm, hf), (wf, 0.0), (wf, hf)),

        // Upper-right triangles — diagonal from top edge to right edge.
        0x1FB62 => fill_tri(alpha, w, h, (xm, 0.0), (wf, 0.0), (wf, y1)),
        0x1FB63 => fill_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, y1)),
        0x1FB64 => fill_tri(alpha, w, h, (xm, 0.0), (wf, 0.0), (wf, y2)),
        0x1FB65 => fill_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, y2)),
        0x1FB66 => fill_tri(alpha, w, h, (xm, 0.0), (wf, 0.0), (wf, hf)),
        // Upper-right diagonal band.
        0x1FB67 => fill_quad(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, y2), (0.0, y1)),

        _ => {}
    }
}

// --- Block diagonal shapes (U+1FB68–U+1FB6F) ------------------------------
//
// The cell is divided into 4 triangles meeting at the centre point (w/2, h/2):
//   LEFT  = top-left → centre → bottom-left
//   UPPER = top-left → top-right → centre
//   RIGHT = top-right → bottom-right → centre
//   LOWER = bottom-left → bottom-right → centre
//
// 0x1FB6C–6F draw one quarter; 0x1FB68–6B draw the three-quarter complement.

fn draw_block_diagonal(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let wf = w as f32;
    let hf = h as f32;
    let cx = wf / 2.0;
    let cy = hf / 2.0;

    match cp {
        // Quarter triangles (direct).
        0x1FB6C => fill_tri(alpha, w, h, (0.0, 0.0), (cx, cy), (0.0, hf)),
        0x1FB6D => fill_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (cx, cy)),
        0x1FB6E => fill_tri(alpha, w, h, (wf, 0.0), (wf, hf), (cx, cy)),
        0x1FB6F => fill_tri(alpha, w, h, (0.0, hf), (wf, hf), (cx, cy)),
        // Three-quarter complements.
        0x1FB68 => complement_tri(alpha, w, h, (0.0, 0.0), (cx, cy), (0.0, hf)),
        0x1FB69 => complement_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (cx, cy)),
        0x1FB6A => complement_tri(alpha, w, h, (wf, 0.0), (wf, hf), (cx, cy)),
        0x1FB6B => complement_tri(alpha, w, h, (0.0, hf), (wf, hf), (cx, cy)),
        _ => {}
    }
}

// --- Shade patterns and diagonal splits (U+1FB90–U+1FB9F) -----------------

fn draw_shade_pattern(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let wf = w as f32;
    let hf = h as f32;
    let xm = wf / 2.0;
    let ym = hf / 2.0;

    match cp {
        // Inverse medium shade — same density as U+2592 but semantically
        // inverted by the terminal's fg/bg swap. We emit the same alpha.
        0x1FB90 => fill_rect_with(alpha, w, h, 0, 0, w, h, 0x80),
        // Mixed shade halves: one half solid, the other half medium shade.
        0x1FB91 => {
            let mid = eighths(4, h);
            fill_rect(alpha, w, h, 0, 0, w, mid);
            fill_rect_with(alpha, w, h, 0, mid, w, h, 0x80);
        }
        0x1FB92 => {
            let mid = eighths(4, h);
            fill_rect_with(alpha, w, h, 0, 0, w, mid, 0x80);
            fill_rect(alpha, w, h, 0, mid, w, h);
        }
        0x1FB93 => {
            let mid = eighths(4, w);
            fill_rect(alpha, w, h, 0, 0, mid, h);
            fill_rect_with(alpha, w, h, mid, 0, w, h, 0x80);
        }
        0x1FB94 => {
            let mid = eighths(4, w);
            fill_rect_with(alpha, w, h, 0, 0, mid, h, 0x80);
            fill_rect(alpha, w, h, mid, 0, w, h);
        }
        // Checkerboard fills — 1-pixel alternating pattern at full alpha.
        0x1FB95 => {
            for py in 0..h {
                for px in 0..w {
                    if (px + py) % 2 == 0 {
                        alpha[py * w + px] = 0xFF;
                    }
                }
            }
        }
        0x1FB96 => {
            for py in 0..h {
                for px in 0..w {
                    if (px + py) % 2 != 0 {
                        alpha[py * w + px] = 0xFF;
                    }
                }
            }
        }
        // Heavy horizontal fill — 2-on / 2-off horizontal stripes.
        0x1FB97 => {
            for py in 0..h {
                if (py / 2) % 2 == 1 {
                    for px in 0..w {
                        alpha[py * w + px] = 0xFF;
                    }
                }
            }
        }
        // Diagonal hatching fills.
        0x1FB98 => {
            // UL-to-LR stripes: perpendicular coordinate is (x + y).
            for py in 0..h {
                for px in 0..w {
                    if (px + py) % 4 < 2 {
                        alpha[py * w + px] = 0xFF;
                    }
                }
            }
        }
        0x1FB99 => {
            // UR-to-LL stripes: perpendicular coordinate is (x − y).
            for py in 0..h {
                for px in 0..w {
                    if (px as isize - py as isize).rem_euclid(4) < 2 {
                        alpha[py * w + px] = 0xFF;
                    }
                }
            }
        }
        // Diagonal half blocks — two opposite quarter-triangles meeting
        // at the cell centre, forming a bowtie / hourglass shape.
        0x1FB9A => {
            // Upper + lower.
            fill_tri(alpha, w, h, (0.0, 0.0), (wf, 0.0), (xm, ym));
            fill_tri(alpha, w, h, (0.0, hf), (wf, hf), (xm, ym));
        }
        0x1FB9B => {
            // Left + right.
            fill_tri(alpha, w, h, (0.0, 0.0), (xm, ym), (0.0, hf));
            fill_tri(alpha, w, h, (wf, 0.0), (wf, hf), (xm, ym));
        }
        // Triangular medium shade — half-cell triangle (corner-to-corner
        // diagonal) at 50% alpha.
        0x1FB9C => fill_tri_with(alpha, w, h, (0.0, 0.0), (wf, 0.0), (0.0, hf), 0x80),
        0x1FB9D => fill_tri_with(alpha, w, h, (0.0, 0.0), (wf, 0.0), (wf, hf), 0x80),
        0x1FB9E => fill_tri_with(alpha, w, h, (wf, 0.0), (wf, hf), (0.0, hf), 0x80),
        0x1FB9F => fill_tri_with(alpha, w, h, (0.0, 0.0), (0.0, hf), (wf, hf), 0x80),
        _ => {}
    }
}

// --- Diagonal box-drawing lines (U+1FBA0–U+1FBAF) -------------------------
//
// Four possible line segments connecting mid-edge points, encoded as a
// bitmask:  bit 0 = UL (top-centre → left-centre),
//           bit 1 = UR (top-centre → right-centre),
//           bit 2 = LL (left-centre → bottom-centre),
//           bit 3 = LR (right-centre → bottom-centre).

fn draw_diagonal_lines(
    cp: u32,
    alpha: &mut [u8],
    w: usize,
    h: usize,
) {
    let wf = w as f32;
    let hf = h as f32;
    let xm = wf / 2.0;
    let ym = hf / 2.0;

    // U+1FBAF is a plus/cross (horizontal + vertical through centre).
    // Use fill_rect for pixel-perfect axis-aligned strokes.
    if cp == 0x1FBAF {
        let lw = ((wf.min(hf) / 8.0).round() as usize).max(1);
        let vx = (w - lw) / 2;
        let hy = (h - lw) / 2;
        fill_rect(alpha, w, h, vx, 0, vx + lw, h);
        fill_rect(alpha, w, h, 0, hy, w, hy + lw);
        return;
    }

    let mask: u8 = match cp {
        0x1FBA0 => 0b0001,
        0x1FBA1 => 0b0010,
        0x1FBA2 => 0b0100,
        0x1FBA3 => 0b1000,
        0x1FBA4 => 0b0101,
        0x1FBA5 => 0b1010,
        0x1FBA6 => 0b1100,
        0x1FBA7 => 0b0011,
        0x1FBA8 => 0b1001,
        0x1FBA9 => 0b0110,
        0x1FBAA => 0b1110,
        0x1FBAB => 0b1101,
        0x1FBAC => 0b1011,
        0x1FBAD => 0b0111,
        0x1FBAE => 0b1111,
        _ => return,
    };

    let thickness = (wf.min(hf) / 8.0).max(1.0);
    let tc = (xm, 0.0); // top-centre
    let lc = (0.0, ym); // left-centre
    let rc = (wf, ym); // right-centre
    let bc = (xm, hf); // bottom-centre

    if mask & 0b0001 != 0 {
        stroke_line(alpha, w, h, tc, lc, thickness);
    }
    if mask & 0b0010 != 0 {
        stroke_line(alpha, w, h, tc, rc, thickness);
    }
    if mask & 0b0100 != 0 {
        stroke_line(alpha, w, h, lc, bc, thickness);
    }
    if mask & 0b1000 != 0 {
        stroke_line(alpha, w, h, rc, bc, thickness);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_round_trip() {
        for cp in [
            0x2580u32, 0x258C, 0x259F, 0x2800, 0x283F, 0x28FF, 0x1FB00, 0x1FB3B, 0x1FB70, 0x1FB8F,
        ] {
            let c = char::from_u32(cp).unwrap();
            let g = encode(c).expect("encode");
            assert_eq!(decode(g), cp, "round trip for U+{cp:04X}");
        }
    }

    #[test]
    fn rejects_non_legacy_and_clusters() {
        assert_eq!(encode_single("a"), None);
        assert_eq!(encode_single("█a"), None); // multi-char cluster
        assert_eq!(encode_single(""), None);
        assert_eq!(encode_single("\u{1FB81}"), None); // intentionally skipped
    }

    #[test]
    fn full_block_fills_entire_cell() {
        let g = rasterize(0x2588, 8, 12, 10.0, 1);
        assert_eq!(g.bitmap.len(), 8 * 12 * 4);
        for chunk in g.bitmap.chunks_exact(4) {
            assert_eq!(chunk[3], 0xFF);
        }
    }

    #[test]
    fn all_dot_braille_fills_entire_cell() {
        // U+28FF is the braille pattern with every dot set; when drawn
        // as solid tiles it must cover the whole cell so adjacent
        // full-braille cells form one continuous block.
        let g = rasterize(0x28FF, 8, 12, 10.0, 1);
        for chunk in g.bitmap.chunks_exact(4) {
            assert_eq!(chunk[3], 0xFF, "expected full coverage for U+28FF");
        }
    }

    #[test]
    fn empty_braille_has_no_coverage() {
        let g = rasterize(0x2800, 8, 12, 10.0, 1);
        for chunk in g.bitmap.chunks_exact(4) {
            assert_eq!(chunk[3], 0);
        }
    }

    #[test]
    fn quadrants_split_the_cell() {
        // ▞ = UR + LL. The two diagonally-opposite quadrants each cover
        // exactly w/2 × h/2 pixels, so fully-set coverage is half the cell.
        let w = 10u32;
        let h = 12u32;
        let g = rasterize(0x259E, w, h, 10.0, 1);
        let set: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0xFF).count();
        assert_eq!(set, 2 * (w as usize / 2) * (h as usize / 2));
    }

    #[test]
    fn shade_midlevel_alpha() {
        // Medium shade fills every pixel at alpha 0x80 — a flat density
        // level that tints uniformly via the fg colour.
        let g = rasterize(0x2592, 8, 12, 10.0, 1);
        for chunk in g.bitmap.chunks_exact(4) {
            assert_eq!(chunk[3], 0x80);
        }
    }

    #[test]
    fn sextant_subset_boundaries() {
        // First, mid (just past the 21 skip), and last entries.
        assert_eq!(sextant_subset(0x1FB00), 1);
        assert_eq!(sextant_subset(0x1FB00 + 20), 22); // skipped 21
        assert_eq!(sextant_subset(0x1FB00 + 40), 43); // skipped 42
        assert_eq!(sextant_subset(0x1FB3B), 62); // last before 63
    }

    #[test]
    fn full_sextant_fills_near_full_cell() {
        // Sextant subset 62 = 0b111110 has 5 of 6 tiles filled; bit 0 is
        // clear so the empty tile is position 1 (row 0, col 0 = top-left).
        // The bottom-right tile (bit 5) is set.
        let w = 8u32;
        let h = 12u32;
        let g = rasterize(0xFB3B, w, h, 10.0, 1);
        let idx = |x: u32, y: u32| ((y * w + x) * 4 + 3) as usize;
        assert_eq!(g.bitmap[idx(0, 0)], 0, "top-left empty");
        assert_eq!(g.bitmap[idx(w - 1, h - 1)], 0xFF, "bottom-right covered");
    }

    #[test]
    fn quadrant_outer_edges_match_half_blocks_on_every_side() {
        // Regression: with a single `eighths(4, _)` boundary for both
        // halves of a quadrant char, the upper/left halves lined up with
        // ▀/▌ but the lower/right halves fell one pixel short of ▄/▐ at
        // odd cell dimensions. Check each corner character against the
        // half-block stripe it's supposed to abut. Probe at a column or
        // row where only ONE of the quadrant's three quadrants is set so
        // we can isolate that edge's boundary.
        let w = 9u32; // odd
        let h = 13u32; // odd
        let cov = |g: &RasterizedGlyph, x: u32, y: u32| -> bool {
            g.bitmap[((y * w + x) * 4 + 3) as usize] == 0xFF
        };

        let my_top = eighths(4, h as usize) as u32; // bottom of upper stripe
        let my_bot = h - my_top; // top of lower stripe
        let mx_top = eighths(4, w as usize) as u32; // right of left stripe
        let mx_bot = w - mx_top; // left of right stripe

        // Half blocks: first check the canonical stripe boundaries hold.
        let top = rasterize(0x2580, w, h, 10.0, 1);
        let bot = rasterize(0x2584, w, h, 10.0, 1);
        let left = rasterize(0x258C, w, h, 10.0, 1);
        let right = rasterize(0x2590, w, h, 10.0, 1);
        assert!(cov(&top, 0, my_top - 1) && !cov(&top, 0, my_top));
        assert!(cov(&bot, 0, my_bot) && !cov(&bot, 0, my_bot - 1));
        assert!(cov(&left, mx_top - 1, 0) && !cov(&left, mx_top, 0));
        assert!(cov(&right, mx_bot, 0) && !cov(&right, mx_bot - 1, 0));

        // ▛ UL+UR+LL — right column isolates UR (top stripe).
        let tl = rasterize(0x259B, w, h, 10.0, 1);
        assert!(cov(&tl, w - 1, my_top - 1), "▛ UR fills through top_end");
        assert!(!cov(&tl, w - 1, my_top), "▛ UR stops at top_end");
        // Bottom row isolates LL (left stripe).
        assert!(cov(&tl, mx_top - 1, h - 1), "▛ LL fills through left_end");
        assert!(!cov(&tl, mx_top, h - 1), "▛ LL stops at left_end");

        // ▜ UL+UR+LR — left column isolates UL (top stripe).
        let tr = rasterize(0x259C, w, h, 10.0, 1);
        assert!(cov(&tr, 0, my_top - 1), "▜ UL fills through top_end");
        assert!(!cov(&tr, 0, my_top), "▜ UL stops at top_end");
        // Bottom row isolates LR (right stripe).
        assert!(cov(&tr, mx_bot, h - 1), "▜ LR starts at right_start");
        assert!(!cov(&tr, mx_bot - 1, h - 1), "▜ LR gap before right_start");

        // ▙ UL+LL+LR — top row isolates UL (left stripe).
        let bl = rasterize(0x2599, w, h, 10.0, 1);
        assert!(cov(&bl, mx_top - 1, 0), "▙ UL fills through left_end");
        assert!(!cov(&bl, mx_top, 0), "▙ UL stops at left_end");
        // Right column isolates LR (bottom stripe).
        assert!(cov(&bl, w - 1, my_bot), "▙ LR starts at bot_start");
        assert!(!cov(&bl, w - 1, my_bot - 1), "▙ LR gap before bot_start");

        // ▟ UR+LL+LR — top row isolates UR (right stripe).
        let br = rasterize(0x259F, w, h, 10.0, 1);
        assert!(cov(&br, mx_bot, 0), "▟ UR starts at right_start");
        assert!(!cov(&br, mx_bot - 1, 0), "▟ UR gap before right_start");
        // Left column isolates LL (bottom stripe).
        assert!(cov(&br, 0, my_bot), "▟ LL starts at bot_start");
        assert!(!cov(&br, 0, my_bot - 1), "▟ LL gap before bot_start");
    }

    #[test]
    fn quadrant_meets_half_block_without_seam() {
        // Regression: at odd cell dimensions, using h/2 for the quadrant
        // midline while block elements use eighths(4,h) produced a 1-pixel
        // notch at the inside corner where ▛ abuts ▀ (and the symmetric
        // cases at the top-right and bottom-left of a rectangle outline).
        // Both sides must terminate their "top half" at the same y row so
        // the outline's bottom edge is straight across the seam.
        let w = 9u32;
        let h = 13u32; // deliberately odd
        let q = rasterize(0x259B, w, h, 10.0, 1); // ▛ UL+UR+LL
        let hb = rasterize(0x2580, w, h, 10.0, 1); // ▀ upper half

        // Last filled y on the right column of ▛ (UR quadrant) must equal
        // last filled y anywhere in ▀ (the whole cell shares the same top
        // half). Coverage shape for both is rows [0, eighths(4, h)).
        let last_filled_ur = (0..h as usize)
            .rev()
            .find(|&y| q.bitmap[((y as u32 * w + (w - 1)) * 4 + 3) as usize] == 0xFF)
            .unwrap();
        let last_filled_hb = (0..h as usize)
            .rev()
            .find(|&y| hb.bitmap[((y as u32 * w) * 4 + 3) as usize] == 0xFF)
            .unwrap();
        assert_eq!(
            last_filled_ur, last_filled_hb,
            "▛ and ▀ must end their top-half at the same row"
        );
    }

    #[test]
    fn vertical_one_eighth_blocks_tile_seamlessly() {
        // U+258F (left 1/8) + U+1FB70 (column 2) + U+1FB71 (column 3) +
        // U+1FB72 (column 4) must together cover [0, 4/8) of the width
        // when placed in four adjacent cells. Within one cell their
        // boundaries hand off cleanly: eighths(1) == eighths(1) start of
        // next cell's column-2 tile.
        let w = 16usize;
        let h = 12u32;
        let g1 = rasterize(0x258F, w as u32, h, 10.0, 1);
        let g2 = rasterize(0xFB70, w as u32, h, 10.0, 1);
        // Column where g1 ends must equal column where g2 begins.
        let g1_end = (0..w)
            .rposition(|x| g1.bitmap[(x * 4) + 3] == 0xFF)
            .unwrap()
            + 1;
        let g2_start = (0..w).position(|x| g2.bitmap[(x * 4) + 3] == 0xFF).unwrap();
        assert_eq!(
            g1_end, g2_start,
            "left-1/8 ends at x={g1_end}, column-2 starts at x={g2_start}"
        );
    }

    // --- Wedge / diagonal / new-shape tests --------------------------------

    #[test]
    fn encode_round_trip_extended() {
        for cp in [
            0x1FB3Cu32, 0x1FB67, 0x1FB68, 0x1FB6F, 0x1FB90, 0x1FB93, 0x1FB9F, 0x1FBA0, 0x1FBAF,
        ] {
            let c = char::from_u32(cp).unwrap();
            let g = encode(c).expect("encode");
            assert_eq!(decode(g), cp, "round trip for U+{cp:04X}");
        }
    }

    #[test]
    fn complement_plus_triangle_fills_cell() {
        // U+1FB3C (small lower-left triangle) and U+1FB52 (its complement)
        // together must cover every pixel exactly once.
        let w = 8u32;
        let h = 12u32;
        let tri = rasterize(0xFB3C, w, h, 10.0, 1);
        let comp = rasterize(0xFB52, w, h, 10.0, 1);
        for i in 0..(w * h) as usize {
            let ta = tri.bitmap[i * 4 + 3];
            let ca = comp.bitmap[i * 4 + 3];
            assert_eq!(
                ta as u16 + ca as u16,
                0xFF,
                "pixel {i}: triangle {ta} + complement {ca} ≠ 0xFF"
            );
        }
    }

    #[test]
    fn all_wedge_complement_pairs_partition_cell() {
        // Every direct/complement pair must sum to 0xFF at every pixel.
        let pairs: [(u16, u16); 20] = [
            // lower-left ↔ upper-right
            (0xFB3C, 0xFB52),
            (0xFB3D, 0xFB53),
            (0xFB3E, 0xFB54),
            (0xFB3F, 0xFB55),
            (0xFB40, 0xFB56),
            // lower-right ↔ upper-left (of lower-right)
            (0xFB47, 0xFB5D),
            (0xFB48, 0xFB5E),
            (0xFB49, 0xFB5F),
            (0xFB4A, 0xFB60),
            (0xFB4B, 0xFB61),
            // upper-left ↔ lower-left complement
            (0xFB57, 0xFB41),
            (0xFB58, 0xFB42),
            (0xFB59, 0xFB43),
            (0xFB5A, 0xFB44),
            (0xFB5B, 0xFB45),
            // upper-right ↔ lower-right complement
            (0xFB62, 0xFB4C),
            (0xFB63, 0xFB4D),
            (0xFB64, 0xFB4E),
            (0xFB65, 0xFB4F),
            (0xFB66, 0xFB50),
        ];
        let w = 10u32;
        let h = 15u32;
        for (a_id, b_id) in pairs {
            let a = rasterize(a_id, w, h, 12.0, 1);
            let b = rasterize(b_id, w, h, 12.0, 1);
            for i in 0..(w * h) as usize {
                assert_eq!(
                    a.bitmap[i * 4 + 3] as u16 + b.bitmap[i * 4 + 3] as u16,
                    0xFF,
                    "pair ({a_id:#06X}, {b_id:#06X}) pixel {i}"
                );
            }
        }
    }

    #[test]
    fn block_diagonal_quarter_fills_quarter_cell() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0xFB6C, w, h, 20.0, 1); // LEFT quarter
        let filled: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0xFF).count();
        let total = (w * h) as usize;
        let quarter = total / 4;
        assert!(
            filled > quarter * 80 / 100 && filled < quarter * 120 / 100,
            "left quarter: filled {filled}, expected ~{quarter}"
        );
    }

    #[test]
    fn block_diagonal_quarter_and_complement_partition() {
        let w = 12u32;
        let h = 18u32;
        // LEFT quarter (0x1FB6C) + its complement (0x1FB68).
        let q = rasterize(0xFB6C, w, h, 14.0, 1);
        let c = rasterize(0xFB68, w, h, 14.0, 1);
        for i in 0..(w * h) as usize {
            assert_eq!(
                q.bitmap[i * 4 + 3] as u16 + c.bitmap[i * 4 + 3] as u16,
                0xFF,
                "quarter/complement pixel {i}"
            );
        }
    }

    #[test]
    fn diagonal_half_blocks_are_bowties() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0xFB9A, w, h, 20.0, 1); // upper + lower
        let filled: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0xFF).count();
        let total = (w * h) as usize;
        let half = total / 2;
        assert!(
            filled > half * 80 / 100 && filled < half * 120 / 100,
            "bowtie: filled {filled}, expected ~{half}"
        );
    }

    #[test]
    fn checkerboard_is_half_filled() {
        let w = 8u32;
        let h = 12u32;
        let g = rasterize(0xFB95, w, h, 10.0, 1);
        let filled: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0xFF).count();
        assert_eq!(filled, (w * h / 2) as usize);
    }

    #[test]
    fn inverse_checkerboard_complements_checkerboard() {
        let w = 8u32;
        let h = 12u32;
        let a = rasterize(0xFB95, w, h, 10.0, 1);
        let b = rasterize(0xFB96, w, h, 10.0, 1);
        for i in 0..(w * h) as usize {
            assert_eq!(
                a.bitmap[i * 4 + 3] as u16 + b.bitmap[i * 4 + 3] as u16,
                0xFF,
                "checkerboard pixel {i}"
            );
        }
    }

    #[test]
    fn diagonal_line_produces_output() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0xFBA0, w, h, 20.0, 1); // single diagonal segment
        let any_filled = g.bitmap.chunks_exact(4).any(|c| c[3] > 0);
        assert!(any_filled, "diagonal line should produce visible output");
    }

    #[test]
    fn all_four_diagonal_lines_fill_diamond() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0xFBAE, w, h, 20.0, 1); // all 4 segments
        // Should produce roughly a diamond shape; check 4 quadrants have coverage.
        let qw = w / 2;
        let qh = h / 2;
        let count_in = |x0: u32, y0: u32, x1: u32, y1: u32| -> usize {
            let mut n = 0;
            for y in y0..y1 {
                for x in x0..x1 {
                    if g.bitmap[((y * w + x) * 4 + 3) as usize] > 0 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(count_in(0, 0, qw, qh) > 0, "upper-left quadrant empty");
        assert!(count_in(qw, 0, w, qh) > 0, "upper-right quadrant empty");
        assert!(count_in(0, qh, qw, h) > 0, "lower-left quadrant empty");
        assert!(count_in(qw, qh, w, h) > 0, "lower-right quadrant empty");
    }

    #[test]
    fn triangular_medium_shade_is_half_alpha() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0xFB9C, w, h, 20.0, 1); // upper-left triangular medium shade
        let shaded: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0x80).count();
        let total = (w * h) as usize;
        let half = total / 2;
        assert!(
            shaded > half * 80 / 100 && shaded < half * 120 / 100,
            "triangular shade: {shaded} at 0x80, expected ~{half}"
        );
    }

    #[test]
    fn plus_cross_has_horizontal_and_vertical() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0xFBAF, w, h, 20.0, 1);
        // Check centre row has coverage and centre column has coverage.
        let mid_y = h / 2;
        let mid_x = w / 2;
        let row_filled = (0..w).any(|x| g.bitmap[((mid_y * w + x) * 4 + 3) as usize] > 0);
        let col_filled = (0..h).any(|y| g.bitmap[((y * w + mid_x) * 4 + 3) as usize] > 0);
        assert!(row_filled, "plus/cross missing horizontal");
        assert!(col_filled, "plus/cross missing vertical");
    }

    #[test]
    fn mixed_shade_half_has_both_regions() {
        let w = 8u32;
        let h = 12u32;
        let g = rasterize(0xFB91, w, h, 10.0, 1); // upper solid + lower shade
        let solid: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0xFF).count();
        let shade: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0x80).count();
        assert!(solid > 0, "missing solid region");
        assert!(shade > 0, "missing shade region");
        assert_eq!(solid + shade, (w * h) as usize, "gaps in coverage");
    }

    #[test]
    fn box_vertical_line_touches_top_and_bottom_edges() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0x2502, w, h, 20.0, 1);
        let mid_x = (w / 2) as usize;
        assert!(
            g.bitmap[mid_x * 4 + 3] > 0,
            "vertical line missing at top edge"
        );
        let bottom = (((h - 1) * w + w / 2) * 4 + 3) as usize;
        assert!(g.bitmap[bottom] > 0, "vertical line missing at bottom edge");
    }

    #[test]
    fn box_top_right_corner_connects_left_and_down() {
        let w = 16u32;
        let h = 24u32;
        let g = rasterize(0x2510, w, h, 20.0, 1);
        let mid_y = (h / 2) as usize;
        let left = (mid_y * w as usize) * 4 + 3;
        let down = (mid_y * w as usize + (w / 2) as usize) * 4 + 3;
        assert!(g.bitmap[left] > 0, "corner missing horizontal arm");
        assert!(g.bitmap[down] > 0, "corner missing vertical arm");
    }
}
