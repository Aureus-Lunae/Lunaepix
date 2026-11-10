//! Lossy JPG: the source is decoded to RGB or greyscale pixels and encoded
//! again by mozjpeg at the run's quality, with mozjpeg's default profile
//! (`JCP_MAX_COMPRESSION`: progressive, with optimised Huffman tables).
//!
//! A child of `jpg`, so it uses the same libjpeg guards ([`Decompress`],
//! [`Compress`]), error manager and [`run_libjpeg`]: a libjpeg failure
//! unwinds to `run_libjpeg` and becomes `Err`, and the guards destroy the
//! libjpeg objects on the way. Nothing here installs a callback of its own,
//! so no Rust panic can reach the C frames.
//!
//! The source's APPn and COM segments (EXIF, ICC, XMP, comments) are copied
//! byte for byte, in source order, except its JFIF APP0 and Adobe APP14:
//! libjpeg writes its own JFIF for the YCbCr or greyscale output, and an
//! Adobe segment describes a colour transform the output no longer has.
//!
//! A source in another colour space (CMYK, YCCK) gets the lossless treatment
//! with [`Note::KeptLosslessCmyk`].

use std::os::raw::{c_int, c_uint, c_ulong};
use std::slice;

use mozjpeg_sys::{
    J_COLOR_SPACE, JCP_MAX_COMPRESSION, JINT_COMPRESS_PROFILE, jpeg_c_set_int_param,
    jpeg_decompress_struct, jpeg_finish_compress, jpeg_finish_decompress, jpeg_mem_src,
    jpeg_read_header, jpeg_read_scanlines, jpeg_save_markers, jpeg_set_defaults, jpeg_set_quality,
    jpeg_start_compress, jpeg_start_decompress, jpeg_write_marker, jpeg_write_scanlines,
};

use super::{Compress, Decompress, check_start, fail, run_libjpeg, source_len};
use crate::lossy::{Lossy, Note, Quality, Shape, check_shape, finish, kept_lossless};

/// The first APPn marker code; APP15 is `APP0 + 15`.
const APP0: u8 = 0xE0;
/// The APPn marker an Adobe segment uses.
const APP14: u8 = 0xEE;
/// The comment marker code.
const COM: u8 = 0xFE;
/// Save each marker whole: a segment's length field is 16 bits, so its
/// payload is never longer than this.
const MARKER_LIMIT: c_uint = 0xFFFF;

/// The source's pixels and the markers to carry over: what an encoder gets.
struct Decoded {
    width: u32,
    height: u32,
    colour: Colour,
    /// `width × height` pixels, row by row, of one (grey) or three (RGB)
    /// bytes each.
    pixels: Vec<u8>,
    /// Every saved APPn and COM segment, in source order.
    markers: Vec<Marker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Colour {
    Grey,
    Rgb,
}

impl Colour {
    fn space(self) -> J_COLOR_SPACE {
        match self {
            Colour::Grey => J_COLOR_SPACE::JCS_GRAYSCALE,
            Colour::Rgb => J_COLOR_SPACE::JCS_RGB,
        }
    }

    fn components(self) -> usize {
        match self {
            Colour::Grey => 1,
            Colour::Rgb => 3,
        }
    }
}

/// One saved marker segment: its code and its payload.
struct Marker {
    code: u8,
    data: Vec<u8>,
}

impl Marker {
    /// Whether the encoder writes this segment: all but a JFIF APP0 and an
    /// Adobe APP14 (identifiers as libjpeg's jdmarker.c tests them).
    fn is_copied(&self) -> bool {
        let jfif = self.code == APP0 && self.data.starts_with(b"JFIF\0");
        let adobe = self.code == APP14 && self.data.starts_with(b"Adobe");
        !(jfif || adobe)
    }
}

/// What the source decodes to.
enum Source {
    Pixels(Decoded),
    /// Neither YCbCr/RGB nor greyscale: CMYK, YCCK or unknown.
    OtherColourSpace,
}

/// Compress JPEG bytes lossily at `quality`.
///
/// The result is never larger than the source (see [`finish`]); a CMYK or
/// YCCK source gets the lossless treatment with [`Note::KeptLosslessCmyk`].
/// The error is a message for the user, as for the lossless path.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the run by a later task")
)]
pub(crate) fn optimize_lossy(bytes: &[u8], quality: Quality) -> Result<Lossy, String> {
    optimize_lossy_with(bytes, quality, encode)
}

