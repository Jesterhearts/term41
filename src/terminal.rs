use std::collections::BTreeMap;
use std::collections::VecDeque;

use palette::Srgb;

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
    fn new(cols: u32) -> Self {
        let n = cols as usize;
        Self {
            chars: vec![' '; n],
            fg: vec![default_fg(); n],
            bg: vec![default_bg(); n],
            wrapped: false,
        }
    }

    fn len(&self) -> u32 {
        self.chars.len() as u32
    }

    fn content_len(&self) -> u32 {
        self.chars
            .iter()
            .rposition(|c| *c != ' ')
            .map_or(0, |p| p + 1) as u32
    }

    fn resize(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.chars.resize(new_len, ' ');
        self.fg.resize(new_len, default_fg());
        self.bg.resize(new_len, default_bg());
    }

    fn truncate(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.chars.truncate(new_len);
        self.fg.truncate(new_len);
        self.bg.truncate(new_len);
    }

    fn clear(&mut self) {
        self.clear_range(0..self.chars.len())
    }

    fn clear_range(
        &mut self,
        range: std::ops::Range<usize>,
    ) {
        self.chars[range.clone()].fill(' ');
        self.fg[range.clone()].fill(default_fg());
        self.bg[range].fill(default_bg());
    }

    fn copy_within(
        &mut self,
        src: std::ops::Range<usize>,
        dest: usize,
    ) {
        self.chars.copy_within(src.clone(), dest);
        self.fg.copy_within(src.clone(), dest);
        self.bg.copy_within(src.clone(), dest);
    }

    fn copy_from(
        &mut self,
        other: &Self,
        src_offset: usize,
        dest_offset: usize,
    ) -> usize {
        let copy_len = ((other.content_len() as usize).saturating_sub(src_offset))
            .min((self.content_len() as usize).saturating_sub(dest_offset));
        self.chars[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.chars[src_offset..src_offset + copy_len]);
        self.fg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.fg[src_offset..src_offset + copy_len]);
        self.bg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.bg[src_offset..src_offset + copy_len]);

        copy_len
    }
}

#[derive(Debug, Default)]
pub struct Cursor {
    pub col: u32,
    pub row: u32,
}

#[derive(Debug)]
pub struct Grid {
    pub rows: VecDeque<Row>,
    pub scrollback_limit: u32,
    /// Running count of rows popped from the front (for image position
    /// tracking).
    pub total_popped: usize,
}

impl Grid {
    pub fn scrollback_len(
        &self,
        viewport: &Viewport,
    ) -> u32 {
        (self.rows.len() as u32).saturating_sub(viewport.rows)
    }

    pub fn push_visible_row(
        &mut self,
        viewport: &Viewport,
    ) {
        self.rows.push_back(Row::new(viewport.cols));

        let max_rows = viewport.rows as usize + self.scrollback_limit as usize;
        if self.rows.len() > max_rows {
            self.rows.pop_front();
            self.total_popped += 1;
        }
    }

