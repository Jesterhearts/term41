use std::collections::BTreeMap;
use std::collections::VecDeque;

use crop::Rope;
use crop::RopeBuilder;
use crop::RopeSlice;
use palette::Srgb;
use unicode_segmentation::UnicodeSegmentation;

use crate::sixel::SixelImage;
use crate::sixel::parse_sixel;
use crate::vte;
use crate::vte::Params;

pub const fn default_fg() -> Srgb<u8> {
    Srgb::new(204, 204, 204)
}

pub const fn default_bg() -> Srgb<u8> {
    Srgb::new(0, 0, 0)
}

/// The standard 256-color palette.
const fn ansi_color(index: u8) -> Srgb<u8> {
    match index {
        0 => Srgb::new(0, 0, 0),
        1 => Srgb::new(205, 0, 0),
        2 => Srgb::new(0, 205, 0),
        3 => Srgb::new(205, 205, 0),
        4 => Srgb::new(0, 0, 238),
        5 => Srgb::new(205, 0, 205),
        6 => Srgb::new(0, 205, 205),
        7 => Srgb::new(229, 229, 229),
        8 => Srgb::new(127, 127, 127),
        9 => Srgb::new(255, 0, 0),
        10 => Srgb::new(0, 255, 0),
        11 => Srgb::new(255, 255, 0),
        12 => Srgb::new(92, 92, 255),
        13 => Srgb::new(255, 0, 255),
        14 => Srgb::new(0, 255, 255),
        15 => Srgb::new(255, 255, 255),
        16..=231 => {
            const fn to_val(c: u8) -> u8 {
                if c == 0 { 0 } else { 55 + 40 * c }
            }

            let idx = index - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            Srgb::new(to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Srgb::new(v, v, v)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellColors {
    pub fg: Srgb<u8>,
    pub bg: Srgb<u8>,
}

impl Default for CellColors {
    fn default() -> Self {
        Self {
            fg: default_fg(),
            bg: default_bg(),
        }
    }
}

impl CellColors {
    fn is_default(&self) -> bool {
        self.fg == default_fg() && self.bg == default_bg()
    }
}

pub struct Row<'a> {
    pub chars: RopeSlice<'a>,
}

#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub image: SixelImage,
    pub id: u64,
    /// Absolute row index in `grid.rows` where the image top-left is placed.
    pub row: usize,
    /// Column position of the image top-left.
    pub col: u32,
}

/// A reference to an image visible in the current viewport.
pub struct VisibleImage<'a> {
    pub image: &'a SixelImage,
    pub id: u64,
    /// Row relative to the top of the viewport (0 = top).
    pub screen_row: u32,
    /// Column position.
    pub screen_col: u32,
}

#[derive(Debug, Default)]
pub struct Cursor {
    pub col: u32,
    pub row: u32,
}

#[derive(Debug)]
pub struct Grid {
    pub rows: Rope,
    pub wrapped: VecDeque<bool>,
    pub scrollback_limit: u32,
    /// Running count of rows popped from the front (for image position
    /// tracking).
    pub total_popped: usize,
    /// Per-grapheme color map. Keys are absolute byte offsets
    /// (`rope_byte_offset + bytes_popped`) so that scrollback trimming is
    /// O(log n) via `split_off` instead of O(n) to shift every key.
    pub colors: BTreeMap<usize, CellColors>,
    /// Accumulated bytes deleted from the front of the rope by scrollback
    /// trimming. Color map keys are absolute: `rope_offset + bytes_popped`.
    bytes_popped: usize,
}

impl Grid {
    /// Convert a rope byte offset to an absolute color-map key.
    fn color_key(
        &self,
        rope_offset: usize,
    ) -> usize {
        rope_offset + self.bytes_popped
    }

    /// Store the color for a cell at `rope_offset`. Default colors are omitted
    /// to keep the map sparse.
    fn set_color(
        &mut self,
        rope_offset: usize,
        colors: CellColors,
    ) {
        let key = self.color_key(rope_offset);
        if colors.is_default() {
            self.colors.remove(&key);
        } else {
            self.colors.insert(key, colors);
        }
    }

    /// Look up the color at `rope_offset`, falling back to defaults.
    fn get_color(
        &self,
        rope_offset: usize,
    ) -> CellColors {
        self.colors
            .get(&self.color_key(rope_offset))
            .copied()
            .unwrap_or_default()
    }

    /// Remove all color entries whose rope byte offsets fall in `start..end`.
    fn clear_colors(
        &mut self,
        start: usize,
        end: usize,
    ) {
        let abs_start = self.color_key(start);
        let abs_end = self.color_key(end);
        let keys: Vec<usize> = self
            .colors
            .range(abs_start..abs_end)
            .map(|(&k, _)| k)
            .collect();
        for k in keys {
            self.colors.remove(&k);
        }
    }

    /// Shift all color entries at rope byte offsets `>= from` by `delta`.
    /// Used after insertions/deletions that change byte layout.
    fn shift_colors(
        &mut self,
        from: usize,
        delta: isize,
    ) {
        let abs_from = self.color_key(from);
        let tail = self.colors.split_off(&abs_from);
        for (k, v) in tail {
            self.colors.insert((k as isize + delta) as usize, v);
        }
    }

    /// Discard color entries for bytes that have been trimmed from the front
    /// of the rope.
    fn trim_colors_front(
        &mut self,
        bytes_deleted: usize,
    ) {
        self.bytes_popped += bytes_deleted;
        let keep = self.colors.split_off(&self.bytes_popped);
        self.colors = keep;
    }

    pub fn scrollback_len(
        &self,
        viewport: &Viewport,
    ) -> u32 {
        (self.rows.line_len() as u32).saturating_sub(viewport.rows)
    }

    pub fn push_visible_row(
        &mut self,
        viewport: &Viewport,
    ) {
        self.rows
            .insert(self.rows.byte_len(), " ".repeat(viewport.cols as usize));
        self.rows.insert(self.rows.byte_len(), "\n");
        self.wrapped.push_back(false);

        let max_rows = viewport.rows as usize + self.scrollback_limit as usize;
        if self.rows.line_len() > max_rows {
            let eol = self.rows.byte_of_line(1);
            self.rows.delete(0..eol);
            self.wrapped.pop_front();
            self.total_popped += 1;
            self.trim_colors_front(eol);
        }
    }

    pub fn erase_in_display(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let first_visible = self.rows.line_len().saturating_sub(viewport.rows as usize);

        match mode {
            // Erase from cursor to end of screen.
            0 => {
                self.erase_in_line(cursor, viewport, 0);
                let empty = " ".repeat(viewport.cols as usize);
                for r in (active + 1)..self.rows.line_len() {
                    let start = self.rows.byte_of_line(r);
                    let len = self.rows.line(r).byte_len();
                    self.clear_colors(start, start + len);
                    self.rows.replace(start..start + len, &empty);
                }
            }
            // Erase from start of screen to cursor (inclusive).
            1 => {
                let empty = " ".repeat(viewport.cols as usize);
                for r in first_visible..active {
                    let start = self.rows.byte_of_line(r);
                    let len = self.rows.line(r).byte_len();
                    self.clear_colors(start, start + len);
                    self.rows.replace(start..start + len, &empty);
                }

                self.erase_in_line(cursor, viewport, 1);
            }
            // Erase entire screen.
            2 => {
                let empty = " ".repeat(viewport.cols as usize);
                for r in first_visible..self.rows.line_len() {
                    let start = self.rows.byte_of_line(r);
                    let len = self.rows.line(r).byte_len();
                    self.clear_colors(start, start + len);
                    self.rows.replace(start..start + len, &empty);
                }
            }
            // Erase scrollback buffer.
            3 => {
                self.total_popped += first_visible;
                let mut bytes = 0;
                for r in 0..first_visible {
                    bytes += self.rows.line(r).byte_len();
                }
                self.rows.delete(0..bytes);
                self.trim_colors_front(bytes);
            }
            _ => {}
        }
    }

    pub fn erase_in_line(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let col = cursor.col as usize;

        let line_start = self.rows.byte_of_line(active);
        let line_byte_len = self.rows.line(active).byte_len();

        match mode {
            // Erase from cursor to end of line.
            0 => {
                let col_start: usize = self
                    .rows
                    .line(active)
                    .graphemes()
                    .take(col)
                    .map(|c| c.len())
                    .sum();
                let start = line_start + col_start;
                let end = line_start + line_byte_len;
                let erase_len = end - start;
                self.clear_colors(start, end);
                self.rows.replace(start..end, " ".repeat(erase_len));
            }
            // Erase from start of line to cursor (inclusive).
            1 => {
                let col_end: usize = self
                    .rows
                    .line(active)
                    .graphemes()
                    .take(col + 1)
                    .map(|c| c.len())
                    .sum();
                self.clear_colors(line_start, line_start + col_end);
                self.rows
                    .replace(line_start..line_start + col_end, " ".repeat(col_end));
            }
            // Erase entire line.
            2 => {
                self.clear_colors(line_start, line_start + line_byte_len);
                self.rows.replace(
                    line_start..line_start + line_byte_len,
                    " ".repeat(line_byte_len),
                );
            }
            _ => {}
        }
    }

    pub fn delete_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let width = viewport.cols as usize;
        let cursor = cursor.col as usize;
        let count = (n as usize).min(width - cursor);

        let line_start = self.rows.byte_of_line(active);
        let line_len = self.rows.line(active).byte_len();
        let mut to_cursor = 0;
        let mut cursor_to_end_n = 0;
        for (idx, c) in self.rows.line(active).graphemes().enumerate() {
            if idx < cursor {
                to_cursor += c.len();
            } else if idx < cursor + count {
                cursor_to_end_n += c.len();
            }
        }
        let tail_len = line_len - (to_cursor + cursor_to_end_n);

        self.clear_colors(
            line_start + to_cursor,
            line_start + to_cursor + cursor_to_end_n,
        );
        self.rows
            .delete(line_start + to_cursor..line_start + to_cursor + cursor_to_end_n);
        let delta = count as isize - cursor_to_end_n as isize;
        if delta != 0 {
            self.shift_colors(line_start + to_cursor, delta);
        }
        self.rows
            .insert(line_start + to_cursor + tail_len, " ".repeat(count));
    }

    pub fn insert_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let width = viewport.cols as usize;
        let cursor = cursor.col as usize;
        let count = (n as usize).min(width - cursor);

        let line_start = self.rows.byte_of_line(active);
        let to_cursor = self
            .rows
            .line(active)
            .graphemes()
            .map(|c| c.len())
            .take(cursor)
            .sum::<usize>();
        self.shift_colors(line_start + to_cursor, count as isize);
        self.rows.insert(line_start + to_cursor, " ".repeat(count));
    }

    pub fn erase_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let width = viewport.cols as usize;
        let cursor = cursor.col as usize;
        let count = (n as usize).min(width - cursor);

        let line_start = self.rows.byte_of_line(active);
        let mut to_cursor = 0;
        let mut cursor_to_end_n = 0;
        for (idx, c) in self.rows.line(active).graphemes().enumerate() {
            if idx < cursor {
                to_cursor += c.len();
            } else if idx < cursor + count {
                cursor_to_end_n += c.len();
            }
        }

        self.clear_colors(
            line_start + to_cursor,
            line_start + to_cursor + cursor_to_end_n,
        );
        let delta = count as isize - cursor_to_end_n as isize;
        if delta != 0 {
            self.shift_colors(line_start + to_cursor, delta);
        }
        self.rows
            .delete(line_start + to_cursor..line_start + to_cursor + cursor_to_end_n);
        self.rows.insert(line_start + to_cursor, " ".repeat(count));
    }

    /// Scroll content up within a region: remove line at `top`, insert blank
    /// at `bottom`. Both are viewport-relative row indices.
    fn scroll_up_in_region(
        &mut self,
        viewport: &Viewport,
        top: u32,
        bottom: u32,
        n: u32,
    ) {
        let first_visible = self.rows.line_len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        let empty = " ".repeat(viewport.cols as usize);
        for _ in 0..n {
            let top_line_start = self.rows.byte_of_line(abs_top);
            let top_line_len = self.rows.line(abs_top).byte_len();
            // +1 for the newline delimiter between lines.
            let deleted = top_line_len + 1;
            self.clear_colors(top_line_start, top_line_start + top_line_len);
            self.rows
                .delete(top_line_start..top_line_start + top_line_len);
            // Shift colors for everything after the deleted line left.
            self.shift_colors(top_line_start, -(deleted as isize));
            let bottom_line_start = self.rows.byte_of_line(abs_bottom);
            // Shift colors for content after the insert point right.
            self.shift_colors(bottom_line_start, empty.len() as isize + 1);
            self.rows.insert(bottom_line_start, &empty);
        }
    }

    /// Scroll content down within a region: insert blank at `top`, remove
    /// line at `bottom`. Both are viewport-relative row indices.
    fn scroll_down_in_region(
        &mut self,
        viewport: &Viewport,
        top: u32,
        bottom: u32,
        n: u32,
    ) {
        let first_visible = self.rows.line_len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        let empty = " ".repeat(viewport.cols as usize);
        for _ in 0..n {
            let bottom_line_start = self.rows.byte_of_line(abs_bottom);
            let bottom_line_len = self.rows.line(abs_bottom).byte_len();
            let deleted = bottom_line_len + 1;
            self.clear_colors(bottom_line_start, bottom_line_start + bottom_line_len);
            self.rows
                .delete(bottom_line_start..bottom_line_start + bottom_line_len);
            self.shift_colors(bottom_line_start, -(deleted as isize));
            let top_line_start = self.rows.byte_of_line(abs_top);
            self.shift_colors(top_line_start, empty.len() as isize + 1);
            self.rows.insert(top_line_start, &empty);
        }
    }

    pub fn active_row_index(
        &self,
        cursor: &Cursor,
        viewport: &Viewport,
    ) -> usize {
        let idx = self.rows.line_len().saturating_sub(viewport.rows as usize) + cursor.row as usize;
        idx.min(self.rows.line_len().saturating_sub(1))
    }

    fn reflow(
        &mut self,
        new_width: u32,
    ) {
        if self.rows.line_len() == 0 {
            return;
        }

        let old_width = self.rows.line(0).graphemes().count();
        let new_width = new_width as usize;
        if old_width == new_width {
            return;
        }

        let mut new_rope = Rope::new();
        let mut new_wrapped = VecDeque::new();
        let mut new_colors = BTreeMap::new();
        let last_row = self.wrapped.len() - 1;

        let mut logical = String::new();
        let mut logical_colors: Vec<CellColors> = Vec::new();

        for (idy, line) in self.rows.lines().enumerate() {
            let line_start = self.rows.byte_of_line(idy);
            let mut byte_off = 0;
            for grapheme in line.graphemes() {
                logical_colors.push(self.get_color(line_start + byte_off));
                byte_off += grapheme.len();
            }

            for chunk in line.chunks() {
                logical.push_str(chunk);
            }

            if !self.wrapped[idy] || idy == last_row {
                let trimmed = logical.trim_end_matches(' ');
                let grapheme_count = trimmed.graphemes(true).count();
                logical_colors.truncate(grapheme_count);
                let graphemes: Vec<&str> = trimmed.graphemes(true).collect();

                if graphemes.is_empty() {
                    new_rope.insert(new_rope.byte_len(), &" ".repeat(new_width));
                    new_rope.insert(new_rope.byte_len(), "\n");
                    new_wrapped.push_back(false);
                } else {
                    let total_chunks = graphemes.chunks(new_width).len();
                    let mut color_idx = 0;
                    for (i, chunk) in graphemes.chunks(new_width).enumerate() {
                        let mut byte_pos = new_rope.byte_len();
                        for &g in chunk {
                            let colors = logical_colors[color_idx];
                            if !colors.is_default() {
                                new_colors.insert(byte_pos, colors);
                            }
                            byte_pos += g.len();
                            color_idx += 1;
                        }

                        let content: String = chunk.iter().copied().collect();
                        let len = chunk.len();
                        new_rope.insert(new_rope.byte_len(), &content);
                        if len < new_width {
                            new_rope.insert(new_rope.byte_len(), &" ".repeat(new_width - len));
                        }
                        new_rope.insert(new_rope.byte_len(), "\n");
                        new_wrapped.push_back(i < total_chunks - 1);
                    }
                }

                logical.clear();
                logical_colors.clear();
            }
        }

        self.rows = new_rope;
        self.wrapped = new_wrapped;
        self.colors = new_colors;
        self.bytes_popped = 0;
    }
}

