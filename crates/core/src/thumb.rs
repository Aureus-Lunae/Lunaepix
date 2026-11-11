//! Thumbnails: a small RGBA PNG of a JPG, PNG or WebP, made with the
//! decoders the product already ships, or an SVG's own bytes.
//!
//! Pure: a thumbnail is computed from the file and returned; nothing is
//! written. The decoding is bounded as the optimisers bound it: the file's
//! size ([`MAX_FILE_BYTES`], or [`MAX_SVG_THUMB_BYTES`] for an SVG) before
//! it is read, and the declared size ([`crate::MAX_PIXELS`]) from the header
//! before anything is decoded ([`decode`]). The picture is returned as
//! stored: no EXIF orientation is applied.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use crate::MAX_FILE_BYTES;
use crate::plan::{self, Format};

mod decode;

use decode::Picture;

/// The longest side of a thumbnail, in pixels.
pub const THUMB_SIDE: u32 = 64;

/// The largest SVG whose bytes are passed through as its thumbnail (2 MiB).
pub const MAX_SVG_THUMB_BYTES: u64 = 2 * 1024 * 1024;

/// A thumbnail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thumbnail {
    /// An 8-bit RGBA PNG whose longest side is at most [`THUMB_SIDE`].
    Png(Vec<u8>),
    /// An SVG file's own bytes, for the webview to draw.
    Svg(Vec<u8>),
}

/// Why a file has no thumbnail: a reason for logs and tests, never shown to
/// the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CannotShow(pub String);

/// The thumbnail of `src`.
///
/// The format comes from the extension, as for optimising
/// (case-insensitive `png`, `jpg`/`jpeg`, `webp`, `svg`), and the content
/// must match it. `src` is opened only when it is a regular file (a link to
/// one counts; a folder, a FIFO or a device does not) of at most
/// [`MAX_FILE_BYTES`], or [`MAX_SVG_THUMB_BYTES`] for an SVG, and at most
/// one byte past that limit is read if it grows in between.
///
/// - A JPG, PNG or WebP is decoded ([`decode::picture`]), its header
///   checked against [`crate::MAX_PIXELS`] first, then scaled down with a
///   plain area average to [`fit`] [`THUMB_SIDE`] and written as an 8-bit
///   RGBA PNG. A JPG and a still WebP are decoded at a reduced scale where
///   the size allows; an animated PNG gives its default image and an
///   animated WebP its first frame, its canvas checked against
///   [`crate::MAX_ANIMATION_CANVAS_PIXELS`] first. A CMYK or YCCK JPG
///   cannot be shown.
/// - An SVG is returned as its own bytes when they are UTF-8 text whose
///   first character, after an optional byte order mark and whitespace, is
///   `<`. It is not parsed or rendered.
///
/// Anything else, and any failure on the way, is [`CannotShow`].
pub fn thumbnail(src: &Path) -> Result<Thumbnail, CannotShow> {
    let format = plan::format_of(src)
        .ok_or_else(|| CannotShow("not a PNG, JPG, SVG or WebP file name".to_owned()))?;
    let limit = match format {
        Format::Svg => MAX_SVG_THUMB_BYTES,
        Format::Png | Format::Jpg | Format::Webp => MAX_FILE_BYTES,
    };
    let bytes = read_source(src, limit).map_err(CannotShow)?;
    if format == Format::Svg {
        return if is_svg_text(&bytes) {
            Ok(Thumbnail::Svg(bytes))
        } else {
            Err(CannotShow(
                "not an SVG: not UTF-8 text that starts with '<'".to_owned(),
            ))
        };
    }
    let picture = decode::picture(format, &bytes, THUMB_SIDE).map_err(CannotShow)?;
    drop(bytes);
    let (width, height) = fit(picture.width(), picture.height(), THUMB_SIDE);
    let rgba = area_average(&picture, width, height);
    drop(picture);
    encode_png(width, height, &rgba)
        .map(Thumbnail::Png)
        .map_err(CannotShow)
}

/// The bytes of `src`, if it is a regular file of at most `limit` bytes.
/// The type and size are checked before it is opened, so a FIFO is never
/// opened (which would block) and a large file is never read.
fn read_source(src: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let unreadable = |cause: std::io::Error| format!("could not read the file: {cause}");
    let metadata = fs::metadata(src).map_err(unreadable)?;
    if !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    let too_large = || format!("too large: more than {limit} bytes");
    if metadata.len() > limit {
        return Err(too_large());
    }
    // It may have grown since; read one byte past the limit at most.
    let file = fs::File::open(src).map_err(unreadable)?;
    crate::read_at_most(file, limit)
        .map_err(unreadable)?
        .ok_or_else(too_large)
}

