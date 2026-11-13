//! Thumbnails: a small RGBA PNG of a JPG, PNG or WebP, made with the
//! decoders the product already ships, or an SVG's own bytes.
//!
//! Pure: a thumbnail is computed from the file and returned; nothing is
//! written. The decoding is bounded as the optimisers bound it: the file's
//! size ([`MAX_FILE_BYTES`], or [`MAX_SVG_THUMB_BYTES`] for an SVG) before
//! it is read, and the declared size ([`crate::MAX_PIXELS`]) from the header
//! before anything is decoded ([`decode`]). A JPG's or WebP's EXIF
//! orientation is applied last ([`orient`]), so the thumbnail shows the
//! picture upright; a damaged EXIF block is no error, just no turn.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use crate::MAX_FILE_BYTES;
use crate::plan::{self, Format};

pub(crate) mod decode;

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
/// - A JPG or WebP whose EXIF block ([`decode::orientation`]) holds an
///   Orientation of 2 to 8 is turned to match ([`orient`]) after it is
///   scaled, so for 5 to 8 the width and height swap. A PNG is never
///   turned; no EXIF block, a damaged one or an Orientation outside 1 to
///   8 leaves the picture as stored.
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
    let orientation = decode::orientation(format, &bytes);
    drop(bytes);
    let (width, height) = fit(picture.width(), picture.height(), THUMB_SIDE);
    let rgba = area_average(&picture, width, height);
    drop(picture);
    let (rgba, width, height) = orient(rgba, width, height, orientation.unwrap_or(1));
    encode_png(width, height, &rgba)
        .map(Thumbnail::Png)
        .map_err(CannotShow)
}

/// Why [`read_regular`] gave no bytes.
#[derive(Debug)]
pub(crate) enum SourceError {
    /// Its type and size could not be looked at (a missing file, say).
    NotThere(std::io::Error),
    /// Not a regular file (a folder, a FIFO, a device, a socket).
    NotRegular,
    /// More than the limit: by its size, or because it grew while read.
    TooLarge,
    /// It could not be opened or read.
    Unreadable(std::io::Error),
}

/// The bytes of `src`, if it is a regular file of at most `limit` bytes.
/// The type and size are checked before it is opened, so a FIFO is never
/// opened (which would block) and a large file is never read. At most one
/// byte past `limit` is read if the file grows in between.
pub(crate) fn read_regular(src: &Path, limit: u64) -> Result<Vec<u8>, SourceError> {
    let metadata = fs::metadata(src).map_err(SourceError::NotThere)?;
    if !metadata.is_file() {
        return Err(SourceError::NotRegular);
    }
    if metadata.len() > limit {
        return Err(SourceError::TooLarge);
    }
    let file = fs::File::open(src).map_err(SourceError::Unreadable)?;
    crate::read_at_most(file, limit)
        .map_err(SourceError::Unreadable)?
        .ok_or(SourceError::TooLarge)
}