#[derive(Debug, Default)]
pub struct Viewport {
    pub rows: u32,
    pub cols: u32,
    /// How many rows the viewport is scrolled back from the bottom.
    /// 0 = viewing the live terminal. Positive = scrolled into history.
    pub offset: u32,
    /// Top row of the scroll region (0-indexed, inclusive).
    pub scroll_top: u32,
    /// Bottom row of the scroll region (0-indexed, inclusive).
    pub scroll_bottom: u32,
}

/// Terminal state: a grid of rows plus cursor position and attributes.
///
/// The grid contains both scrollback history and the visible area.
/// Visible rows are the last `rows` entries in the grid. Scrollback
/// rows sit before them, capped at `scrollback_limit`.
#[derive(Debug)]
pub struct Terminal {
    pub grid: Grid,
    pub cursor: Cursor,
    pub viewport: Viewport,
    pub images: BTreeMap<u64, PlacedImage>,

    pub fg: Srgb<u8>,
    pub bg: Srgb<u8>,

    /// Cell height in pixels, used to convert sixel image pixel height to rows.
    cell_height: u32,

    next_image_id: u64,

    parser: vte::Parser,
    hook_bytes: Vec<Vec<u8>>,
    hook_params: Vec<Params>,
    hook_action: Vec<char>,
}

