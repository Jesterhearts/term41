bitflags::bitflags! {
    /// Per-cell text rendering attributes. Packed as a small bitmask so it
    /// rides alongside `fg`/`bg` in the row's struct-of-arrays without
    /// inflating memory or breaking the memset-style fills in
    /// `put_ascii_run`.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CellAttrs: u8 {
        const BOLD      = 0b0000_0001;
        const ITALIC    = 0b0000_0010;
        const UNDERLINE = 0b0000_0100;
        const REVERSE   = 0b0000_1000;
        const DIM       = 0b0001_0000;
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
        assert!(!a.contains(CellAttrs::UNDERLINE));
    }

    #[test]
    fn insert_and_remove_individual_flags() {
        let mut a = CellAttrs::empty();
        a.insert(CellAttrs::BOLD);
        a.insert(CellAttrs::UNDERLINE);
        assert!(a.contains(CellAttrs::BOLD));
        assert!(a.contains(CellAttrs::UNDERLINE));
        assert!(!a.contains(CellAttrs::ITALIC));

        a.remove(CellAttrs::BOLD);
        assert!(!a.contains(CellAttrs::BOLD));
        assert!(a.contains(CellAttrs::UNDERLINE));
    }
}
