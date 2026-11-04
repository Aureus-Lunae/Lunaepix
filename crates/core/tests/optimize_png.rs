//! `optimize_file` for PNG, through the public API only. Fixtures are made
//! here: nothing depends on `samples/`.

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use lunaepix_core::{OptimizeError, optimize_file};
use tempfile::TempDir;

// --- fixtures ---------------------------------------------------------------

/// A 64x64 RGBA image with a few flat colours, encoded at the lowest
/// compression, without filtering, and with a redundant text chunk. Plenty for
/// oxipng to remove.
fn wasteful_png() -> Vec<u8> {
    let (w, h) = (64u32, 64u32);
    let mut pixels = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let band = ((x / 16) + (y / 16)) % 3;
            pixels.extend_from_slice(&match band {
                0 => [200, 30, 30, 255],
                1 => [30, 200, 30, 128],
                _ => [30, 30, 200, 255],
            });
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_filter(png::FilterType::NoFilter);
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: 2835,
        yppu: 2835,
        unit: png::Unit::Meter,
    }));
    encoder
        .add_text_chunk("Comment".to_string(), "x".repeat(2000))
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

/// Real JPEG bytes (start-of-image marker plus JFIF header), not a PNG.
fn jpeg_bytes() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    bytes.extend_from_slice(&[0xFF, 0xD9]);
    bytes
}

/// A valid PNG signature and header, cut off in the middle of the image data.
fn truncated_png() -> Vec<u8> {
    let full = wasteful_png();
    full[..full.len() / 2].to_vec()
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

/// Decoded form used to compare images: dimensions plus RGBA16 pixels, so a
/// colour-type or bit-depth reduction by the optimiser still compares exactly.
fn decode(bytes: &[u8]) -> (u32, u32, Vec<u16>) {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap();
    (img.width(), img.height(), img.to_rgba16().into_raw())
}

fn assert_same_pixels(a: &[u8], b: &[u8]) {
    assert_eq!(decode(a), decode(b), "decoded images differ");
}

// --- smaller, identical pixels ------------------------------------------------

#[test]
fn optimizes_a_png_to_a_smaller_file_with_identical_pixels() {
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    let src = write(tmp.path(), "a.png", &original);
    let dest = tmp.path().join("optimized-a.png");

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
    // Proof that `assert_same_pixels` can fail: an image with one pixel changed.
    let original = wasteful_png();
    let decoded = image::load_from_memory(&original).unwrap().to_rgba8();
    let mut changed_img = decoded.clone();
    changed_img.get_pixel_mut(0, 0).0 = [1, 2, 3, 255];
    let mut changed = Vec::new();
    changed_img
        .write_to(
            &mut std::io::Cursor::new(&mut changed),
            image::ImageFormat::Png,
        )
        .unwrap();
    let result = std::panic::catch_unwind(|| assert_same_pixels(&original, &changed));
    assert!(result.is_err());
}

#[test]
fn optimizes_a_png_with_an_uppercase_extension() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "A.PNG", &wasteful_png());
    let dest = tmp.path().join("optimized-A.PNG");
    optimize_file(&src, &dest).unwrap();
    assert!(fs::metadata(&dest).unwrap().len() < fs::metadata(&src).unwrap().len());
}

#[test]
fn reports_equal_sizes_for_a_png_that_cannot_shrink_further() {
    let tmp = TempDir::new().unwrap();
    let first = write(tmp.path(), "a.png", &wasteful_png());
    let once = tmp.path().join("once.png");
    optimize_file(&first, &once).unwrap();
    let twice = tmp.path().join("twice.png");

    let outcome = optimize_file(&once, &twice).unwrap();

    assert_eq!(outcome.before, outcome.after);
    assert_eq!(fs::read(&once).unwrap(), fs::read(&twice).unwrap());
}

// --- the source is never touched ------------------------------------------------

#[test]
fn keeps_the_source_bytes_unchanged_after_a_successful_run() {
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    let src = write(tmp.path(), "a.png", &original);
    optimize_file(&src, &tmp.path().join("optimized-a.png")).unwrap();
    assert_eq!(fs::read(&src).unwrap(), original);
}

#[test]
fn keeps_the_source_unchanged_and_leaves_no_temp_file_when_the_source_is_corrupt() {
    let tmp = TempDir::new().unwrap();
    let corrupt = truncated_png();
    let src = write(tmp.path(), "a.png", &corrupt);
    let before = listing(tmp.path());

    assert!(optimize_file(&src, &tmp.path().join("optimized-a.png")).is_err());

    assert_eq!(fs::read(&src).unwrap(), corrupt);
    assert_eq!(listing(tmp.path()), before);
}

