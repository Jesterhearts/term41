use std::ops::RangeBounds;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use palette::Srgb;
use smol_str::SmolStr;

use crate::screen::hyperlink::HyperlinkId;

/// Inline SmolStr for the default blank cell. Cheap to clone.
pub(crate) const fn blank_cell() -> SmolStr {
    SmolStr::new_inline(" ")
}

pub(crate) const fn continuation_cell() -> SmolStr {
    SmolStr::new_inline("")
}

/// DEC line rendering attribute. Set by ESC#3 (DECDHL top), ESC#4 (DECDHL
/// bottom), ESC#5 (DECSWL, normal single-width), and ESC#6 (DECDWL,
/// double-width). Applies to the whole row; per-cell attrs are separate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LineAttr {
    /// Normal single-width, single-height row.
    #[default]
    Normal,
    /// Double-width row.
    DoubleWidth,
    /// Top half of a double-height row pair.
    DoubleHeightTop,
    /// Bottom half of a double-height row pair.
    DoubleHeightBottom,
}

/// A terminal row stored as struct-of-arrays for cache-friendly access.
/// Each cell holds one grapheme cluster as a [`SmolStr`] (inline up to
/// 23 bytes), so combining marks are stored alongside their base character.
#[derive(Debug, Default)]
pub struct Row {
    /// Grapheme cluster stored in each cell.
    pub cells: Vec<SmolStr>,
    /// Per-cell foreground colors.
    pub fg: Vec<Srgb<u8>>,
    /// Per-cell background colors.
    pub bg: Vec<Srgb<u8>>,
    /// Per-cell text attributes (bold/italic/strikethrough). Set from
    /// `screen.attrs` at write time alongside `fg`/`bg`.
    pub attrs: Vec<CellAttrs>,
    /// Per-cell underline rendering style. Separated from `attrs` because
    /// the styles are mutually exclusive (an enum, not a flag set).
    pub underline: Vec<UnderlineStyle>,
    /// Per-cell underline color override. `None` means "use the cell's
    /// foreground color" (the default). Set via SGR 58, cleared by SGR 59.
    pub underline_color: Vec<Option<Srgb<u8>>>,
    /// Hyperlink id per cell, set from the screen's current OSC 8 span at
    /// write time. `None` for plain cells; reused ids share the same target
    /// in the screen's hyperlink registry so adjacent cells of one link
    /// render as one underlined region.
    pub links: Vec<Option<HyperlinkId>>,
    /// True if this row continues into the next row (soft wrap).
    pub wrapped: bool,
    /// OSC 133 `A` was emitted on this row — shell prompt starts here.
    /// Only set on the head of a logical line (the non-continuation row), so
    /// reflow naturally keeps the mark with its prompt.
    pub prompt_start: bool,
    /// OSC 133 `C` was emitted on this row — command output starts here.
    /// Mirrors `prompt_start`: head-of-logical-line only.
    pub output_start: bool,
    /// Exit status of the command whose prompt begins on this row, set when
    /// an OSC 133 `D` arrives and can be resolved back to the matching
    /// prompt. `None` when the command is still running, had no
    /// shell-integration `D`, or when `D` arrived after the prompt row
    /// scrolled out of history.
    pub exit_status: Option<i32>,
    /// DEC double-width / double-height attribute for this row. Set by ESC#3
    /// (DECDHL top half), ESC#4 (DECDHL bottom half), ESC#5 (DECSWL, reset
    /// to normal), and ESC#6 (DECDWL, double-width single-height). Cleared
    /// to `Normal` on a full-row wipe (`clear()`).
    pub line_attr: LineAttr,
}

impl Row {
    /// Create a blank row of `cols` cells using the provided default colors.
    pub fn new(
        cols: u32,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) -> Self {
        let n = cols as usize;
        Self {
            cells: vec![blank_cell(); n],
            fg: vec![fg; n],
            bg: vec![bg; n],
            attrs: vec![CellAttrs::default(); n],
            underline: vec![UnderlineStyle::None; n],
            underline_color: vec![None; n],
            links: vec![None; n],
            wrapped: false,
            prompt_start: false,
            output_start: false,
            exit_status: None,
            line_attr: LineAttr::Normal,
        }
    }

