//! Text attribute types stored alongside terminal row cells.

bitflags::bitflags! {
    /// Per-cell text rendering attributes. Packed as a small bitmask so it
    /// rides alongside `fg`/`bg` in the row's struct-of-arrays without
    /// inflating memory or breaking the memset-style fills in
    /// `put_ascii_run`.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CellAttrs: u16 {
        /// SGR 1 bold/intense text.
        const BOLD          = 0b0000_0000_0001;
        /// SGR 3 italic text.
        const ITALIC        = 0b0000_0000_0010;
        /// SGR 7 reverse-video text.
        const REVERSE       = 0b0000_0000_0100;
        /// SGR 2 faint/dim text.
        const DIM           = 0b0000_0000_1000;
        /// SGR 9 strikethrough text.
        const STRIKETHROUGH = 0b0000_0001_0000;
        /// SGR 53 overlined text.
        const OVERLINE      = 0b0000_0010_0000;
        /// SGR 8 concealed text.
        const HIDDEN        = 0b0000_0100_0000;
        /// SGR 5 slow blink.
        const BLINK         = 0b0000_1000_0000;
        /// SGR 6 rapid blink.
        const RAPID_BLINK   = 0b0001_0000_0000;
        /// DECSCA character protection. Protected cells are skipped by
        /// DECSED (`CSI ? J`) and DECSEL (`CSI ? K`). Set via
        /// `CSI 1 " q`, cleared by `CSI 0 " q` or `CSI 2 " q`.
        const PROTECTED     = 0b0010_0000_0000;
    }
}

/// Underline rendering style. Separated from [`CellAttrs`] because the
/// styles are mutually exclusive (an enum, not a flag set). Stored in a
/// parallel `Vec<UnderlineStyle>` in the row's struct-of-arrays layout.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UnderlineStyle {
    /// No underline.
    #[default]
    None = 0,
    /// SGR 4:1 single underline.
    Single = 1,
    /// SGR 4:2 double underline.
    Double = 2,
    /// SGR 4:3 curly underline.
    Curly = 3,
    /// SGR 4:4 dotted underline.
    Dotted = 4,
    /// SGR 4:5 dashed underline.
    Dashed = 5,
}

impl UnderlineStyle {
    /// Map an SGR 4 sub-parameter to the corresponding style. Values outside
    /// the defined range fall back to single underline, matching xterm
    /// behavior for unrecognized sub-params.
    pub fn from_sgr(sub: u16) -> Self {
        match sub {
            0 => Self::None,
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::Single,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let a = CellAttrs::default();
        assert!(!a.contains(CellAttrs::BOLD));
        assert!(!a.contains(CellAttrs::ITALIC));
        assert!(!a.contains(CellAttrs::STRIKETHROUGH));
    }

    #[test]
    fn insert_and_remove_individual_flags() {
        let mut a = CellAttrs::empty();
        a.insert(CellAttrs::BOLD);
        a.insert(CellAttrs::STRIKETHROUGH);
        assert!(a.contains(CellAttrs::BOLD));
        assert!(a.contains(CellAttrs::STRIKETHROUGH));
        assert!(!a.contains(CellAttrs::ITALIC));

        a.remove(CellAttrs::BOLD);
        assert!(!a.contains(CellAttrs::BOLD));
        assert!(a.contains(CellAttrs::STRIKETHROUGH));
    }

    #[test]
    fn underline_style_from_sgr() {
        assert_eq!(UnderlineStyle::from_sgr(0), UnderlineStyle::None);
        assert_eq!(UnderlineStyle::from_sgr(1), UnderlineStyle::Single);
        assert_eq!(UnderlineStyle::from_sgr(2), UnderlineStyle::Double);
        assert_eq!(UnderlineStyle::from_sgr(3), UnderlineStyle::Curly);
        assert_eq!(UnderlineStyle::from_sgr(4), UnderlineStyle::Dotted);
        assert_eq!(UnderlineStyle::from_sgr(5), UnderlineStyle::Dashed);
        assert_eq!(UnderlineStyle::from_sgr(99), UnderlineStyle::Single);
    }

    #[test]
    fn default_underline_style_is_none() {
        assert_eq!(UnderlineStyle::default(), UnderlineStyle::None);
    }
}