/// Whether `bytes` are UTF-8 text whose first character, after one
/// optional byte order mark and any whitespace, is `<`.
fn is_svg_text(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    text.trim_start().starts_with('<')
}

/// The size of a thumbnail of a `width`×`height` picture: the longest side
/// brought down to `side` with the aspect ratio kept (rounded to the
/// nearest pixel), each side at least 1, and never larger than the picture.
pub(crate) fn fit(width: u32, height: u32, side: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= side {
        return (width, height);
    }
    // `other * side` fits 64 bits; the result is at most `side`.
    let scale = |other: u32| {
        let scaled =
            (u64::from(other) * u64::from(side) + u64::from(longest) / 2) / u64::from(longest);
        (scaled as u32).max(1)
    };
    if width >= height {
        (side, scale(height))
    } else {
        (scale(width), side)
    }
}

/// `picture` brought down to `width`×`height` (each at most the picture's
/// own) by a plain area average: every target pixel is the rounded mean of
/// the block of source pixels it covers, each channel on its own, alpha
/// included. The blocks split the source at whole pixels, so every source
/// pixel counts once.
fn area_average(picture: &Picture, width: u32, height: u32) -> Vec<u8> {
    let (source_width, source_height) = (picture.width() as usize, picture.height() as usize);
    let (width, height) = (width as usize, height as usize);
    let source = picture.rgba();
    // Where block `i` of `n` starts in `len` source pixels.
    let edge = |i: usize, n: usize, len: usize| i * len / n;
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let (top, bottom) = (
            edge(y, height, source_height),
            edge(y + 1, height, source_height),
        );
        for x in 0..width {
            let (left, right) = (
                edge(x, width, source_width),
                edge(x + 1, width, source_width),
            );
            let mut sums = [0u64; 4];
            for row in top..bottom {
                let start = (row * source_width + left) * 4;
                let end = (row * source_width + right) * 4;
                for pixel in source[start..end].as_chunks::<4>().0 {
                    for (sum, &value) in sums.iter_mut().zip(pixel) {
                        *sum += u64::from(value);
                    }
                }
            }
            let count = ((bottom - top) * (right - left)) as u64;
            out.extend(sums.map(|sum| ((sum + count / 2) / count) as u8));
        }
    }
    out
}

