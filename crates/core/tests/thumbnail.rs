//! `thumbnail`, through the public API only. Fixtures are made here, with
//! encoders other than the code under test (`image`, `png`, and libwebp
//! through the shared WebP fixtures), and the thumbnails are read back with
//! `image`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::jpeg_fixtures::{ADOBE_CMYK, ADOBE_YCCK, four_component_jpeg, grey_header};
use common::webp_fixtures as fx;
use lunaepix_core::{
    CannotShow, MAX_FILE_BYTES, MAX_SVG_THUMB_BYTES, THUMB_SIDE, Thumbnail, thumbnail,
};
use tempfile::TempDir;

// --- fixtures ---------------------------------------------------------------

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// `w`x`h` RGBA pixels of one colour.
fn solid(w: u32, h: u32, colour: [u8; 4]) -> Vec<u8> {
    colour.repeat((w * h) as usize)
}

/// `w`x`h` RGBA pixels: the left half red, the right half blue.
fn halves(w: u32, h: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..h {
        for x in 0..w {
            pixels.extend_from_slice(if x < w / 2 { &RED } else { &BLUE });
        }
    }
    pixels
}

/// RGBA without its alpha bytes.
fn rgb_of(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, _]| [r, g, b])
        .collect()
}

/// A PNG from the `png` encoder with exactly this colour type and depth.
fn png_of(w: u32, h: u32, colour: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
    png_with(w, h, colour, depth, data, |_| {})
}

/// [`png_of`], with `setup` run on the encoder before the header is
/// written (for a palette and its `tRNS`).
fn png_with(
    w: u32,
    h: u32,
    colour: png::ColorType,
    depth: png::BitDepth,
    data: &[u8],
    setup: impl FnOnce(&mut png::Encoder<&mut Vec<u8>>),
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(colour);
    encoder.set_depth(depth);
    setup(&mut encoder);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(data).unwrap();
    writer.finish().unwrap();
    out
}

fn rgba_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    png_of(w, h, png::ColorType::Rgba, png::BitDepth::Eight, rgba)
}

