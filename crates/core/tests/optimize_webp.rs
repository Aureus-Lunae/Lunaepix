//! `optimize_file` for WebP, through the public API only. Fixtures are made in
//! `common/webp_fixtures.rs`, with encoders other than the code under test;
//! nothing depends on `samples/`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use lunaepix_core::{FileResult, OptimizeError, Worker, optimize_file, plan_folder, run};
use tempfile::TempDir;

mod common;
use common::webp_fixtures as fx;

// --- fixtures ---------------------------------------------------------------

const SIZE: u32 = 24;

/// A lossless file with a 10 KB EXIF chunk and an ICC profile: it has
/// something to lose, and something that must stay.
fn lossless_with_exif() -> Vec<u8> {
    let base = fx::lossless(&fx::photo_rgba(SIZE, SIZE, true), SIZE, SIZE);
    fx::with_chunks(
        &base,
        &[(*b"ICCP", &[7u8; 128]), (*b"EXIF", &[0x45; 10_240])],
    )
}

/// A lossless file with no metadata: stripping gains nothing.
fn lossless_clean() -> Vec<u8> {
    fx::lossless(&fx::photo_rgba(SIZE, SIZE, true), SIZE, SIZE)
}

/// Three different 24x24 lossless frames, as an animated file.
fn animated_file() -> Vec<u8> {
    let frames: Vec<Vec<u8>> = (0u8..3)
        .map(|k| {
            let rgba: Vec<u8> = fx::photo_rgba(SIZE, SIZE, true)
                .iter()
                .map(|b| b.wrapping_add(k * 40))
                .collect();
            fx::lossless(&rgba, SIZE, SIZE)
        })
        .collect();
    fx::animated(&frames, SIZE, SIZE)
}

fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 8, 8);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[90; 8 * 8 * 3]).unwrap();
    writer.finish().unwrap();
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

/// Optimise `bytes` as `a.webp` and give the output.
fn optimized(bytes: &[u8]) -> Vec<u8> {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.webp", bytes);
    let dest = tmp.path().join("optimized-a.webp");
    optimize_file(&src, &dest).unwrap();
    fs::read(&dest).unwrap()
}

// --- the fixtures say what the tests rely on --------------------------------

#[test]
fn the_fixtures_make_the_files_the_tests_name() {
    let with_exif = lossless_with_exif();
    assert_eq!(
        fx::chunk_names(&with_exif),
        ["VP8X", "ICCP", "VP8L", "EXIF"]
    );
    let flags = with_exif[fx::VP8X_FLAGS_AT];
    assert_eq!(
        flags & (fx::FLAG_ICC | fx::FLAG_EXIF),
        fx::FLAG_ICC | fx::FLAG_EXIF
    );
    assert_eq!(
        fx::chunk_payload(&with_exif, b"EXIF").unwrap().len(),
        10_240
    );

    assert_eq!(fx::chunk_names(&lossless_clean()), ["VP8L"]);

    let lossy = fx::lossy(&fx::photo_rgba(SIZE, SIZE, true), SIZE, SIZE, 75.0);
    assert_eq!(fx::chunk_names(&lossy), ["VP8X", "ALPH", "VP8 "]);
    let lossy_opaque = fx::lossy(&fx::photo_rgba(SIZE, SIZE, false), SIZE, SIZE, 75.0);
    assert_eq!(fx::chunk_names(&lossy_opaque), ["VP8 "]);

    let animated = animated_file();
    assert_eq!(
        fx::chunk_names(&animated),
        ["VP8X", "ANIM", "ANMF", "ANMF", "ANMF"]
    );
    assert_eq!(fx::decode_frames(&animated).len(), 3);

    // A missing chunk is None, not an empty payload.
    assert_eq!(fx::chunk_payload(&lossless_clean(), b"EXIF"), None);
}

// --- strips metadata, keeps the picture ---------------------------------------

#[test]
fn writes_a_smaller_lossless_file_without_its_exif_and_with_the_same_pixels() {
    let original = lossless_with_exif();
    let out = optimized(&original);

    assert!(out.len() < original.len() - 10_000, "{} bytes", out.len());
    assert_eq!(fx::chunk_names(&out), ["VP8X", "ICCP", "VP8L"]);
    assert_eq!(fx::chunk_payload(&out, b"ICCP"), Some(vec![7u8; 128]));
    assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&original));
}