    pub fn erase_in_display(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let first_visible = self.rows.len() - viewport.rows as usize;
        let col = cursor.col as usize;

        match mode {
            // Erase from cursor to end of screen.
            0 => {
                let cols = self.rows[active].chars.len();
                self.rows[active].clear_range(col..cols);
                for r in (active + 1)..self.rows.len() {
                    self.rows[r].clear();
                }
            }
            // Erase from start of screen to cursor (inclusive).
            1 => {
                for r in first_visible..active {
                    self.rows[r].clear();
                }
                self.rows[active].clear_range(0..col + 1);
            }
            // Erase entire screen.
            2 => {
                for r in first_visible..self.rows.len() {
                    self.rows[r].clear();
                }
            }
            // Erase scrollback buffer.
            3 => {
                self.total_popped += first_visible;
                self.rows.drain(0..first_visible);
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
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;

        match mode {
            // Erase from cursor to end of line.
            0 => self.rows[active].clear_range(col..cols),
            // Erase from start of line to cursor (inclusive).
            1 => self.rows[active].clear_range(0..col + 1),
            // Erase entire line.
            2 => self.rows[active].clear(),
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
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;
        let count = (n as usize).min(cols - col);

        self.rows[active].copy_within(col + count..cols, col);
        self.rows[active].clear_range(cols - count..cols);
    }

    pub fn insert_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;
        let count = (n as usize).min(cols - col);

        self.rows[active].copy_within(col..cols - count, col + count);
        self.rows[active].clear_range(col..col + count);
    }

    pub fn erase_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;
        let end = (col + n as usize).min(cols);

        self.rows[active].clear_range(col..end);
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
        let first_visible = self.rows.len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        for _ in 0..n {
            self.rows.remove(abs_top);
            self.rows.insert(abs_bottom, Row::new(viewport.cols));
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
        let first_visible = self.rows.len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        for _ in 0..n {
            self.rows.remove(abs_bottom);
            self.rows.insert(abs_top, Row::new(viewport.cols));
        }
    }

    pub fn active_row_index(
        &self,
        cursor: &Cursor,
        viewport: &Viewport,
    ) -> usize {
        self.rows.len() - viewport.rows as usize + cursor.row as usize
    }

    fn reflow(
        &mut self,
        new_width: u32,
    ) {
        if self.rows.len() == 0 {
            return;
        }

        let old_width = self.rows[0].len();
        if old_width == new_width {
            return;
        }

        if new_width > old_width {
            let mut row = 0;
            while row < self.rows.len() as u32 {
                self.rows[row as usize].resize(new_width);

                if self.rows[row as usize].wrapped {
                    let (advanced, skipped) = self.reflow_soft_grow(row, old_width, new_width);
                    let skipped = skipped.unwrap_or_default();
                    row += advanced;

                    for _ in 0..skipped {
                        self.rows.remove(row as usize + 1);
                    }
                } else {
                    row += 1;
                }
            }
        } else {
            let mut row = 0;
            while row < self.rows.len() {
                if self.rows[row].len() > new_width {
                    let was_wrapped = self.rows[row].wrapped;

                    let has_content_overflow = self.rows[row].chars[new_width as usize..]
                        .iter()
                        .any(|&c| c != ' ');

                    if has_content_overflow {
                        let overflow = Row {
                            chars: self.rows[row].chars.split_off(new_width as usize),
                            fg: self.rows[row].fg.split_off(new_width as usize),
                            bg: self.rows[row].bg.split_off(new_width as usize),
                            wrapped: was_wrapped,
                        };

                        self.rows[row].wrapped = true;
                        self.rows.insert(row + 1, overflow);
                    } else {
                        self.rows[row].truncate(new_width);
                    }
                } else {
                    self.rows[row].resize(new_width);
                }
                row += 1;
            }
        }
    }

    fn reflow_soft_grow(
        &mut self,
        row: u32,
        old_width: u32,
        new_width: u32,
    ) -> (u32, Option<u32>) {
        let old_row = row;

        let delta = new_width - old_width;

        let mut dest_col = old_width;
        let mut row = row as usize;
        let mut next = row + 1;
        while next < self.rows.len() && self.rows[row].wrapped {
            let (front, back) = self.rows.as_mut_slices();
            let current_row;
            let next_row;

            if row < front.len() && next >= front.len() {
                next_row = &mut back[next - front.len()];
                current_row = &mut front[row];
            } else if row < front.len() && next < front.len() {
                let (first, second) = front.split_at_mut(next);
                next_row = &mut second[0];
                current_row = &mut first[row];
            } else {
                let (first, second) = back.split_at_mut(next - front.len());
                next_row = &mut second[0];
                current_row = &mut first[row - front.len()];
            }

            let copied = current_row.copy_from(next_row, 0, dest_col as usize);
            next_row.resize(new_width);
            next_row.copy_within(copied as usize..new_width as usize, 0);

            if delta >= dest_col {
                // We have a delta that spills into the next row, so advance next and
                // keep row here
                self.rows[row].wrapped = self.rows[next].wrapped;
                next += 1;
                dest_col = new_width - dest_col;
            } else {
                dest_col -= delta;
                row += 1;
                next += 1;
            }
        }

        self.rows[row].wrapped = false;
        (row as u32 - old_row, Some((next - row - 1) as u32))
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
    rows: &VecDeque<Row>,
    images: &BTreeMap<u64, PlacedImage>,
) -> Vec<(u64, usize, usize)> {
    images
        .values()
        .map(|img| {
            let lines_below = (img.row + 1..rows.len())
                .filter(|&r| !rows[r].wrapped)
                .count();

            let mut row_offset = 0;
            let mut r = img.row;
            while r > 0 && rows[r].wrapped {
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
    rows: &VecDeque<Row>,
    anchors: &[(u64, usize, usize)],
    images: &mut BTreeMap<u64, PlacedImage>,
) {
    for &(id, lines_below, row_offset) in anchors {
        let mut count = 0;
        let mut found = None;
        for r in (0..rows.len()).rev() {
            if r == 0 || !rows[r].wrapped {
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
                while end < rows.len() && rows[end].wrapped {
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
        let mut cells = VecDeque::with_capacity(rows as usize + scrollback_limit as usize);
        for _ in 0..rows {
            cells.push_back(Row::new(cols));
        }

        Self {
            grid: Grid {
                rows: cells,
                scrollback_limit,
                total_popped: 0,
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
    ) -> &Row {
        let base =
            self.grid.rows.len() - self.viewport.rows as usize - self.viewport.offset as usize;
        &self.grid.rows[base + screen_row as usize]
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
        let viewport_top =
            self.grid.rows.len() - self.viewport.rows as usize - self.viewport.offset as usize;
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
        while self.grid.rows.len() > cursor_abs + 1 {
            if self
                .grid
                .rows
                .back()
                .map_or(false, |r| r.chars.iter().all(|&c| c == ' '))
            {
                self.grid.rows.pop_back();
            } else {
                break;
            }
        }
        self.viewport.rows = self.viewport.rows.min(self.grid.rows.len() as u32);
        let visible_start = self
            .grid
            .rows
            .len()
            .saturating_sub(self.viewport.rows as usize);
        self.cursor.row = cursor_abs.saturating_sub(visible_start) as u32;

        let old_cols = self.viewport.cols as usize;
        let new_cols = cols as usize;

        let max_rows = rows as usize + self.grid.scrollback_limit as usize;

        if new_cols != old_cols {
            let anchors = anchor_images(&self.grid.rows, &self.images);

            let old_distance_from_bottom = self
                .grid
                .rows
                .len()
                .saturating_sub(self.grid.active_row_index(&self.cursor, &self.viewport) + 1);

            self.grid.reflow(cols);

            while self.grid.rows.len() > max_rows {
                self.grid.rows.pop_front();
            }

            // Restore images and compute cursor position before padding so
            // that empty padding rows don't corrupt logical-line counts.
            restore_images(&self.grid.rows, &anchors, &mut self.images);

            let new_abs = self
                .grid
                .rows
                .len()
                .saturating_sub(old_distance_from_bottom + 1);

            // Pad at the back so content stays top-aligned when there is no
            // scrollback to reveal.
            while self.grid.rows.len() < rows as usize {
                self.grid.rows.push_back(Row::new(cols));
            }

            let visible_start = self.grid.rows.len().saturating_sub(rows as usize);
            self.cursor.row = new_abs.saturating_sub(visible_start).min(rows as usize - 1) as u32;
            self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        } else {
            let old_len = self.grid.rows.len();
            let old_abs = self.grid.active_row_index(&self.cursor, &self.viewport);

            while self.grid.rows.len() > max_rows {
                self.grid.rows.pop_front();
            }

            let popped = old_len - self.grid.rows.len();

            // Pad at the back so content stays top-aligned.
            while self.grid.rows.len() < rows as usize {
                self.grid.rows.push_back(Row::new(cols));
            }

            // Only front pops shift image indices; push_back is invisible.
            if popped > 0 {
                self.images.retain(|_, img| img.row >= popped);
                for img in self.images.values_mut() {
                    img.row -= popped;
                }
            }

            let new_abs = old_abs.saturating_sub(popped);
            let visible_start = self.grid.rows.len().saturating_sub(rows as usize);
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
                vte::Action::Print(c) => put_char(
                    &mut self.cursor,
                    &mut self.grid,
                    &mut self.viewport,
                    c,
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
    ch: char,
    fg: Srgb<u8>,
    bg: Srgb<u8>,
) {
    if cursor.col >= viewport.cols {
        // Soft wrap: mark the current row as a continuation.
        cursor.col = 0;
        let r = grid.active_row_index(cursor, viewport);
        grid.rows[r].wrapped = true;
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

    let r = grid.active_row_index(cursor, viewport);
    let c = cursor.col as usize;
    grid.rows[r].chars[c] = ch;
    grid.rows[r].fg[c] = fg;
    grid.rows[r].bg[c] = bg;
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
        let mut grid_rows = VecDeque::new();
        for &(text, wrapped) in rows {
            let mut row = Row::new(width);
            for (i, ch) in text.chars().enumerate() {
                if i < width as usize {
                    row.chars[i] = ch;
                }
            }
            row.wrapped = wrapped;
            grid_rows.push_back(row);
        }
        Grid {
            rows: grid_rows,
            scrollback_limit: 1000,
            total_popped: 0,
        }
    }

    fn row_chars(row: &Row) -> String {
        row.chars.iter().collect()
    }

    // ── Row unit tests ──────────────────────────────────────────────

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
        dst.copy_from(&src, 0, 2);
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
        dst.copy_from(&src, 2, 0);
        assert_eq!(row_chars(&dst), "cd   ");
    }

    // ── Reflow: grow with no wrapping ───────────────────────────────

    #[test]
    fn reflow_grow_no_wrapping() {
        let mut grid = make_grid(3, &[("abc", false), ("def", false)]);
        grid.reflow(5);
        assert_eq!(row_chars(&grid.rows[0]), "abc  ");
        assert_eq!(row_chars(&grid.rows[1]), "def  ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_same_width_is_noop() {
        let mut grid = make_grid(4, &[("abcd", false), ("efgh", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert_eq!(grid.rows.len(), 2);
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
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_merges_three_wrapped_rows() {
        // "abcdefghi" soft-wrapped at width 3.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(9);
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghi");
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_partial_merge() {
        // "abcdefghi" at width 3, grow to 5.
        // Should become two rows: "abcde" / "fghi_".
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", true)]);
        grid.reflow(5);
        assert_eq!(row_chars(&grid.rows[0]), "abcde");
        assert_eq!(row_chars(&grid.rows[1]), "fghi ");
        assert!(grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_grow_mixed_wrapped_and_unwrapped() {
        // Two logical lines: "abcdef" (wrapped) then "ghi" (not wrapped).
        let mut grid = make_grid(3, &[("abc", true), ("def", false), ("ghi", false)]);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert_eq!(row_chars(&grid.rows[1]), "ghi   ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
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
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert_eq!(row_chars(&grid.rows[1]), "xx    ");
        assert_eq!(row_chars(&grid.rows[2]), "ghijkl");
        assert_eq!(grid.rows.len(), 3);
    }

    // ── Reflow: single row ──────────────────────────────────────────

    #[test]
    fn reflow_single_row_grow() {
        let mut grid = make_grid(3, &[("abc", false)]);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abc   ");
        assert_eq!(grid.rows.len(), 1);
    }

    // ── Reflow: shrink splits rows ─────────────────────────────────

    #[test]
    fn reflow_shrink_no_content_overflow() {
        // "abc" and "def" padded to width 6; trailing spaces discarded.
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_full_row() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_into_three() {
        let mut grid = make_grid(9, &[("abcdefghi", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_two_logical_lines() {
        let mut grid = make_grid(6, &[("abcdef", false), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert!(grid.rows[2].wrapped);
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_already_wrapped() {
        // "abcdefghijkl" soft-wrapped at width 6, shrink to 3.
        let mut grid = make_grid(6, &[("abcdef", true), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(grid.rows[2].wrapped);
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_uneven_split() {
        // 5 chars into width 3: "abcde" -> "abc" + "de "
        let mut grid = make_grid(5, &[("abcde", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "de ");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), standalone "xx", "ghijkl" (wrapped).
        let mut grid = make_grid(
            6,
            &[("abcdef", false), ("xx    ", false), ("ghijkl", false)],
        );
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "xx ");
        assert_eq!(row_chars(&grid.rows[3]), "ghi");
        assert_eq!(row_chars(&grid.rows[4]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert!(grid.rows[3].wrapped);
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);
    }

    // ── Reflow: trailing space stripping ────────────────────────────

    #[test]
    fn reflow_grow_strips_trailing_spaces() {
        // "ab" with trailing padding on a wrapped row, then "cd".
        let mut grid = make_grid(5, &[("ab   ", true), ("cd   ", false)]);
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "abcd      ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_shrink_drops_trailing_space_overflow() {
        // Wrapped row where overflow portion is all spaces — no split needed.
        let mut grid = make_grid(6, &[("abc   ", true), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_grow_roundtrip_with_trailing_spaces() {
        // Shrink then grow should recover original content, modulo trailing spaces.
        let mut grid = make_grid(10, &[("hello     ", true), ("world     ", false)]);
        grid.reflow(5);
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "helloworld");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }
}