/// [`read_regular`], with the reason as the thumbnail's text.
fn read_source(src: &Path, limit: u64) -> Result<Vec<u8>, String> {
    read_regular(src, limit).map_err(|error| match error {
        SourceError::NotRegular => "not a regular file".to_owned(),
        SourceError::TooLarge => format!("too large: more than {limit} bytes"),
        SourceError::NotThere(cause) | SourceError::Unreadable(cause) => {
            format!("could not read the file: {cause}")
        }
    })
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

/// `rgba` (`width`×`height` pixels of four bytes) turned the way the EXIF
/// Orientation `orientation` says it is stored, so it shows upright; gives
/// the pixels and the new width and height.
///
/// Where the stored top left pixel goes (EXIF's table): 1 stays, 2 top
/// right (mirrored left-right), 3 bottom right (turned 180°), 4 bottom
/// left (mirrored top-bottom), 5 stays top left (transposed), 6 top right
/// (turned 90° clockwise), 7 bottom right (transversed), 8 bottom left
/// (turned 90° counter-clockwise). From 5 on the sides swap.
///
/// A value outside 1 to 8, and a buffer that is not `width * height * 4`
/// bytes, come back as they are.
pub(crate) fn orient(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    orientation: u8,
) -> (Vec<u8>, u32, u32) {
    let (w, h) = (width as usize, height as usize);
    let pixels = rgba.as_chunks::<4>().0;
    let sized = w.checked_mul(h) == Some(pixels.len()) && rgba.len() == pixels.len() * 4;
    if orientation <= 1 || orientation > 8 || !sized {
        return (rgba, width, height);
    }
    // The turned size, and where the stored pixel (x, y) lands in it.
    let (out_w, out_h) = if orientation >= 5 { (h, w) } else { (w, h) };
    let target = |x: usize, y: usize| match orientation {
        2 => (w - 1 - x, y),
        3 => (w - 1 - x, h - 1 - y),
        4 => (x, h - 1 - y),
        5 => (y, x),
        6 => (h - 1 - y, x),
        7 => (h - 1 - y, w - 1 - x),
        _ => (y, w - 1 - x),
    };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (tx, ty) = target(x, y);
            let at = (ty * out_w + tx) * 4;
            out[at..at + 4].copy_from_slice(&pixels[y * w + x]);
        }
    }
    (out, out_w as u32, out_h as u32)
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

    // --- orient -----------------------------------------------------------------

    /// A picture with one distinct pixel per letter of `rows`.
    fn grid(rows: &[&str]) -> (Vec<u8>, u32, u32) {
        let height = rows.len() as u32;
        let width = rows[0].len() as u32;
        let rgba = rows
            .iter()
            .flat_map(|row| row.bytes())
            .flat_map(|letter| [letter, letter ^ 0x55, 7, 255])
            .collect();
        (rgba, width, height)
    }

    #[test]
    fn orient_puts_every_pixel_where_the_standard_says() {
        // The result of each EXIF value on ABC / DEF, as rows of letters,
        // written from the standard's description of the stored image:
        // 2 mirrored left-right, 3 turned 180, 4 mirrored top-bottom,
        // 5 transposed, 6 turned 90 clockwise, 7 transversed, 8 turned 90
        // counter-clockwise.
        let expected: [(u8, &[&str]); 8] = [
            (1, &["ABC", "DEF"]),
            (2, &["CBA", "FED"]),
            (3, &["FED", "CBA"]),
            (4, &["DEF", "ABC"]),
            (5, &["AD", "BE", "CF"]),
            (6, &["DA", "EB", "FC"]),
            (7, &["FC", "EB", "DA"]),
            (8, &["CF", "BE", "AD"]),
        ];
        let (rgba, width, height) = grid(&["ABC", "DEF"]);
        for (value, rows) in expected {
            let got = orient(rgba.clone(), width, height, value);
            assert_eq!(got, grid(rows), "orientation {value}");
        }
    }

    #[test]
    fn orient_1_leaves_the_buffer_and_the_size_unchanged() {
        let (rgba, width, height) = grid(&["ABC", "DEF"]);
        assert_eq!(orient(rgba.clone(), width, height, 1), (rgba, 3, 2));
    }

    #[test]
    fn orient_5_to_8_swap_the_sides_and_1_to_4_keep_them() {
        for (width, height) in [(3, 2), (1, 5), (5, 1), (64, 63)] {
            let rgba = vec![9; (width * height * 4) as usize];
            for value in 1..=4 {
                let (_, w, h) = orient(rgba.clone(), width, height, value);
                assert_eq!((w, h), (width, height), "{value} on {width}x{height}");
            }
            for value in 5..=8 {
                let (_, w, h) = orient(rgba.clone(), width, height, value);
                assert_eq!((w, h), (height, width), "{value} on {width}x{height}");
            }
        }
    }

    #[test]
    fn orient_gives_back_every_pixel_once() {
        for (width, height) in [(1, 1), (3, 2), (1, 5), (5, 1), (64, 63)] {
            let rgba: Vec<u8> = (0..width * height)
                .flat_map(|i| [i as u8, (i >> 8) as u8, 1, 255])
                .collect();
            let sorted = |rgba: &[u8]| {
                let mut pixels = rgba.as_chunks::<4>().0.to_vec();
                pixels.sort();
                pixels
            };
            for value in 1..=8 {
                let (out, ..) = orient(rgba.clone(), width, height, value);
                assert_eq!(sorted(&out), sorted(&rgba), "{value} on {width}x{height}");
            }
        }
    }

    #[test]
    fn orient_leaves_a_buffer_of_the_wrong_length_and_other_values_unchanged() {
        let (rgba, width, height) = grid(&["ABC", "DEF"]);
        for value in [0, 9, 10, 255] {
            let got = orient(rgba.clone(), width, height, value);
            assert_eq!(got, (rgba.clone(), 3, 2), "orientation {value}");
        }
        // 3x2 pixels need 24 bytes.
        for len in [0, 4, 20, 23, 25, 28] {
            let wrong = vec![1u8; len];
            let got = orient(wrong.clone(), width, height, 6);
            assert_eq!(got, (wrong, 3, 2), "length {len}");
        }
    }

    // --- read_regular -----------------------------------------------------------

    #[test]
    fn read_regular_reads_a_regular_file_within_the_limit_and_one_of_exactly_the_limit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.png");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_regular(&path, 10).unwrap(), b"0123456789");
        assert_eq!(read_regular(&path, 100).unwrap(), b"0123456789");
    }

    #[test]
    fn read_regular_says_too_large_for_a_file_over_the_limit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.png");
        fs::write(&path, b"0123456789A").unwrap();
        assert!(matches!(
            read_regular(&path, 10),
            Err(SourceError::TooLarge)
        ));
    }

    #[test]
    fn read_regular_says_not_regular_for_a_folder_and_not_unreadable_or_too_large() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(matches!(
            read_regular(tmp.path(), 10),
            Err(SourceError::NotRegular)
        ));
    }

    #[test]
    fn read_regular_says_not_there_with_the_os_error_for_a_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        match read_regular(&tmp.path().join("gone.png"), 10) {
            Err(SourceError::NotThere(cause)) => {
                assert_eq!(cause.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected NotThere, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_regular_says_unreadable_with_the_os_error_for_a_file_it_cannot_open() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.png");
        fs::write(&path, b"0123456789").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&path).is_ok() {
            // Running as root: the file opens whatever its mode says.
            return;
        }
        match read_regular(&path, 100) {
            Err(SourceError::Unreadable(cause)) => {
                assert_eq!(cause.kind(), std::io::ErrorKind::PermissionDenied);
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    /// A file that says it is empty and then reads as more than the limit:
    /// what a file that grows after its size was looked at does.
    #[cfg(target_os = "linux")]
    #[test]
    fn read_regular_says_too_large_for_a_file_that_grows_past_the_limit_after_its_size_was_read() {
        let status = Path::new("/proc/self/status");
        assert_eq!(fs::metadata(status).unwrap().len(), 0);
        assert!(matches!(
            read_regular(status, 10),
            Err(SourceError::TooLarge)
        ));
        // The pair: with room for it, the same file is read.
        assert!(read_regular(status, 1 << 20).unwrap().len() > 10);
    }
}