    pub(crate) fn len(&self) -> u32 {
        self.cells.len() as u32
    }

    pub(crate) fn content_len(&self) -> u32 {
        if self.wrapped {
            self.len()
        } else {
            self.cells
                .iter()
                .rposition(|c| c != " ")
                .map_or(0, |p| p + 1) as u32
        }
    }

    pub(crate) fn resize(
        &mut self,
        new_len: u32,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) {
        let new_len = new_len as usize;
        self.cells.resize(new_len, blank_cell());
        self.fg.resize(new_len, fg);
        self.bg.resize(new_len, bg);
        self.attrs.resize(new_len, CellAttrs::default());
        self.underline.resize(new_len, UnderlineStyle::None);
        self.underline_color.resize(new_len, None);
        self.links.resize(new_len, None);
    }

    pub(crate) fn truncate(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.cells.truncate(new_len);
        self.fg.truncate(new_len);
        self.bg.truncate(new_len);
        self.attrs.truncate(new_len);
        self.underline.truncate(new_len);
        self.underline_color.truncate(new_len);
        self.links.truncate(new_len);
    }

    pub(crate) fn clear(
        &mut self,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) {
        self.clear_range(0..self.cells.len(), fg, bg);
        // A full-row wipe drops the row's semantic (OSC 133) marks. Partial
        // clears via `clear_range` leave them alone, so apps that use SGR to
        // redraw a prompt line in place don't lose the mark mid-update.
        self.prompt_start = false;
        self.output_start = false;
        self.exit_status = None;
    }

    /// Reset this row for reuse at the bottom of the grid — used when the
    /// scrollback limit is hit and we'd otherwise drop+reallocate the
    /// row's four backing vectors. Resizes to `cols` if the viewport width
    /// changed, blanks every cell, and clears the soft-wrap marker.
    pub(crate) fn reset_for_reuse(
        &mut self,
        cols: u32,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) {
        let n = cols as usize;
        if self.cells.len() != n {
            self.resize(cols, fg, bg);
        }
        self.clear(fg, bg);
        self.wrapped = false;
        self.line_attr = LineAttr::Normal;
    }

    pub(crate) fn clear_range(
        &mut self,
        range: std::ops::Range<usize>,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) {
        let range = self.expand_grapheme_erase_range(range);
        if range.is_empty() {
            return;
        }
        self.cells[range.clone()].fill(blank_cell());
        self.fg[range.clone()].fill(fg);
        self.bg[range.clone()].fill(bg);
        self.attrs[range.clone()].fill(CellAttrs::default());
        self.underline[range.clone()].fill(UnderlineStyle::None);
        self.underline_color[range.clone()].fill(None);
        self.links[range].fill(None);
    }

    /// Selective clear: erase only cells whose `PROTECTED` bit is *not* set.
    /// Used by DECSED (`CSI ? J`) and DECSEL (`CSI ? K`).
    pub(crate) fn clear_range_selective(
        &mut self,
        range: std::ops::Range<usize>,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) {
        let range = self.expand_grapheme_erase_range(range);
        for i in range {
            if !self.attrs[i].contains(CellAttrs::PROTECTED) {
                self.cells[i] = blank_cell();
                self.fg[i] = fg;
                self.bg[i] = bg;
                self.attrs[i] = CellAttrs::default();
                self.underline[i] = UnderlineStyle::None;
                self.underline_color[i] = None;
                self.links[i] = None;
            }
        }
    }

    /// Selective full-row clear: like [`clear`] but skips protected cells
    /// and preserves semantic marks (since partial content may survive).
    pub(crate) fn clear_selective(
        &mut self,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) {
        self.clear_range_selective(0..self.cells.len(), fg, bg);
    }

    fn expand_grapheme_erase_range(
        &self,
        range: std::ops::Range<usize>,
    ) -> std::ops::Range<usize> {
        let mut start = range.start.min(self.cells.len());
        let mut end = range.end.min(self.cells.len());
        if start >= end {
            return start..start;
        }

        loop {
            let expanded_start = self
                .grapheme_span_at(start)
                .map_or(start, |span| span.start.min(start));
            let expanded_end = self
                .grapheme_span_at(end - 1)
                .map_or(end, |span| span.end.max(end));
            if expanded_start == start && expanded_end == end {
                break;
            }
            start = expanded_start;
            end = expanded_end.min(self.cells.len());
        }

        start..end
    }

