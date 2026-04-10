use std::collections::VecDeque;

use crate::sixel;

/// A single cell in the terminal grid.
#[derive(Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::default_fg(),
            bg: Color::default_bg(),
        }
    }
}

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

/// Terminal state: a grid of cells plus cursor position and attributes.
pub struct Terminal {
    pub cols: u16,
    pub rows: u16,
    grid: VecDeque<Vec<Cell>>,
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
}

fn new_row(cols: u16) -> Vec<Cell> {
    vec![Cell::default(); cols as usize]
}

impl Terminal {
    pub fn new(
        cols: u16,
        rows: u16,
        cell_height: u32,
    ) -> Self {
        let mut grid = VecDeque::with_capacity(rows as usize);
        for _ in 0..rows {
            grid.push_back(new_row(cols));
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
        }
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
    ) {
        // Adjust columns in existing rows.
        for row in &mut self.grid {
            row.resize(cols as usize, Cell::default());
        }
        // Add or remove rows.
        while self.grid.len() < rows as usize {
            self.grid.push_back(new_row(cols));
        }
        while self.grid.len() > rows as usize {
            self.grid.pop_back();
        }

        self.cols = cols;
        self.rows = rows;
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
    }

    pub fn cell(
        &self,
        col: u16,
        row: u16,
    ) -> &Cell {
        &self.grid[row as usize][col as usize]
    }

    fn cell_mut(
        &mut self,
        col: u16,
        row: u16,
    ) -> &mut Cell {
        &mut self.grid[row as usize][col as usize]
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

    fn put_char(
        &mut self,
        ch: char,
    ) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.cursor_row += 1;
            if self.cursor_row >= self.rows {
                self.scroll_up();
                self.cursor_row = self.rows - 1;
            }
        }

        self.grid[self.cursor_row as usize][self.cursor_col as usize] = Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
        };
        self.cursor_col += 1;
    }

    fn scroll_up(&mut self) {
        self.grid.pop_front();
        self.grid.push_back(new_row(self.cols));
        self.scroll_count += 1;
    }

    fn scroll_down(&mut self) {
        self.grid.pop_back();
        self.grid.push_front(new_row(self.cols));
    }

    /// Remove images that have scrolled entirely off the top of the screen.
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
                for row in &mut self.grid {
                    row.fill(Cell::default());
                }
            }
            _ => {}
        }
    }

    fn erase_in_line(
        &mut self,
        mode: u16,
    ) {
        let row = self.cursor_row;
        match mode {
            0 => {
                for col in self.cursor_col..self.cols {
                    *self.cell_mut(col, row) = Cell::default();
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(self.cols - 1) {
                    *self.cell_mut(col, row) = Cell::default();
                }
            }
            2 => {
                self.clear_row(row);
            }
            _ => {}
        }
    }

    fn clear_row(
        &mut self,
        row: u16,
    ) {
        self.grid[row as usize].fill(Cell::default());
    }

    fn erase_chars(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let col = self.cursor_col as usize;
        let end = (col + n).min(self.cols as usize);
        self.grid[row][col..end].fill(Cell::default());
    }

    fn insert_lines(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let rows = self.rows as usize;
        let n = n.min(rows - row);

        // Remove n rows from the bottom, insert n blank rows at cursor.
        for _ in 0..n {
            self.grid.pop_back();
        }
        for _ in 0..n {
            self.grid.insert(row, new_row(self.cols));
        }
    }

    fn delete_lines(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let rows = self.rows as usize;
        let n = n.min(rows - row);

        // Remove n rows at cursor, add n blank rows at the bottom.
        for i in (0..n).rev() {
            self.grid.remove(row + i);
        }
        for _ in 0..n {
            self.grid.push_back(new_row(self.cols));
        }
    }

    fn delete_chars(
        &mut self,
        n: usize,
    ) {
        let row = &mut self.grid[self.cursor_row as usize];
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - col);

        row.copy_within(col + n..cols, col);
        row[cols - n..].fill(Cell::default());
    }

    fn insert_chars(
        &mut self,
        n: usize,
    ) {
        let row = &mut self.grid[self.cursor_row as usize];
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - col);

        row.copy_within(col..cols - n, col + n);
        row[col..col + n].fill(Cell::default());
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
                *self = Terminal::new(cols, rows, self.cell_height);
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
