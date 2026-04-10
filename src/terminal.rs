use std::collections::VecDeque;

use crate::sixel;

/// RGB color.
#[derive(Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(
        r: u8,
        g: u8,
        b: u8,
    ) -> Self {
        Self { r, g, b }
    }

    pub const fn default_fg() -> Self {
        Self::new(204, 204, 204)
    }

    pub const fn default_bg() -> Self {
        Self::new(0, 0, 0)
    }
}

/// The standard 256-color palette.
fn ansi_color(index: u8) -> Color {
    match index {
        0 => Color::new(0, 0, 0),
        1 => Color::new(205, 0, 0),
        2 => Color::new(0, 205, 0),
        3 => Color::new(205, 205, 0),
        4 => Color::new(0, 0, 238),
        5 => Color::new(205, 0, 205),
        6 => Color::new(0, 205, 205),
        7 => Color::new(229, 229, 229),
        8 => Color::new(127, 127, 127),
        9 => Color::new(255, 0, 0),
        10 => Color::new(0, 255, 0),
        11 => Color::new(255, 255, 0),
        12 => Color::new(92, 92, 255),
        13 => Color::new(255, 0, 255),
        14 => Color::new(0, 255, 255),
        15 => Color::new(255, 255, 255),
        16..=231 => {
            let idx = index - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            let to_val = |c: u8| if c == 0 { 0 } else { 55 + 40 * c };
            Color::new(to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Color::new(v, v, v)
        }
    }
}

/// A sixel image placed at a specific position in the terminal grid.
pub struct PlacedImage {
    pub id: u64,
    pub col: u16,
    pub row_at_placement: u16,
    pub scroll_at_placement: u64,
    pub image: sixel::SixelImage,
}

/// A terminal row stored as struct-of-arrays for cache-friendly access.
/// The renderer can borrow `&[char]` directly for shaping without copying.
pub struct Row {
    pub chars: Vec<char>,
    pub fg: Vec<Color>,
    pub bg: Vec<Color>,
    /// True if this row is a continuation of the previous row (soft wrap).
    pub wrapped: bool,
}

impl Row {
    fn new(cols: u16) -> Self {
        let n = cols as usize;
        Self {
            chars: vec![' '; n],
            fg: vec![Color::default_fg(); n],
            bg: vec![Color::default_bg(); n],
            wrapped: false,
        }
    }

    fn clear_range(
        &mut self,
        range: std::ops::Range<usize>,
    ) {
        self.chars[range.clone()].fill(' ');
        self.fg[range.clone()].fill(Color::default_fg());
        self.bg[range].fill(Color::default_bg());
    }

    fn copy_within(
        &mut self,
        src: std::ops::Range<usize>,
        dest: usize,
    ) {
        self.chars.copy_within(src.clone(), dest);
        self.fg.copy_within(src.clone(), dest);
        self.bg.copy_within(src, dest);
    }
}

/// Terminal state: a grid of rows plus cursor position and attributes.
///
/// The grid contains both scrollback history and the visible area.
/// Visible rows are the last `rows` entries in the grid. Scrollback
/// rows sit before them, capped at `scrollback_limit`.
pub struct Terminal {
    pub cols: u16,
    pub rows: u16,
    pub grid: VecDeque<Row>,
    pub cursor_col: u16,
    pub cursor_row: u16,
    fg: Color,
    bg: Color,
    parser: vte::Parser,
    sixel_parser: Option<sixel::SixelParser>,
    pub images: Vec<PlacedImage>,
    pub scroll_count: u64,
    next_image_id: u64,
    cell_height: u32,
    scrollback_limit: u32,
    /// How many rows the viewport is scrolled back from the bottom.
    /// 0 = viewing the live terminal. Positive = scrolled into history.
    pub viewport_offset: u32,
}

/// A logical line: the joined content of one or more soft-wrapped rows.
struct LogicalLine {
    chars: Vec<char>,
    fg: Vec<Color>,
    bg: Vec<Color>,
}

/// Reflow a grid to a new column width.
/// Joins soft-wrapped rows into logical lines, then re-wraps them.
fn reflow_grid(
    old_grid: VecDeque<Row>,
    new_cols: usize,
) -> VecDeque<Row> {
    // Phase 1: collect logical lines by joining soft-wrapped rows.
    let mut lines: Vec<LogicalLine> = Vec::new();
    for row in old_grid {
        if row.wrapped && !lines.is_empty() {
            // Continuation — append to the previous logical line.
            let last = lines.last_mut().unwrap();
            last.chars.extend_from_slice(&row.chars);
            last.fg.extend_from_slice(&row.fg);
            last.bg.extend_from_slice(&row.bg);
        } else {
            // New logical line.
            lines.push(LogicalLine {
                chars: row.chars,
                fg: row.fg,
                bg: row.bg,
            });
        }
    }

    // Phase 2: trim trailing spaces from each logical line so we don't
    // create spurious wrapped rows from padding.
    for line in &mut lines {
        while line.chars.last() == Some(&' ') && line.chars.len() > 1 {
            line.chars.pop();
            line.fg.pop();
            line.bg.pop();
        }
    }

    // Phase 3: re-wrap each logical line to the new width.
    let mut new_grid = VecDeque::new();
    for line in lines {
        let len = line.chars.len();
        if len == 0 {
            new_grid.push_back(Row::new(new_cols as u16));
            continue;
        }

        let mut offset = 0;
        let mut first_chunk = true;
        while offset < len {
            let end = (offset + new_cols).min(len);
            let mut row = Row::new(new_cols as u16);
            let chunk_len = end - offset;
            row.chars[..chunk_len].copy_from_slice(&line.chars[offset..end]);
            row.fg[..chunk_len].copy_from_slice(&line.fg[offset..end]);
            row.bg[..chunk_len].copy_from_slice(&line.bg[offset..end]);
            row.wrapped = !first_chunk;
            new_grid.push_back(row);
            offset = end;
            first_chunk = false;
        }
    }

    new_grid
}

impl Terminal {
    pub fn new(
        cols: u16,
        rows: u16,
        cell_height: u32,
        scrollback_limit: u32,
    ) -> Self {
        let mut grid = VecDeque::with_capacity(rows as usize + scrollback_limit as usize);
        for _ in 0..rows {
            grid.push_back(Row::new(cols));
        }
        Self {
            cols,
            rows,
            grid,
            cursor_col: 0,
            cursor_row: 0,
            fg: Color::default_fg(),
            bg: Color::default_bg(),
            parser: vte::Parser::new(),
            sixel_parser: None,
            images: Vec::new(),
            scroll_count: 0,
            next_image_id: 0,
            cell_height,
            scrollback_limit,
            viewport_offset: 0,
        }
    }

    /// The number of scrollback rows currently stored.
    pub fn scrollback_len(&self) -> usize {
        self.grid.len().saturating_sub(self.rows as usize)
    }

    /// Returns the visible row at the given screen position (0 = top of
    /// viewport).
    pub fn visible_row(
        &self,
        screen_row: u16,
    ) -> &Row {
        let base = self.grid.len() - self.rows as usize - self.viewport_offset as usize;
        &self.grid[base + screen_row as usize]
    }

    /// Scroll the viewport up (into history). Returns actual lines scrolled.
    pub fn scroll_viewport_up(
        &mut self,
        lines: u32,
    ) -> u32 {
        let max = self.scrollback_len() as u32;
        let delta = lines.min(max - self.viewport_offset);
        self.viewport_offset += delta;
        delta
    }

    /// Scroll the viewport down (toward live). Returns actual lines scrolled.
    pub fn scroll_viewport_down(
        &mut self,
        lines: u32,
    ) -> u32 {
        let delta = lines.min(self.viewport_offset);
        self.viewport_offset -= delta;
        delta
    }

    /// Reset viewport to the bottom (live terminal).
    pub fn reset_viewport(&mut self) {
        self.viewport_offset = 0;
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
    ) {
        let old_cols = self.cols as usize;
        let new_cols = cols as usize;

        if new_cols != old_cols {
            // Remember how far the cursor was from the bottom of the grid.
            let old_distance_from_bottom = self
                .grid
                .len()
                .saturating_sub(self.active_row_index(self.cursor_row) + 1);

            // Reflow: join soft-wrapped rows into logical lines, then re-wrap.
            let old_grid = std::mem::take(&mut self.grid);
            self.grid = reflow_grid(old_grid, new_cols);

            // Ensure at least `rows` entries in the grid after reflow.
            while self.grid.len() < rows as usize {
                self.grid.push_back(Row::new(cols));
            }

            // Restore cursor position relative to the bottom of the grid.
            let new_abs = self.grid.len().saturating_sub(old_distance_from_bottom + 1);
            let visible_start = self.grid.len().saturating_sub(rows as usize);
            self.cursor_row = new_abs.saturating_sub(visible_start).min(rows as usize - 1) as u16;
            self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        } else {
            // Column count unchanged — just adjust row count.
            while self.grid.len() < rows as usize {
                self.grid.push_back(Row::new(cols));
            }
            self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        }

        self.cols = cols;
        self.rows = rows;
        self.viewport_offset = self.viewport_offset.min(self.scrollback_len() as u32);
    }

    /// Process raw bytes from the PTY through the vte parser.
    pub fn process(
        &mut self,
        data: &[u8],
    ) {
        let mut parser = std::mem::replace(&mut self.parser, vte::Parser::new());
        parser.advance(self, data);
        self.parser = parser;
    }

    /// Convert a cursor row (0-based within visible area) to a grid index.
    fn active_row_index(
        &self,
        cursor_row: u16,
    ) -> usize {
        self.grid.len() - self.rows as usize + cursor_row as usize
    }

    fn put_char(
        &mut self,
        ch: char,
    ) {
        if self.cursor_col >= self.cols {
            // Soft wrap: mark the next row as a continuation.
            self.cursor_col = 0;
            self.cursor_row += 1;
            if self.cursor_row >= self.rows {
                self.scroll_up();
                self.cursor_row = self.rows - 1;
            }
            let r = self.active_row_index(self.cursor_row);
            self.grid[r].wrapped = true;
        }

        // New output resets viewport to bottom.
        self.viewport_offset = 0;

        let r = self.active_row_index(self.cursor_row);
        let c = self.cursor_col as usize;
        self.grid[r].chars[c] = ch;
        self.grid[r].fg[c] = self.fg;
        self.grid[r].bg[c] = self.bg;
        self.cursor_col += 1;
    }

    fn scroll_up(&mut self) {
        // The top visible row moves into scrollback (stays in grid).
        self.grid.push_back(Row::new(self.cols));
        self.scroll_count += 1;

        // Trim scrollback if over the limit.
        let max_grid = self.rows as usize + self.scrollback_limit as usize;
        if self.grid.len() > max_grid {
            self.grid.pop_front();
        }
    }

    fn scroll_down(&mut self) {
        self.grid.pop_back();
        self.grid.push_front(Row::new(self.cols));
    }

    pub fn prune_offscreen_images(
        &mut self,
        cell_height: u32,
    ) {
        self.images.retain(|img| {
            let scroll_delta = self.scroll_count.saturating_sub(img.scroll_at_placement);
            let effective_row = img.row_at_placement as i64 - scroll_delta as i64;
            let image_rows =
                (img.image.height as i64 + cell_height as i64 - 1) / cell_height as i64;
            effective_row + image_rows > 0
        });
    }

    fn erase_in_display(
        &mut self,
        mode: u16,
    ) {
        match mode {
            0 => {
                self.erase_in_line(0);
                for row in (self.cursor_row + 1)..self.rows {
                    self.clear_row(row);
                }
            }
            1 => {
                for row in 0..self.cursor_row {
                    self.clear_row(row);
                }
                self.erase_in_line(1);
            }
            2 | 3 => {
                for r in 0..self.rows {
                    self.clear_row(r);
                }
            }
            _ => {}
        }
    }

    fn erase_in_line(
        &mut self,
        mode: u16,
    ) {
        let r = self.active_row_index(self.cursor_row);
        match mode {
            0 => {
                let start = self.cursor_col as usize;
                let end = self.cols as usize;
                self.grid[r].clear_range(start..end);
            }
            1 => {
                let end = (self.cursor_col as usize + 1).min(self.cols as usize);
                self.grid[r].clear_range(0..end);
            }
            2 => {
                self.clear_row(self.cursor_row);
            }
            _ => {}
        }
    }

    fn clear_row(
        &mut self,
        row: u16,
    ) {
        let r = self.active_row_index(row);
        let n = self.cols as usize;
        self.grid[r].clear_range(0..n);
    }

    fn erase_chars(
        &mut self,
        n: usize,
    ) {
        let r = self.active_row_index(self.cursor_row);
        let col = self.cursor_col as usize;
        let end = (col + n).min(self.cols as usize);
        self.grid[r].clear_range(col..end);
    }

    fn insert_lines(
        &mut self,
        n: usize,
    ) {
        let grid_row = self.active_row_index(self.cursor_row);
        let rows = self.rows as usize;
        let n = n.min(rows - self.cursor_row as usize);

        // Remove n rows from the bottom of visible area.
        let bottom = self.grid.len();
        for i in (0..n).rev() {
            self.grid.remove(bottom - 1 - i);
        }
        // Insert n blank rows at cursor position.
        for _ in 0..n {
            self.grid.insert(grid_row, Row::new(self.cols));
        }
    }

    fn delete_lines(
        &mut self,
        n: usize,
    ) {
        let grid_row = self.active_row_index(self.cursor_row);
        let rows = self.rows as usize;
        let n = n.min(rows - self.cursor_row as usize);

        for i in (0..n).rev() {
            self.grid.remove(grid_row + i);
        }
        for _ in 0..n {
            self.grid.push_back(Row::new(self.cols));
        }
    }

    fn delete_chars(
        &mut self,
        n: usize,
    ) {
        let r = self.active_row_index(self.cursor_row);
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - col);

        self.grid[r].copy_within(col + n..cols, col);
        self.grid[r].clear_range(cols - n..cols);
    }

    fn insert_chars(
        &mut self,
        n: usize,
    ) {
        let r = self.active_row_index(self.cursor_row);
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - col);

        self.grid[r].copy_within(col..cols - n, col + n);
        self.grid[r].clear_range(col..col + n);
    }

    fn handle_sgr(
        &mut self,
        params: &vte::Params,
    ) {
        let params: Vec<u16> = params.iter().map(|p| p[0]).collect();

        if params.is_empty() {
            self.fg = Color::default_fg();
            self.bg = Color::default_bg();
            return;
        }

        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.fg = Color::default_fg();
                    self.bg = Color::default_bg();
                }
                30..=37 => self.fg = ansi_color((params[i] - 30) as u8),
                39 => self.fg = Color::default_fg(),
                40..=47 => self.bg = ansi_color((params[i] - 40) as u8),
                49 => self.bg = Color::default_bg(),
                90..=97 => self.fg = ansi_color((params[i] - 90 + 8) as u8),
                100..=107 => self.bg = ansi_color((params[i] - 100 + 8) as u8),
                38 => {
                    if let Some(color) = parse_extended_color(&params, &mut i) {
                        self.fg = color;
                    }
                }
                48 => {
                    if let Some(color) = parse_extended_color(&params, &mut i) {
                        self.bg = color;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

impl vte::Perform for Terminal {
    fn print(
        &mut self,
        c: char,
    ) {
        self.put_char(c);
    }

    fn execute(
        &mut self,
        byte: u8,
    ) {
        match byte {
            b'\n' => {
                self.cursor_row += 1;
                if self.cursor_row >= self.rows {
                    self.scroll_up();
                    self.cursor_row = self.rows - 1;
                }
            }
            b'\r' => {
                self.cursor_col = 0;
            }
            0x08 => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
            }
            b'\t' => {
                let next = (self.cursor_col / 8 + 1) * 8;
                self.cursor_col = next.min(self.cols - 1);
            }
            0x07 | 0x00 => {}
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
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
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
            }
            'C' => {
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
            }
            'D' => {
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                let row = p.first().copied().unwrap_or(1).max(1) - 1;
                let col = p.get(1).copied().unwrap_or(1).max(1) - 1;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
            'J' => {
                let mode = p.first().copied().unwrap_or(0);
                self.erase_in_display(mode);
            }
            'K' => {
                let mode = p.first().copied().unwrap_or(0);
                self.erase_in_line(mode);
            }
            'm' => {
                self.handle_sgr(params);
            }
            'd' => {
                let row = p.first().copied().unwrap_or(1).max(1) - 1;
                self.cursor_row = row.min(self.rows - 1);
            }
            'G' => {
                let col = p.first().copied().unwrap_or(1).max(1) - 1;
                self.cursor_col = col.min(self.cols - 1);
            }
            'L' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.insert_lines(n);
            }
            'M' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.delete_lines(n);
            }
            'P' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.delete_chars(n);
            }
            '@' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.insert_chars(n);
            }
            'X' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.erase_chars(n);
            }
            'S' => {
                let n = p.first().copied().unwrap_or(1).max(1);
                for _ in 0..n {
                    self.scroll_up();
                }
            }
            'T' => {
                let n = p.first().copied().unwrap_or(1).max(1);
                for _ in 0..n {
                    self.scroll_down();
                }
            }
            'r' | 'n' | 'c' => {}
            _ => {}
        }
    }

    fn esc_dispatch(
        &mut self,
        intermediates: &[u8],
        _ignore: bool,
        byte: u8,
    ) {
        if intermediates.first().is_some_and(|&b| b"()*+".contains(&b)) {
            return;
        }

        match byte {
            b'c' => {
                let cols = self.cols;
                let rows = self.rows;
                *self = Terminal::new(cols, rows, self.cell_height, self.scrollback_limit);
            }
            b'M' => {
                if self.cursor_row == 0 {
                    self.scroll_down();
                } else {
                    self.cursor_row -= 1;
                }
            }
            b'7' | b'8' => {}
            b'=' | b'>' => {}
            _ => {}
        }
    }

    fn osc_dispatch(
        &mut self,
        _params: &[&[u8]],
        _bell_terminated: bool,
    ) {
    }

    fn hook(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        self.sixel_parser = sixel::SixelParser::new(params, action);
    }

    fn put(
        &mut self,
        byte: u8,
    ) {
        if let Some(parser) = &mut self.sixel_parser {
            parser.put(byte);
        }
    }

    fn unhook(&mut self) {
        if let Some(parser) = self.sixel_parser.take() {
            let image = parser.finish();
            if image.width > 0 && image.height > 0 {
                let image_rows = image.height / self.cell_height;
                let placed = PlacedImage {
                    id: self.next_image_id,
                    col: self.cursor_col,
                    row_at_placement: self.cursor_row,
                    scroll_at_placement: self.scroll_count,
                    image,
                };
                self.next_image_id += 1;
                self.images.push(placed);

                self.cursor_col = 0;
                for _ in 0..image_rows {
                    self.cursor_row += 1;
                    if self.cursor_row >= self.rows {
                        self.scroll_up();
                        self.cursor_row = self.rows - 1;
                    }
                }
            }
        }
    }
}

fn parse_extended_color(
    params: &[u16],
    i: &mut usize,
) -> Option<Color> {
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
                Some(Color::new(
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