/// A JPEG from the `image` crate at quality 95: RGB, or greyscale with
/// `grey`.
fn jpg_of(w: u32, h: u32, pixels: &[u8], grey: bool) -> Vec<u8> {
    let colour = if grey {
        image::ExtendedColorType::L8
    } else {
        image::ExtendedColorType::Rgb8
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
        .encode(pixels, w, h, colour)
        .unwrap();
    out
}

fn rgb_jpg(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    jpg_of(w, h, &rgb_of(rgba), false)
}

/// The same `w`x`h` RGBA picture as a JPG, a PNG, a lossless WebP and a
/// lossy WebP, each with its file name.
fn every_format(w: u32, h: u32, rgba: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.jpg", rgb_jpg(w, h, rgba)),
        ("a.png", rgba_png(w, h, rgba)),
        ("a.webp", fx::lossless(rgba, w, h)),
        ("b.webp", fx::lossy(rgba, w, h, 95.0)),
    ]
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// The thumbnail of `bytes` saved as `name`.
fn thumb_of(name: &str, bytes: &[u8]) -> Result<Thumbnail, CannotShow> {
    let tmp = TempDir::new().unwrap();
    thumbnail(&write(tmp.path(), name, bytes))
}

/// A thumbnail that must be a PNG, read back by `image`: it must be an
/// 8-bit RGBA PNG, and it comes back as its pixels.
fn png_pixels(result: Result<Thumbnail, CannotShow>, what: &str) -> image::RgbaImage {
    let bytes = match result {
        Ok(Thumbnail::Png(bytes)) => bytes,
        other => panic!("{what}: expected a PNG thumbnail, got {other:?}"),
    };
    match image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap() {
        image::DynamicImage::ImageRgba8(pixels) => pixels,
        other => panic!("{what}: not 8-bit RGBA but {:?}", other.color()),
    }
}

fn reason(result: Result<Thumbnail, CannotShow>, what: &str) -> String {
    match result {
        Err(CannotShow(reason)) => reason,
        Ok(thumb) => panic!("{what}: expected CannotShow, got {thumb:?}"),
    }
}

/// Whether `pixel` is within 16 of `colour` on every channel.
fn near(pixel: &image::Rgba<u8>, colour: [u8; 4]) -> bool {
    pixel
        .0
        .iter()
        .zip(colour)
        .all(|(&p, c)| p.abs_diff(c) <= 16)
}

fn has_blue(pixels: &image::RgbaImage) -> bool {
    pixels.pixels().any(|p| p.0[2] > 128)
}

// --- size ---------------------------------------------------------------------

#[test]
fn a_400x200_jpg_png_and_webp_each_give_a_64x32_rgba_png() {
    assert_eq!(THUMB_SIDE, 64);
    for (name, bytes) in every_format(400, 200, &fx::photo_rgba(400, 200, false)) {
        let pixels = png_pixels(thumb_of(name, &bytes), name);
        assert_eq!(pixels.dimensions(), (64, 32), "{name}");
    }
    // Portrait the other way round.
    for (name, bytes) in every_format(200, 400, &fx::photo_rgba(200, 400, false)) {
        let pixels = png_pixels(thumb_of(name, &bytes), name);
        assert_eq!(pixels.dimensions(), (32, 64), "{name}");
    }
}

#[test]
fn a_20x10_image_is_not_enlarged() {
    for (name, bytes) in every_format(20, 10, &fx::photo_rgba(20, 10, false)) {
        let pixels = png_pixels(thumb_of(name, &bytes), name);
        assert_eq!(pixels.dimensions(), (20, 10), "{name}");
    }
}

#[test]
fn a_64x64_image_keeps_its_size() {
    for (name, bytes) in every_format(64, 64, &fx::photo_rgba(64, 64, false)) {
        let pixels = png_pixels(thumb_of(name, &bytes), name);
        assert_eq!(pixels.dimensions(), (64, 64), "{name}");
    }
}

#[test]
fn a_65x64_image_becomes_64x63() {
    for (name, bytes) in every_format(65, 64, &fx::photo_rgba(65, 64, false)) {
        let pixels = png_pixels(thumb_of(name, &bytes), name);
        assert_eq!(pixels.dimensions(), (64, 63), "{name}");
    }
}

// --- colour -------------------------------------------------------------------

#[test]
fn a_red_and_blue_image_keeps_red_on_the_left_and_blue_on_the_right() {
    for (name, bytes) in every_format(400, 200, &halves(400, 200)) {
        let pixels = png_pixels(thumb_of(name, &bytes), name);
        let (left, right) = (pixels.get_pixel(2, 16), pixels.get_pixel(61, 16));
        assert!(near(left, RED), "{name}: left {left:?}");
        assert!(near(right, BLUE), "{name}: right {right:?}");
    }
}

#[test]
fn png_colour_types_each_give_an_rgba_thumbnail() {
    use png::{BitDepth, ColorType};
    let (w, h) = (4, 2);
    let n = (w * h) as usize;
    let grey = png_of(w, h, ColorType::Grayscale, BitDepth::Eight, &vec![100; n]);
    let grey_alpha = png_of(
        w,
        h,
        ColorType::GrayscaleAlpha,
        BitDepth::Eight,
        &[100, 50].repeat(n),
    );
    // 16-bit samples: 0x1234, 0x5678, 0x9ABC, big-endian.
    let rgb16 = png_of(
        w,
        h,
        ColorType::Rgb,
        BitDepth::Sixteen,
        &[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC].repeat(n),
    );
    let cases = [
        ("grey", grey, [100, 100, 100, 255]),
        ("grey with alpha", grey_alpha, [100, 100, 100, 50]),
        ("16-bit RGB", rgb16, [0x12, 0x56, 0x9A, 255]),
    ];
    for (what, bytes, expected) in cases {
        let pixels = png_pixels(thumb_of("a.png", &bytes), what);
        assert_eq!(pixels.dimensions(), (w, h), "{what}");
        assert!(pixels.pixels().all(|p| p.0 == expected), "{what}");
    }
}

/// A 4x2 palette PNG: red and blue, index 0 in the top-left corner and 1
/// everywhere else; with `trns`, entry 0 is fully transparent.
fn palette_png(trns: bool) -> Vec<u8> {
    let mut indices = vec![1u8; 8];
    indices[0] = 0;
    png_with(
        4,
        2,
        png::ColorType::Indexed,
        png::BitDepth::Eight,
        &indices,
        |encoder| {
            encoder.set_palette(vec![255, 0, 0, 0, 0, 255]);
            if trns {
                encoder.set_trns(vec![0, 255]);
            }
        },
    )
}

#[test]
fn the_transparent_pixel_of_a_palette_png_stays_transparent() {
    let pixels = png_pixels(thumb_of("a.png", &palette_png(true)), "palette");
    assert_eq!(pixels.dimensions(), (4, 2));
    assert_eq!(pixels.get_pixel(0, 0).0[3], 0);
    assert_eq!(pixels.get_pixel(1, 0).0, BLUE);
}

#[test]
fn an_opaque_palette_png_is_opaque_everywhere() {
    let pixels = png_pixels(thumb_of("a.png", &palette_png(false)), "palette");
    assert_eq!(pixels.get_pixel(0, 0).0, RED);
    assert!(pixels.pixels().all(|p| p.0[3] == 255));
}

#[test]
fn a_greyscale_jpg_gives_a_grey_thumbnail() {
    let bytes = jpg_of(400, 200, &vec![128; 400 * 200], true);
    let pixels = png_pixels(thumb_of("a.jpg", &bytes), "grey jpg");
    assert_eq!(pixels.dimensions(), (64, 32));
    for p in pixels.pixels() {
        let [r, g, b, a] = p.0;
        assert!(r == g && g == b, "{p:?}");
        assert!(r.abs_diff(128) <= 4, "{p:?}");
        assert_eq!(a, 255);
    }
}

#[test]
fn a_cmyk_or_ycck_jpg_cannot_be_shown() {
    for transform in [ADOBE_CMYK, ADOBE_YCCK] {
        let why = reason(
            thumb_of("a.jpg", &four_component_jpeg(transform)),
            "four components",
        );
        assert!(why.contains("CMYK"), "{why}");
    }
}

// --- animation ----------------------------------------------------------------

/// A 16x16 APNG: with `default`, a separate default image (not part of the
/// animation) in that colour first; then one frame per colour of `frames`.
fn apng(default: Option<[u8; 4]>, frames: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 16, 16);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(frames.len() as u32, 0).unwrap();
    encoder.set_sep_def_img(default.is_some()).unwrap();
    let mut writer = encoder.write_header().unwrap();
    for colour in default.iter().chain(frames) {
        writer.write_image_data(&solid(16, 16, *colour)).unwrap();
    }
    writer.finish().unwrap();
    out
}

