use atoi::FromRadix10;
use palette::Hsla;
use palette::IntoColor;
use palette::Srgba;

use crate::image::DecodedImage;
use crate::vte;

struct SixelRow {
    default_color: Srgba<u8>,
    cursor: usize,
    pixels: [Vec<Srgba<u8>>; 6],
}

impl SixelRow {
    fn new(color: Srgba<u8>) -> Self {
        Self {
            default_color: color,
            cursor: 0,
            pixels: [vec![], vec![], vec![], vec![], vec![], vec![]],
        }
    }
}

pub fn parse_sixel(
    params: vte::Params,
    string: Vec<u8>,
) -> DecodedImage {
    let mut palette = default_palette();
    let mut palette_entry = 0;

    let transparent = params
        .iter()
        .nth(1)
        .is_some_and(|p| p.first().is_some_and(|p| *p == 1));

    let mut max_w = 0;
    let mut rows = vec![];

    let mut cursor = 0;
    while cursor < string.len() {
        cursor += process_data(
            &mut palette,
            &mut palette_entry,
            &mut rows,
            &mut max_w,
            &string[cursor..],
            transparent,
        );
    }

    let width = max_w as u32;
    let height = rows.len() as u32 * 6;
    let mut pixels = vec![0u8; max_w * rows.len() * 6 * 4];

    for (idy, row) in rows.into_iter().enumerate() {
        let default_color = row.default_color;
        for (offset_y, row) in row.pixels.into_iter().enumerate() {
            for (idx, pixel) in row
                .into_iter()
                .chain(std::iter::repeat(default_color))
                .take(max_w)
                .enumerate()
            {
                let pixel_row = idy * 6 + offset_y;
                pixels[(pixel_row * max_w + idx) * 4] = pixel.red;
                pixels[(pixel_row * max_w + idx) * 4 + 1] = pixel.green;
                pixels[(pixel_row * max_w + idx) * 4 + 2] = pixel.blue;
                pixels[(pixel_row * max_w + idx) * 4 + 3] = pixel.alpha;
            }
        }
    }

    DecodedImage::single_frame(width, height, pixels)
}

fn process_data(
    palette: &mut [Srgba<u8>; 256],
    palette_entry: &mut usize,
    rows: &mut Vec<SixelRow>,
    max_w: &mut usize,
    string: &[u8],
    transparent: bool,
) -> usize {
    match string[0] {
        byte @ 0x3F..=0x7E => {
            if rows.is_empty() {
                rows.push(SixelRow::new(if transparent {
                    Srgba::default()
                } else {
                    palette[0]
                }));
            }
            let row = rows.last_mut().unwrap();
            write_sixel(row, max_w, byte, 1, palette[*palette_entry]);
            1
        }
        b'"' => 1 + process_raster(rows, max_w, &string[1..]),
        b'#' => 1 + process_color(palette, palette_entry, &string[1..]),
        b'!' => {
            if rows.is_empty() {
                rows.push(SixelRow::new(if transparent {
                    Srgba::default()
                } else {
                    palette[0]
                }));
            }
            let row = rows.last_mut().unwrap();

            1 + process_repeat(row, max_w, &string[1..], palette[*palette_entry])
        }
        b'$' => {
            if let Some(row) = rows.last_mut() {
                row.cursor = 0;
            }
            1
        }
        b'-' => {
            rows.push(SixelRow::new(if transparent {
                Srgba::default()
            } else {
                palette[0]
            }));

            1
        }
        _ => 1,
    }
}

fn write_sixel(
    row: &mut SixelRow,
    max_w: &mut usize,
    byte: u8,
    count: usize,
    color: Srgba<u8>,
) {
    let end_x = row.cursor + count;
    let cursor = row.cursor;
    row.cursor += count;

    let value = byte - 0x3F;
    if value == 0 {
        return;
    }

    *max_w = (*max_w).max(end_x);

    for bit in 0..6 {
        if value & (1 << bit) == 0 {
            continue;
        }

        if end_x > row.pixels[bit].len() {
            row.pixels[bit].resize(end_x, row.default_color);
        }
        row.pixels[bit][cursor..end_x].fill(color);
    }
}