/// [`optimize_lossy`] with the encoder as an argument, so tests can pass a
/// double for it.
fn optimize_lossy_with(
    bytes: &[u8],
    quality: Quality,
    encode: impl FnOnce(&Decoded, Quality) -> Result<Vec<u8>, String>,
) -> Result<Lossy, String> {
    check_start(bytes)?;
    let decoded = match decode(bytes)? {
        Source::Pixels(decoded) => decoded,
        Source::OtherColourSpace => {
            return Ok(kept_lossless(
                super::optimize(bytes)?,
                Note::KeptLosslessCmyk,
            ));
        }
    };
    let result = encode(&decoded, quality)?;
    let found = read_shape(&result)
        .map_err(|reason| format!("the lossy result is not a decodable JPG ({reason})"))?;
    let expected = Shape {
        width: decoded.width,
        height: decoded.height,
        duration_ms: None,
    };
    check_shape(expected, found)?;
    Ok(finish(bytes, result))
}

/// Decode the source to pixels, or find that its colour space is not one
/// this module re-encodes.
fn decode(bytes: &[u8]) -> Result<Source, String> {
    let len = source_len(bytes)?;
    // SAFETY: `read_pixels` must run inside `run_libjpeg`, which it does
    // here, and `len` is `bytes.len()`.
    run_libjpeg(|| unsafe { read_pixels(bytes, len) })
}

/// # Safety
///
/// Must run inside [`run_libjpeg`]: libjpeg failures unwind out of it. `len`
/// must be `bytes.len()`.
unsafe fn read_pixels(bytes: &[u8], len: c_ulong) -> Source {
    let mut decompress = Decompress::new();
    let source = decompress.cinfo();
    // SAFETY: the standard libjpeg decompression sequence on a decompressor
    // that `Decompress` created and will destroy. `bytes` outlives
    // `decompress`, and libjpeg only reads through that pointer, at most
    // `len` bytes. The saved markers live in the decompressor's pool and are
    // copied out before it is dropped.
    unsafe {
        jpeg_mem_src(source, bytes.as_ptr(), len);
        for code in (APP0..=APP0 + 15).chain([COM]) {
            jpeg_save_markers(source, c_int::from(code), MARKER_LIMIT);
        }
        // With `require_image` set, a file without an image fails inside; a
        // memory source never suspends, so the header is read when it returns.
        jpeg_read_header(source, 1);
        // Refuse a decompression bomb before `jpeg_start_decompress`
        // allocates for every declared row.
        if let Err(message) = crate::plan::check_pixels(source.image_width, source.image_height) {
            fail(message);
        }
        let colour = match source.jpeg_color_space {
            J_COLOR_SPACE::JCS_GRAYSCALE => Colour::Grey,
            J_COLOR_SPACE::JCS_YCbCr | J_COLOR_SPACE::JCS_RGB => Colour::Rgb,
            _ => return Source::OtherColourSpace,
        };
        source.out_color_space = colour.space();
        let markers = saved_markers(source);
        jpeg_start_decompress(source);
        let width = source.output_width;
        let height = source.output_height;
        // At most 100 megapixels of 3 bytes (`check_pixels` above).
        let row = width as usize * colour.components();
        let mut pixels =
            reserve_pixels(row * height as usize).unwrap_or_else(|message| fail(message));
        while source.output_scanline < height {
            let at = source.output_scanline as usize * row;
            let mut rows = [pixels[at..at + row].as_mut_ptr()];
            // A memory source never suspends, so libjpeg returns 0 rows only
            // in a state it has already reported; refuse to loop forever.
            if jpeg_read_scanlines(source, rows.as_mut_ptr(), 1) == 0 {
                fail("could not read the JPEG data: libjpeg returned no rows".to_owned());
            }
        }
        jpeg_finish_decompress(source);
        Source::Pixels(Decoded {
            width,
            height,
            colour,
            pixels,
            markers,
        })
    }
}