#[test]
fn keeps_the_source_unchanged_and_leaves_no_temp_file_when_the_final_rename_fails() {
    // The destination is an existing folder, so renaming a file onto it fails
    // after the optimised bytes were already written to a temporary file.
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    let src = write(tmp.path(), "a.png", &original);
    let dest = tmp.path().join("optimized-a.png");
    fs::create_dir(&dest).unwrap();
    write(&dest, "keep.txt", b"keep");
    let before = listing(tmp.path());

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Io { .. }), "{err:?}");
    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(listing(tmp.path()), before);
    assert_eq!(listing(&dest), [OsString::from("keep.txt")]);
}

// --- corrupt and non-PNG input ----------------------------------------------------

#[test]
fn rejects_a_corrupt_png_and_leaves_no_output_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &truncated_png());
    let dest = tmp.path().join("optimized-a.png");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(!dest.exists());
}

#[test]
fn rejects_a_png_with_damaged_image_data_and_leaves_no_output_file() {
    // Same length and signature as a good file, but bytes in the middle are
    // overwritten, so the chunk checksum no longer matches.
    let tmp = TempDir::new().unwrap();
    let mut bytes = wasteful_png();
    let mid = bytes.len() / 2;
    for byte in &mut bytes[mid..mid + 16] {
        *byte ^= 0xFF;
    }
    let src = write(tmp.path(), "a.png", &bytes);
    let dest = tmp.path().join("optimized-a.png");

    assert!(optimize_file(&src, &dest).is_err());
    assert!(!dest.exists());
}

#[test]
fn rejects_a_png_extension_that_holds_jpeg_bytes() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "photo.png", &jpeg_bytes());
    let dest = tmp.path().join("optimized-photo.png");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("not a PNG"), "{err}");
    assert!(!dest.exists());
    assert_eq!(fs::read(&src).unwrap(), jpeg_bytes());
}

#[test]
fn rejects_an_empty_png_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "empty.png", b"");
    let dest = tmp.path().join("optimized-empty.png");
    assert!(matches!(
        optimize_file(&src, &dest),
        Err(OptimizeError::Invalid { .. })
    ));
    assert!(!dest.exists());
}

#[test]
fn rejects_a_missing_source_with_an_io_error_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let dest_dir = tmp.path().join("optimized");
    let err = optimize_file(&tmp.path().join("gone.png"), &dest_dir.join("gone.png")).unwrap_err();
    assert!(matches!(err, OptimizeError::Io { .. }), "{err:?}");
    assert!(!dest_dir.exists());
}

// --- other formats ---------------------------------------------------------------

#[test]
fn rejects_a_gif_as_unsupported_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.gif", b"GIF89a");
    let dest = tmp.path().join("optimized-a.gif");
    let err = optimize_file(&src, &dest).unwrap_err();
    assert!(
        matches!(err, OptimizeError::UnsupportedType { .. }),
        "{err:?}"
    );
    assert!(!dest.exists());
}

#[test]
fn rejects_jpg_and_svg_with_a_clear_error_until_they_are_built() {
    let tmp = TempDir::new().unwrap();
    for name in ["a.jpg", "a.JPEG", "a.svg"] {
        let src = write(tmp.path(), name, b"data");
        let dest = tmp.path().join(format!("optimized-{name}"));
        let err = optimize_file(&src, &dest).unwrap_err();
        assert!(
            matches!(err, OptimizeError::NotYetSupported { .. }),
            "{name}: {err:?}"
        );
        assert!(err.to_string().contains("cannot be optimised yet"), "{err}");
        assert!(!dest.exists());
    }
}

// --- replacing and the destination folder ---------------------------------------------

#[test]
fn replaces_an_existing_optimized_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    let dest = write(
        tmp.path(),
        "optimized-a.png",
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
fn keeps_an_existing_optimized_file_when_the_new_run_fails() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &truncated_png());
    let dest = write(
        tmp.path(),
        "optimized-a.png",
        b"output of an earlier good run",
    );

    assert!(optimize_file(&src, &dest).is_err());

    assert_eq!(fs::read(&dest).unwrap(), b"output of an earlier good run");
}

#[test]
fn creates_the_missing_destination_folder_when_it_writes() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    let dest = tmp.path().join("optimized").join("a.png");

    optimize_file(&src, &dest).unwrap();

    assert!(dest.is_file());
    assert_eq!(
        listing(&tmp.path().join("optimized")),
        [OsString::from("a.png")]
    );
}

#[test]
fn does_not_create_the_destination_folder_when_the_source_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &truncated_png());
    let dest = tmp.path().join("optimized").join("a.png");

    assert!(optimize_file(&src, &dest).is_err());

    assert!(!tmp.path().join("optimized").exists());
}

#[test]
fn fails_cleanly_when_the_destination_folder_cannot_be_created() {
    // `blocker` is a file, so `blocker/out.png` can never be written.
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    write(tmp.path(), "blocker", b"a file");
    let before = listing(tmp.path());

    let err = optimize_file(&src, &tmp.path().join("blocker").join("out.png")).unwrap_err();

    assert!(matches!(err, OptimizeError::Io { .. }), "{err:?}");
    assert_eq!(listing(tmp.path()), before);
}