fn process_raster(
    rows: &mut Vec<SixelRow>,
    max_w: &mut usize,
    string: &[u8],
) -> usize {
    let mut processed = 0;
    if string.is_empty() || string[0] == b'-' {
        return processed;
    }

    let (_, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 || string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (_, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 || string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (ph, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 || string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (pv, end) = usize::from_radix_10(string);
    processed += end;

    *max_w = ph;
    rows.reserve(pv);

    processed
}

fn process_color(
    palette: &mut [Srgba<u8>; 256],
    palette_entry: &mut usize,
    string: &[u8],
) -> usize {
    let mut processed = 0;
    let (pc, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 {
        return processed;
    }

    *palette_entry = pc.min(255);
    if string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (pu, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 || string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (px, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 || string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (py, end) = usize::from_radix_10(string);
    let string = &string[end..];
    processed += end;

    if end == 0 || string.is_empty() || string[0] != b';' {
        return processed;
    }

    let string = &string[1..];
    processed += 1;
    let (pz, end) = usize::from_radix_10(string);
    processed += end;

    let rgb = match pu {
        2 => Srgba::new(
            (px * 255 / 100) as u8,
            (py * 255 / 100) as u8,
            (pz * 255 / 100) as u8,
            255,
        ),
        1 => {
            let hsl = Hsla::new(
                (px as isize - 120).rem_euclid(360) as f32,
                pz as f32 / 100.0,
                py as f32 / 100.0,
                1.0,
            );
            let rgb: Srgba = hsl.into_color();
            rgb.into_format()
        }
        _ => Srgba::default(),
    };

    palette[*palette_entry] = rgb;

    processed
}

fn process_repeat(
    row: &mut SixelRow,
    max_w: &mut usize,
    string: &[u8],
    color: Srgba<u8>,
) -> usize {
    let (repeat, end) = usize::from_radix_10(string);
    let string = &string[end..];

    if string.is_empty() || !(0x3F..=0x7Eu8).contains(&string[0]) {
        return end;
    }

    write_sixel(row, max_w, string[0], repeat, color);

    end + 1
}

/// Initialize the default VT340 16-color palette.
const fn default_palette() -> [Srgba<u8>; 256] {
    let defaults: [Srgba<u8>; 16] = [
        Srgba::new(0, 0, 0, 255),       // 0: black
        Srgba::new(51, 51, 204, 255),   // 1: blue
        Srgba::new(204, 33, 33, 255),   // 2: red
        Srgba::new(51, 204, 51, 255),   // 3: green
        Srgba::new(204, 51, 204, 255),  // 4: magenta
        Srgba::new(51, 204, 204, 255),  // 5: cyan
        Srgba::new(204, 204, 51, 255),  // 6: yellow
        Srgba::new(135, 135, 135, 255), // 7: gray 50%
        Srgba::new(38, 38, 38, 255),    // 8: dark gray
        Srgba::new(84, 84, 255, 255),   // 9: light blue
        Srgba::new(255, 84, 84, 255),   // 10: light red
        Srgba::new(84, 255, 84, 255),   // 11: light green
        Srgba::new(255, 84, 255, 255),  // 12: light magenta
        Srgba::new(84, 255, 255, 255),  // 13: light cyan
        Srgba::new(255, 255, 84, 255),  // 14: light yellow
        Srgba::new(255, 255, 255, 255), // 15: white
    ];

    let mut palette = [Srgba::new(0, 0, 0, 0); 256];
    let mut i = 0;
    while i < defaults.len() {
        palette[i] = defaults[i];
        i += 1;
    }

    palette
}
