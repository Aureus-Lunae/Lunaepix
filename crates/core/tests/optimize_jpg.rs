//! `optimize_file` for JPG, through the public API only. Fixtures are made
//! here: nothing depends on `samples/`. The DCT-coefficient comparison needs
//! libjpeg, so it lives with the other `unsafe` code in `src/jpg.rs`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use lunaepix_core::{OptimizeError, optimize_file};
use tempfile::TempDir;

// --- fixtures ---------------------------------------------------------------

/// RGB pixels: smooth gradients plus deterministic noise, so the image is
/// neither trivial nor random, like a photo.
fn photo_pixels(w: u32, h: u32) -> Vec<u8> {
    let mut seed = 0x2545_F491_u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 24) as u8
    };
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.push(((x * 255) / w) as u8 / 2 + noise());
            pixels.push(((y * 255) / h) as u8 / 2 + noise());
            pixels.push((((x + y) * 255) / (w + h)) as u8 / 2 + noise());
        }
    }
    pixels
}

/// A baseline JPEG from the `image` crate's encoder. It uses the standard
/// Huffman tables and writes a JFIF APP0, so there is something to optimise.
fn baseline_jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode(&photo_pixels(w, h), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// The usual fixture: big enough that progressive coding wins.
fn photo_jpeg() -> Vec<u8> {
    baseline_jpeg(256, 192)
}

/// A valid JPEG header, cut off in the middle of the entropy-coded scan.
fn truncated_jpeg() -> Vec<u8> {
    let full = photo_jpeg();
    let scan = sos_offset(&full);
    full[..scan + (full.len() - scan) / 2].to_vec()
}

/// A real PNG file.
fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbImage::from_raw(8, 8, vec![90; 8 * 8 * 3])
        .unwrap()
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

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

// --- JPEG structure helpers -----------------------------------------------------

/// The marker segments before the first SOS: `(marker, payload)`, where the
/// payload excludes the two length bytes.
fn segments(jpeg: &[u8]) -> Vec<(u8, Vec<u8>)> {
    assert_eq!(&jpeg[..2], [0xFF, 0xD8], "not a JPEG");
    let mut found = Vec::new();
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        assert_eq!(jpeg[at], 0xFF, "unparsable JPEG at {at}");
        let marker = jpeg[at + 1];
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        found.push((marker, jpeg[at + 4..at + 2 + len].to_vec()));
        if marker == 0xDA {
            break;
        }
        at += 2 + len;
    }
    found
}

/// Offset of the first SOS marker.
fn sos_offset(jpeg: &[u8]) -> usize {
    let mut at = 2;
    for (marker, payload) in segments(jpeg) {
        if marker == 0xDA {
            return at;
        }
        at += 4 + payload.len();
    }
    panic!("no SOS");
}

/// The start-of-frame marker: 0xC0 baseline, 0xC2 progressive.
fn frame_marker(jpeg: &[u8]) -> u8 {
    segments(jpeg)
        .into_iter()
        .map(|(marker, _)| marker)
        .find(|m| matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC))
        .expect("no SOF marker")
}

fn has_segment(jpeg: &[u8], marker: u8, prefix: &[u8]) -> bool {
    segments(jpeg)
        .iter()
        .any(|(m, payload)| *m == marker && payload.starts_with(prefix))
}

fn has_jfif(jpeg: &[u8]) -> bool {
    has_segment(jpeg, 0xE0, b"JFIF\0")
}

/// `jpeg` with the APP0 JFIF segment removed.
fn without_jfif(jpeg: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    let mut at = 2;
    for (marker, payload) in segments(jpeg) {
        let end = at + 4 + payload.len();
        if !(marker == 0xE0 && payload.starts_with(b"JFIF\0")) {
            out.extend_from_slice(&jpeg[at..end]);
        }
        at = end;
        if marker == 0xDA {
            break;
        }
    }
    out.extend_from_slice(&jpeg[at..]);
    out
}

