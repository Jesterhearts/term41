bitflags::bitflags! {
    /// Per-cell text rendering attributes. Packed as a small bitmask so it
    /// rides alongside `fg`/`bg` in the row's struct-of-arrays without
    /// inflating memory or breaking the memset-style fills in
    /// `put_ascii_run`.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CellAttrs: u8 {
        const BOLD          = 0b0000_0001;
        const ITALIC        = 0b0000_0010;
        const REVERSE       = 0b0000_0100;
        const DIM           = 0b0000_1000;
        const STRIKETHROUGH = 0b0001_0000;
        const OVERLINE      = 0b0010_0000;
        const HIDDEN        = 0b0100_0000;
    }
}

/// Underline rendering style. Separated from [`CellAttrs`] because the
/// styles are mutually exclusive (an enum, not a flag set). Stored in a
/// parallel `Vec<UnderlineStyle>` in the row's struct-of-arrays layout.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UnderlineStyle {
    #[default]
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
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