/// Save image positions as logical-line anchors that survive reflow.
///
/// Each image is mapped to (id, logical_lines_below, row_offset_in_line).
/// The count of hard line boundaries between the image and the grid end is
/// invariant through reflow, so it can be used to relocate the image after.
fn anchor_images(
    wrapped: &VecDeque<bool>,
    images: &BTreeMap<u64, PlacedImage>,
) -> Vec<(u64, usize, usize)> {
    images
        .values()
        .map(|img| {
            let lines_below = (img.row + 1..wrapped.len())
                .filter(|&r| !wrapped[r])
                .count();

            let mut row_offset = 0;
            let mut r = img.row;
            while r > 0 && wrapped[r] {
                row_offset += 1;
                r -= 1;
            }

            (img.id, lines_below, row_offset)
        })
        .collect()
}

/// Restore image row positions from logical-line anchors produced by
/// [`anchor_images`]. Images whose logical line was trimmed away are removed.
fn restore_images(
    wrapped: &VecDeque<bool>,
    anchors: &[(u64, usize, usize)],
    images: &mut BTreeMap<u64, PlacedImage>,
) {
    for &(id, lines_below, row_offset) in anchors {
        let mut count = 0;
        let mut found = None;
        for r in (0..wrapped.len()).rev() {
            if r == 0 || !wrapped[r] {
                if count == lines_below {
                    found = Some(r);
                    break;
                }
                count += 1;
            }
        }

        match found {
            Some(start) => {
                let mut end = start + 1;
                while end < wrapped.len() && wrapped[end] {
                    end += 1;
                }
                let new_row = start + row_offset.min(end - start - 1);
                if let Some(img) = images.get_mut(&id) {
                    img.row = new_row;
                }
            }
            None => {
                images.remove(&id);
            }
        }
    }
}