/// A zeroed pixel buffer of `len` bytes, or an error when that memory cannot
/// be had (instead of aborting the process).
fn reserve_pixels(len: usize) -> Result<Vec<u8>, String> {
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| "not enough memory for the pixels".to_owned())?;
    pixels.resize(len, 0);
    Ok(pixels)
}

/// Copies of the markers libjpeg saved, in source order.
///
/// # Safety
///
/// `source.marker_list` must be libjpeg's list of saved markers: valid after
/// `jpeg_read_header` and until the decompressor is finished or destroyed.
unsafe fn saved_markers(source: &jpeg_decompress_struct) -> Vec<Marker> {
    let mut markers = Vec::new();
    let mut node = source.marker_list;
    while !node.is_null() {
        // SAFETY: a non-null node of libjpeg's list (this function's
        // contract), valid while the decompressor is.
        let marker = unsafe { &*node };
        let data = if marker.data_length == 0 {
            Vec::new()
        } else {
            // SAFETY: `data` holds `data_length` bytes.
            unsafe { slice::from_raw_parts(marker.data, marker.data_length as usize) }.to_vec()
        };
        markers.push(Marker {
            code: marker.marker,
            data,
        });
        node = marker.next;
    }
    markers
}

/// The real encoder: mozjpeg's default profile at `quality`, with the
/// source's markers (but JFIF and Adobe) written right after the headers.
fn encode(decoded: &Decoded, quality: Quality) -> Result<Vec<u8>, String> {
    // SAFETY: `write_jpeg` must run inside `run_libjpeg`, which it does here.
    run_libjpeg(|| unsafe { write_jpeg(decoded, quality) })
}

/// # Safety
///
/// Must run inside [`run_libjpeg`]: libjpeg failures unwind out of it.
unsafe fn write_jpeg(decoded: &Decoded, quality: Quality) -> Vec<u8> {
    let mut compress = Compress::new();
    let output = compress.cinfo();
    let components = decoded.colour.components();
    // SAFETY: the standard libjpeg compression sequence on a compressor that
    // `Compress` created, whose destination is its own `VecDestination`, and
    // which it destroys. Each marker pointer points at `data.len()` bytes,
    // and each row pointer at one row of `width × components` bytes:
    // `chunks_exact` yields only whole rows. Too few or too many rows are
    // libjpeg errors or warnings, so they fail, never read out of bounds.
    unsafe {
        output.image_width = decoded.width;
        output.image_height = decoded.height;
        // 1 or 3.
        output.input_components = components as c_int;
        output.in_color_space = decoded.colour.space();
        // Before `jpeg_set_defaults`, which applies the profile. It is
        // mozjpeg's default today, set explicitly so a version bump cannot
        // change it silently.
        jpeg_c_set_int_param(output, JINT_COMPRESS_PROFILE, JCP_MAX_COMPRESSION as c_int);
        // Picks YCbCr for RGB input and greyscale for greyscale input.
        jpeg_set_defaults(output);
        jpeg_set_quality(output, c_int::from(quality.get()), 1);
        jpeg_start_compress(output, 1);
        for marker in decoded.markers.iter().filter(|marker| marker.is_copied()) {
            // A saved payload is at most 65533 bytes; libjpeg refuses a
            // longer one through `error_exit`.
            let len = c_uint::try_from(marker.data.len()).unwrap_or(c_uint::MAX);
            jpeg_write_marker(output, c_int::from(marker.code), marker.data.as_ptr(), len);
        }
        // `max(1)`: a zero width has already failed in `jpeg_start_compress`.
        let row = (decoded.width as usize * components).max(1);
        for line in decoded.pixels.chunks_exact(row) {
            let rows = [line.as_ptr()];
            jpeg_write_scanlines(output, rows.as_ptr(), 1);
        }
        jpeg_finish_compress(output);
    }
    compress.into_output()
}