#[test]
fn an_animated_png_gives_its_default_image() {
    // The first frame is the default image, and a default image that is not
    // a frame at all.
    for (what, bytes) in [
        ("first frame", apng(None, &[RED, BLUE])),
        ("separate default", apng(Some(RED), &[BLUE, BLUE])),
    ] {
        let pixels = png_pixels(thumb_of("a.png", &bytes), what);
        assert_eq!(pixels.dimensions(), (16, 16), "{what}");
        assert!(pixels.pixels().all(|p| near(p, RED)), "{what}");
        assert!(!has_blue(&pixels), "{what}");
    }
}

#[test]
fn an_animated_webp_gives_its_first_frame() {
    let frames = [
        fx::lossless(&solid(16, 16, RED), 16, 16),
        fx::lossless(&solid(16, 16, BLUE), 16, 16),
    ];
    let bytes = fx::animated(&frames, 16, 16);
    let pixels = png_pixels(thumb_of("a.webp", &bytes), "animated webp");
    assert_eq!(pixels.dimensions(), (16, 16));
    assert!(pixels.pixels().all(|p| near(p, RED)));
    assert!(!has_blue(&pixels));
}

#[test]
fn a_large_animated_webp_is_scaled_down_from_its_canvas() {
    let frames = [
        fx::lossless(&halves(400, 200), 400, 200),
        fx::lossless(&solid(400, 200, BLUE), 400, 200),
    ];
    let bytes = fx::animated(&frames, 400, 200);
    let pixels = png_pixels(thumb_of("a.webp", &bytes), "animated webp");
    assert_eq!(pixels.dimensions(), (64, 32));
    assert!(near(pixels.get_pixel(2, 16), RED));
    assert!(near(pixels.get_pixel(61, 16), BLUE));
}

// --- bounds -------------------------------------------------------------------

/// A PNG chunk with its CRC.
fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
    out
}

/// A PNG of only a signature, an RGBA IHDR of `w`x`h` and IEND: no image
/// data at all.
fn png_header_only(w: u32, h: u32) -> Vec<u8> {
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(png_chunk(b"IHDR", &ihdr));
    bytes.extend(png_chunk(b"IEND", &[]));
    bytes
}

