//! Image decoders for in-band graphics protocols (sixel, kitty, iterm).

pub mod iterm;
pub mod kitty;
pub mod sixel;

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode an 8-bit PNG into an RGBA [`DecodedImage`]. Returns `None` on any
/// decode failure — unsupported bit depth (16-bit), indexed colour, or
/// malformed data. Shared between kitty (`f=100`) and iterm2 payloads, both
/// of which carry raw PNG bytes.
pub fn decode_png(data: &[u8]) -> Option<DecodedImage> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().ok()?;

    let info = reader.info();
    let width = info.width;
    let height = info.height;
    let color_type = info.color_type;
    let bit_depth = info.bit_depth;

    if bit_depth != png::BitDepth::Eight {
        return None;
    }

    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let raw = &buf[..frame.buffer_size()];

    let pixels = match color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in raw.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in raw.chunks_exact(2) {
                let g = chunk[0];
                let a = chunk[1];
                rgba.extend_from_slice(&[g, g, g, a]);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for &g in raw {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
            rgba
        }
        png::ColorType::Indexed => {
            // Indexed PNG needs palette expansion — rare for in-band graphics.
            return None;
        }
    };

    Some(DecodedImage {
        pixels,
        width,
        height,
    })
}
