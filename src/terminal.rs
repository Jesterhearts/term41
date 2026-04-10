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

use crate::sixel;

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
    pub cells: Vec<Cell>,
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

impl Terminal {
    pub fn new(
        cols: u16,
        rows: u16,
        cell_height: u32,
    ) -> Self {
        let size = cols as usize * rows as usize;
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); size],
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
        let mut new_cells = vec![Cell::default(); cols as usize * rows as usize];
        let copy_cols = self.cols.min(cols) as usize;
        let copy_rows = self.rows.min(rows) as usize;

        for row in 0..copy_rows {
            let src_start = row * self.cols as usize;
            let dst_start = row * cols as usize;
            new_cells[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.cells[src_start..src_start + copy_cols]);
        }

        self.cols = cols;
        self.rows = rows;
        self.cells = new_cells;
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
    }

    pub fn cell(
        &self,
        col: u16,
        row: u16,
    ) -> &Cell {
        &self.cells[row as usize * self.cols as usize + col as usize]
    }

    fn cell_mut(
        &mut self,
        col: u16,
        row: u16,
    ) -> &mut Cell {
        &mut self.cells[row as usize * self.cols as usize + col as usize]
    }

    /// Process raw bytes from the PTY through the vte parser.
    pub fn process(
        &mut self,
        data: &[u8],
    ) {
        // vte::Parser borrows self mutably, but Perform callbacks also need
        // mutable access to the terminal state. We work around this by
        // temporarily taking the parser out.
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

        let idx = self.cursor_row as usize * self.cols as usize + self.cursor_col as usize;
        self.cells[idx] = Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
        };
        self.cursor_col += 1;
    }

    fn scroll_up(&mut self) {
        let cols = self.cols as usize;
        self.cells.drain(0..cols);
        self.cells
            .extend(std::iter::repeat_n(Cell::default(), cols));
        self.scroll_count += 1;
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

    fn scroll_down(&mut self) {
        let cols = self.cols as usize;
        let len = self.cells.len();
        self.cells.truncate(len - cols);
        let new_row = vec![Cell::default(); cols];
        self.cells.splice(0..0, new_row);
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
                self.cells.fill(Cell::default());
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
        let start = row as usize * self.cols as usize;
        let end = start + self.cols as usize;
        self.cells[start..end].fill(Cell::default());
    }

    fn erase_chars(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let end = (col + n).min(cols);
        let base = row * cols;
        for c in col..end {
            self.cells[base + c] = Cell::default();
        }
    }

    fn insert_lines(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let n = n.min(rows - row);

        for r in (row + n..rows).rev() {
            let src = (r - n) * cols;
            let dst = r * cols;
            self.cells.copy_within(src..src + cols, dst);
        }
        for r in row..row + n {
            self.clear_row(r as u16);
        }
    }

    fn delete_lines(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let n = n.min(rows - row);

        for r in row..rows - n {
            let src = (r + n) * cols;
            let dst = r * cols;
            self.cells.copy_within(src..src + cols, dst);
        }
        for r in (rows - n)..rows {
            self.clear_row(r as u16);
        }
    }

    fn delete_chars(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - col);
        let base = row * cols;

        self.cells
            .copy_within(base + col + n..base + cols, base + col);
        for c in (cols - n)..cols {
            self.cells[base + c] = Cell::default();
        }
    }

    fn insert_chars(
        &mut self,
        n: usize,
    ) {
        let row = self.cursor_row as usize;
        let col = self.cursor_col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - col);
        let base = row * cols;

        self.cells
            .copy_within(base + col..base + cols - n, base + col + n);
        for c in col..col + n {
            self.cells[base + c] = Cell::default();
        }
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
                // Backspace.
                self.cursor_col = self.cursor_col.saturating_sub(1);
            }
            b'\t' => {
                let next = (self.cursor_col / 8 + 1) * 8;
                self.cursor_col = next.min(self.cols - 1);
            }
            0x07 | 0x00 => {
                // Bell, Null — ignore.
            }
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
        // DEC private modes (intermediates contains '?').
        if intermediates.contains(&b'?') {
            // Cursor visibility, alt screen, etc. — ignore for now.
            return;
        }

        // Other prefixed sequences we don't handle (e.g. '>' for DA2).
        if !intermediates.is_empty() {
            return;
        }

        let p: Vec<u16> = params.iter().map(|p| p[0]).collect();

        match action {
            'A' => {
                // CUU — cursor up.
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                // CUD — cursor down.
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
            }
            'C' => {
                // CUF — cursor forward.
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
            }
            'D' => {
                // CUB — cursor back.
                let n = p.first().copied().unwrap_or(1).max(1);
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                // CUP — cursor position.
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
                // VPA — line position absolute.
                let row = p.first().copied().unwrap_or(1).max(1) - 1;
                self.cursor_row = row.min(self.rows - 1);
            }
            'G' => {
                // CHA — cursor character absolute.
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
            'r' | 'n' | 'c' => {
                // DECSTBM, DSR, DA — ignore for now.
            }
            _ => {}
        }
    }

    fn esc_dispatch(
        &mut self,
        intermediates: &[u8],
        _ignore: bool,
        byte: u8,
    ) {
        // Character set designation — ignore.
        if intermediates.first().is_some_and(|&b| b"()*+".contains(&b)) {
            return;
        }

        match byte {
            b'c' => {
                // RIS — full reset.
                let cols = self.cols;
                let rows = self.rows;
                *self = Terminal::new(cols, rows, self.cell_height);
            }
            b'M' => {
                // RI — reverse index.
                if self.cursor_row == 0 {
                    self.scroll_down();
                } else {
                    self.cursor_row -= 1;
                }
            }
            b'7' | b'8' => {
                // DECSC / DECRC — save/restore cursor. Ignore for now.
            }
            b'=' | b'>' => {
                // DECKPAM / DECKPNM — keypad modes. Ignore.
            }
            _ => {}
        }
    }

    fn osc_dispatch(
        &mut self,
        _params: &[&[u8]],
        _bell_terminated: bool,
    ) {
        // OSC sequences (window title, etc.) — ignore for now.
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
                // Advance cursor below the image.
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
