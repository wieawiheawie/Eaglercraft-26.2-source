use std::io::Cursor;

const MAX_TEXTURE_DIMENSION: u32 = 4096;
const MAX_TEXTURE_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedTexture {
    pub width: u32,
    pub height: u32,
    /// Row-major, straight-alpha RGBA8 pixels.
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PngDecodeError {
    InvalidPng(String),
    DimensionsTooLarge { width: u32, height: u32 },
    UnsupportedColorType,
    InvalidDecodedBuffer,
}

pub fn validate_texture_dimensions(width: u32, height: u32) -> Result<(), PngDecodeError> {
    if width == 0
        || height == 0
        || width > MAX_TEXTURE_DIMENSION
        || height > MAX_TEXTURE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_TEXTURE_PIXELS
    {
        Err(PngDecodeError::DimensionsTooLarge { width, height })
    } else {
        Ok(())
    }
}

pub fn decode_png_rgba(bytes: &[u8]) -> Result<DecodedTexture, PngDecodeError> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|error| PngDecodeError::InvalidPng(error.to_string()))?;
    reader.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);

    let mut frame = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut frame)
        .map_err(|error| PngDecodeError::InvalidPng(error.to_string()))?;
    let width = info.width;
    let height = info.height;
    validate_texture_dimensions(width, height)?;

    let source = &frame[..info.buffer_size()];
    let pixel_count = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| PngDecodeError::InvalidDecodedBuffer)?;
    let mut pixels = Vec::with_capacity(pixel_count * 4);

    match info.color_type {
        png::ColorType::Rgba => pixels.extend_from_slice(source),
        png::ColorType::Rgb => {
            for rgb in source.chunks_exact(3) {
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for gray in source {
                pixels.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for gray_alpha in source.chunks_exact(2) {
                pixels.extend_from_slice(&[
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[1],
                ]);
            }
        }
        png::ColorType::Indexed => return Err(PngDecodeError::UnsupportedColorType),
    }

    if pixels.len() != pixel_count * 4 {
        return Err(PngDecodeError::InvalidDecodedBuffer);
    }

    Ok(DecodedTexture { width, height, pixels })
}

#[cfg(test)]
mod tests {
    use super::{decode_png_rgba, PngDecodeError};

    fn encode_rgba(width: u32, height: u32, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        bytes
    }

    #[test]
    fn decodes_rgba_png_data() {
        let bytes = encode_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]);
        let decoded = decode_png_rgba(&bytes).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixels, [255, 0, 0, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn rejects_non_png_bytes() {
        assert!(matches!(
            decode_png_rgba(b"not a png"),
            Err(PngDecodeError::InvalidPng(_))
        ));
    }

    #[test]
    fn rejects_excessive_texture_dimensions() {
        let bytes = encode_rgba(1, 1, &[0, 0, 0, 255]);
        let mut decoded = decode_png_rgba(&bytes).unwrap();
        decoded.width = 4097;
        assert_eq!(
            super::validate_texture_dimensions(decoded.width, decoded.height),
            Err(PngDecodeError::DimensionsTooLarge {
                width: 4097,
                height: 1
            })
        );
    }
}
