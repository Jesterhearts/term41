use std::ops::RangeBounds;

use palette::Srgb;

use crate::terminal::color::default_bg;
use crate::terminal::color::default_fg;

/// A terminal row stored as struct-of-arrays for cache-friendly access.
/// The renderer can borrow `&[char]` directly for shaping without copying.
#[derive(Debug, Default)]
pub struct Row {
    pub chars: Vec<char>,
    pub fg: Vec<Srgb<u8>>,
    pub bg: Vec<Srgb<u8>>,
    /// True if this row is a continuation of the previous row (soft wrap).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: u32) -> Self {
        let n = cols as usize;
        Self {
            chars: vec![' '; n],
            fg: vec![default_fg(); n],
            bg: vec![default_bg(); n],
            wrapped: false,
        }
    }

    pub(super) fn len(&self) -> u32 {
        self.chars.len() as u32
    }

    pub(super) fn content_len(&self) -> u32 {
        if self.wrapped {
            self.len()
        } else {
            self.chars
                .iter()
                .rposition(|c| *c != ' ')
                .map_or(0, |p| p + 1) as u32
        }
    }

    pub(super) fn resize(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.chars.resize(new_len, ' ');
        self.fg.resize(new_len, default_fg());
        self.bg.resize(new_len, default_bg());
    }

    pub(super) fn truncate(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.chars.truncate(new_len);
        self.fg.truncate(new_len);
        self.bg.truncate(new_len);
    }

    pub(super) fn clear(&mut self) {
        self.clear_range(0..self.chars.len())
    }

    pub(super) fn clear_range(
        &mut self,
        range: std::ops::Range<usize>,
    ) {
        self.chars[range.clone()].fill(' ');
        self.fg[range.clone()].fill(default_fg());
        self.bg[range].fill(default_bg());
    }

    pub(super) fn copy_within<R>(
        &mut self,
        src: R,
        dest: usize,
    ) where
        R: RangeBounds<usize> + Clone,
    {
        self.chars.copy_within(src.clone(), dest);
        self.fg.copy_within(src.clone(), dest);
        self.bg.copy_within(src, dest);
    }

    pub(super) fn copy_from(
        &mut self,
        other: &Self,
        src: std::ops::Range<usize>,
        dest_offset: usize,
    ) -> usize {
        let copy_len = ((other.content_len() as usize).saturating_sub(src.start))
            .min((self.len() as usize).saturating_sub(dest_offset))
            .min(src.len());
        self.chars[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.chars[src.start..src.start + copy_len]);
        self.fg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.fg[src.start..src.start + copy_len]);
        self.bg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.bg[src.start..src.start + copy_len]);

        copy_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_chars(row: &Row) -> String {
        row.chars.iter().collect()
    }

    #[test]
    fn row_new_filled_with_spaces() {
        let row = Row::new(4);
        assert_eq!(row.chars, vec![' '; 4]);
        assert_eq!(row.fg, vec![default_fg(); 4]);
        assert_eq!(row.bg, vec![default_bg(); 4]);
        assert!(!row.wrapped);
    }

    #[test]
    fn row_len() {
        let row = Row::new(5);
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_grow() {
        let mut row = Row::new(3);
        row.chars[0] = 'a';
        row.chars[1] = 'b';
        row.chars[2] = 'c';
        row.resize(5);
        assert_eq!(row_chars(&row), "abc  ");
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_shrink() {
        let mut row = Row::new(5);
        row.chars[0] = 'a';
        row.chars[1] = 'b';
        row.chars[2] = 'c';
        row.resize(2);
        assert_eq!(row_chars(&row), "ab");
    }

    #[test]
    fn row_clear() {
        let mut row = Row::new(3);
        row.chars[0] = 'x';
        row.chars[1] = 'y';
        row.fg[0] = Srgb::new(255, 0, 0);
        row.clear();
        assert_eq!(row.chars, vec![' '; 3]);
        assert_eq!(row.fg, vec![default_fg(); 3]);
    }

    #[test]
    fn row_clear_range() {
        let mut row = Row::new(5);
        for (i, ch) in "abcde".chars().enumerate() {
            row.chars[i] = ch;
        }
        row.clear_range(1..4);
        assert_eq!(row_chars(&row), "a   e");
    }

    #[test]
    fn row_copy_within() {
        let mut row = Row::new(6);
        for (i, ch) in "abcdef".chars().enumerate() {
            row.chars[i] = ch;
        }
        row.copy_within(0..3, 3);
        assert_eq!(row_chars(&row), "abcabc");
    }

    #[test]
    fn row_copy_from() {
        let mut dst = Row::new(6);
        let mut src = Row::new(3);
        for (i, ch) in "xyz".chars().enumerate() {
            src.chars[i] = ch;
        }
        dst.copy_from(&src, 0..3, 2);
        assert_eq!(row_chars(&dst), "  xyz ");
    }

    #[test]
    fn row_copy_from_with_offset() {
        let mut dst = Row::new(5);
        let mut src = Row::new(4);
        for (i, ch) in "abcd".chars().enumerate() {
            src.chars[i] = ch;
        }
        // Copy from src offset 2 to dst offset 0 → copies "cd" (length min(2,5)=2)
        dst.copy_from(&src, 2..4, 0);
        assert_eq!(row_chars(&dst), "cd   ");
    }
}