impl Terminal {
    pub fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        cell_height: u32,
    ) -> Self {
        let mut cells = RopeBuilder::new();

        let empty = " ".repeat(cols as usize);
        for _ in 0..rows {
            cells.append(&empty);
            cells.append("\n");
        }

        Self {
            grid: Grid {
                rows: cells.build(),
                wrapped: std::iter::repeat(false).take(rows as usize).collect(),
                scrollback_limit,
                total_popped: 0,
                colors: BTreeMap::new(),
                bytes_popped: 0,
            },
            viewport: Viewport {
                rows,
                cols,
                offset: 0,
                scroll_top: 0,
                scroll_bottom: rows - 1,
            },
            cursor: Cursor::default(),
            images: BTreeMap::new(),
            fg: default_fg(),
            bg: default_bg(),
            cell_height,
            parser: vte::Parser::new(),
            next_image_id: 0,
            hook_bytes: vec![],
            hook_params: vec![],
            hook_action: vec![],
        }
    }

    /// Returns the visible row at the given screen position (0 = top of
    /// viewport).
    pub fn visible_row(
        &self,
        screen_row: u32,
    ) -> Row<'_> {
        let base = self
            .grid
            .rows
            .line_len()
            .saturating_sub(self.viewport.rows as usize)
            .saturating_sub(self.viewport.offset as usize);
        let idx = (base + screen_row as usize).min(self.grid.rows.line_len() - 1);
        Row {
            chars: self.grid.rows.line(idx),
        }
    }

    /// Returns per-column fg/bg colors for a visible row.
    pub fn visible_row_colors(
        &self,
        screen_row: u32,
    ) -> Vec<CellColors> {
        let base = self
            .grid
            .rows
            .line_len()
            .saturating_sub(self.viewport.rows as usize)
            .saturating_sub(self.viewport.offset as usize);
        let line_idx = (base + screen_row as usize).min(self.grid.rows.line_len() - 1);
        let line_start = self.grid.rows.byte_of_line(line_idx);

        let mut result = Vec::with_capacity(self.viewport.cols as usize);
        let mut byte_offset = 0;
        for grapheme in self.grid.rows.line(line_idx).graphemes() {
            result.push(self.grid.get_color(line_start + byte_offset));
            byte_offset += grapheme.len();
        }
        result
    }

    /// Scroll the viewport up (into history). Returns actual lines scrolled.
    pub fn scroll_viewport_up(
        &mut self,
        lines: u32,
    ) -> u32 {
        let max = self.grid.scrollback_len(&self.viewport);
        let delta = lines.min(max.saturating_sub(self.viewport.offset));
        self.viewport.offset += delta;
        delta
    }

    /// Scroll the viewport down (toward live). Returns actual lines scrolled.
    pub fn scroll_viewport_down(
        &mut self,
        lines: u32,
    ) -> u32 {
        let delta = lines.min(self.viewport.offset);
        self.viewport.offset -= delta;
        delta
    }

    /// Reset viewport to the bottom (live terminal).
    pub fn reset_viewport(&mut self) {
        self.viewport.offset = 0;
    }

    /// Return images whose top-left falls within the current viewport,
    /// with screen-relative row/col positions.
    pub fn visible_images(&self) -> impl Iterator<Item = VisibleImage<'_>> {
        let viewport_top = self
            .grid
            .rows
            .line_len()
            .saturating_sub(self.viewport.rows as usize)
            .saturating_sub(self.viewport.offset as usize);
        let viewport_bottom = viewport_top + self.viewport.rows as usize;

        self.images.values().filter_map(move |img| {
            if img.row >= viewport_top && img.row < viewport_bottom {
                Some(VisibleImage {
                    image: &img.image,
                    id: img.id,
                    screen_row: (img.row - viewport_top) as u32,
                    screen_col: img.col,
                })
            } else {
                None
            }
        })
    }

    pub fn resize(
        &mut self,
        cols: u32,
        rows: u32,
    ) {
        // Trim trailing empty rows that accumulated from padding in previous
        // resizes, so content stays visible when the viewport shrinks.
        let cursor_abs = self.grid.active_row_index(&self.cursor, &self.viewport);
        while self.grid.rows.line_len() > cursor_abs + 1 {
            if self
                .grid
                .rows
                .line(self.grid.rows.line_len() - 1)
                .chars()
                .all(|c| c == ' ')
            {
                let start = self.grid.rows.byte_of_line(self.grid.rows.line_len() - 1);
                self.grid.clear_colors(start, self.grid.rows.byte_len());
                self.grid.rows.delete(start..);
            } else {
                break;
            }
        }
        self.viewport.rows = self.viewport.rows.min(self.grid.rows.line_len() as u32);
        let visible_start = self
            .grid
            .rows
            .line_len()
            .saturating_sub(self.viewport.rows as usize);
        self.cursor.row = cursor_abs.saturating_sub(visible_start) as u32;

        let old_cols = self.viewport.cols as usize;
        let new_cols = cols as usize;

        let max_rows = rows as usize + self.grid.scrollback_limit as usize;

        if new_cols != old_cols {
            let anchors = anchor_images(&self.grid.wrapped, &self.images);

            let old_distance_from_bottom = self
                .grid
                .rows
                .line_len()
                .saturating_sub(self.grid.active_row_index(&self.cursor, &self.viewport) + 1);

            self.grid.reflow(cols);

            while self.grid.rows.line_len() > max_rows {
                let eol = self.grid.rows.byte_of_line(1);
                self.grid.rows.delete(0..eol);
                self.grid.trim_colors_front(eol);
            }

            // Restore images and compute cursor position before padding so
            // that empty padding rows don't corrupt logical-line counts.
            restore_images(&self.grid.wrapped, &anchors, &mut self.images);

            let new_abs = self
                .grid
                .rows
                .line_len()
                .saturating_sub(old_distance_from_bottom + 1);

            // Pad at the back so content stays top-aligned when there is no
            // scrollback to reveal.
            let empty = " ".repeat(cols as usize);
            while self.grid.rows.line_len() < rows as usize {
                self.grid.rows.insert(self.grid.rows.byte_len(), &empty);
                self.grid.rows.insert(self.grid.rows.byte_len(), "\n");
            }

            let visible_start = self.grid.rows.line_len().saturating_sub(rows as usize);
            self.cursor.row = new_abs.saturating_sub(visible_start).min(rows as usize - 1) as u32;
            self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        } else {
            let old_len = self.grid.rows.line_len();
            let old_abs = self.grid.active_row_index(&self.cursor, &self.viewport);

            while self.grid.rows.line_len() > max_rows {
                let eol = self.grid.rows.byte_of_line(1);
                self.grid.rows.delete(0..eol);
                self.grid.trim_colors_front(eol);
            }

            let popped = old_len - self.grid.rows.line_len();

            // Pad at the back so content stays top-aligned.
            let empty = " ".repeat(cols as usize);
            while self.grid.rows.line_len() < rows as usize {
                self.grid.rows.insert(self.grid.rows.byte_len(), &empty);
                self.grid.rows.insert(self.grid.rows.byte_len(), "\n");
            }

            // Only front pops shift image indices; push_back is invisible.
            if popped > 0 {
                self.images.retain(|_, img| img.row >= popped);
                for img in self.images.values_mut() {
                    img.row -= popped;
                }
            }

            let new_abs = old_abs.saturating_sub(popped);
            let visible_start = self.grid.rows.line_len().saturating_sub(rows as usize);
            self.cursor.row = new_abs.saturating_sub(visible_start).min(rows as usize - 1) as u32;
        }

        self.viewport.cols = cols;
        self.viewport.rows = rows;
        self.viewport.offset = self
            .viewport
            .offset
            .min(self.grid.scrollback_len(&self.viewport));
        self.viewport.scroll_top = 0;
        self.viewport.scroll_bottom = rows - 1;
    }

    /// Process raw bytes from the PTY through the VTE parser.
    pub fn process(
        &mut self,
        data: &[u8],
    ) {
        for action in self.parser.parse(data) {
            let popped_before = self.grid.total_popped;

            match action {
                vte::Action::PrintByte(c) => put_char(
                    &mut self.cursor,
                    &mut self.grid,
                    &mut self.viewport,
                    &c.to_string(),
                    self.fg,
                    self.bg,
                ),
                vte::Action::Print(c) => put_char(
                    &mut self.cursor,
                    &mut self.grid,
                    &mut self.viewport,
                    &c,
                    self.fg,
                    self.bg,
                ),
                vte::Action::Execute(byte) => {
                    execute(&mut self.cursor, &mut self.grid, &self.viewport, byte)
                }
                vte::Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => csi_dispatch(
                    &mut self.cursor,
                    &mut self.grid,
                    &mut self.viewport,
                    &mut self.fg,
                    &mut self.bg,
                    &params,
                    intermediates.as_slice(),
                    action,
                ),
                vte::Action::EscDispatch {
                    intermediates,
                    byte,
                } => esc_dispatch(
                    &mut self.cursor,
                    &mut self.grid,
                    &mut self.viewport,
                    intermediates.as_slice(),
                    byte,
                ),
                vte::Action::OscDispatch => {}
                vte::Action::Hook { params, action } => {
                    self.hook_bytes.push(vec![]);
                    self.hook_params.push(params);
                    self.hook_action.push(action);
                }
                vte::Action::Put(byte) => {
                    if let Some(last) = self.hook_bytes.last_mut() {
                        last.push(byte);
                    }
                }
                vte::Action::Unhook => {
                    let bytes = self.hook_bytes.pop().unwrap();
                    let params = self.hook_params.pop().unwrap();
                    let action = self.hook_action.pop().unwrap();
                    match action {
                        'q' => {
                            let image = parse_sixel(params, bytes);
                            let id = self.next_image_id;
                            self.next_image_id += 1;
                            let row = self.grid.active_row_index(&self.cursor, &self.viewport);
                            let image_rows =
                                (image.height + self.cell_height - 1) / self.cell_height;
                            self.images.insert(
                                id,
                                PlacedImage {
                                    image,
                                    id,
                                    row,
                                    col: self.cursor.col,
                                },
                            );

                            // Advance cursor past the image, scrolling as needed.
                            for _ in 0..image_rows {
                                self.cursor.row += 1;
                                if self.cursor.row >= self.viewport.rows {
                                    self.grid.push_visible_row(&self.viewport);
                                    self.cursor.row = self.viewport.rows - 1;
                                }
                            }
                            self.cursor.col = 0;
                            self.viewport.offset = 0;
                        }
                        _ => {}
                    }
                }
            }

            let newly_popped = self.grid.total_popped - popped_before;
            if newly_popped > 0 {
                self.images.retain(|_, img| img.row >= newly_popped);
                for img in self.images.values_mut() {
                    img.row -= newly_popped;
                }
            }
        }
    }
}

