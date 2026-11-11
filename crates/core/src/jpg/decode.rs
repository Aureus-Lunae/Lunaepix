//! Scaled JPG decoding for thumbnails: libjpeg decodes at 1/2, 1/4 or 1/8
//! of the size where the thumbnail allows, which skips most of the work for
//! a large photo.
//!
//! A child of `jpg`, so it uses the same libjpeg guard ([`Decompress`]),
//! error manager and [`run_libjpeg`]: a libjpeg error or warning (a damaged
//! file) unwinds to `run_libjpeg` and becomes `Err`, and the guard destroys
//! the decompressor on the way.

use std::os::raw::c_ulong;

use mozjpeg_sys::{
    J_COLOR_SPACE, jpeg_finish_decompress, jpeg_mem_src, jpeg_read_header, jpeg_start_decompress,
};

use super::lossy::read_shape;
use super::{Decompress, check_start, fail, read_scanlines, run_libjpeg, source_len};
use crate::plan::check_pixels;

/// Bytes per pixel of the decoded rows: libjpeg's `JCS_EXT_RGBA`, RGB with
/// an alpha byte of 255.
const RGBA: usize = 4;

/// Refuse bytes that do not start like a JPEG, and a header that declares
/// more than [`crate::MAX_PIXELS`]. libjpeg reads the header only.
pub(crate) fn check_header(bytes: &[u8]) -> Result<(), String> {
    check_start(bytes)?;
    let shape = read_shape(bytes)?;
    check_pixels(shape.width, shape.height)
}

/// Decode a JPEG for a thumbnail whose longest side is `side`: at the
/// largest of 1/1, 1/2, 1/4 and 1/8 ([`scale_denom`]) that keeps both sides
/// at least the thumbnail's ([`crate::thumb::fit`]), as 8-bit RGBA (alpha
/// 255), greyscale expanded. Gives the decoded width and height and the
/// pixels.
///
/// The error is a reason: not a JPEG, a header over [`crate::MAX_PIXELS`]
/// (checked before anything is decoded), a CMYK or YCCK image (another
/// colour space a thumbnail does not convert), or what libjpeg reports,
/// warnings included, so a truncated file fails.
pub(crate) fn decode_scaled(bytes: &[u8], side: u32) -> Result<(u32, u32, Vec<u8>), String> {
    check_start(bytes)?;
    let len = source_len(bytes)?;
    // SAFETY: `read_scaled` must run inside `run_libjpeg`, which it does
    // here, and `len` is `bytes.len()`.
    run_libjpeg(|| unsafe { read_scaled(bytes, len, side) })?
}

/// # Safety
///
/// Must run inside [`run_libjpeg`]: libjpeg failures unwind out of it. `len`
/// must be `bytes.len()`.
unsafe fn read_scaled(
    bytes: &[u8],
    len: c_ulong,
    side: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decompress = Decompress::new();
    let source = decompress.cinfo();
    // SAFETY: the standard libjpeg decompression sequence on a decompressor
    // that `Decompress` created and will destroy. `bytes` outlives
    // `decompress`, and libjpeg only reads through that pointer, at most
    // `len` bytes. The rows are read into our own buffer
    // (`read_scanlines`), sized from the output size after
    // `jpeg_start_decompress`.
    unsafe {
        jpeg_mem_src(source, bytes.as_ptr(), len);
        // With `require_image` set, a file without an image fails inside; a
        // memory source never suspends, so the header is read when it returns.
        jpeg_read_header(source, 1);
        // Refuse a decompression bomb before `jpeg_start_decompress`
        // allocates for the declared rows. Failing here unwinds through
        // `decompress`, whose guard destroys the decompressor.
        if let Err(message) = check_pixels(source.image_width, source.image_height) {
            fail(message);
        }
        match source.jpeg_color_space {
            J_COLOR_SPACE::JCS_GRAYSCALE | J_COLOR_SPACE::JCS_YCbCr | J_COLOR_SPACE::JCS_RGB => {}
            other => {
                return Err(format!(
                    "a CMYK or YCCK JPG is not shown ({other:?}): its colours are not converted"
                ));
            }
        }
        // libjpeg expands greyscale and converts YCbCr into this itself.
        source.out_color_space = J_COLOR_SPACE::JCS_EXT_RGBA;
        let target = crate::thumb::fit(source.image_width, source.image_height, side);
        source.scale_num = 1;
        source.scale_denom = scale_denom(source.image_width, source.image_height, target);
        jpeg_start_decompress(source);
        // The output is at most the image's size, checked above.
        let pixels = read_scanlines(source, RGBA);
        jpeg_finish_decompress(source);
        Ok((source.output_width, source.output_height, pixels))
    }
}

