/// A completed sixel image ready for rendering.
pub struct SixelImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA row-major, 4 bytes per pixel
    pub transparent_bg: bool,
}

/// Parser state machine for accumulating a single sixel image.
pub struct SixelParser {
    state: State,
    palette: [(u8, u8, u8); 256],
    current_color: u8,
    /// Pre-packed RGBA pixel for the current color (avoids palette lookup per
    /// byte).
    current_pixel: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    cursor_x: u32,
    cursor_y: u32,
    max_x: u32,
    max_y: u32,
    param_buf: Vec<u8>,
    repeat_count: u32,
    transparent_bg: bool,
    raster_seen: bool,
}

enum State {
    Data,
    RasterParams,
    ColorDef,
    RepeatCount,
}

impl SixelParser {
    /// Create from DCS hook parameters. Returns None if action != 'q'.
    pub fn new(
        params: &vte::Params,
        action: char,
    ) -> Option<Self> {
        if action != 'q' {
            return None;
        }

        let mut p = [0u16; 3];
        for (i, param) in params.iter().take(3).enumerate() {
            p[i] = param[0];
        }

        // P2: background select. 1 = transparent, 0 or 2 = fill with background.
        let transparent_bg = p[1] == 1;

        let mut palette = [(0u8, 0u8, 0u8); 256];
        init_default_palette(&mut palette);
        let (r, g, b) = palette[0];

        let parser = Self {
            state: State::Data,
            palette,
            current_color: 0,
            current_pixel: u32::from_ne_bytes([r, g, b, 255]),
            width: 0,
            height: 0,
            pixels: Vec::new(),
            cursor_x: 0,
            cursor_y: 0,
            max_x: 0,
            max_y: 0,
            param_buf: Vec::with_capacity(32),
            repeat_count: 0,
            transparent_bg,
            raster_seen: false,
        };

        Some(parser)
    }

    /// Feed a byte from the DCS payload.
    pub fn put(
        &mut self,
        byte: u8,
    ) {
        match self.state {
            State::Data => self.process_data(byte),
            State::RasterParams => self.process_raster(byte),
            State::ColorDef => self.process_color(byte),
            State::RepeatCount => self.process_repeat(byte),
        }
    }

    /// Finalize and return the completed image.
    pub fn finish(mut self) -> SixelImage {
        // If no raster attributes were given, trim to actual content.
        if !self.raster_seen && self.max_x > 0 && self.max_y > 0 {
            let trimmed_w = self.max_x;
            let trimmed_h = self.max_y;
            let mut trimmed = vec![0u8; (trimmed_w * trimmed_h * 4) as usize];
            for y in 0..trimmed_h {
                let src_start = (y * self.width * 4) as usize;
                let dst_start = (y * trimmed_w * 4) as usize;
                let row_bytes = (trimmed_w * 4) as usize;
                if src_start + row_bytes <= self.pixels.len() {
                    trimmed[dst_start..dst_start + row_bytes]
                        .copy_from_slice(&self.pixels[src_start..src_start + row_bytes]);
                }
            }
            self.width = trimmed_w;
            self.height = trimmed_h;
            self.pixels = trimmed;
        }

        SixelImage {
            width: self.width,
            height: self.height,
            pixels: self.pixels,
            transparent_bg: self.transparent_bg,
        }
    }

    #[inline(always)]
    fn process_data(
        &mut self,
        byte: u8,
    ) {
        // Data bytes are the hot path — check first.
        if byte >= 0x3F && byte <= 0x7E {
            self.write_sixel(byte, 1);
            return;
        }
        match byte {
            b'"' => {
                self.param_buf.clear();
                self.state = State::RasterParams;
            }
            b'#' => {
                self.param_buf.clear();
                self.state = State::ColorDef;
            }
            b'!' => {
                self.repeat_count = 0;
                self.state = State::RepeatCount;
            }
            b'$' => {
                self.cursor_x = 0;
            }
            b'-' => {
                self.cursor_x = 0;
                self.cursor_y += 6;
            }
            _ => {}
        }
    }

    fn process_raster(
        &mut self,
        byte: u8,
    ) {
        if byte.is_ascii_digit() || byte == b';' {
            self.param_buf.push(byte);
            return;
        }

        // End of raster attributes — parse Pan;Pad;Ph;Pv.
        let params = parse_params(&self.param_buf);
        if params.len() >= 4 {
            let ph = params[2]; // width
            let pv = params[3]; // height
            if ph > 0 && pv > 0 {
                self.width = ph;
                self.height = pv;
                self.pixels = vec![0u8; (ph * pv * 4) as usize];
                self.raster_seen = true;
            }
        }

        self.state = State::Data;
        // Re-process this byte as data.
        self.process_data(byte);
    }

    fn process_color(
        &mut self,
        byte: u8,
    ) {
        if byte.is_ascii_digit() || byte == b';' {
            self.param_buf.push(byte);
            return;
        }

        // Parse #Pc[;Pu;Px;Py;Pz].
        let params = parse_params(&self.param_buf);
        if let Some(&pc) = params.first() {
            let idx = pc.min(255) as usize;
            if params.len() >= 5 {
                // Color definition + selection.
                let pu = params[1];
                let px = params[2];
                let py = params[3];
                let pz = params[4];
                let (r, g, b) = match pu {
                    2 => {
                        // RGB, values 0-100.
                        (
                            (px * 255 / 100) as u8,
                            (py * 255 / 100) as u8,
                            (pz * 255 / 100) as u8,
                        )
                    }
                    1 => {
                        // HLS with VT340 hue rotation (blue at 0°).
                        // Subtract 120° to convert to standard hue (red at 0°).
                        let hue = ((px as i32 - 120).rem_euclid(360)) as f32;
                        let lightness = py as f32 / 100.0;
                        let saturation = pz as f32 / 100.0;
                        hls_to_rgb(hue, lightness, saturation)
                    }
                    _ => (0, 0, 0),
                };
                if idx < 256 {
                    self.palette[idx] = (r, g, b);
                }
            }
            // Always select the color and cache the pixel value.
            self.current_color = idx as u8;
            let (r, g, b) = self.palette[idx];
            self.current_pixel = u32::from_ne_bytes([r, g, b, 255]);
        }

        self.state = State::Data;
        self.process_data(byte);
    }