#[cfg(unix)]
#[test]
fn replaces_a_symlink_at_the_destination_instead_of_writing_through_it() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    let other = write(
        tmp.path(),
        "other.png",
        b"another file that must not change",
    );
    let dest = tmp.path().join("optimized-a.png");
    symlink(&other, &dest).unwrap();

    optimize_file(&src, &dest).unwrap();

    assert_eq!(
        fs::read(&other).unwrap(),
        b"another file that must not change"
    );
    let meta = fs::symlink_metadata(&dest).unwrap();
    assert!(
        meta.file_type().is_file(),
        "destination must now be a regular file"
    );
    assert_same_pixels(&fs::read(&src).unwrap(), &fs::read(&dest).unwrap());
}

#[cfg(unix)]
fn optimize_with_source_mode(mode: u32) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();
    let dest = tmp.path().join("optimized-a.png");
    optimize_file(&src, &dest).unwrap();
    fs::metadata(&dest).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn gives_the_output_the_permissions_of_a_private_source() {
    assert_eq!(optimize_with_source_mode(0o600), 0o600);
}

#[cfg(unix)]
#[test]
fn gives_the_output_the_permissions_of_a_world_readable_source() {
    // The counterpart of the test above: the output follows the source, it is
    // not fixed to the private mode either.
    assert_eq!(optimize_with_source_mode(0o644), 0o644);
}

// --- source and destination are the same file ---------------------------------------

#[test]
fn refuses_to_optimize_a_file_onto_itself_and_keeps_it_byte_identical() {
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    let src = write(tmp.path(), "a.png", &original);

    let err = optimize_file(&src, &src).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("same file"), "{err}");
    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(listing(tmp.path()), [OsString::from("a.png")]);
}

#[test]
fn refuses_the_same_file_spelled_with_a_different_path() {
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    let src = write(tmp.path(), "a.png", &original);
    let spelled = tmp.path().join(".").join("a.png");

    assert!(optimize_file(&src, &spelled).is_err());

    assert_eq!(fs::read(&src).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn refuses_a_destination_that_is_a_symlink_to_the_source() {
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    let src = write(tmp.path(), "a.png", &original);
    let dest = tmp.path().join("optimized-a.png");
    std::os::unix::fs::symlink(&src, &dest).unwrap();

    assert!(optimize_file(&src, &dest).is_err());

    assert_eq!(fs::read(&src).unwrap(), original);
    assert!(
        fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn optimizes_into_the_same_file_name_in_another_folder() {
    // Same name, different folder: not the same file, so it must work.
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    let dest = tmp.path().join("optimized").join("a.png");
    optimize_file(&src, &dest).unwrap();
    assert!(dest.is_file());
}

#[cfg(unix)]
#[test]
fn refuses_a_destination_folder_that_is_a_symlink_and_writes_nothing_behind_it() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &wasteful_png());
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, tmp.path().join("optimized")).unwrap();

    let result = optimize_file(&src, &tmp.path().join("optimized").join("a.png"));

    assert!(
        matches!(result, Err(OptimizeError::Io { .. })),
        "{result:?}"
    );
    assert!(listing(&elsewhere).is_empty());
}

// --- chunk stripping (strip = Safe) -----------------------------------------------------

/// The chunk types of a PNG file, in order.
fn chunk_types(png: &[u8]) -> Vec<[u8; 4]> {
    let mut types = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        types.push(png[at + 4..at + 8].try_into().unwrap());
        at += 12 + len;
    }
    types
}

#[test]
fn strips_text_chunks_but_keeps_display_chunks() {
    let tmp = TempDir::new().unwrap();
    let original = wasteful_png();
    assert!(
        chunk_types(&original).contains(b"tEXt"),
        "fixture must carry a tEXt chunk"
    );
    assert!(
        chunk_types(&original).contains(b"pHYs"),
        "fixture must carry a pHYs chunk"
    );
    let src = write(tmp.path(), "a.png", &original);
    let dest = tmp.path().join("optimized-a.png");

    optimize_file(&src, &dest).unwrap();

    let types = chunk_types(&fs::read(&dest).unwrap());
    // Positive: metadata that does not affect display is gone.
    assert!(!types.contains(b"tEXt"), "{types:?}");
    // Negative: a chunk that does affect display (pixel density) is kept.
    assert!(types.contains(b"pHYs"), "{types:?}");
}

// --- error text ---------------------------------------------------------------------

#[test]
fn shows_the_io_cause_once_in_the_message_and_not_again_as_a_source() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("gone.png");
    // The platform's own wording of the cause, whatever the OS.
    let cause = fs::read(&missing).unwrap_err().to_string();
    let err = optimize_file(&missing, &tmp.path().join("out.png")).unwrap_err();
    let message = err.to_string();
    assert_eq!(message.matches(&cause).count(), 1, "{message}");
    assert!(message.contains("gone.png"), "{message}");
    assert!(err.source().is_none());
}
