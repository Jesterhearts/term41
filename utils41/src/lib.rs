//! The epnonymous "utils" module. This is a grab-bag for small helper functions
//! and types that don't fit anywhere else. Is any project really complete
//! without one of these?

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