/// The size in the header of a lossy result.
fn read_shape(bytes: &[u8]) -> Result<Shape, String> {
    let len = source_len(bytes)?;
    run_libjpeg(|| {
        let mut decompress = Decompress::new();
        let header = decompress.cinfo();
        // SAFETY: `bytes` outlives `decompress`, and libjpeg reads at most
        // `len` bytes through the pointer. Failures unwind to `run_libjpeg`.
        unsafe {
            jpeg_mem_src(header, bytes.as_ptr(), len);
            jpeg_read_header(header, 1);
        }
        Shape {
            width: header.image_width,
            height: header.image_height,
            duration_ms: None,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::raw::{c_int, c_ulong};
    use std::panic;
    use std::time::{Duration, Instant};

    use mozjpeg_sys::{
        J_COLOR_SPACE, jpeg_finish_compress, jpeg_mem_src, jpeg_read_header, jpeg_set_colorspace,
        jpeg_set_defaults, jpeg_set_quality, jpeg_start_compress, jpeg_write_scanlines,
    };

    use super::*;
    use crate::jpg::tests::{frame_marker, photo_pixels};
    use crate::jpg::{Compress, Decompress, run_libjpeg};
    use crate::lossy::Note;

    const W: u32 = 256;
    const H: u32 = 192;

    fn q(value: u8) -> Quality {
        Quality::new(value).unwrap()
    }

    // --- fixtures ---------------------------------------------------------

    /// An RGB or greyscale JPEG from the `image` crate: baseline, standard
    /// tables, with a JFIF APP0.
    fn image_jpeg(pixels: &[u8], w: u32, h: u32, grey: bool, quality: u8) -> Vec<u8> {
        let colour = if grey {
            image::ExtendedColorType::L8
        } else {
            image::ExtendedColorType::Rgb8
        };
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode(pixels, w, h, colour)
            .unwrap();
        out
    }

    fn photo(quality: u8) -> Vec<u8> {
        image_jpeg(&photo_pixels(W, H), W, H, false, quality)
    }

    fn grey_pixels(w: u32, h: u32) -> Vec<u8> {
        photo_pixels(w, h)
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| p[0])
            .collect()
    }

    /// A JPEG written by libjpeg itself from `components` samples per pixel
    /// in `input`, stored in `stored` (CMYK, YCCK, or RGB with an Adobe
    /// APP14 and transform 0), at quality 95.
    fn libjpeg_jpeg(
        pixels: &[u8],
        w: u32,
        h: u32,
        input: J_COLOR_SPACE,
        components: c_int,
        stored: J_COLOR_SPACE,
    ) -> Vec<u8> {
        run_libjpeg(|| {
            let mut compress = Compress::new();
            let output = compress.cinfo();
            let row = w as usize * components as usize;
            // SAFETY: the standard libjpeg compression sequence on a
            // compressor `Compress` owns; each row pointer points at `row`
            // bytes of `pixels`.
            unsafe {
                output.image_width = w;
                output.image_height = h;
                output.input_components = components;
                output.in_color_space = input;
                jpeg_set_defaults(output);
                jpeg_set_colorspace(output, stored);
                jpeg_set_quality(output, 95, 1);
                jpeg_start_compress(output, 1);
                for line in pixels.chunks_exact(row) {
                    let rows = [line.as_ptr()];
                    jpeg_write_scanlines(output, rows.as_ptr(), 1);
                }
                jpeg_finish_compress(output);
            }
            compress.into_output()
        })
        .unwrap()
    }

    fn cmyk_jpeg(stored: J_COLOR_SPACE) -> Vec<u8> {
        let cmyk: Vec<u8> = photo_pixels(W, H)
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[c, m, y]| [c, m, y, 40])
            .collect();
        libjpeg_jpeg(&cmyk, W, H, J_COLOR_SPACE::JCS_CMYK, 4, stored)
    }

    /// The header's colour space as libjpeg reads it.
    fn colour_space(bytes: &[u8]) -> J_COLOR_SPACE {
        let len = c_ulong::try_from(bytes.len()).unwrap();
        run_libjpeg(|| {
            let mut decompress = Decompress::new();
            let source = decompress.cinfo();
            // SAFETY: `bytes` outlives `decompress`.
            unsafe {
                jpeg_mem_src(source, bytes.as_ptr(), len);
                jpeg_read_header(source, 1);
            }
            source.jpeg_color_space
        })
        .unwrap()
    }

    /// Every marker segment before the first scan, as (code, payload).
    fn segments(jpeg: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut found = Vec::new();
        let mut at = 2;
        while at + 4 <= jpeg.len() && jpeg[at] == 0xFF && jpeg[at + 1] != 0xDA {
            let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
            found.push((jpeg[at + 1], jpeg[at + 4..at + 2 + len].to_vec()));
            at += 2 + len;
        }
        found
    }

    /// The APPn and COM segments, without a JFIF APP0 (which libjpeg writes
    /// itself for every YCbCr or greyscale output).
    fn copied_segments(jpeg: &[u8]) -> Vec<(u8, Vec<u8>)> {
        segments(jpeg)
            .into_iter()
            .filter(|(code, _)| matches!(code, 0xE0..=0xEF | 0xFE))
            .filter(|(code, data)| !(*code == 0xE0 && data.starts_with(b"JFIF\0")))
            .collect()
    }

    /// `jpeg` with `extra` segments inserted after its leading APP0 (or right
    /// after SOI when it has none).
    fn with_segments(jpeg: &[u8], extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut at = 2;
        if jpeg[3] == 0xE0 {
            at += 2 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
        }
        let mut out = jpeg[..at].to_vec();
        for (code, data) in extra {
            out.extend_from_slice(&[0xFF, *code]);
            out.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_be_bytes());
            out.extend_from_slice(data);
        }
        out.extend_from_slice(&jpeg[at..]);
        out
    }

    /// An EXIF APP1 payload with orientation 6 (rotate 90° clockwise).
    fn exif_orientation_6() -> Vec<u8> {
        let mut data = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        data.extend_from_slice(&[0x01, 0x00]); // one entry
        data.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0, 0, 0, 0x06, 0, 0, 0]);
        data.extend_from_slice(&[0, 0, 0, 0]); // no next IFD
        data
    }

    fn icc_app2() -> Vec<u8> {
        let mut data = b"ICC_PROFILE\0\x01\x01".to_vec();
        data.extend((0..600u32).map(|i| (i * 7 % 251) as u8));
        data
    }

    fn decode_rgb(jpeg: &[u8]) -> image::RgbImage {
        image::load_from_memory(jpeg).unwrap().to_rgb8()
    }

    /// The mean absolute difference per channel, the largest of the three.
    fn mean_channel_difference(a: &image::RgbImage, b: &image::RgbImage) -> f64 {
        assert_eq!(a.dimensions(), b.dimensions());
        let mut sums = [0u64; 3];
        for (x, y) in a.pixels().zip(b.pixels()) {
            for (sum, (p, q)) in sums.iter_mut().zip(x.0.iter().zip(y.0.iter())) {
                *sum += u64::from(p.abs_diff(*q));
            }
        }
        let count = f64::from(a.width() * a.height());
        sums.iter().map(|&s| s as f64 / count).fold(0.0, f64::max)
    }

    /// A greyscale header of `w`×`h` with an empty scan.
    fn grey_header(w: u16, h: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
        bytes.extend_from_slice(&[1; 64]);
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0xFF, 0xD9]);
        bytes
    }

    /// An encoder double that must not be called.
    fn never(_: &Decoded, _: Quality) -> Result<Vec<u8>, String> {
        panic!("the encoder must not be called")
    }

    // --- the real encoder ---------------------------------------------------

    #[test]
    fn a_photo_jpg_gets_smaller_at_75_and_keeps_its_size() {
        let source = photo(95);
        assert_eq!(frame_marker(&source), 0xC0, "the fixture is baseline");
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert!(lossy.bytes.len() < source.len());
        let decoded = image::load_from_memory(&lossy.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (W, H));
        // mozjpeg's default profile: progressive.
        assert_eq!(frame_marker(&lossy.bytes), 0xC2);
    }

    #[test]
    fn an_already_small_jpg_at_100_keeps_the_original_with_the_note() {
        let source = photo(20);
        let lossy = optimize_lossy(&source, q(100)).unwrap();
        assert_eq!(lossy.note, Some(Note::OriginalKept));
        assert_eq!(lossy.bytes, source);
    }

    #[test]
    fn exif_with_orientation_6_and_icc_are_kept_byte_for_byte() {
        let extra = vec![
            (0xE1, exif_orientation_6()),
            (0xE2, icc_app2()),
            (
                0xE1,
                b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x='adobe:ns:meta/'/>".to_vec(),
            ),
            (0xFE, b"a comment from the test".to_vec()),
        ];
        let source = with_segments(&photo(95), &extra);
        assert_eq!(copied_segments(&source), extra, "fixture");
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        // Byte for byte, in source order, and nothing else.
        assert_eq!(copied_segments(&lossy.bytes), extra);
        // The pixels are not rotated: orientation stays metadata.
        let decoded = image::load_from_memory(&lossy.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (W, H));
    }

    #[test]
    fn a_source_without_exif_gains_none() {
        let source = photo(95);
        assert_eq!(copied_segments(&source), [], "fixture");
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(copied_segments(&lossy.bytes), []);
    }

    #[test]
    fn a_source_jfif_app0_is_not_copied_twice() {
        // A JFIF APP0 with a distinctive density (300 dpi): libjpeg writes
        // its own JFIF, and the source's is not copied next to it.
        let jfif = b"JFIF\0\x01\x02\x01\x01\x2C\x01\x2C\0\0".to_vec();
        let source = with_segments(&photo(95), &[(0xE0, jfif.clone())]);
        let in_source = segments(&source)
            .into_iter()
            .filter(|segment| *segment == (0xE0, jfif.clone()))
            .count();
        assert_eq!(in_source, 1, "fixture");
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        let app0: Vec<_> = segments(&lossy.bytes)
            .into_iter()
            .filter(|(code, _)| *code == 0xE0)
            .collect();
        assert_eq!(app0.len(), 1, "{app0:?}");
        assert!(app0[0].1.starts_with(b"JFIF\0"));
        assert_ne!(app0[0].1, jfif);
    }

    #[test]
    fn an_adobe_app14_with_transform_0_is_not_copied_and_colours_stay_close() {
        let pixels = photo_pixels(W, H);
        let rgb = J_COLOR_SPACE::JCS_RGB;
        let source = libjpeg_jpeg(&pixels, W, H, rgb, 3, rgb);
        let adobe = |jpeg: &[u8]| {
            segments(jpeg)
                .into_iter()
                .filter(|(code, data)| *code == 0xEE && data.starts_with(b"Adobe"))
                .collect::<Vec<_>>()
        };
        let source_adobe = adobe(&source);
        assert_eq!(source_adobe.len(), 1, "fixture");
        assert_eq!(source_adobe[0].1[11], 0, "fixture: transform 0");
        assert_eq!(colour_space(&source), rgb, "fixture");
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(adobe(&lossy.bytes), []);
        assert_eq!(colour_space(&lossy.bytes), J_COLOR_SPACE::JCS_YCbCr);
        let difference = mean_channel_difference(&decode_rgb(&source), &decode_rgb(&lossy.bytes));
        assert!(difference < 8.0, "{difference}");
    }

    #[test]
    fn the_colour_comparison_detects_swapped_channels() {
        let source = decode_rgb(&photo(95));
        let mut swapped = source.clone();
        for pixel in swapped.pixels_mut() {
            pixel.0.swap(0, 1);
        }
        assert!(mean_channel_difference(&source, &swapped) >= 8.0);
        assert_eq!(mean_channel_difference(&source, &source), 0.0);
    }

    #[test]
    fn a_greyscale_jpg_stays_one_component() {
        let grey = image_jpeg(&grey_pixels(W, H), W, H, true, 95);
        let lossy = optimize_lossy(&grey, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(colour_space(&lossy.bytes), J_COLOR_SPACE::JCS_GRAYSCALE);
        let decoded = image::load_from_memory(&lossy.bytes).unwrap();
        assert_eq!(decoded.color(), image::ColorType::L8);
        // The counterpart: an RGB source keeps its three components.
        let rgb = optimize_lossy(&photo(95), q(75)).unwrap();
        assert_eq!(colour_space(&rgb.bytes), J_COLOR_SPACE::JCS_YCbCr);
        let decoded = image::load_from_memory(&rgb.bytes).unwrap();
        assert_eq!(decoded.color(), image::ColorType::Rgb8);
    }

    #[test]
    fn a_cmyk_jpg_is_kept_lossless_with_the_note() {
        for stored in [J_COLOR_SPACE::JCS_CMYK, J_COLOR_SPACE::JCS_YCCK] {
            let source = cmyk_jpeg(stored);
            assert_eq!(colour_space(&source), stored, "fixture");
            let lossy = optimize_lossy_with(&source, q(75), never).unwrap();
            assert_eq!(lossy.note, Some(Note::KeptLosslessCmyk), "{stored:?}");
            assert_eq!(
                lossy.bytes,
                crate::jpg::optimize(&source).unwrap(),
                "{stored:?}"
            );
            // And through the real encoder.
            assert_eq!(optimize_lossy(&source, q(75)).unwrap(), lossy);
        }
    }

    #[test]
    fn an_rgb_jpg_in_the_same_run_is_encoded_lossy() {
        let cmyk = optimize_lossy(&cmyk_jpeg(J_COLOR_SPACE::JCS_CMYK), q(75)).unwrap();
        assert_eq!(cmyk.note, Some(Note::KeptLosslessCmyk));
        let source = photo(95);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_ne!(lossy.bytes, crate::jpg::optimize(&source).unwrap());
    }

    #[test]
    fn a_damaged_cmyk_jpg_fails_with_the_lossless_reason() {
        let full = cmyk_jpeg(J_COLOR_SPACE::JCS_CMYK);
        let truncated = &full[..full.len() * 2 / 3];
        let err = optimize_lossy_with(truncated, q(75), never).unwrap_err();
        assert!(err.contains("damaged"), "{err}");
        assert_eq!(Err(err), crate::jpg::optimize(truncated));
    }

    #[test]
    fn quality_30_is_smaller_than_quality_90() {
        let source = photo(100);
        let low = optimize_lossy(&source, q(30)).unwrap();
        let high = optimize_lossy(&source, q(90)).unwrap();
        assert_eq!((low.note, high.note), (None, None));
        assert!(low.bytes.len() < high.bytes.len());
    }

    #[test]
    fn quality_90_is_not_smaller_than_quality_30() {
        let source = photo(100);
        let low = optimize_lossy(&source, q(30)).unwrap();
        let high = optimize_lossy(&source, q(90)).unwrap();
        assert_eq!((low.note, high.note), (None, None));
        assert!(high.bytes.len() >= low.bytes.len());
    }

    #[test]
    fn quality_0_gives_a_valid_jpg_of_the_same_size() {
        let lossy = optimize_lossy(&photo(95), q(0)).unwrap();
        assert_eq!(lossy.note, None);
        let decoded = image::load_from_memory(&lossy.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (W, H));
    }

    #[test]
    fn the_real_encoder_turns_a_libjpeg_failure_into_an_error() {
        // Wider than libjpeg's JPEG_MAX_DIMENSION (65500): refused by
        // `jpeg_start_compress` through `error_exit`.
        let decoded = Decoded {
            width: 70_000,
            height: 1,
            colour: Colour::Grey,
            pixels: vec![0; 70_000],
            markers: Vec::new(),
        };
        let err = encode(&decoded, q(75)).unwrap_err();
        assert!(err.contains("65500"), "{err}");
    }

    #[test]
    fn reserving_an_impossible_pixel_buffer_fails_with_a_reason() {
        let err = reserve_pixels(usize::MAX).unwrap_err();
        assert_eq!(err, "not enough memory for the pixels");
        assert_eq!(reserve_pixels(12).unwrap().len(), 12);
    }

    #[test]
    fn writes_a_marker_of_the_largest_segment_size() {
        // A 65533-byte APP15 payload, the most one segment holds, is copied.
        let big = (0xEF, vec![0x42; 65_533]);
        let source = with_segments(&photo(95), std::slice::from_ref(&big));
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(copied_segments(&lossy.bytes), [big]);
    }

    // --- encoder doubles -----------------------------------------------------

    #[test]
    fn the_encoder_double_gets_the_decoded_pixels_and_the_quality() {
        let source = photo(95);
        let seen = Cell::new(None);
        let err = optimize_lossy_with(&source, q(42), |decoded, quality| {
            seen.set(Some((
                decoded.width,
                decoded.height,
                decoded.colour,
                decoded.pixels.len(),
                quality.get(),
            )));
            Err("recorded".to_owned())
        })
        .unwrap_err();
        assert_eq!(err, "recorded");
        assert_eq!(
            seen.get(),
            Some((W, H, Colour::Rgb, (W * H * 3) as usize, 42))
        );
    }

    #[test]
    fn an_encoder_double_returning_a_different_size_fails_with_the_reason() {
        let other = image_jpeg(&photo_pixels(16, 16), 16, 16, false, 20);
        let err = optimize_lossy_with(&photo(95), q(75), |_, _| Ok(other)).unwrap_err();
        assert_eq!(
            err,
            "the lossy result has a different size (16×16, expected 256×192)"
        );
    }

    #[test]
    fn an_encoder_double_returning_garbage_fails_as_not_decodable() {
        for garbage in [
            b"not a JPEG at all".to_vec(),
            Vec::new(),
            vec![0xFF, 0xD8, 0xFF, 0xD9],
        ] {
            let err =
                optimize_lossy_with(&photo(95), q(75), |_, _| Ok(garbage.clone())).unwrap_err();
            assert!(
                err.starts_with("the lossy result is not a decodable JPG"),
                "{garbage:?}: {err}"
            );
        }
    }

    #[test]
    fn an_encoder_double_returning_an_error_fails_with_its_reason() {
        let err = optimize_lossy_with(&photo(95), q(75), |_, _| Err("encoder broke".to_owned()))
            .unwrap_err();
        assert_eq!(err, "encoder broke");
    }

    #[test]
    fn the_same_double_returning_a_valid_jpg_gets_it_back() {
        let small = image_jpeg(&photo_pixels(W, H), W, H, false, 20);
        let lossy = optimize_lossy_with(&photo(95), q(75), |_, _| Ok(small.clone())).unwrap();
        assert_eq!(lossy.bytes, small);
        assert_eq!(lossy.note, None);
    }

    // --- hostile input -------------------------------------------------------

    #[test]
    fn bytes_without_a_jpeg_start_marker_are_refused() {
        for bytes in [&b"\x89PNG\r\n\x1a\n"[..], &[0xFF, 0xD8], b""] {
            let err = optimize_lossy_with(bytes, q(75), never).unwrap_err();
            assert!(err.contains("not a JPEG"), "{err}");
        }
    }

    #[test]
    fn a_header_over_max_pixels_is_refused_before_decoding() {
        // 20000x20000 greyscale: 400 MP in a header of a few bytes.
        let start = Instant::now();
        let err = optimize_lossy_with(&grey_header(20_000, 20_000), q(75), never).unwrap_err();
        assert_eq!(err, "image too large: 20000×20000 exceeds 100 megapixels");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_header_within_max_pixels_is_passed_on_to_libjpeg() {
        // The same header at 64x64 is refused for its missing scan data.
        let err = optimize_lossy_with(&grey_header(64, 64), q(75), never).unwrap_err();
        assert!(!err.contains("image too large"), "{err}");
    }

    #[test]
    fn a_corrupt_jpg_fails_with_a_reason_without_panic() {
        let full = photo(95);
        let truncated = full[..full.len() * 2 / 3].to_vec();
        let mut garbage = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        garbage.extend_from_slice(&[0x5A; 64]);
        for bytes in [truncated, garbage] {
            let start = Instant::now();
            let result = panic::catch_unwind(|| optimize_lossy(&bytes, q(75)));
            let err = result.expect("no panic").unwrap_err();
            assert!(err.contains("damaged"), "{err}");
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }
}
