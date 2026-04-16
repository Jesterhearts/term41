//! Cursor appearance state.
//!
//! Apps drive this through DECSCUSR (`CSI Ps SP q`); a config-supplied
//! [`CursorStyle`] sets the initial value before any sequence arrives. The
//! shape and blink axes are independent — DECSCUSR conflates them into a
//! single 1–6 selector, but downstream code (renderer, config) reads them
//! separately so adding new shapes or a "force-disable blink" preference is a
//! one-line change.

use serde::Deserialize;

/// Geometry of the cursor overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    /// Full-cell block. The glyph beneath inverts so the character stays
    /// readable.
    #[default]
    Block,
    /// Thin horizontal bar at the bottom of the cell.
    #[serde(alias = "underscore")]
    Underline,
    /// Thin vertical bar at the left edge of the cell.
    #[serde(alias = "bar")]
    #[serde(alias = "ibeam")]
    Beam,
}

/// Combined shape + blink state. `Default` matches the long-standing xterm
/// default of a blinking block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blink: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
        }
    }
}

impl CursorStyle {
    /// Apply a DECSCUSR parameter (`CSI Ps SP q`). Values are taken from the
    /// VT520 manual; 0 and 1 are interchangeable per the spec, both meaning
    /// "blinking block". Out-of-range values are ignored, matching xterm.
    pub fn apply_decscusr(
        &mut self,
        ps: u16,
    ) {
        let style = match ps {
            0 | 1 => Self {
                shape: CursorShape::Block,
                blink: true,
            },
            2 => Self {
                shape: CursorShape::Block,
                blink: false,
            },
            3 => Self {
                shape: CursorShape::Underline,
                blink: true,
            },
            4 => Self {
                shape: CursorShape::Underline,
                blink: false,
            },
            5 => Self {
                shape: CursorShape::Beam,
                blink: true,
            },
            6 => Self {
                shape: CursorShape::Beam,
                blink: false,
            },
            _ => return,
        };
        *self = style;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_blinking_block() {
        let s = CursorStyle::default();
        assert_eq!(s.shape, CursorShape::Block);
        assert!(s.blink);
    }

    #[test]
    fn decscusr_2_is_steady_block() {
        let mut s = CursorStyle::default();
        s.apply_decscusr(2);
        assert_eq!(
            s,
            CursorStyle {
                shape: CursorShape::Block,
                blink: false
            }
        );
    }

    #[test]
    fn decscusr_5_is_blinking_beam() {
        let mut s = CursorStyle::default();
        s.apply_decscusr(5);
        assert_eq!(
            s,
            CursorStyle {
                shape: CursorShape::Beam,
                blink: true
            }
        );
    }

    #[test]
    fn decscusr_zero_resets_to_blinking_block() {
        let mut s = CursorStyle {
            shape: CursorShape::Beam,
            blink: false,
        };
        s.apply_decscusr(0);
        assert_eq!(s, CursorStyle::default());
    }

    #[test]
    fn decscusr_out_of_range_is_ignored() {
        let mut s = CursorStyle {
            shape: CursorShape::Beam,
            blink: false,
        };
        s.apply_decscusr(42);
        assert_eq!(
            s,
            CursorStyle {
                shape: CursorShape::Beam,
                blink: false
            }
        );
    }
}
