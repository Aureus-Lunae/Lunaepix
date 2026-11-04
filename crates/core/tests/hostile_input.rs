//! Crafted, hostile input across formats, through the public API only.
//!
//! Every refusal here must be an error that leaves no output file and no
//! temporary file, and comes back quickly: a refusal that first allocates for
//! the declared image is the defect these tests exist to catch. Step 6 (SVG)
//! adds its crafted inputs here too.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lunaepix_core::{OptimizeError, optimize_file};
use tempfile::TempDir;

/// Generous: a refusal takes milliseconds; allocating and touching gigabytes
/// takes far longer.
const QUICK: Duration = Duration::from_secs(5);

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn listing(dir: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    names
}

/// Optimise `bytes` as `name` and assert the hostile-input contract: an
/// `Invalid` error, quickly, with no output and nothing else left behind.
/// Returns the error text for further checks.
fn assert_refused(name: &str, bytes: &[u8]) -> String {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), name, bytes);
    let dest = tmp.path().join(format!("optimized-{name}"));
    let before = listing(tmp.path());

    let started = Instant::now();
    let result = optimize_file(&src, &dest);
    let elapsed = started.elapsed();

    let err = result.expect_err("hostile input must be refused");
    assert!(elapsed < QUICK, "took {elapsed:?}");
    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists(), "an output file was written");
    assert_eq!(listing(tmp.path()), before, "something was left behind");
    err.to_string()
}

// --- PNG ---------------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// One chunk with a valid CRC.
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
    out
}

/// IHDR data: `w`x`h`, the given bit depth and colour type, no interlace.
fn ihdr(w: u32, h: u32, depth: u8, colour: u8) -> Vec<u8> {
    let mut data = w.to_be_bytes().to_vec();
    data.extend_from_slice(&h.to_be_bytes());
    data.extend_from_slice(&[depth, colour, 0, 0, 0]);
    chunk(b"IHDR", &data)
}

/// A zlib stream of one empty stored block (header, block, Adler-32 of
/// nothing): a token IDAT.
const TOKEN_IDAT: [u8; 11] = [
    0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01,
];

fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    for c in chunks {
        bytes.extend_from_slice(c);
    }
    bytes.extend(chunk(b"IEND", &[]));
    bytes
}