/// `jpeg` with extra APPn segments inserted right after its first segments
/// (after SOI and the JFIF APP0, if any).
fn with_segments(jpeg: &[u8], extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut at = 2;
    if let Some((0xE0, payload)) = segments(jpeg).first() {
        at += 4 + payload.len();
    }
    let mut out = jpeg[..at].to_vec();
    for (marker, payload) in extra {
        out.extend_from_slice(&[0xFF, *marker]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&jpeg[at..]);
    out
}

/// An APP1 EXIF segment payload: a minimal big-endian TIFF header with one
/// empty IFD, plus a recognisable tail.
fn exif_payload() -> Vec<u8> {
    let mut payload = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\0\0\0\0\0".to_vec();
    payload.extend_from_slice(b"lunaepix-exif-marker");
    payload
}

/// An APP2 ICC segment payload: chunk 1 of 1, with opaque profile bytes.
fn icc_payload() -> Vec<u8> {
    let mut payload = b"ICC_PROFILE\0\x01\x01".to_vec();
    payload.extend_from_slice(&[0x42; 300]);
    payload
}

/// Decoded RGB pixels and dimensions.
fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).unwrap();
    (img.width(), img.height(), img.to_rgb8().into_raw())
}

fn assert_same_pixels(a: &[u8], b: &[u8]) {
    assert!(decode(a) == decode(b), "decoded images differ");
}

// --- not larger, identical pixels ----------------------------------------------

#[test]
fn optimizes_a_jpeg_to_a_smaller_file_with_identical_pixels() {
    let tmp = TempDir::new().unwrap();
    let original = photo_jpeg();
    let src = write(tmp.path(), "a.jpg", &original);
    let dest = tmp.path().join("optimized-a.jpg");

    let outcome = optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    assert!(
        optimized.len() < original.len(),
        "{} !< {}",
        optimized.len(),
        original.len()
    );
    assert_same_pixels(&original, &optimized);
    assert_eq!(outcome.before, original.len() as u64);
    assert_eq!(outcome.after, optimized.len() as u64);
}

#[test]
fn the_pixel_comparison_detects_a_different_image() {
    // Proof that `assert_same_pixels` can fail: the same picture at another
    // quality decodes to other pixels.
    let original = photo_jpeg();
    let mut other = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut other, 60)
        .encode(
            &photo_pixels(256, 192),
            256,
            192,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let result = std::panic::catch_unwind(|| assert_same_pixels(&original, &other));
    assert!(result.is_err());
}

#[test]
fn rejects_a_jpeg_truncated_mid_scan_and_leaves_no_output_or_temp_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.jpg", &truncated_jpeg());
    let dest = tmp.path().join("optimized-a.jpg");
    let before = listing(tmp.path());

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists());
    assert_eq!(listing(tmp.path()), before);
}

// --- the source is never touched ------------------------------------------------

#[test]
fn keeps_the_source_bytes_unchanged_after_a_successful_run() {
    let tmp = TempDir::new().unwrap();
    let original = photo_jpeg();
    let src = write(tmp.path(), "a.jpg", &original);
    optimize_file(&src, &tmp.path().join("optimized-a.jpg")).unwrap();
    assert_eq!(fs::read(&src).unwrap(), original);
}

#[test]
fn keeps_the_source_unchanged_when_the_run_fails() {
    let tmp = TempDir::new().unwrap();
    let corrupt = truncated_jpeg();
    let src = write(tmp.path(), "a.jpg", &corrupt);

    assert!(optimize_file(&src, &tmp.path().join("optimized-a.jpg")).is_err());

    assert_eq!(fs::read(&src).unwrap(), corrupt);
}

// --- the smaller of the two candidates, or the original ------------------------------

#[test]
fn writes_a_baseline_input_that_progressive_coding_shrinks_as_progressive() {
    let tmp = TempDir::new().unwrap();
    let original = photo_jpeg();
    assert_eq!(frame_marker(&original), 0xC0, "fixture must be baseline");
    let src = write(tmp.path(), "a.jpg", &original);
    let dest = tmp.path().join("optimized-a.jpg");

    optimize_file(&src, &dest).unwrap();

    assert_eq!(frame_marker(&fs::read(&dest).unwrap()), 0xC2);
}

#[test]
fn keeps_a_tiny_baseline_input_baseline_when_progressive_coding_does_not_pay() {
    // The counterpart of the test above: progressive is not always chosen. On
    // a tiny image the extra scan headers cost more than they save.
    let tmp = TempDir::new().unwrap();
    let original = baseline_jpeg(16, 16);
    let src = write(tmp.path(), "a.jpg", &original);
    let dest = tmp.path().join("optimized-a.jpg");

    optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    assert_eq!(frame_marker(&optimized), 0xC0);
    assert!(optimized.len() < original.len());
    assert_same_pixels(&original, &optimized);
}