/// The largest of 8, 4 and 2 at which libjpeg's output of a `width`×`height`
/// image (each side divided by it and rounded up, as libjpeg's
/// `jdiv_round_up` does) is still at least `target` on both sides; 1 when
/// none is.
fn scale_denom(width: u32, height: u32, target: (u32, u32)) -> u32 {
    [8, 4, 2]
        .into_iter()
        .find(|&denom| width.div_ceil(denom) >= target.0 && height.div_ceil(denom) >= target.1)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jpg::tests::photo_pixels;

    fn jpg(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode(&photo_pixels(w, h), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    // --- the scale --------------------------------------------------------------

    #[test]
    fn scale_denom_picks_the_largest_scale_that_keeps_the_target() {
        assert_eq!(scale_denom(400, 200, (64, 32)), 4);
        assert_eq!(scale_denom(512, 256, (64, 32)), 8);
        assert_eq!(scale_denom(128, 128, (64, 64)), 2);
        assert_eq!(scale_denom(10_000, 1, (64, 1)), 8);
        // libjpeg rounds each side up: 500 / 8 is 63 (62.5 up), short of 64.
        assert_eq!(scale_denom(500, 250, (64, 32)), 4);
        // 449 / 8 rounds up to 57, short of 64; 449 / 4 to 113.
        assert_eq!(scale_denom(449, 1, (64, 1)), 4);
    }

    #[test]
    fn scale_denom_is_1_when_the_picture_is_already_the_target() {
        assert_eq!(scale_denom(64, 64, (64, 64)), 1);
        assert_eq!(scale_denom(20, 10, (20, 10)), 1);
        assert_eq!(scale_denom(65, 64, (64, 63)), 1);
    }

    #[test]
    fn scale_denom_never_scales_below_the_target_nor_stops_short_of_a_larger_scale() {
        let sizes = [
            1, 7, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, 4000,
        ];
        for &w in &sizes {
            for &h in &sizes {
                let target = crate::thumb::fit(w, h, 64);
                let d = scale_denom(w, h, target);
                assert!([1, 2, 4, 8].contains(&d), "{w}x{h}: {d}");
                let at = |d: u32| (w.div_ceil(d), h.div_ceil(d));
                let (sw, sh) = at(d);
                assert!(sw >= target.0 && sh >= target.1, "{w}x{h} at 1/{d}");
                if d < 8 {
                    let (bw, bh) = at(d * 2);
                    assert!(bw < target.0 || bh < target.1, "{w}x{h}: 1/{} fits", d * 2);
                }
            }
        }
    }

    // --- decoding ---------------------------------------------------------------

    #[test]
    fn a_400x200_jpg_decodes_at_a_quarter_to_rgba() {
        let (w, h, rgba) = decode_scaled(&jpg(400, 200), 64).unwrap();
        assert_eq!((w, h), (100, 50));
        assert_eq!(rgba.len(), 100 * 50 * 4);
        assert!(rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }

    #[test]
    fn a_64x64_jpg_decodes_at_full_scale() {
        let (w, h, rgba) = decode_scaled(&jpg(64, 64), 64).unwrap();
        assert_eq!((w, h), (64, 64));
        assert_eq!(rgba.len(), 64 * 64 * 4);
    }

    #[test]
    fn decode_scaled_refuses_a_header_over_max_pixels() {
        let err =
            decode_scaled(&crate::jpeg_fixtures::grey_header(20_000, 20_000), 64).unwrap_err();
        assert_eq!(err, "image too large: 20000×20000 exceeds 100 megapixels");
    }

    #[test]
    fn decode_scaled_passes_a_header_within_max_pixels_on_to_libjpeg() {
        // Refused for its empty scan, not for its size.
        let err = decode_scaled(&crate::jpeg_fixtures::grey_header(64, 64), 64).unwrap_err();
        assert!(!err.contains("too large"), "{err}");
        assert!(err.contains("JPEG"), "{err}");
    }

    #[test]
    fn decode_scaled_refuses_a_cmyk_or_ycck_jpg() {
        for transform in [
            crate::jpeg_fixtures::ADOBE_CMYK,
            crate::jpeg_fixtures::ADOBE_YCCK,
        ] {
            let err = decode_scaled(&crate::jpeg_fixtures::four_component_jpeg(transform), 64)
                .unwrap_err();
            assert!(err.contains("CMYK"), "{err}");
        }
    }

    #[test]
    fn decode_scaled_refuses_bytes_that_are_not_a_jpeg() {
        let err = decode_scaled(b"\x89PNG\r\n\x1a\n", 64).unwrap_err();
        assert!(err.contains("not a JPEG"), "{err}");
    }
}