/// A WebP of one `VP8L` chunk whose header declares `w`x`h`, over a
/// bitstream of zeros.
fn webp_header_only(w: u32, h: u32) -> Vec<u8> {
    let bits = (w - 1) | ((h - 1) << 14);
    let mut data = vec![0x2F];
    data.extend_from_slice(&bits.to_le_bytes());
    data.extend_from_slice(&[0; 5]);
    fx::from_chunks(&[(*b"VP8L", data)])
}

/// Header-only fixtures of `w`x`h` in every decoded format.
fn headers_only(w: u32, h: u32) -> [(&'static str, Vec<u8>); 3] {
    [
        ("a.png", png_header_only(w, h)),
        (
            "a.jpg",
            grey_header(u16::try_from(w).unwrap(), u16::try_from(h).unwrap()),
        ),
        ("a.webp", webp_header_only(w, h)),
    ]
}

#[test]
fn one_pixel_more_cannot_be_shown_without_decoding() {
    for (name, bytes) in headers_only(10_000, 10_001) {
        let why = reason(thumb_of(name, &bytes), name);
        // The header check's reason, not a decoder's: it came first.
        assert_eq!(
            why, "image too large: 10000×10001 exceeds 100 megapixels",
            "{name}"
        );
    }
}

#[test]
fn an_image_of_exactly_max_pixels_is_not_refused_for_its_size() {
    // No image data, so each still fails, but later, in its decoder.
    for (name, bytes) in headers_only(10_000, 10_000) {
        let why = reason(thumb_of(name, &bytes), name);
        assert!(!why.contains("too large"), "{name}: {why}");
    }
}

#[test]
fn a_file_over_max_file_bytes_cannot_be_shown() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.png");
    let file = fs::File::create(&path).unwrap();
    // Sparse: nothing is written.
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    let why = reason(thumbnail(&path), "over the limit");
    assert!(why.contains("too large"), "{why}");
}

#[test]
fn a_file_of_exactly_max_file_bytes_is_read() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.png");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_FILE_BYTES).unwrap();
    // Read, and refused for its content (zeros), not for its size.
    let why = reason(thumbnail(&path), "at the limit");
    assert!(why.contains("not a PNG"), "{why}");
}

// --- not images ---------------------------------------------------------------

#[test]
fn an_empty_file_cannot_be_shown() {
    for name in ["a.png", "a.jpg", "a.webp"] {
        let why = reason(thumb_of(name, b""), name);
        assert!(!why.is_empty(), "{name}");
    }
}

#[test]
fn a_text_file_named_png_cannot_be_shown() {
    let why = reason(thumb_of("a.png", b"just some text\n"), "text");
    assert!(why.contains("not a PNG"), "{why}");
}

#[test]
fn a_png_named_jpg_or_webp_cannot_be_shown() {
    let png = rgba_png(20, 10, &solid(20, 10, RED));
    assert!(thumb_of("a.png", &png).is_ok(), "fixture");
    for name in ["a.jpg", "a.webp"] {
        reason(thumb_of(name, &png), name);
    }
}

#[test]
fn a_truncated_jpg_cannot_be_shown() {
    let full = rgb_jpg(400, 200, &fx::photo_rgba(400, 200, false));
    assert!(thumb_of("a.jpg", &full).is_ok(), "fixture");
    let why = reason(thumb_of("a.jpg", &full[..full.len() * 2 / 3]), "cut");
    assert!(why.contains("damaged"), "{why}");
}

#[test]
fn a_truncated_png_or_webp_cannot_be_shown() {
    let pixels = fx::photo_rgba(400, 200, false);
    for (name, full) in [
        ("a.png", rgba_png(400, 200, &pixels)),
        ("a.webp", fx::lossless(&pixels, 400, 200)),
    ] {
        reason(thumb_of(name, &full[..full.len() * 2 / 3]), name);
    }
}

#[test]
fn a_folder_named_a_png_or_a_svg_cannot_be_shown() {
    let tmp = TempDir::new().unwrap();
    for name in ["a.png", "a.svg"] {
        let folder = tmp.path().join(name);
        fs::create_dir(&folder).unwrap();
        let why = reason(thumbnail(&folder), name);
        assert!(why.contains("not a regular file"), "{name}: {why}");
    }
}

#[test]
fn a_missing_file_cannot_be_shown() {
    let tmp = TempDir::new().unwrap();
    reason(thumbnail(&tmp.path().join("gone.png")), "missing");
}