#[test]
fn writes_the_original_bytes_when_stripping_gains_nothing() {
    let original = lossless_clean();
    assert_eq!(optimized(&original), original);
}

#[test]
fn keeps_the_source_bytes_unchanged_after_a_successful_run() {
    let tmp = TempDir::new().unwrap();
    let original = lossless_with_exif();
    let src = write(tmp.path(), "a.webp", &original);
    let dest = tmp.path().join("optimized-a.webp");

    let outcome = optimize_file(&src, &dest).unwrap();

    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(outcome.before, original.len() as u64);
    assert_eq!(outcome.after, fs::metadata(&dest).unwrap().len());
    assert!(outcome.after < outcome.before);
}

#[test]
fn keeps_every_frame_of_an_animation_and_drops_its_metadata() {
    let animated = animated_file();
    let original = fx::with_chunks(
        &animated,
        &[(*b"EXIF", &[1u8; 4_000]), (*b"XMP ", &[2u8; 4_000])],
    );
    let out = optimized(&original);

    assert!(out.len() < original.len() - 7_000);
    assert_eq!(out, animated);
    let frames = fx::decode_frames(&out);
    assert_eq!(frames, fx::decode_frames(&original));
    assert_ne!(
        frames[0].0, frames[1].0,
        "the frames differ from each other"
    );
}

#[test]
fn copies_an_animation_without_metadata_as_it_is() {
    let animated = animated_file();
    assert_eq!(optimized(&animated), animated);
}

#[test]
fn keeps_the_lossy_image_and_its_alpha_byte_for_byte() {
    let lossy = fx::lossy(&fx::photo_rgba(SIZE, SIZE, true), SIZE, SIZE, 75.0);
    let original = fx::with_chunks(&lossy, &[(*b"EXIF", &[3u8; 5_000])]);
    let out = optimized(&original);

    assert!(out.len() < original.len() - 4_000);
    assert_eq!(out, lossy);
    for fourcc in [b"VP8 ", b"ALPH"] {
        assert_eq!(
            fx::chunk_payload(&out, fourcc),
            fx::chunk_payload(&original, fourcc)
        );
    }
    // The decoder reads the same picture from both.
    assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&original));
}

#[test]
fn copies_a_lossy_file_without_metadata_as_it_is() {
    let lossy = fx::lossy(&fx::photo_rgba(SIZE, SIZE, false), SIZE, SIZE, 75.0);
    assert_eq!(optimized(&lossy), lossy);
}

// --- a folder run ---------------------------------------------------------------

fn worker() -> Worker {
    Worker::new(env!("CARGO_BIN_EXE_lunaepix-worker"))
}

#[test]
fn a_folder_run_writes_a_webp_into_optimized_under_its_own_name() {
    let tmp = TempDir::new().unwrap();
    let original = lossless_with_exif();
    let src = write(tmp.path(), "a.webp", &original);
    let jobs = plan_folder(tmp.path());

    let results = run(&jobs, &worker(), |_, _, _| {});

    assert!(
        matches!(results[..], [FileResult::Optimized(_)]),
        "{results:?}"
    );
    let out = fs::read(tmp.path().join("optimized").join("a.webp")).unwrap();
    assert!(out.len() < original.len());
    assert_eq!(fx::decode_rgba(&out), fx::decode_rgba(&original));
    assert_eq!(fs::read(&src).unwrap(), original, "the source is untouched");
}

#[test]
fn a_folder_run_fails_png_bytes_named_webp_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.webp", &png_bytes());
    let jobs = plan_folder(tmp.path());

    let results = run(&jobs, &worker(), |_, _, _| {});

    assert!(
        matches!(results[..], [FileResult::Failed(_)]),
        "{results:?}"
    );
    assert_eq!(listing(tmp.path()), [OsString::from("a.webp")]);
    assert_eq!(fs::read(&src).unwrap(), png_bytes());
}

#[test]
fn rejects_png_bytes_named_webp_and_leaves_no_output_file() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.webp", &png_bytes());
    let dest = tmp.path().join("optimized-a.webp");

    let err = optimize_file(&src, &dest).unwrap_err();

    assert!(matches!(err, OptimizeError::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("not a WebP file"), "{err}");
    assert!(!dest.exists());
}
