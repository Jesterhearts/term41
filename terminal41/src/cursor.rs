//! Cursor appearance state.
//!
//! Apps drive this through DECSCUSR (`CSI Ps SP q`); a config-supplied
//! [`CursorStyle`] sets the initial value before any sequence arrives. The
//! shape and blink axes are independent — DECSCUSR conflates them into a
//! single 1–6 selector, but downstream code (renderer, config) reads them
//! separately so adding new shapes or a "force-disable blink" preference is a
//! one-line change.

use serde::Deserialize;

/// DECSCUSR parameter values (CSI Ps SP q).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum DecCusr {
    Default = 0,
    BlinkingBlock = 1,
    SteadyBlock = 2,
    BlinkingUnderline = 3,
    SteadyUnderline = 4,
    BlinkingBeam = 5,
    SteadyBeam = 6,
}

impl TryFrom<u16> for DecCusr {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Default),
            1 => Ok(Self::BlinkingBlock),
            2 => Ok(Self::SteadyBlock),
            3 => Ok(Self::BlinkingUnderline),
            4 => Ok(Self::SteadyUnderline),
            5 => Ok(Self::BlinkingBeam),
            6 => Ok(Self::SteadyBeam),
            _ => Err(()),
        }
    }
}

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
    /// Cursor overlay geometry.
    pub shape: CursorShape,
    /// Whether the renderer should blink the cursor.
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
        let Ok(ps) = DecCusr::try_from(ps) else {
            return;
        };

        let style = match ps {
            DecCusr::Default | DecCusr::BlinkingBlock => Self {
                shape: CursorShape::Block,
                blink: true,
            },
            DecCusr::SteadyBlock => Self {
                shape: CursorShape::Block,
                blink: false,
            },
            DecCusr::BlinkingUnderline => Self {
                shape: CursorShape::Underline,
                blink: true,
            },
            DecCusr::SteadyUnderline => Self {
                shape: CursorShape::Underline,
                blink: false,
            },
            DecCusr::BlinkingBeam => Self {
                shape: CursorShape::Beam,
                blink: true,
            },
            DecCusr::SteadyBeam => Self {
                shape: CursorShape::Beam,
                blink: false,
            },
        };
        *self = style;
    }
}

#[cfg(test)]
mod integration_tests {
    use super::CursorShape;
    use super::CursorStyle;
    use crate::test_support::TestTerm;

    #[test]
    fn decscusr_sets_steady_block() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[2 q");
        assert_eq!(
            term.cursor_style,
            CursorStyle {
                shape: CursorShape::Block,
                blink: false,
            }
        );
    }

    #[test]
    fn decscusr_sets_blinking_beam() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[5 q");
        assert_eq!(
            term.cursor_style,
            CursorStyle {
                shape: CursorShape::Beam,
                blink: true,
            }
        );
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
