//! Decoding a JPG, PNG or WebP to the RGBA pixels a thumbnail is made from,
//! with the decoders the optimisers use and within their bounds.

use crate::plan::Format;

/// Decoded 8-bit RGBA pixels, four bytes per pixel, row by row. Built only
/// through [`Picture::new`], so the pixels always match the size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Picture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Picture {
    /// A picture of `width`×`height`, refused unless both are at least 1
    /// and `rgba` holds exactly four bytes per pixel.
    pub(super) fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Picture, String> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if width == 0 || height == 0 || expected != Some(rgba.len()) {
            return Err(format!(
                "the decoder gave {} bytes for {width}×{height} pixels",
                rgba.len()
            ));
        }
        Ok(Picture {
            width,
            height,
            rgba,
        })
    }

    pub(super) fn width(&self) -> u32 {
        self.width
    }

    pub(super) fn height(&self) -> u32 {
        self.height
    }

    pub(super) fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// Check that `bytes` start like `format` and that the size their header
/// declares is within [`crate::MAX_PIXELS`] (`check_pixels`), without
/// decoding any image data. Each format reads its header as its optimiser
/// does. An SVG has no pixels to check and is refused.
pub(super) fn check_header(format: Format, bytes: &[u8]) -> Result<(), String> {
    match format {
        Format::Png => crate::png::check_header(bytes),
        Format::Jpg => crate::jpg::check_header(bytes),
        Format::Webp => crate::webp::check_header(bytes),
        Format::Svg => Err(NOT_DECODED.to_owned()),
    }
}

/// An SVG is passed through, never decoded here.
const NOT_DECODED: &str = "an SVG is not decoded";

/// The picture a thumbnail of `side` pixels is made from, after
/// [`check_header`]:
/// - PNG: the `png` crate, expanded to 8-bit RGBA (palette and `tRNS`,
///   greyscale, 16-bit); an animated PNG gives its default image.
/// - JPG: libjpeg at the largest scale of 1/1 to 1/8 that keeps both sides
///   at least the thumbnail's, as RGBA (greyscale expanded); CMYK and YCCK
///   are refused.
/// - WebP: a still image decoded by libwebp straight to the thumbnail's
///   size; an animation's first frame at its canvas size.
pub(super) fn picture(format: Format, bytes: &[u8], side: u32) -> Result<Picture, String> {
    check_header(format, bytes)?;
    let (width, height, rgba) = match format {
        Format::Png => {
            let image = crate::png::decode_rgba(bytes, "could not decode the PNG")?;
            (image.width, image.height, image.pixels)
        }
        Format::Jpg => crate::jpg::decode_scaled(bytes, side)?,
        Format::Webp => crate::webp::thumbnail_rgba(bytes, side)?,
        Format::Svg => return Err(NOT_DECODED.to_owned()),
    };
    Picture::new(width, height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::png::tests::{file, ihdr};

    /// A WebP of one `VP8L` chunk whose header declares `w`x`h`.
    fn webp_header(w: u32, h: u32) -> Vec<u8> {
        let bits = (w - 1) | ((h - 1) << 14);
        let mut chunk = b"VP8L".to_vec();
        chunk.extend_from_slice(&10u32.to_le_bytes());
        chunk.push(0x2F);
        chunk.extend_from_slice(&bits.to_le_bytes());
        chunk.extend_from_slice(&[0; 5]);
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(4 + chunk.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(&chunk);
        bytes
    }

    /// Header-only fixtures of `w`x`h`: nothing in them can be decoded.
    fn headers(w: u32, h: u32) -> [(Format, Vec<u8>); 3] {
        [
            (Format::Png, file(&[ihdr(w, h, 8, 6)])),
            (
                Format::Jpg,
                crate::jpeg_fixtures::grey_header(w.try_into().unwrap(), h.try_into().unwrap()),
            ),
            (Format::Webp, webp_header(w, h)),
        ]
    }

    #[test]
    fn an_image_of_exactly_max_pixels_passes_the_check() {
        for (format, bytes) in headers(10_000, 10_000) {
            assert_eq!(check_header(format, &bytes), Ok(()), "{format:?}");
        }
    }

    #[test]
    fn one_pixel_more_fails_the_header_check() {
        for (format, bytes) in headers(10_000, 10_001) {
            assert_eq!(
                check_header(format, &bytes),
                Err("image too large: 10000×10001 exceeds 100 megapixels".to_owned()),
                "{format:?}"
            );
        }
    }

    #[test]
    fn the_header_check_refuses_the_content_of_another_format() {
        let [(_, png), (_, jpg), (_, webp)] = headers(16, 16);
        for (format, bytes) in [
            (Format::Png, &jpg),
            (Format::Png, &webp),
            (Format::Jpg, &png),
            (Format::Jpg, &webp),
            (Format::Webp, &png),
            (Format::Webp, &jpg),
        ] {
            let err = check_header(format, bytes).unwrap_err();
            assert!(err.starts_with("not a"), "{format:?}: {err}");
        }
    }

    #[test]
    fn an_svg_is_not_decoded() {
        assert!(check_header(Format::Svg, b"<svg/>").is_err());
        assert!(picture(Format::Svg, b"<svg/>", 64).is_err());
    }

    #[test]
    fn a_picture_holds_four_bytes_per_pixel() {
        let picture = Picture::new(3, 2, vec![7; 24]).unwrap();
        assert_eq!((picture.width(), picture.height()), (3, 2));
        assert_eq!(picture.rgba(), &[7; 24]);
    }

    #[test]
    fn a_picture_refuses_pixels_that_do_not_match_its_size() {
        assert!(Picture::new(3, 2, vec![0; 23]).is_err());
        assert!(Picture::new(3, 2, vec![0; 28]).is_err());
        assert!(Picture::new(0, 2, vec![]).is_err());
        assert!(Picture::new(u32::MAX, u32::MAX, vec![]).is_err());
    }
}