    fn grapheme_span_at(
        &self,
        col: usize,
    ) -> Option<std::ops::Range<usize>> {
        if col >= self.cells.len() {
            return None;
        }

        if self.is_wide_anchor_at(col) {
            return Some(col..(col + 2).min(self.cells.len()));
        }

        if col > 0 && self.cells[col].is_empty() && self.is_wide_anchor_at(col - 1) {
            return Some((col - 1)..(col + 1).min(self.cells.len()));
        }

        None
    }

    fn is_wide_anchor_at(
        &self,
        col: usize,
    ) -> bool {
        let Some(anchor) = self.cells.get(col) else {
            return false;
        };
        let Some(right) = self.cells.get(col + 1) else {
            return false;
        };
        let anchor = anchor.as_str();
        !anchor.is_empty() && anchor != " " && right.is_empty()
    }

    pub(crate) fn copy_within<R>(
        &mut self,
        src: R,
        dest: usize,
    ) where
        R: RangeBounds<usize> + Clone,
    {
        // SmolStr isn't Copy, so copy_within isn't available — use a manual
        // forward/backward clone loop to handle overlapping ranges.
        let (start, end) = range_bounds(src.clone(), self.cells.len());
        let count = end - start;
        if dest <= start {
            for i in 0..count {
                self.cells[dest + i] = self.cells[start + i].clone();
            }
        } else {
            for i in (0..count).rev() {
                self.cells[dest + i] = self.cells[start + i].clone();
            }
        }
        self.fg.copy_within(src.clone(), dest);
        self.bg.copy_within(src.clone(), dest);
        self.attrs.copy_within(src.clone(), dest);
        self.underline.copy_within(src.clone(), dest);
        self.underline_color.copy_within(src.clone(), dest);
        self.links.copy_within(src, dest);
    }