fn put_char(
    cursor: &mut Cursor,
    grid: &mut Grid,
    viewport: &mut Viewport,
    ch: &str,
    fg: Srgb<u8>,
    bg: Srgb<u8>,
) {
    if cursor.col >= viewport.cols {
        // Soft wrap: mark the current row as a continuation.
        cursor.col = 0;
        let r = grid.active_row_index(cursor, viewport);
        if r < grid.wrapped.len() {
            grid.wrapped[r] = true;
        }
        if cursor.row == viewport.scroll_bottom {
            if viewport.scroll_top == 0 && viewport.scroll_bottom == viewport.rows - 1 {
                grid.push_visible_row(viewport);
            } else {
                grid.scroll_up_in_region(viewport, viewport.scroll_top, viewport.scroll_bottom, 1);
            }
        } else if cursor.row < viewport.rows - 1 {
            cursor.row += 1;
        }
    }

    // New output resets viewport to bottom.
    viewport.offset = 0;

    // Ensure the grid has enough lines for the cursor position.
    let r = grid.active_row_index(cursor, viewport);
    while r >= grid.rows.line_len() {
        let empty = " ".repeat(viewport.cols as usize);
        grid.rows.insert(grid.rows.byte_len(), &empty);
        grid.rows.insert(grid.rows.byte_len(), "\n");
        grid.wrapped.push_back(false);
    }
    let start = grid.rows.byte_of_line(r);
    let c = cursor.col as usize;

    let mut c_off = 0;
    let mut c_len = 0;
    for (idx, g) in grid.rows.line(r).graphemes().take(c + 1).enumerate() {
        if idx < c {
            c_off += g.len();
        } else {
            c_len += g.len();
        }
    }

    grid.rows.delete(start + c_off..start + c_off + c_len);
    grid.rows.insert(start + c_off, ch);

    let delta = ch.len() as isize - c_len as isize;
    if delta != 0 {
        grid.shift_colors(start + c_off + ch.len(), delta);
    }
    grid.set_color(start + c_off, CellColors { fg, bg });

    cursor.col += 1;
}

fn execute(
    cursor: &mut Cursor,
    grid: &mut Grid,
    viewport: &Viewport,
    byte: u8,
) {
    match byte {
        b'\n' => {
            if cursor.row == viewport.scroll_bottom {
                if viewport.scroll_top == 0 && viewport.scroll_bottom == viewport.rows - 1 {
                    grid.push_visible_row(viewport);
                } else {
                    grid.scroll_up_in_region(
                        viewport,
                        viewport.scroll_top,
                        viewport.scroll_bottom,
                        1,
                    );
                }
            } else if cursor.row < viewport.rows - 1 {
                cursor.row += 1;
            }
        }
        b'\r' => {
            cursor.col = 0;
        }
        0x08 => {
            cursor.col = cursor.col.saturating_sub(1);
        }
        b'\t' => {
            let next = (cursor.col / 8 + 1) * 8;
            cursor.col = next.min(viewport.cols - 1);
        }
        0x07 | 0x00 => {}
        _ => {}
    }
}