#[test]
fn a_picture_without_a_supported_extension_cannot_be_shown() {
    let png = rgba_png(20, 10, &solid(20, 10, RED));
    for name in ["a.txt", "a.gif", "png"] {
        let why = reason(thumb_of(name, &png), name);
        assert!(why.contains("not a PNG, JPG, SVG or WebP"), "{name}: {why}");
    }
}

/// A named pipe that nothing writes to: opening it for reading blocks.
#[cfg(unix)]
fn fifo(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo failed");
    path
}

#[cfg(unix)]
#[test]
fn a_fifo_named_a_png_cannot_be_shown_and_is_never_opened() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let trap = fifo(tmp.path(), "a.png");
    let (tx, rx) = mpsc::channel();
    let path = trap.clone();
    thread::spawn(move || {
        let _ = tx.send(thumbnail(&path));
    });
    let result = rx.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        // Release the reader stuck in the pipe so the test run ends.
        thread::spawn(move || drop(fs::OpenOptions::new().write(true).open(trap)));
    }
    let result = result.expect("thumbnail blocked opening the FIFO");
    let why = reason(result, "fifo");
    assert!(why.contains("not a regular file"), "{why}");
}

#[cfg(unix)]
#[test]
fn a_device_named_a_png_cannot_be_shown() {
    let tmp = TempDir::new().unwrap();
    let link = tmp.path().join("a.png");
    std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
    let why = reason(thumbnail(&link), "device");
    assert!(why.contains("not a regular file"), "{why}");
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_opened_cannot_be_shown() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let path = write(tmp.path(), "a.png", &rgba_png(20, 10, &solid(20, 10, RED)));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::File::open(&path).is_err(),
        "fixture: this user cannot open a mode-000 file"
    );
    let why = reason(thumbnail(&path), "unreadable");
    assert!(why.contains("could not read"), "{why}");
}

// --- SVG ----------------------------------------------------------------------

const SVG: &[u8] =
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#f00"/></svg>"##;

/// [`SVG`] followed by spaces up to exactly `len` bytes.
fn svg_of_len(len: u64) -> Vec<u8> {
    let mut bytes = SVG.to_vec();
    bytes.resize(usize::try_from(len).unwrap(), b' ');
    bytes
}

#[test]
fn an_svg_is_passed_through_as_it_is() {
    assert_eq!(thumb_of("a.svg", SVG), Ok(Thumbnail::Svg(SVG.to_vec())));
    // Upper case, and an XML declaration first.
    let declared = [b"<?xml version=\"1.0\"?>\n".as_slice(), SVG].concat();
    assert_eq!(thumb_of("A.SVG", &declared), Ok(Thumbnail::Svg(declared)));
}

#[test]
fn an_svg_with_a_bom_and_leading_whitespace_is_still_an_svg() {
    let bytes = [b"\xEF\xBB\xBF \n\t\r\n".as_slice(), SVG].concat();
    assert_eq!(thumb_of("a.svg", &bytes), Ok(Thumbnail::Svg(bytes)));
}

#[test]
fn an_svg_of_exactly_max_svg_thumb_bytes_is_passed_through() {
    assert_eq!(MAX_SVG_THUMB_BYTES, 2 * 1024 * 1024);
    let bytes = svg_of_len(MAX_SVG_THUMB_BYTES);
    assert_eq!(thumb_of("a.svg", &bytes), Ok(Thumbnail::Svg(bytes)));
}

#[test]
fn an_svg_one_byte_over_max_svg_thumb_bytes_cannot_be_shown() {
    let why = reason(
        thumb_of("a.svg", &svg_of_len(MAX_SVG_THUMB_BYTES + 1)),
        "big svg",
    );
    assert!(why.contains("too large"), "{why}");
}

#[test]
fn bytes_named_svg_that_are_not_svg_text_cannot_be_shown() {
    let png = rgba_png(20, 10, &solid(20, 10, RED));
    let mut broken_utf8 = SVG.to_vec();
    broken_utf8.extend_from_slice(b"<!-- \xFF -->");
    let cases: [(&str, &[u8]); 5] = [
        ("binary", &png),
        ("text without a tag", b"hello <svg/>"),
        ("not UTF-8 after the start", &broken_utf8),
        ("empty", b""),
        ("only whitespace and a BOM", b"\xEF\xBB\xBF  \n"),
    ];
    for (what, bytes) in cases {
        reason(thumb_of("a.svg", bytes), what);
    }
}