    pub(crate) fn copy_from(
        &mut self,
        other: &Self,
        src: std::ops::Range<usize>,
        dest_offset: usize,
    ) -> usize {
        let copy_len = ((other.content_len() as usize).saturating_sub(src.start))
            .min((self.len() as usize).saturating_sub(dest_offset))
            .min(src.len());
        self.cells[dest_offset..dest_offset + copy_len]
            .clone_from_slice(&other.cells[src.start..src.start + copy_len]);
        self.fg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.fg[src.start..src.start + copy_len]);
        self.bg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.bg[src.start..src.start + copy_len]);
        self.attrs[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.attrs[src.start..src.start + copy_len]);
        self.underline[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.underline[src.start..src.start + copy_len]);
        self.underline_color[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.underline_color[src.start..src.start + copy_len]);
        self.links[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.links[src.start..src.start + copy_len]);

        copy_len
    }

    /// Snapshot a column slice [left, right_excl) into a new Row for DECCRA.
    /// The returned Row is as wide as the slice; semantic flags are not copied.
    pub(crate) fn snap_range(
        &self,
        left: usize,
        right_excl: usize,
    ) -> Self {
        let right_excl = right_excl.min(self.cells.len());

        Self {
            cells: self.cells[left..right_excl].to_vec(),
            fg: self.fg[left..right_excl].to_vec(),
            bg: self.bg[left..right_excl].to_vec(),
            attrs: self.attrs[left..right_excl].to_vec(),
            underline: self.underline[left..right_excl].to_vec(),
            underline_color: self.underline_color[left..right_excl].to_vec(),
            links: self.links[left..right_excl].to_vec(),
            wrapped: false,
            prompt_start: false,
            output_start: false,
            exit_status: None,
            line_attr: LineAttr::Normal,
        }
    }

    /// Write a snapshot row (from `snap_range`) into this row starting at
    /// `dst_start`. Columns outside [dst_start, dst_start+snap.len()) are
    /// left untouched.
    pub(crate) fn paste_range(
        &mut self,
        snap: &Self,
        dst_start: usize,
    ) {
        let avail = self.cells.len().saturating_sub(dst_start);
        let copy_len = snap.cells.len().min(avail);
        if copy_len == 0 {
            return;
        }
        self.cells[dst_start..dst_start + copy_len].clone_from_slice(&snap.cells[..copy_len]);
        self.fg[dst_start..dst_start + copy_len].copy_from_slice(&snap.fg[..copy_len]);
        self.bg[dst_start..dst_start + copy_len].copy_from_slice(&snap.bg[..copy_len]);
        self.attrs[dst_start..dst_start + copy_len].copy_from_slice(&snap.attrs[..copy_len]);
        self.underline[dst_start..dst_start + copy_len]
            .copy_from_slice(&snap.underline[..copy_len]);
        self.underline_color[dst_start..dst_start + copy_len]
            .copy_from_slice(&snap.underline_color[..copy_len]);
        self.links[dst_start..dst_start + copy_len].copy_from_slice(&snap.links[..copy_len]);
    }

    pub(crate) fn has_drawn_cell_at(
        &self,
        col: usize,
    ) -> bool {
        col < self.content_len() as usize
    }

    /// Apply SGR attribute parameters to every cell in [left, right_excl).
    /// Used by DECCARA. VT420 recognizes only bold, underline, blink, and
    /// reverse-image toggles here; the rest are ignored.
    pub(crate) fn apply_attrs_in_range(
        &mut self,
        left: usize,
        right_excl: usize,
        sgr_params: &[u16],
    ) {
        let right_excl = right_excl.min(self.cells.len());
        if left >= right_excl {
            return;
        }
        for c in left..right_excl {
            self.apply_attrs_at(c, sgr_params);
        }
    }

    pub(crate) fn apply_attrs_at(
        &mut self,
        col: usize,
        sgr_params: &[u16],
    ) {
        for &p in sgr_params {
            match p {
                0 => {
                    self.attrs[col].remove(CellAttrs::BOLD | CellAttrs::BLINK | CellAttrs::REVERSE);
                    self.underline[col] = UnderlineStyle::None;
                }
                1 => self.attrs[col].insert(CellAttrs::BOLD),
                4 => self.underline[col] = UnderlineStyle::Single,
                5 => self.attrs[col].insert(CellAttrs::BLINK),
                7 => self.attrs[col].insert(CellAttrs::REVERSE),
                22 => self.attrs[col].remove(CellAttrs::BOLD),
                24 => self.underline[col] = UnderlineStyle::None,
                25 => self.attrs[col].remove(CellAttrs::BLINK),
                27 => self.attrs[col].remove(CellAttrs::REVERSE),
                _ => {}
            }
        }
    }

    /// Toggle (XOR) SGR attributes in [left, right_excl). Used by DECRARA.
    /// Underline is toggled between None and Single (it is an enum, not a
    /// bitflag, so it must be handled separately).
    pub(crate) fn toggle_attrs_in_range(
        &mut self,
        left: usize,
        right_excl: usize,
        sgr_params: &[u16],
    ) {
        let right_excl = right_excl.min(self.cells.len());
        if left >= right_excl {
            return;
        }
        for c in left..right_excl {
            self.toggle_attrs_at(c, sgr_params);
        }
    }

    pub(crate) fn toggle_attrs_at(
        &mut self,
        col: usize,
        sgr_params: &[u16],
    ) {
        let mut flags = CellAttrs::empty();
        let mut toggle_ul = false;
        if sgr_params.is_empty() || sgr_params.contains(&0) {
            flags |= CellAttrs::BOLD | CellAttrs::REVERSE | CellAttrs::BLINK;
            toggle_ul = true;
        }
        for &p in sgr_params {
            match p {
                1 => flags |= CellAttrs::BOLD,
                4 => toggle_ul = true,
                5 => flags |= CellAttrs::BLINK,
                7 => flags |= CellAttrs::REVERSE,
                _ => {}
            }
        }
        if flags.is_empty() && !toggle_ul {
            return;
        }
        self.attrs[col] ^= flags;
        if toggle_ul {
            self.underline[col] = if self.underline[col] != UnderlineStyle::None {
                UnderlineStyle::None
            } else {
                UnderlineStyle::Single
            };
        }
    }
}

