//! Text attribute types stored alongside terminal row cells.

bitflags::bitflags! {
    /// Per-cell text rendering attributes. Packed as a small bitmask so it
    /// rides alongside `fg`/`bg` in the row's struct-of-arrays without
    /// inflating memory or breaking the memset-style fills in
    /// `put_ascii_run`.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CellAttrs: u16 {
        /// SGR 1 bold/intense text.
        const BOLD              = 0b0000_0000_0000_0001;
        /// SGR 3 italic text.
        const ITALIC            = 0b0000_0000_0000_0010;
        /// SGR 7 reverse-video text0000_.
        const REVERSE           = 0b0000_0000_0000_0100;
        /// SGR 2 faint/dim text.
        const DIM               = 0b0000_0000_0000_1000;
        /// SGR 9 strikethrough text0000_.
        const STRIKETHROUGH     = 0b0000_0000_0001_0000;
        /// SGR 53 overlined text.
        const OVERLINE          = 0b0000_0000_0010_0000;
        /// SGR 8 concealed text.
        const HIDDEN            = 0b0000_0000_0100_0000;
        /// SGR 5 slow blink.
        const BLINK             = 0b0000_0000_1000_0000;
        /// SGR 6 rapid blink.
        const RAPID_BLINK       = 0b0000_0001_0000_0000;
        /// DECSCA character protection. Protected cells are skipped by
        /// DECSED (`CSI ? J`) and DECSEL (`CSI ? K`). Set via
        /// `CSI 1 " q`, cleared by `CSI 0 " q` or `CSI 2 " q`.
        const PROTECTED         = 0b0000_0010_0000_0000;
        const SINGLE_UNDERLINE  = 0b0000_0100_0000_0000;
        const DOUBLE_UNDERLINE  = 0b0000_1000_0000_0000;
        const CURLY_UNDERLINE   = 0b0001_0000_0000_0000;
        const DOTTED_UNDERLINE  = 0b0010_0000_0000_0000;
        const DASHED_UNDERLINE  = 0b0100_0000_0000_0000;

        const UNDERLINE_MASK = Self::SINGLE_UNDERLINE.bits()
            | Self::DOUBLE_UNDERLINE.bits()
            | Self::CURLY_UNDERLINE.bits()
            | Self::DOTTED_UNDERLINE.bits()
            | Self::DASHED_UNDERLINE.bits();
    }
}

impl CellAttrs {
    pub fn underline_from_sgr(sub: u16) -> Self {
        match sub {
            0 => Self::empty(),
            1 => Self::SINGLE_UNDERLINE,
            2 => Self::DOUBLE_UNDERLINE,
            3 => Self::CURLY_UNDERLINE,
            4 => Self::DOTTED_UNDERLINE,
            5 => Self::DASHED_UNDERLINE,
            _ => Self::SINGLE_UNDERLINE,
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
}
