//! Image decoders for in-band graphics protocols (sixel, kitty).

pub mod kitty;
pub mod sixel;

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