fn csi_dispatch(
    cursor: &mut Cursor,
    grid: &mut Grid,
    viewport: &mut Viewport,
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    params: &vte::Params,
    intermediates: &[u8],
    action: char,
) {
    if intermediates.contains(&b'?') {
        return;
    }

    if !intermediates.is_empty() {
        return;
    }

    let p: Vec<u16> = params.iter().map(|p| p[0]).collect();

    match action {
        'A' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = cursor.row.saturating_sub(n);
        }
        'B' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = (cursor.row + n).min(viewport.rows - 1);
        }
        'C' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = (cursor.col + n).min(viewport.cols - 1);
        }
        'D' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = cursor.col.saturating_sub(n);
        }
        'H' | 'f' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.row = row.min(viewport.rows - 1);
            cursor.col = col.min(viewport.cols - 1);
        }
        'J' => {
            let mode = p.first().copied().unwrap_or(0);
            grid.erase_in_display(cursor, viewport, mode);
        }
        'K' => {
            let mode = p.first().copied().unwrap_or(0);
            grid.erase_in_line(cursor, viewport, mode);
        }
        'm' => apply_sgr(fg, bg, params),
        'd' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.row = row.min(viewport.rows - 1);
        }
        'G' => {
            let col = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.col = col.min(viewport.cols - 1);
        }
        'L' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= viewport.scroll_top && cursor.row <= viewport.scroll_bottom {
                grid.scroll_down_in_region(viewport, cursor.row, viewport.scroll_bottom, n);
            }
        }
        'M' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= viewport.scroll_top && cursor.row <= viewport.scroll_bottom {
                grid.scroll_up_in_region(viewport, cursor.row, viewport.scroll_bottom, n);
            }
        }
        'P' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            grid.delete_chars(cursor, viewport, n);
        }
        '@' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            grid.insert_chars(cursor, viewport, n);
        }
        'X' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            grid.erase_chars(cursor, viewport, n);
        }
        'S' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if viewport.scroll_top == 0 && viewport.scroll_bottom == viewport.rows - 1 {
                for _ in 0..n {
                    grid.push_visible_row(viewport);
                }
            } else {
                grid.scroll_up_in_region(viewport, viewport.scroll_top, viewport.scroll_bottom, n);
            }
        }
        'T' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            grid.scroll_down_in_region(viewport, viewport.scroll_top, viewport.scroll_bottom, n);
        }
        'r' => {
            let top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let bottom = p.get(1).copied().unwrap_or(viewport.rows as u16).max(1) as u32 - 1;
            viewport.scroll_top = top.min(viewport.rows - 1);
            viewport.scroll_bottom = bottom.min(viewport.rows - 1).max(viewport.scroll_top);
            cursor.row = 0;
            cursor.col = 0;
        }
        'n' | 'c' => {}
        _ => {}
    }
}

fn esc_dispatch(
    cursor: &mut Cursor,
    grid: &mut Grid,
    viewport: &mut Viewport,
    intermediates: &[u8],
    byte: u8,
) {
    if intermediates.first().is_some_and(|&b| b"()*+".contains(&b)) {
        return;
    }

    match byte {
        b'c' => {
            todo!()
        }
        b'M' => {
            if cursor.row == viewport.scroll_top {
                grid.scroll_down_in_region(
                    viewport,
                    viewport.scroll_top,
                    viewport.scroll_bottom,
                    1,
                );
            } else if cursor.row > 0 {
                cursor.row -= 1;
            }
        }
        b'7' | b'8' => {}
        b'=' | b'>' => {}
        _ => {}
    }
}