    fn process_repeat(
        &mut self,
        byte: u8,
    ) {
        if byte.is_ascii_digit() {
            self.repeat_count = self
                .repeat_count
                .saturating_mul(10)
                .saturating_add((byte - b'0') as u32);
            return;
        }

        // The next byte should be a sixel data character.
        let count = self.repeat_count.max(1);
        self.state = State::Data;

        if (0x3F..=0x7E).contains(&byte) {
            self.write_sixel(byte, count);
        } else {
            // Not a data byte — re-process as normal data.
            self.process_data(byte);
        }
    }

    fn write_sixel(
        &mut self,
        byte: u8,
        count: u32,
    ) {
        let value = byte - 0x3F;
        if value == 0 {
            // All pixels off — just advance cursor.
            self.cursor_x += count;
            return;
        }

        let pixel = self.current_pixel;
        let end_x = self.cursor_x + count;
        let end_y = self.cursor_y + 6;

        // Ensure buffer fits the entire repeat span.
        if !self.raster_seen || end_x > self.width || end_y > self.height {
            if end_x > self.width || end_y > self.height {
                self.grow_buffer(end_x, end_y);
            }
        }

        let stride = self.width as usize;
        let cx = self.cursor_x as usize;
        let cy = self.cursor_y as usize;
        let count = count as usize;
        let pixels: &mut [u32] = bytemuck::cast_slice_mut(&mut self.pixels);

        // For each set bit, write a horizontal run of `count` pixels.
        for bit in 0..6usize {
            if value & (1 << bit) == 0 {
                continue;
            }
            let row = cy + bit;
            let start = row * stride + cx;
            pixels[start..start + count].fill(pixel);
        }

        self.cursor_x = end_x as u32;
        self.max_x = self.max_x.max(end_x as u32);
        self.max_y = self.max_y.max(end_y as u32);
    }

    fn grow_buffer(
        &mut self,
        needed_x: u32,
        needed_y: u32,
    ) {
        let new_w = round_up_64(needed_x.max(self.width));
        let new_h = round_up_64(needed_y.max(self.height));

        if new_w == self.width && new_h == self.height {
            return;
        }

        let mut new_pixels = vec![0u8; (new_w * new_h * 4) as usize];

        // Copy existing rows.
        if self.width > 0 && self.height > 0 {
            let copy_w = self.width.min(new_w);
            let row_bytes = (copy_w * 4) as usize;
            for y in 0..self.height.min(new_h) {
                let src_start = (y * self.width * 4) as usize;
                let dst_start = (y * new_w * 4) as usize;
                if src_start + row_bytes <= self.pixels.len() {
                    new_pixels[dst_start..dst_start + row_bytes]
                        .copy_from_slice(&self.pixels[src_start..src_start + row_bytes]);
                }
            }
        }

        self.width = new_w;
        self.height = new_h;
        self.pixels = new_pixels;
    }
}

fn round_up_64(v: u32) -> u32 {
    (v + 63) & !63
}

fn parse_params(buf: &[u8]) -> Vec<u32> {
    if buf.is_empty() {
        return vec![];
    }
    buf.split(|&b| b == b';')
        .map(|part| {
            part.iter().fold(0u32, |acc, &b| {
                if b.is_ascii_digit() {
                    acc.saturating_mul(10).saturating_add((b - b'0') as u32)
                } else {
                    acc
                }
            })
        })
        .collect()
}

/// Initialize the default VT340 16-color palette.
fn init_default_palette(palette: &mut [(u8, u8, u8); 256]) {
    let defaults: [(u8, u8, u8); 16] = [
        (0, 0, 0),       // 0: black
        (51, 51, 204),   // 1: blue
        (204, 33, 33),   // 2: red
        (51, 204, 51),   // 3: green
        (204, 51, 204),  // 4: magenta
        (51, 204, 204),  // 5: cyan
        (204, 204, 51),  // 6: yellow
        (135, 135, 135), // 7: gray 50%
        (38, 38, 38),    // 8: dark gray
        (84, 84, 255),   // 9: light blue
        (255, 84, 84),   // 10: light red
        (84, 255, 84),   // 11: light green
        (255, 84, 255),  // 12: light magenta
        (84, 255, 255),  // 13: light cyan
        (255, 255, 84),  // 14: light yellow
        (255, 255, 255), // 15: white
    ];
    for (i, &color) in defaults.iter().enumerate() {
        palette[i] = color;
    }
}

/// Convert HLS (hue 0-360, lightness 0-1, saturation 0-1) to RGB bytes.
fn hls_to_rgb(
    hue: f32,
    lightness: f32,
    saturation: f32,
) -> (u8, u8, u8) {
    if saturation == 0.0 {
        let v = (lightness * 255.0) as u8;
        return (v, v, v);
    }

    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let h = hue / 360.0;

    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);

    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn hue_to_rgb(
    p: f32,
    q: f32,
    mut t: f32,
) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}
