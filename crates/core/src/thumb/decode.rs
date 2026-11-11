//! Decoding a JPG, PNG or WebP to the RGBA pixels a thumbnail is made from,
//! with the decoders the optimisers use and within their bounds, and finding
//! the EXIF orientation the thumbnail is turned by.

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
///   size; an animation's first frame at its canvas size, a canvas over
///   [`crate::MAX_ANIMATION_CANVAS_PIXELS`] refused before decoding.
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

/// The EXIF Orientation of `bytes` as 1 to 8, which a thumbnail turns the
/// picture by:
/// - JPG: the first `Exif\0\0` APP1 segment ([`crate::jpg::exif_payload`]);
/// - WebP: the first `EXIF` chunk ([`crate::webp::exif_orientation`]);
/// - PNG and SVG: `None`, whatever the bytes hold.
///
/// `None` too when there is no EXIF block or it cannot be read: a damaged
/// block is no error, the picture just stays as stored. The format, not the
/// content, picks where to look; the content must still be what
/// [`picture`] decoded.
pub(super) fn orientation(format: Format, bytes: &[u8]) -> Option<u8> {
    match format {
        Format::Jpg => crate::webp::orientation(&crate::jpg::exif_payload(bytes)?),
        Format::Webp => crate::webp::exif_orientation(bytes),
        Format::Png | Format::Svg => None,
    }
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

    // --- orientation ------------------------------------------------------------

    use crate::jpeg_fixtures::with_app1;
    use crate::webp::fixtures as fx;

    fn jpg_of_orientation(value: u16) -> Vec<u8> {
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 90)
            .encode(&[128; 16 * 16 * 3], 16, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        with_app1(&plain, &fx::exif_with_orientation(value, false, true))
    }

    fn webp_of_orientation(value: u16) -> Vec<u8> {
        let plain = fx::lossless(&fx::photo_rgba(16, 16, false), 16, 16);
        fx::with_chunks(
            &plain,
            &[(*b"EXIF", &fx::exif_with_orientation(value, true, false))],
        )
    }

    #[test]
    fn orientation_reads_a_jpg_and_a_webp() {
        assert_eq!(orientation(Format::Jpg, &jpg_of_orientation(6)), Some(6));
        assert_eq!(orientation(Format::Jpg, &jpg_of_orientation(1)), Some(1));
        assert_eq!(orientation(Format::Webp, &webp_of_orientation(8)), Some(8));
        assert_eq!(orientation(Format::Webp, &webp_of_orientation(1)), Some(1));
    }

    #[test]
    fn orientation_is_none_for_a_png_an_svg_and_the_wrong_content() {
        let (jpg, webp) = (jpg_of_orientation(6), webp_of_orientation(6));
        // The format decides where to look, not what the bytes hold.
        for format in [Format::Png, Format::Svg] {
            assert_eq!(orientation(format, &jpg), None, "{format:?}");
            assert_eq!(orientation(format, &webp), None, "{format:?}");
        }
        assert_eq!(orientation(Format::Jpg, &webp), None);
        assert_eq!(orientation(Format::Webp, &jpg), None);
        // And without a rotation to find, nothing is made up.
        assert_eq!(orientation(Format::Jpg, b""), None);
        assert_eq!(orientation(Format::Webp, b""), None);
    }
}