#[test]
fn writes_an_already_optimal_jpeg_as_a_byte_identical_copy_of_equal_size() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.jpg", &photo_jpeg());
    let once = tmp.path().join("once.jpg");
    optimize_file(&first, &once).unwrap();
    let twice = tmp.path().join("twice.jpg");

    let outcome = optimize_file(&once, &twice).unwrap();

    assert_eq!(outcome.before, outcome.after);
    assert_eq!(fs::read(&once).unwrap(), fs::read(&twice).unwrap());
}

// --- content and extension ------------------------------------------------------

#[test]
fn rejects_png_bytes_named_jpg_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "picture.jpg", &png_bytes());
    let dest = tmp.path().join("optimized-picture.jpg");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("not a JPEG"), "{err}");
    assert!(!dest.exists());
    assert_eq!(fs::read(&src).unwrap(), png_bytes());
}

#[test]
fn accepts_real_jpeg_bytes_named_with_an_uppercase_jpeg_extension() {
    let tmp = TempDir::new().unwrap();
    let original = photo_jpeg();
    let src = write(tmp.path(), "A.JPEG", &original);
    let dest = tmp.path().join("optimized-A.JPEG");

    optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    assert!(optimized.len() < original.len());
    assert_same_pixels(&original, &optimized);
}

#[test]
fn rejects_an_empty_jpg_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "empty.jpg", b"");
    let dest = tmp.path().join("optimized-empty.jpg");
    assert!(matches!(
        optimize_file(&src, &dest),
        Err(OptimizeError::Invalid { .. })
    ));
    assert!(!dest.exists());
}

#[test]
fn rejects_a_jpeg_start_marker_followed_by_garbage() {
    // Passes the SOI check, so libjpeg itself must refuse it.
    let tmp = TempDir::new().unwrap();
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    bytes.extend_from_slice(&[0x5A; 200]);
    let src = write(tmp.path(), "a.jpg", &bytes);
    let dest = tmp.path().join("optimized-a.jpg");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists());
}

// --- markers ------------------------------------------------------------------------

#[test]
fn keeps_the_exif_and_icc_markers() {
    let tmp = TempDir::new().unwrap();
    let original = with_segments(
        &photo_jpeg(),
        &[(0xE1, exif_payload()), (0xE2, icc_payload())],
    );
    let src = write(tmp.path(), "a.jpg", &original);
    let dest = tmp.path().join("optimized-a.jpg");

    optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    let kept = segments(&optimized);
    assert!(kept.contains(&(0xE1, exif_payload())), "EXIF lost");
    assert!(kept.contains(&(0xE2, icc_payload())), "ICC lost");
    assert_same_pixels(&original, &optimized);
}

#[test]
fn does_not_add_a_jfif_header_to_a_source_without_one() {
    let tmp = TempDir::new().unwrap();
    let original = without_jfif(&photo_jpeg());
    assert!(!has_jfif(&original), "fixture must have no JFIF APP0");
    let src = write(tmp.path(), "a.jpg", &original);
    let dest = tmp.path().join("optimized-a.jpg");

    optimize_file(&src, &dest).unwrap();

    let optimized = fs::read(&dest).unwrap();
    assert!(!has_jfif(&optimized));
    assert_same_pixels(&original, &optimized);
}

#[test]
fn keeps_the_jfif_header_of_a_source_that_has_one() {
    let tmp = TempDir::new().unwrap();
    let original = photo_jpeg();
    assert!(has_jfif(&original), "fixture must have a JFIF APP0");
    let src = write(tmp.path(), "a.jpg", &original);
    let dest = tmp.path().join("optimized-a.jpg");

    optimize_file(&src, &dest).unwrap();

    assert!(has_jfif(&fs::read(&dest).unwrap()));
}

#[test]
fn the_marker_helpers_see_an_inserted_segment_and_a_removed_jfif_header() {
    // Proof that the marker assertions above can fail.
    let plain = photo_jpeg();
    assert!(!has_segment(&plain, 0xE1, b"Exif\0\0"));
    assert!(has_segment(
        &with_segments(&plain, &[(0xE1, exif_payload())]),
        0xE1,
        b"Exif\0\0"
    ));
    assert!(has_jfif(&plain));
    assert!(!has_jfif(&without_jfif(&plain)));
}