/// `rgba` (`width`×`height` pixels of four bytes) as an 8-bit RGBA PNG.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let failed = |error: ::png::EncodingError| format!("could not write the thumbnail: {error}");
    let mut out = Vec::new();
    let mut encoder = ::png::Encoder::new(Cursor::new(&mut out), width, height);
    encoder.set_color(::png::ColorType::Rgba);
    encoder.set_depth(::png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(rgba).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fit ------------------------------------------------------------------

    #[test]
    fn fit_scales_the_longest_side_to_the_side_and_keeps_the_aspect() {
        assert_eq!(fit(400, 200, 64), (64, 32));
        assert_eq!(fit(200, 400, 64), (32, 64));
        assert_eq!(fit(65, 64, 64), (64, 63));
        assert_eq!(fit(10_000, 1, 64), (64, 1));
        assert_eq!(fit(1, 10_000, 64), (1, 64));
    }

    #[test]
    fn fit_never_enlarges() {
        assert_eq!(fit(20, 10, 64), (20, 10));
        assert_eq!(fit(64, 64, 64), (64, 64));
        assert_eq!(fit(1, 1, 64), (1, 1));
    }

    #[test]
    fn fit_never_gives_a_side_of_0_or_over_the_side_or_over_its_input() {
        let sizes = [1, 2, 3, 31, 63, 64, 65, 127, 128, 129, 1000, 9999, 100_000];
        for &width in &sizes {
            for &height in &sizes {
                let (w, h) = fit(width, height, 64);
                let at = format!("{width}x{height} -> {w}x{h}");
                assert!(w >= 1 && h >= 1, "{at}");
                assert!(w <= 64 && h <= 64, "{at}");
                assert!(w <= width && h <= height, "{at}");
                // The longest side reaches the side whenever it was longer.
                assert_eq!(w.max(h), width.max(height).min(64), "{at}");
            }
        }
    }

    // --- area average -----------------------------------------------------------

    fn picture(width: u32, height: u32, rgba: Vec<u8>) -> decode::Picture {
        decode::Picture::new(width, height, rgba).unwrap()
    }

    #[test]
    fn area_average_takes_the_mean_of_each_block() {
        // 4x2: the left 2x2 block is 0,0,0,0 / 10,20,30,40 / 20,.. / 30,..;
        // the right one all 200.
        let mut rgba = Vec::new();
        for (y, x) in [
            (0, 0),
            (0, 1),
            (0, 2),
            (0, 3),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
        ] {
            if x < 2 {
                let k = (y * 2 + x) as u8 * 10;
                rgba.extend_from_slice(&[k, k * 2, k * 3, 255 - k]);
            } else {
                rgba.extend_from_slice(&[200; 4]);
            }
        }
        let out = area_average(&picture(4, 2, rgba), 2, 1);
        // Means of 0,10,20,30 = 15; 0,20,40,60 = 30; 0,30,60,90 = 45;
        // 255,245,235,225 = 240.
        assert_eq!(out, [15, 30, 45, 240, 200, 200, 200, 200]);
    }

    #[test]
    fn area_average_counts_every_source_pixel_once_when_the_sizes_do_not_divide() {
        // 5x1 to 2x1: the blocks are pixels 0..2 and 2..5.
        let values = [10u8, 20, 100, 110, 120];
        let rgba: Vec<u8> = values.iter().flat_map(|&v| [v, v, v, 255]).collect();
        let out = area_average(&picture(5, 1, rgba), 2, 1);
        assert_eq!(out, [15, 15, 15, 255, 110, 110, 110, 255]);
    }

    #[test]
    fn area_average_rounds_to_the_nearest_value() {
        // Means of 1 and 2 (1.5) and of 1,1,2 (1.33).
        let rgba: Vec<u8> = [1u8, 2].iter().flat_map(|&v| [v, v, v, v]).collect();
        assert_eq!(area_average(&picture(2, 1, rgba), 1, 1), [2, 2, 2, 2]);
        let rgba: Vec<u8> = [1u8, 1, 2].iter().flat_map(|&v| [v, v, v, v]).collect();
        assert_eq!(area_average(&picture(3, 1, rgba), 1, 1), [1, 1, 1, 1]);
    }

    #[test]
    fn area_average_keeps_the_pixels_at_the_same_size() {
        let rgba: Vec<u8> = (0..3 * 2 * 4).map(|i| (i * 11) as u8).collect();
        assert_eq!(area_average(&picture(3, 2, rgba.clone()), 3, 2), rgba);
    }

    // --- PNG encoding -----------------------------------------------------------

    #[test]
    fn encode_png_writes_an_8_bit_rgba_png_of_the_pixels() {
        let rgba: Vec<u8> = (0..3 * 2 * 4).map(|i| (i * 11) as u8).collect();
        let bytes = encode_png(3, 2, &rgba).unwrap();
        let decoder = ::png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((frame.width, frame.height), (3, 2));
        assert_eq!(frame.color_type, ::png::ColorType::Rgba);
        assert_eq!(frame.bit_depth, ::png::BitDepth::Eight);
        assert_eq!(&buffer[..frame.buffer_size()], rgba.as_slice());
    }

    #[test]
    fn encode_png_refuses_pixels_that_do_not_match_the_size() {
        assert!(encode_png(3, 2, &[0; 3 * 2 * 4 - 1]).is_err());
        assert!(encode_png(3, 2, &[0; 3 * 2 * 4 + 4]).is_err());
    }

    // --- SVG text ---------------------------------------------------------------

    #[test]
    fn svg_text_starts_with_a_tag_after_a_bom_and_whitespace() {
        assert!(is_svg_text(b"<svg/>"));
        assert!(is_svg_text(b"\xEF\xBB\xBF<svg/>"));
        assert!(is_svg_text(b" \t\r\n<svg/>"));
        assert!(is_svg_text(b"\xEF\xBB\xBF\n <svg/>"));
    }

    #[test]
    fn svg_text_refuses_bytes_that_are_not_utf8_or_do_not_start_with_a_tag() {
        assert!(!is_svg_text(b""));
        assert!(!is_svg_text(b"svg/>"));
        assert!(!is_svg_text(b"\xEF\xBB\xBF"));
        assert!(!is_svg_text(b"<svg>\xFF</svg>"));
        assert!(!is_svg_text(b"\x89PNG\r\n\x1a\n"));
        // A second BOM is not whitespace.
        assert!(!is_svg_text(b"\xEF\xBB\xBF\xEF\xBB\xBF<svg/>"));
    }
}
