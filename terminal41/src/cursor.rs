//! Cursor appearance state.
//!
//! Apps drive this through DECSCUSR (`CSI Ps SP q`); a config-supplied
//! [`CursorStyle`] sets the initial value before any sequence arrives. The
//! shape and blink axes are independent — DECSCUSR conflates them into a
//! single 1–6 selector, but downstream code (renderer, config) reads them
//! separately so adding new shapes or a "force-disable blink" preference is a
//! one-line change.

use config41::CursorShape;
use config41::CursorStyle;

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

impl DecCusr {
    /// Apply a DECSCUSR parameter (`CSI Ps SP q`). Values are taken from the
    /// VT520 manual; 0 and 1 are interchangeable per the spec, both meaning
    /// "blinking block". Out-of-range values are ignored, matching xterm.
    pub fn apply(
        ps: u16,
        cursor: &mut CursorStyle,
    ) {
        let Ok(ps) = DecCusr::try_from(ps) else {
            return;
        };

        let style = match ps {
            DecCusr::Default | DecCusr::BlinkingBlock => CursorStyle {
                shape: CursorShape::Block,
                blink: true,
            },
            DecCusr::SteadyBlock => CursorStyle {
                shape: CursorShape::Block,
                blink: false,
            },
            DecCusr::BlinkingUnderline => CursorStyle {
                shape: CursorShape::Underline,
                blink: true,
            },
            DecCusr::SteadyUnderline => CursorStyle {
                shape: CursorShape::Underline,
                blink: false,
            },
            DecCusr::BlinkingBeam => CursorStyle {
                shape: CursorShape::Beam,
                blink: true,
            },
            DecCusr::SteadyBeam => CursorStyle {
                shape: CursorShape::Beam,
                blink: false,
            },
        };
        *cursor = style;
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

    #[test]
    fn decscusr_zero_resets_to_configured_default() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        let configured = CursorStyle {
            shape: CursorShape::Underline,
            blink: false,
        };
        term.set_default_cursor_style(configured);

        term.process(b"\x1b[5 q");
        assert_ne!(term.cursor_style, configured);
        term.process(b"\x1b[ q");

        assert_eq!(term.cursor_style, configured);
    }

    #[test]
    fn soft_reset_restores_configured_cursor_default() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        let configured = CursorStyle {
            shape: CursorShape::Underline,
            blink: false,
        };
        term.set_default_cursor_style(configured);

        term.process(b"\x1b[5 q");
        assert_ne!(term.cursor_style, configured);
        term.process(b"\x1b[!p");

        assert_eq!(term.cursor_style, configured);
    }

    #[test]
    fn alt_screen_1049_restores_cursor_style() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        let original = term.cursor_style;

        term.process(b"\x1b[?1049h\x1b[?12l");
        assert!(!term.cursor_style.blink);
        term.process(b"\x1b[?1049l");

        assert_eq!(term.cursor_style, original);
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
        DecCusr::apply(2, &mut s);
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
        DecCusr::apply(5, &mut s);
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
        DecCusr::apply(0, &mut s);
        assert_eq!(s, CursorStyle::default());
    }

    #[test]
    fn decscusr_out_of_range_is_ignored() {
        let mut s = CursorStyle {
            shape: CursorShape::Beam,
            blink: false,
        };
        DecCusr::apply(52, &mut s);
        assert_eq!(
            s,
            CursorStyle {
                shape: CursorShape::Beam,
                blink: false
            }
        );
    }
}