/// Apply SGR (Select Graphic Rendition) parameters to the current fg/bg colors.
fn apply_sgr(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    params: &vte::Params,
) {
    let params: Vec<u16> = params.iter().map(|p| p[0]).collect();

    if params.is_empty() {
        *fg = default_fg();
        *bg = default_bg();
        return;
    }

    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => {
                *fg = default_fg();
                *bg = default_bg();
            }
            30..=37 => *fg = ansi_color((params[i] - 30) as u8),
            39 => *fg = default_fg(),
            40..=47 => *bg = ansi_color((params[i] - 40) as u8),
            49 => *bg = default_bg(),
            90..=97 => *fg = ansi_color((params[i] - 90 + 8) as u8),
            100..=107 => *bg = ansi_color((params[i] - 100 + 8) as u8),
            38 => {
                if let Some(color) = parse_extended_color(&params, &mut i) {
                    *fg = color;
                }
            }
            48 => {
                if let Some(color) = parse_extended_color(&params, &mut i) {
                    *bg = color;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn parse_extended_color(
    params: &[u16],
    i: &mut usize,
) -> Option<Srgb<u8>> {
    if *i + 1 >= params.len() {
        return None;
    }
    match params[*i + 1] {
        5 => {
            if *i + 2 < params.len() {
                *i += 2;
                Some(ansi_color(params[*i] as u8))
            } else {
                None
            }
        }
        2 => {
            if *i + 4 < params.len() {
                *i += 4;
                Some(Srgb::new(
                    params[*i - 2] as u8,
                    params[*i - 1] as u8,
                    params[*i] as u8,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a grid from `(text, wrapped)` pairs. Each row is padded to `width`
    /// with spaces.
    fn make_grid(
        width: u32,
        rows: &[(&str, bool)],
    ) -> Grid {
        let mut cells = RopeBuilder::new();
        let mut wrapped = VecDeque::new();

        for row in rows {
            cells.append(row.0);
            cells.append(" ".repeat(width as usize - row.0.len()));
            cells.append("\n");
            wrapped.push_back(row.1);
        }

        Grid {
            rows: cells.build(),
            wrapped,
            scrollback_limit: 1000,
            total_popped: 0,
            colors: BTreeMap::new(),
            bytes_popped: 0,
        }
    }

    // ── Reflow: grow with no wrapping ───────────────────────────────

    #[test]
    fn reflow_grow_no_wrapping() {
        let mut grid = make_grid(3, &[("abc", false), ("def", false)]);
        grid.reflow(5);
        assert_eq!(grid.rows.line(0), "abc  ");
        assert_eq!(grid.rows.line(1), "def  ");
        assert!(!grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_same_width_is_noop() {
        let mut grid = make_grid(4, &[("abcd", false), ("efgh", false)]);
        grid.reflow(4);
        assert_eq!(grid.rows.line(0), "abcd");
        assert_eq!(grid.rows.line(1), "efgh");
        assert_eq!(grid.rows.line_len(), 2);
    }

    // ── Reflow: grow merges wrapped rows ────────────────────────────

    #[test]
    fn reflow_grow_merges_two_wrapped_rows() {
        // "abcdef" soft-wrapped at width 3 into two rows.
        let mut grid = make_grid(
            3,
            &[
                ("abc", true),
                ("def", false), // continuation
            ],
        );
        // Growing to 6 should merge them into one row.
        grid.reflow(6);
        assert_eq!(grid.rows.line(0), "abcdef");
        assert!(!grid.wrapped[0]);
        assert_eq!(grid.rows.line_len(), 1);
    }

    #[test]
    fn reflow_grow_merges_three_wrapped_rows() {
        // "abcdefghi" soft-wrapped at width 3.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(9);
        assert_eq!(grid.rows.line(0), "abcdefghi");
        assert_eq!(grid.rows.line_len(), 1);
    }

    #[test]
    fn reflow_grow_partial_merge() {
        // "abcdefghi" at width 3, grow to 5.
        // Should become two rows: "abcde" / "fghi_".
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", true)]);
        grid.reflow(5);
        assert_eq!(grid.rows.line(0), "abcde");
        assert_eq!(grid.rows.line(1), "fghi ");
        assert!(grid.wrapped[0]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_grow_mixed_wrapped_and_unwrapped() {
        // Two logical lines: "abcdef" (wrapped) then "ghi" (not wrapped).
        let mut grid = make_grid(3, &[("abc", true), ("def", false), ("ghi", false)]);
        grid.reflow(6);
        assert_eq!(grid.rows.line(0), "abcdef");
        assert_eq!(grid.rows.line(1), "ghi   ");
        assert!(!grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_grow_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), then standalone "xx", then "ghijkl" (wrapped).
        let mut grid = make_grid(
            3,
            &[
                ("abc", true),
                ("def", false),
                ("xx ", false),
                ("ghi", true),
                ("jkl", false),
            ],
        );
        grid.reflow(6);
        assert_eq!(grid.rows.line(0), "abcdef");
        assert_eq!(grid.rows.line(1), "xx    ");
        assert_eq!(grid.rows.line(2), "ghijkl");
        assert_eq!(grid.rows.line_len(), 3);
    }

    // ── Reflow: single row ──────────────────────────────────────────

    #[test]
    fn reflow_single_row_grow() {
        let mut grid = make_grid(3, &[("abc", false)]);
        grid.reflow(6);
        assert_eq!(grid.rows.line(0), "abc   ");
        assert_eq!(grid.rows.line_len(), 1);
    }

    // ── Reflow: shrink splits rows ─────────────────────────────────

    #[test]
    fn reflow_shrink_no_content_overflow() {
        // "abc" and "def" padded to width 6; trailing spaces discarded.
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        assert!(!grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_full_row() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        assert!(grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_into_three() {
        let mut grid = make_grid(9, &[("abcdefghi", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        assert_eq!(grid.rows.line(2), "ghi");
        assert!(grid.wrapped[0]);
        assert!(grid.wrapped[1]);
        assert!(!grid.wrapped[2]);
        assert_eq!(grid.rows.line_len(), 3);
    }

    #[test]
    fn reflow_shrink_two_logical_lines() {
        let mut grid = make_grid(6, &[("abcdef", false), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        assert_eq!(grid.rows.line(2), "ghi");
        assert_eq!(grid.rows.line(3), "jkl");
        assert!(grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert!(grid.wrapped[2]);
        assert!(!grid.wrapped[3]);
        assert_eq!(grid.rows.line_len(), 4);
    }

    #[test]
    fn reflow_shrink_already_wrapped() {
        // "abcdefghijkl" soft-wrapped at width 6, shrink to 3.
        let mut grid = make_grid(6, &[("abcdef", true), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        assert_eq!(grid.rows.line(2), "ghi");
        assert_eq!(grid.rows.line(3), "jkl");
        assert!(grid.wrapped[0]);
        assert!(grid.wrapped[1]);
        assert!(grid.wrapped[2]);
        assert!(!grid.wrapped[3]);
        assert_eq!(grid.rows.line_len(), 4);
    }

    #[test]
    fn reflow_shrink_uneven_split() {
        // 5 chars into width 3: "abcde" -> "abc" + "de "
        let mut grid = make_grid(5, &[("abcde", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "de ");
        assert!(grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_shrink_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), standalone "xx", "ghijkl" (wrapped).
        let mut grid = make_grid(
            6,
            &[("abcdef", false), ("xx    ", false), ("ghijkl", false)],
        );
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        assert_eq!(grid.rows.line(2), "xx ");
        assert_eq!(grid.rows.line(3), "ghi");
        assert_eq!(grid.rows.line(4), "jkl");
        assert!(grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert!(!grid.wrapped[2]);
        assert!(grid.wrapped[3]);
        assert!(!grid.wrapped[4]);
        assert_eq!(grid.rows.line_len(), 5);
    }

    // ── Reflow: trailing space stripping ────────────────────────────

    #[test]
    fn reflow_grow_strips_trailing_spaces() {
        // "ab" with trailing padding on a wrapped row, then "cd".
        let mut grid = make_grid(5, &[("ab   ", true), ("cd   ", false)]);
        grid.reflow(10);
        assert_eq!(grid.rows.line(0), "ab   cd   ");
        assert!(!grid.wrapped[0]);
        assert_eq!(grid.rows.line_len(), 1);
    }

    #[test]
    fn reflow_shrink_drops_trailing_space_overflow() {
        // Wrapped row where overflow portion is all spaces — no split needed.
        let mut grid = make_grid(6, &[("abc   ", true), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "   ");
        assert_eq!(grid.rows.line(2), "def");
        assert!(grid.wrapped[0]);
        assert!(grid.wrapped[1]);
        assert!(!grid.wrapped[2]);
        assert_eq!(grid.rows.line_len(), 3);
    }

    #[test]
    fn reflow_shrink_grow_maintains_space() {
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        grid.reflow(6);
        assert_eq!(grid.rows.line(0), "abc   ");
        assert_eq!(grid.rows.line(1), "def   ");
        assert!(!grid.wrapped[0]);
        assert!(!grid.wrapped[1]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    #[test]
    fn reflow_mixed_wrapping_shrink_then_grow() {
        // Three logical lines at width 8:
        //   "Hi"                 — short unwrapped
        //   "ABCDEFGHIJKLMNOP"   — 16-char wrapped across two rows
        //   "Bye"                — short unwrapped
        let mut grid = make_grid(
            8,
            &[
                ("Hi      ", false),
                ("ABCDEFGH", true),
                ("IJKLMNOP", false),
                ("Bye     ", false),
            ],
        );

        // Shrink 8 → 4: long rows split, short rows truncate padding.
        grid.reflow(4);
        assert_eq!(grid.rows.line(0), "Hi  ");
        assert!(!grid.wrapped[0]);
        assert_eq!(grid.rows.line(1), "ABCD");
        assert!(grid.wrapped[1]);
        assert_eq!(grid.rows.line(2), "EFGH");
        assert!(grid.wrapped[2]);
        assert_eq!(grid.rows.line(3), "IJKL");
        assert!(grid.wrapped[3]);
        assert_eq!(grid.rows.line(4), "MNOP");
        assert!(!grid.wrapped[4]);
        assert_eq!(grid.rows.line(5), "Bye ");
        assert!(!grid.wrapped[5]);
        assert_eq!(grid.rows.line_len(), 6);

        // Grow 4 → 6: wrapped chains partially re-merge.
        // 16 chars at width 6 = three rows: 6 + 6 + 4.
        grid.reflow(6);
        assert_eq!(grid.rows.line(0), "Hi    ");
        assert!(!grid.wrapped[0]);
        assert_eq!(grid.rows.line(1), "ABCDEF");
        assert!(grid.wrapped[1]);
        assert_eq!(grid.rows.line(2), "GHIJKL");
        assert!(grid.wrapped[2]);
        assert_eq!(grid.rows.line(3), "MNOP  ");
        assert!(!grid.wrapped[3]);
        assert_eq!(grid.rows.line(4), "Bye   ");
        assert!(!grid.wrapped[4]);
        assert_eq!(grid.rows.line_len(), 5);
    }

    #[test]
    fn reflow_mixed_wrapping_roundtrip() {
        // Shrink then grow back to original width with mixed lines.
        //   "Hi"             — short unwrapped
        //   "abcdefghijkl"   — 12-char wrapped across two rows
        //   "Lo"             — short unwrapped
        let mut grid = make_grid(
            6,
            &[
                ("Hi    ", false),
                ("abcdef", true),
                ("ghijkl", false),
                ("Lo    ", false),
            ],
        );

        // Shrink 6 → 3: 12-char line becomes 4 rows.
        grid.reflow(3);
        assert_eq!(grid.rows.line_len(), 6);
        assert_eq!(grid.rows.line(0), "Hi ");
        assert!(!grid.wrapped[0]);
        assert_eq!(grid.rows.line(1), "abc");
        assert!(grid.wrapped[1]);
        assert_eq!(grid.rows.line(2), "def");
        assert!(grid.wrapped[2]);
        assert_eq!(grid.rows.line(3), "ghi");
        assert!(grid.wrapped[3]);
        assert_eq!(grid.rows.line(4), "jkl");
        assert!(!grid.wrapped[4]);
        assert_eq!(grid.rows.line(5), "Lo ");
        assert!(!grid.wrapped[5]);

        // Grow 3 → 6: should roundtrip back to original.
        grid.reflow(6);
        assert_eq!(grid.rows.line_len(), 4);
        assert_eq!(grid.rows.line(0), "Hi    ");
        assert!(!grid.wrapped[0]);
        assert_eq!(grid.rows.line(1), "abcdef");
        assert!(grid.wrapped[1]);
        assert_eq!(grid.rows.line(2), "ghijkl");
        assert!(!grid.wrapped[2]);
        assert_eq!(grid.rows.line(3), "Lo    ");
        assert!(!grid.wrapped[3]);
    }

    #[test]
    fn reflow_shrink_grow_roundtrip_with_trailing_spaces() {
        // Shrink then grow should recover original content, modulo trailing spaces.
        let mut grid = make_grid(10, &[("hello     ", true), ("world     ", false)]);
        grid.reflow(5);
        grid.reflow(10);
        assert_eq!(grid.rows.line(0), "hello     ");
        assert_eq!(grid.rows.line(1), "world     ");
        assert!(grid.wrapped[0]);
        assert_eq!(grid.rows.line_len(), 2);
    }

    // ── Color map ──────────────────────────────────────────────────

    const RED: Srgb<u8> = Srgb::new(255, 0, 0);
    const BLUE: Srgb<u8> = Srgb::new(0, 0, 255);

    #[test]
    fn color_set_and_get() {
        let mut grid = make_grid(4, &[("abcd", false)]);
        let c = CellColors {
            fg: RED,
            bg: default_bg(),
        };
        grid.set_color(0, c);
        assert_eq!(grid.get_color(0), c);
        // Unset position returns default.
        assert_eq!(grid.get_color(1), CellColors::default());
    }

    #[test]
    fn color_default_is_not_stored() {
        let mut grid = make_grid(4, &[("abcd", false)]);
        grid.set_color(0, CellColors::default());
        assert!(grid.colors.is_empty());
    }

    #[test]
    fn color_clear_range() {
        let mut grid = make_grid(4, &[("abcd", false)]);
        let c = CellColors {
            fg: RED,
            bg: default_bg(),
        };
        grid.set_color(0, c);
        grid.set_color(1, c);
        grid.set_color(2, c);
        grid.clear_colors(0, 2);
        // Only byte offset 2 survives.
        assert_eq!(grid.get_color(0), CellColors::default());
        assert_eq!(grid.get_color(1), CellColors::default());
        assert_eq!(grid.get_color(2), c);
    }

    #[test]
    fn color_shift_after() {
        let mut grid = make_grid(4, &[("abcd", false)]);
        let c = CellColors {
            fg: RED,
            bg: default_bg(),
        };
        grid.set_color(2, c);
        // Simulate inserting 1 byte at offset 1 — entries at >=1 shift right.
        grid.shift_colors(1, 1);
        assert_eq!(grid.get_color(2), CellColors::default());
        assert_eq!(grid.get_color(3), c);
    }

    #[test]
    fn color_trim_front() {
        let mut grid = make_grid(4, &[("abcd", false), ("efgh", false)]);
        let c = CellColors {
            fg: RED,
            bg: default_bg(),
        };
        // "abcd\nefgh\n" — line 1 starts at byte 5.
        grid.set_color(0, c);
        grid.set_color(5, c);
        // Trim first line (5 bytes: "abcd\n").
        grid.trim_colors_front(5);
        // Old byte 0 is gone; old byte 5 is now rope offset 0.
        assert_eq!(grid.get_color(0), c);
        assert!(grid.colors.len() == 1);
    }

    #[test]
    fn color_survives_reflow() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        let c = CellColors {
            fg: RED,
            bg: default_bg(),
        };
        // Color 'a' (byte 0) and 'd' (byte 3).
        grid.set_color(0, c);
        grid.set_color(3, c);
        // Shrink to width 3: "abc" (wrapped) + "def".
        grid.reflow(3);
        assert_eq!(grid.rows.line(0), "abc");
        assert_eq!(grid.rows.line(1), "def");
        // 'a' is at byte 0 of new rope.
        assert_eq!(grid.get_color(0), c);
        // 'd' is at byte 0 of line 1 = byte 4 ("abc\n" = 4 bytes).
        assert_eq!(grid.get_color(4), c);
        // 'b' (byte 1) has no color.
        assert_eq!(grid.get_color(1), CellColors::default());
    }

    #[test]
    fn color_reflow_roundtrip() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        let ca = CellColors {
            fg: RED,
            bg: default_bg(),
        };
        let cd = CellColors {
            fg: BLUE,
            bg: default_bg(),
        };
        grid.set_color(0, ca);
        grid.set_color(3, cd);
        grid.reflow(3);
        grid.reflow(6);
        // After roundtrip, 'a' is back at byte 0, 'd' at byte 3.
        assert_eq!(grid.get_color(0), ca);
        assert_eq!(grid.get_color(3), cd);
    }
}
