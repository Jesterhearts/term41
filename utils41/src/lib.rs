//! The epnonymous "utils" module. This is a grab-bag for small helper functions
//! and types that don't fit anywhere else. Is any project really complete
//! without one of these?

use palette::Srgb;

/// Linear interpolation between two `f32` values.
pub fn lerp(
    start: f32,
    end: f32,
    t: f32,
) -> f32 {
    start + (end - start) * t
}

/// Linear interpolation between two `u8` values, with rounding and clamping.
pub fn lerp_u8(
    start: u8,
    end: u8,
    t: f32,
) -> u8 {
    lerp(start as f32, end as f32, t).round().clamp(0.0, 255.0) as u8
}

pub fn blend_colors(
    a: Srgb<u8>,
    b: Srgb<u8>,
    t: f32,
) -> Srgb<u8> {
    Srgb::new(
        lerp_u8(a.red, b.red, t),
        lerp_u8(a.green, b.green, t),
        lerp_u8(a.blue, b.blue, t),
    )
}