/// The concatenated IDAT data of a PNG.
fn idat_data(png: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        if &png[at + 4..at + 8] == b"IDAT" {
            data.extend_from_slice(&png[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    data
}

/// A normal 64x64 greyscale PNG from the `png` encoder, with a tEXt chunk
/// after the IHDR and a wasteful compression level.
fn normal_png_with_text() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 64, 64);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_filter(png::FilterType::NoFilter);
    encoder
        .add_text_chunk("Comment".to_string(), "x".repeat(500))
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    let pixels: Vec<u8> = (0..64 * 64).map(|i| (i % 7 * 30) as u8).collect();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

#[test]
fn the_chunk_helper_writes_the_crc_png_requires() {
    // Proof that the crafted files carry real CRCs: IEND's CRC is fixed.
    assert_eq!(&chunk(b"IEND", &[])[8..], [0xAE, 0x42, 0x60, 0x82]);
}

#[test]
fn still_optimises_a_normal_png_with_an_ancillary_chunk_after_its_ihdr() {
    // The positive for the chunk walk: a well-formed file is not refused.
    let tmp = TempDir::new().unwrap();
    let original = normal_png_with_text();
    let src = write(tmp.path(), "a.png", &original);
    let dest = tmp.path().join("optimized-a.png");

    optimize_file(&src, &dest).unwrap();

    assert!(fs::metadata(&dest).unwrap().len() < original.len() as u64);
}

#[test]
fn refuses_a_png_that_declares_65535_by_65535_pixels() {
    let bomb = png(&[ihdr(65_535, 65_535, 8, 6), chunk(b"IDAT", &TOKEN_IDAT)]);
    assert!(bomb.len() < 100, "the fixture must be tiny");

    let message = assert_refused("bomb.png", &bomb);

    assert!(
        message.contains("image too large: 65535×65535 exceeds 100 megapixels"),
        "{message}"
    );
}

/// Huge, but 1-bit greyscale: 537 MB of raw data. Before the chunk walk
/// existed, oxipng allocated that much for these files (and survived it,
/// lazily); in 8-bit RGBA the RED run would have aborted on 17 GB.
fn huge_ihdr() -> Vec<u8> {
    ihdr(65_535, 65_535, 1, 0)
}

#[test]
fn refuses_a_png_that_hides_a_huge_ihdr_behind_an_ancillary_chunk() {
    let bomb = png(&[
        chunk(b"tEXt", b"Comment\0first"),
        huge_ihdr(),
        chunk(b"IDAT", &TOKEN_IDAT),
    ]);

    let message = assert_refused("bomb.png", &bomb);

    assert!(message.contains("first chunk is not IHDR"), "{message}");
}

#[test]
fn refuses_a_png_with_a_second_huge_ihdr_after_a_small_one() {
    // oxipng keeps the last IHDR it sees, so the small one is only a decoy.
    let bomb = png(&[ihdr(1, 1, 8, 0), huge_ihdr(), chunk(b"IDAT", &TOKEN_IDAT)]);

    let message = assert_refused("bomb.png", &bomb);

    assert!(message.contains("more than one IHDR"), "{message}");
}

#[test]
fn refuses_a_png_whose_idat_inflates_far_past_its_small_ihdr() {
    // A 16x16 IHDR (272 bytes of raw data) over the IDAT of a 2048x2048
    // image (4 MiB of zeros, a few KiB compressed). oxipng sizes its inflate
    // buffer from the IHDR and stops when the stream overflows it, so the
    // stream's own size is never allocated.
    let mut big = Vec::new();
    let mut encoder = png::Encoder::new(&mut big, 2048, 2048);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&vec![0; 2048 * 2048]).unwrap();
    writer.finish().unwrap();
    let stream = idat_data(&big);
    assert!(stream.len() < 64 * 1024, "the stream must be small");
    let bomb = png(&[ihdr(16, 16, 8, 0), chunk(b"IDAT", &stream)]);

    let message = assert_refused("bomb.png", &bomb);

    assert!(message.contains("could not read the PNG data"), "{message}");
}

// --- JPEG ---------------------------------------------------------------------------

/// A hand-made baseline JPEG header that declares `w`x`h` greyscale pixels:
/// SOI, one quantisation table, SOF0, SOS, a few bytes of scan, EOI.
fn jpeg_declaring(w: u16, h: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8];
    // DQT: table 0, 8-bit, all ones.
    bytes.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
    bytes.extend_from_slice(&[1; 64]);
    // SOF0: precision 8, height, width, one component (id 1, 1x1, table 0).
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
    // SOS: one component, tables 0/0, full spectrum.
    bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    bytes.extend_from_slice(&[0x00; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD9]);
    bytes
}

#[test]
fn the_handmade_jpeg_header_declares_the_dimensions_it_is_given() {
    // Proof that the fixture says what the tests below rely on: the SOF0
    // payload starts 4 bytes into the segment, after the precision byte.
    let header = jpeg_declaring(65_500, 1234);
    let sof = header.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    assert_eq!(header[sof + 4], 8, "precision");
    assert_eq!(
        u16::from_be_bytes([header[sof + 5], header[sof + 6]]),
        1234,
        "height"
    );
    assert_eq!(
        u16::from_be_bytes([header[sof + 7], header[sof + 8]]),
        65_500,
        "width"
    );
}

#[test]
fn refuses_a_jpeg_that_declares_65500_by_65500_pixels() {
    // 65500 is the largest side libjpeg accepts itself, so only the pixel
    // limit can refuse this one.
    let bomb = jpeg_declaring(65_500, 65_500);
    assert!(bomb.len() < 200, "the fixture must be tiny");

    let message = assert_refused("bomb.jpg", &bomb);

    assert!(
        message.contains("image too large: 65500×65500 exceeds 100 megapixels"),
        "{message}"
    );
}

#[test]
fn refuses_a_jpeg_that_declares_65535_by_65535_pixels() {
    // libjpeg refuses a side over 65500 itself, before the pixel limit.
    let message = assert_refused("bomb.jpg", &jpeg_declaring(65_535, 65_535));

    assert!(
        message.contains("could not read the JPEG data"),
        "{message}"
    );
}