// --- decompression bomb -------------------------------------------------------------

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
fn the_handmade_header_declares_the_dimensions_it_is_given() {
    // Proof that the fixture says what the test below relies on.
    let header = jpeg_declaring(65_500, 1234);
    let sof = segments(&header)
        .into_iter()
        .find(|(m, _)| *m == 0xC0)
        .unwrap()
        .1;
    assert_eq!(u16::from_be_bytes([sof[1], sof[2]]), 1234, "height");
    assert_eq!(u16::from_be_bytes([sof[3], sof[4]]), 65_500, "width");
}

#[test]
fn refuses_a_tiny_jpeg_that_declares_65500_by_65500_pixels_and_writes_nothing() {
    // 65500 is the largest side libjpeg accepts itself, so only the pixel
    // limit can refuse this one (65535 would be refused by libjpeg first).
    let tmp = TempDir::new().unwrap();
    let bomb = jpeg_declaring(65_500, 65_500);
    assert!(bomb.len() < 200, "the fixture must be tiny");
    let src = write(tmp.path(), "bomb.jpg", &bomb);
    let dest = tmp.path().join("optimized-bomb.jpg");
    let before = listing(tmp.path());

    let started = std::time::Instant::now();
    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(
        err.to_string()
            .contains("image too large: 65500×65500 exceeds 100 megapixels"),
        "{err}"
    );
    assert!(!dest.exists());
    assert_eq!(listing(tmp.path()), before);
}

#[test]
fn refuses_a_tiny_jpeg_that_declares_65535_by_65535_pixels_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "bomb.jpg", &jpeg_declaring(65_535, 65_535));
    let dest = tmp.path().join("optimized-bomb.jpg");
    let before = listing(tmp.path());

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists());
    assert_eq!(listing(tmp.path()), before);
}

// --- replacing, and nothing left after a failure ----------------------------------------

#[test]
fn replaces_an_existing_optimized_jpeg() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.jpg", &photo_jpeg());
    let dest = write(
        tmp.path(),
        "optimized-a.jpg",
        b"stale output from an earlier run",
    );

    optimize_file(&src, &dest).unwrap();

    let written = fs::read(&dest).unwrap();
    assert_ne!(written, b"stale output from an earlier run");
    assert_same_pixels(&fs::read(&src).unwrap(), &written);
    assert_eq!(
        listing(tmp.path()).len(),
        2,
        "only the source and the output remain"
    );
}

#[test]
fn keeps_an_existing_optimized_jpeg_and_leaves_nothing_when_the_new_run_fails() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.jpg", &truncated_jpeg());
    let dest = write(
        tmp.path(),
        "optimized-a.jpg",
        b"output of an earlier good run",
    );
    let before = listing(tmp.path());

    assert!(optimize_file(&src, &dest).is_err());

    assert_eq!(fs::read(&dest).unwrap(), b"output of an earlier good run");
    assert_eq!(listing(tmp.path()), before);
}

#[test]
fn does_not_create_the_destination_folder_when_the_jpeg_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.jpg", &truncated_jpeg());
    let dest = tmp.path().join("optimized").join("a.jpg");

    assert!(optimize_file(&src, &dest).is_err());

    assert!(!tmp.path().join("optimized").exists());
}

#[test]
fn writes_a_jpeg_into_the_optimized_folder() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.jpeg", &photo_jpeg());
    let dest = tmp.path().join("optimized").join("a.jpeg");

    optimize_file(&src, &dest).unwrap();

    assert_eq!(
        listing(&tmp.path().join("optimized")),
        [OsString::from("a.jpeg")]
    );
}

#[test]
fn refuses_to_optimize_a_jpeg_onto_itself_and_keeps_it_byte_identical() {
    let tmp = TempDir::new().unwrap();
    let original = photo_jpeg();
    let src = write(tmp.path(), "a.jpg", &original);

    let err = optimize_file(&src, &src).unwrap_err();

    assert!(err.to_string().contains("same file"), "{err}");
    assert_eq!(fs::read(&src).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn gives_the_jpeg_output_the_permissions_of_the_source() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    for mode in [0o600, 0o644] {
        let src = write(tmp.path(), "a.jpg", &photo_jpeg());
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();
        let dest = tmp.path().join("optimized-a.jpg");
        optimize_file(&src, &dest).unwrap();
        assert_eq!(
            fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
}
