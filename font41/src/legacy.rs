//! Custom rasteriser for block drawing, braille, and legacy-computing shapes.
//!
//! Covers:
//! - Block Elements (U+2580–U+259F) — halves, eighths, quadrants, shades
//! - Braille Patterns (U+2800–U+28FF) — all 256 8-dot combinations
//! - Symbols for Legacy Computing sextants (U+1FB00–U+1FB3B)
//! - SFLC one-eighth blocks and partial fills (U+1FB70–U+1FB80,
//!   U+1FB82–U+1FB8F)
//!
//! These codepoints are the building blocks of "pixel art" in the terminal,
//! where users expect adjacent filled cells to abut without visible gaps and
//! dots inside a cell to tile flush with neighbouring cells. Font-supplied
//! glyphs for the same characters typically render dots as circles, reserve
//! empty margins around each cell, or anti-alias the pixel boundaries — all
//! fine for isolated use, ruinous when you're using them as drawing
//! primitives. We side-step the font entirely and fill the exact rectangles
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
        0x2580..=0x259F | 0x2800..=0x28FF => Some(cp as u16),
        // SFLC ranges fit into u16 after subtracting 0x10000 (max is 0xFB8F,
        // disjoint from both BMP ranges above). 0x1FB81 is excluded because
        // its dithered "horizontal one eighth block-1358" pattern isn't a
        // pure rectangle and we'd rather defer to whatever the font provides.
        0x1FB00..=0x1FB3B | 0x1FB70..=0x1FB80 | 0x1FB82..=0x1FB8F => Some((cp - 0x10000) as u16),
        _ => None,
    }
}

fn decode(glyph_id: u16) -> u32 {
    // Range disambiguation: the SFLC shapes encode to 0xFB00..=0xFB8F,
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
) -> RasterizedGlyph {
    if cell_width == 0 || cell_height == 0 {
        return empty();
    }

    let w = cell_width as usize;
    let h = cell_height as usize;
    let mut alpha = vec![0u8; w * h];

    match decode(glyph_id) {
        cp @ 0x2580..=0x259F => draw_block_element(cp, &mut alpha, w, h),
        cp @ 0x2800..=0x28FF => draw_braille(cp, &mut alpha, w, h),
        cp @ 0x1FB00..=0x1FB3B => draw_sextant(cp, &mut alpha, w, h),
        cp @ 0x1FB70..=0x1FB8F => draw_sflc_block(cp, &mut alpha, w, h),
        _ => {}
    }

    // Expand the single-channel alpha buffer to RGBA with RGB=0. The shader
    // reads `.a` on the non-colour path and tints by the cell's foreground
    // colour, matching the outline-glyph contract.
    let mut bitmap = vec![0u8; w * h * 4];
    for (i, a) in alpha.into_iter().enumerate() {
        bitmap[i * 4 + 3] = a;
    }

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
        let g = rasterize(0x2588, 8, 12, 10.0);
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
        let g = rasterize(0x28FF, 8, 12, 10.0);
        for chunk in g.bitmap.chunks_exact(4) {
            assert_eq!(chunk[3], 0xFF, "expected full coverage for U+28FF");
        }
    }

    #[test]
    fn empty_braille_has_no_coverage() {
        let g = rasterize(0x2800, 8, 12, 10.0);
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
        let g = rasterize(0x259E, w, h, 10.0);
        let set: usize = g.bitmap.chunks_exact(4).filter(|c| c[3] == 0xFF).count();
        assert_eq!(set, 2 * (w as usize / 2) * (h as usize / 2));
    }

    #[test]
    fn shade_midlevel_alpha() {
        // Medium shade fills every pixel at alpha 0x80 — a flat density
        // level that tints uniformly via the fg colour.
        let g = rasterize(0x2592, 8, 12, 10.0);
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
        let g = rasterize(0xFB3B, w, h, 10.0);
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
        let top = rasterize(0x2580, w, h, 10.0);
        let bot = rasterize(0x2584, w, h, 10.0);
        let left = rasterize(0x258C, w, h, 10.0);
        let right = rasterize(0x2590, w, h, 10.0);
        assert!(cov(&top, 0, my_top - 1) && !cov(&top, 0, my_top));
        assert!(cov(&bot, 0, my_bot) && !cov(&bot, 0, my_bot - 1));
        assert!(cov(&left, mx_top - 1, 0) && !cov(&left, mx_top, 0));
        assert!(cov(&right, mx_bot, 0) && !cov(&right, mx_bot - 1, 0));

        // ▛ UL+UR+LL — right column isolates UR (top stripe).
        let tl = rasterize(0x259B, w, h, 10.0);
        assert!(cov(&tl, w - 1, my_top - 1), "▛ UR fills through top_end");
        assert!(!cov(&tl, w - 1, my_top), "▛ UR stops at top_end");
        // Bottom row isolates LL (left stripe).
        assert!(cov(&tl, mx_top - 1, h - 1), "▛ LL fills through left_end");
        assert!(!cov(&tl, mx_top, h - 1), "▛ LL stops at left_end");

        // ▜ UL+UR+LR — left column isolates UL (top stripe).
        let tr = rasterize(0x259C, w, h, 10.0);
        assert!(cov(&tr, 0, my_top - 1), "▜ UL fills through top_end");
        assert!(!cov(&tr, 0, my_top), "▜ UL stops at top_end");
        // Bottom row isolates LR (right stripe).
        assert!(cov(&tr, mx_bot, h - 1), "▜ LR starts at right_start");
        assert!(!cov(&tr, mx_bot - 1, h - 1), "▜ LR gap before right_start");

        // ▙ UL+LL+LR — top row isolates UL (left stripe).
        let bl = rasterize(0x2599, w, h, 10.0);
        assert!(cov(&bl, mx_top - 1, 0), "▙ UL fills through left_end");
        assert!(!cov(&bl, mx_top, 0), "▙ UL stops at left_end");
        // Right column isolates LR (bottom stripe).
        assert!(cov(&bl, w - 1, my_bot), "▙ LR starts at bot_start");
        assert!(!cov(&bl, w - 1, my_bot - 1), "▙ LR gap before bot_start");

        // ▟ UR+LL+LR — top row isolates UR (right stripe).
        let br = rasterize(0x259F, w, h, 10.0);
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
        let q = rasterize(0x259B, w, h, 10.0); // ▛ UL+UR+LL
        let hb = rasterize(0x2580, w, h, 10.0); // ▀ upper half

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
        let g1 = rasterize(0x258F, w as u32, h, 10.0);
        let g2 = rasterize(0xFB70, w as u32, h, 10.0);
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
}
