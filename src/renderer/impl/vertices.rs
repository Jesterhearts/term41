use font41::attrs::CellAttrs;
use palette::Srgb;

use crate::renderer::glyph_atlas::GlyphSlot;

/// Packed vertex for background quads: position + color.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BgVertex {
    pub(super) pos: [f32; 2],
    pub(super) color: u32,
}

/// Packed vertex for foreground (glyph) quads: position + UV + color + flags.
/// `flags & 1` selects the color-glyph shader path (sample atlas RGBA as-is
/// instead of tinting it by `color`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct FgVertex {
    pub(super) pos: [f32; 2],
    pub(super) uv: [f32; 2],
    pub(super) color: u32,
    pub(super) flags: u32,
}

#[derive(Clone, Copy)]
pub(super) struct LabelGlyph {
    pub(super) slot: GlyphSlot,
    pub(super) col: u16,
    pub(super) x_offset: f32,
    pub(super) y_offset: f32,
}

/// Packed vertex for image quads: position + UV.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ImageVertex {
    pub(super) pos: [f32; 2],
    pub(super) uv: [f32; 2],
    pub(super) z: f32,
}

pub(super) fn pack_color(
    c: &Srgb<u8>,
    alpha: u8,
) -> u32 {
    u32::from_be_bytes([c.red, c.green, c.blue, alpha])
}

pub(super) fn label_ink_bounds(
    glyphs: &[LabelGlyph],
    cell_w: f32,
) -> Option<(f32, f32)> {
    let mut left = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;

    for glyph in glyphs {
        let glyph_left = glyph.col as f32 * cell_w + glyph.slot.bearing_x as f32 + glyph.x_offset;
        let glyph_right = glyph_left + glyph.slot.width() as f32;
        left = left.min(glyph_left);
        right = right.max(glyph_right);
    }

    left.is_finite().then_some((left, right))
}

pub(super) fn label_ink_y_bounds(
    glyphs: &[LabelGlyph],
    baseline: f32,
) -> Option<(f32, f32)> {
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;

    for glyph in glyphs {
        let glyph_top = baseline - glyph.slot.bearing_y as f32 - glyph.y_offset;
        let glyph_bottom = glyph_top + glyph.slot.height() as f32;
        top = top.min(glyph_top);
        bottom = bottom.max(glyph_bottom);
    }

    top.is_finite().then_some((top, bottom))
}

pub(super) fn fitted_ink_origin_y(
    origin_y: f32,
    region_h: f32,
    ink_top: f32,
    ink_bottom: f32,
) -> f32 {
    const EDGE_INSET: f32 = 1.0;

    if region_h <= EDGE_INSET * 2.0 || ink_bottom <= ink_top {
        return origin_y;
    }

    let target_top = EDGE_INSET;
    let target_bottom = region_h - EDGE_INSET;
    let mut offset = 0.0;

    if ink_top < target_top {
        offset = target_top - ink_top;
    }
    if ink_bottom + offset > target_bottom {
        offset -= ink_bottom + offset - target_bottom;
    }
    if ink_top + offset < target_top {
        offset = target_top - ink_top;
    }

    origin_y + offset
}

/// Emit background-pass quads for the given underline style. `uy` is the
/// baseline Y position for a single underline; `cell_w` and `cell_h` set
/// the horizontal span and vertical budget for multi-line / patterned
/// styles.
pub(super) fn push_underline_quads(
    style: CellAttrs,
    x: f32,
    uy: f32,
    cell_w: f32,
    thickness: f32,
    cell_h: f32,
    color: u32,
    verts: &mut Vec<BgVertex>,
    idxs: &mut Vec<u32>,
) {
    for style in style & CellAttrs::UNDERLINE_MASK {
        match style {
            CellAttrs::SINGLE_UNDERLINE => {
                push_rect(x, uy, cell_w, thickness, color, verts, idxs);
            }
            CellAttrs::DOUBLE_UNDERLINE => {
                let gap = thickness;
                push_rect(
                    x,
                    uy - gap - thickness,
                    cell_w,
                    thickness,
                    color,
                    verts,
                    idxs,
                );
                push_rect(x, uy, cell_w, thickness, color, verts, idxs);
            }
            CellAttrs::CURLY_UNDERLINE => {
                // Approximate a sine wave with short line-segment quads. Four
                // segments per cell gives a recognisable wave without bloating the
                // vertex count.
                let segments = 4u32;
                let seg_w = cell_w / segments as f32;
                let amplitude = (cell_h * 0.08).max(1.5);
                for s in 0..segments {
                    let t0 = s as f32 / segments as f32;
                    let t1 = (s + 1) as f32 / segments as f32;
                    let y0 = uy - amplitude * (t0 * std::f32::consts::TAU).sin();
                    let y1 = uy - amplitude * (t1 * std::f32::consts::TAU).sin();
                    let sx = x + s as f32 * seg_w;
                    let (top, bot) = if y0 < y1 {
                        (y0, y1 + thickness)
                    } else {
                        (y1, y0 + thickness)
                    };
                    push_rect(sx, top, seg_w, bot - top, color, verts, idxs);
                }
            }
            CellAttrs::DOTTED_UNDERLINE => {
                // Dots spaced at roughly 2× thickness apart.
                let dot_size = thickness.max(1.0);
                let gap = dot_size * 2.0;
                let mut dx = x;
                while dx + dot_size <= x + cell_w {
                    push_rect(dx, uy, dot_size, thickness, color, verts, idxs);
                    dx += gap;
                }
            }
            CellAttrs::DASHED_UNDERLINE => {
                // Three dashes per cell.
                let dash_w = cell_w / 5.0;
                let gap = dash_w;
                let mut dx = x;
                while dx + dash_w <= x + cell_w {
                    push_rect(dx, uy, dash_w, thickness, color, verts, idxs);
                    dx += dash_w + gap;
                }
            }
            _ => {
                unreachable!("unexpected underline style bit set: {style:?}");
            }
        }
    }
}

/// Push a single axis-aligned rectangle into the background vertex/index
/// buffers.
pub(super) fn push_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
    verts: &mut Vec<BgVertex>,
    idxs: &mut Vec<u32>,
) {
    let bi = verts.len() as u32;
    verts.extend_from_slice(&[
        BgVertex { pos: [x, y], color },
        BgVertex {
            pos: [x + w, y],
            color,
        },
        BgVertex {
            pos: [x, y + h],
            color,
        },
        BgVertex {
            pos: [x + w, y + h],
            color,
        },
    ]);
    idxs.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
}