fn range_bounds<R: RangeBounds<usize>>(
    range: R,
    len: usize,
) -> (usize, usize) {
    use std::ops::Bound;
    let start = match range.start_bound() {
        Bound::Included(&n) => n,
        Bound::Excluded(&n) => n + 1,
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&n) => n + 1,
        Bound::Excluded(&n) => n,
        Bound::Unbounded => len,
    };
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::default_bg;
    use crate::color::default_fg;

    fn row_text(row: &Row) -> String {
        let mut s = String::new();
        for cell in &row.cells {
            s.push_str(cell);
        }
        s
    }

    fn set_cell(
        row: &mut Row,
        idx: usize,
        ch: char,
    ) {
        let mut buf = [0u8; 4];
        row.cells[idx] = SmolStr::new_inline(ch.encode_utf8(&mut buf));
    }

    #[test]
    fn row_new_filled_with_spaces() {
        let row = Row::new(4, default_fg(), default_bg());
        assert_eq!(row.cells, vec![blank_cell(); 4]);
        assert_eq!(row.fg, vec![default_fg(); 4]);
        assert_eq!(row.bg, vec![default_bg(); 4]);
        assert!(!row.wrapped);
    }

    #[test]
    fn row_len() {
        let row = Row::new(5, default_fg(), default_bg());
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_grow() {
        let mut row = Row::new(3, default_fg(), default_bg());
        set_cell(&mut row, 0, 'a');
        set_cell(&mut row, 1, 'b');
        set_cell(&mut row, 2, 'c');
        row.resize(5, default_fg(), default_bg());
        assert_eq!(row_text(&row), "abc  ");
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_shrink() {
        let mut row = Row::new(5, default_fg(), default_bg());
        set_cell(&mut row, 0, 'a');
        set_cell(&mut row, 1, 'b');
        set_cell(&mut row, 2, 'c');
        row.resize(2, default_fg(), default_bg());
        assert_eq!(row_text(&row), "ab");
    }

    #[test]
    fn row_clear() {
        let mut row = Row::new(3, default_fg(), default_bg());
        set_cell(&mut row, 0, 'x');
        set_cell(&mut row, 1, 'y');
        row.fg[0] = Srgb::new(255, 0, 0);
        row.clear(default_fg(), default_bg());
        assert_eq!(row.cells, vec![blank_cell(); 3]);
        assert_eq!(row.fg, vec![default_fg(); 3]);
    }

    #[test]
    fn row_clear_range() {
        let mut row = Row::new(5, default_fg(), default_bg());
        for (i, ch) in "abcde".chars().enumerate() {
            set_cell(&mut row, i, ch);
        }
        row.clear_range(1..4, default_fg(), default_bg());
        assert_eq!(row_text(&row), "a   e");
    }

    #[test]
    fn row_clear_range_expands_over_wide_cell_continuation() {
        let mut row = Row::new(4, default_fg(), default_bg());
        row.cells[0] = SmolStr::new("👩\u{200D}💻");
        row.cells[1] = SmolStr::default();
        set_cell(&mut row, 2, 'x');

        row.clear_range(1..2, default_fg(), default_bg());

        assert_eq!(row.cells[0].as_str(), " ");
        assert_eq!(row.cells[1].as_str(), " ");
        assert_eq!(row.cells[2].as_str(), "x");
    }

    #[test]
    fn row_copy_within() {
        let mut row = Row::new(6, default_fg(), default_bg());
        for (i, ch) in "abcdef".chars().enumerate() {
            set_cell(&mut row, i, ch);
        }
        row.copy_within(0..3, 3);
        assert_eq!(row_text(&row), "abcabc");
    }

    #[test]
    fn row_copy_from() {
        let mut dst = Row::new(6, default_fg(), default_bg());
        let mut src = Row::new(3, default_fg(), default_bg());
        for (i, ch) in "xyz".chars().enumerate() {
            set_cell(&mut src, i, ch);
        }
        dst.copy_from(&src, 0..3, 2);
        assert_eq!(row_text(&dst), "  xyz ");
    }

    #[test]
    fn row_copy_from_with_offset() {
        let mut dst = Row::new(5, default_fg(), default_bg());
        let mut src = Row::new(4, default_fg(), default_bg());
        for (i, ch) in "abcd".chars().enumerate() {
            set_cell(&mut src, i, ch);
        }
        // Copy from src offset 2 to dst offset 0 → copies "cd" (length min(2,5)=2)
        dst.copy_from(&src, 2..4, 0);
        assert_eq!(row_text(&dst), "cd   ");
    }
}
