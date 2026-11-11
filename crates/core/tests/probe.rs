//! `probe`, `run_probe` and the probe worker mode (`lunaepix-worker
//! --lunaepix-probe <src>`), through the public API and the real worker
//! binary. Fixtures are made here with encoders other than the code under
//! test (`image`, `png`, and libwebp through the shared WebP fixtures).

mod common;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::jpeg_fixtures::{grey_header, with_app1};
use common::webp_fixtures as fx;
use lunaepix_core::{
    Compression, MAX_FILE_BYTES, MAX_SVG_DEPTH, MAX_SVG_ENTITY_BYTES, Options, PROBE_FLAG, Probe,
    ProbeAnswer, Skip, WriteMode, optimize_file, probe, run_probe,
};
use tempfile::TempDir;

const WORKER: &str = env!("CARGO_BIN_EXE_lunaepix-worker");

// --- fixtures ---------------------------------------------------------------

/// A real RGB PNG of `w`x`h` from the `png` encoder, with `extra` chunks
/// (type, data) written right after the IHDR.
fn png_with_chunks(w: u32, h: u32, extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let plain = png_of(w, h);
    let mut out = plain[..8 + 25].to_vec(); // the signature and the IHDR chunk
    for (kind, data) in extra {
        out.extend(chunk(kind, data));
    }
    out.extend_from_slice(&plain[8 + 25..]);
    out
}

fn png_of(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&vec![90; (w * h * 3) as usize])
        .unwrap();
    writer.finish().unwrap();
    out
}

/// One PNG chunk with a valid CRC.
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
    out
}

fn ihdr(w: u32, h: u32) -> Vec<u8> {
    let mut data = w.to_be_bytes().to_vec();
    data.extend_from_slice(&h.to_be_bytes());
    data.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &data)
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// A PNG that is only a header of `w`x`h`: nothing in it can be decoded.
fn png_header(w: u32, h: u32) -> Vec<u8> {
    let mut out = PNG_SIGNATURE.to_vec();
    out.extend(ihdr(w, h));
    out
}

/// A JPEG of `w`x`h` from the `image` crate.
fn jpg_of(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode(
            &vec![120; (w * h * 3) as usize],
            w,
            h,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn jpg_with_orientation(w: u32, h: u32, value: u16, big_endian: bool) -> Vec<u8> {
    with_app1(
        &jpg_of(w, h),
        &fx::exif_with_orientation(value, big_endian, true),
    )
}

fn webp_of(w: u32, h: u32) -> Vec<u8> {
    fx::lossless(&fx::photo_rgba(w, h, false), w, h)
}

fn webp_with_orientation(w: u32, h: u32, value: u16, big_endian: bool) -> Vec<u8> {
    fx::with_chunks(
        &webp_of(w, h),
        &[(
            *b"EXIF",
            &fx::exif_with_orientation(value, big_endian, false),
        )],
    )
}

/// A WebP that is only a `VP8L` header of `w`x`h`.
fn webp_header(w: u32, h: u32) -> Vec<u8> {
    let bits = (w - 1) | ((h - 1) << 14);
    let mut payload = vec![0x2F];
    payload.extend_from_slice(&bits.to_le_bytes());
    payload.extend_from_slice(&[0; 5]);
    fx::from_chunks(&[(*b"VP8L", payload)])
}

/// A 16x16-frame APNG of `w`x`h`: two frames.
fn apng(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    for _ in 0..2 {
        writer
            .write_image_data(&vec![200; (w * h * 4) as usize])
            .unwrap();
    }
    writer.finish().unwrap();
    out
}

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn size(w: u32, h: u32) -> Result<Probe, String> {
    Ok(Probe {
        width: Some(w),
        height: Some(h),
    })
}

/// The probe of `bytes` saved as `name`.
fn probe_of(name: &str, bytes: &[u8]) -> Result<Probe, String> {
    let tmp = TempDir::new().unwrap();
    probe(&write(tmp.path(), name, bytes))
}

/// The reason the optimiser (lossless, as a Batch run does it) gives for
/// `bytes` saved as `name`, or `None` when it optimises the file.
fn optimiser_reason(name: &str, bytes: &[u8]) -> Option<String> {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), name, bytes);
    let options = Options {
        compression: Compression::Lossless,
        write: WriteMode::NoReplace,
    };
    optimize_file(&src, &tmp.path().join("out.bin"), &options)
        .err()
        .map(|error| error.reason())
}

// --- the size -----------------------------------------------------------------

#[test]
fn a_jpg_gives_its_stored_size() {
    assert_eq!(probe_of("a.jpg", &jpg_of(300, 200)), size(300, 200));
    assert_eq!(probe_of("a.jpeg", &jpg_of(7, 9)), size(7, 9));
}

#[test]
fn every_still_format_gives_its_size() {
    assert_eq!(probe_of("a.png", &png_of(300, 200)), size(300, 200));
    assert_eq!(probe_of("a.webp", &webp_of(300, 200)), size(300, 200));
    // The extension's letter case does not matter.
    assert_eq!(probe_of("A.PNG", &png_of(30, 20)), size(30, 20));
}

#[test]
fn an_exif_rotated_jpg_gives_its_upright_size() {
    // Stored 300x200: orientations 5 to 8 turn it a quarter, so it shows 200x300.
    for value in 5..=8 {
        for big_endian in [false, true] {
            let bytes = jpg_with_orientation(300, 200, value, big_endian);
            assert_eq!(
                probe_of("a.jpg", &bytes),
                size(200, 300),
                "orientation {value}, big endian {big_endian}"
            );
        }
    }
}

#[test]
fn an_exif_rotated_webp_gives_its_upright_size() {
    for value in 5..=8 {
        for big_endian in [false, true] {
            let bytes = webp_with_orientation(300, 200, value, big_endian);
            assert_eq!(
                probe_of("a.webp", &bytes),
                size(200, 300),
                "orientation {value}, big endian {big_endian}"
            );
        }
    }
}

#[test]
fn an_exif_orientation_that_does_not_turn_the_picture_keeps_the_stored_size() {
    // 1 to 4 only flip or turn it half way; 0 and 9 are not orientations.
    for value in [0, 1, 2, 3, 4, 9] {
        assert_eq!(
            probe_of("a.jpg", &jpg_with_orientation(300, 200, value, false)),
            size(300, 200),
            "jpg, orientation {value}"
        );
        assert_eq!(
            probe_of("a.webp", &webp_with_orientation(300, 200, value, false)),
            size(300, 200),
            "webp, orientation {value}"
        );
    }
    // No EXIF at all, and an EXIF block that cannot be read.
    assert_eq!(probe_of("a.jpg", &jpg_of(300, 200)), size(300, 200));
    let damaged = with_app1(&jpg_of(300, 200), b"Exif\0\0not a tiff stream");
    assert_eq!(probe_of("a.jpg", &damaged), size(300, 200));
}

#[test]
fn a_png_is_never_turned() {
    // A PNG may carry an eXIf chunk; a browser does not turn it by that here,
    // and neither does the probe.
    let exif = fx::exif_with_orientation(6, true, false);
    let bytes = png_with_chunks(300, 200, &[(b"eXIf", exif)]);
    assert!(bytes.windows(4).any(|window| window == b"eXIf"));
    assert_eq!(probe_of("a.png", &bytes), size(300, 200));
}

#[test]
fn an_animated_png_and_webp_give_their_canvas_size() {
    let apng = apng(20, 10);
    assert!(apng.windows(4).any(|window| window == b"acTL"));
    assert_eq!(probe_of("a.png", &apng), size(20, 10));
    let frame = fx::lossless(&fx::photo_rgba(20, 10, true), 20, 10);
    let animation = fx::animated(&[frame.clone(), frame], 20, 10);
    assert_eq!(probe_of("a.webp", &animation), size(20, 10));
}

#[test]
fn an_animated_webp_without_a_canvas_is_refused() {
    let frame = fx::lossless(&fx::photo_rgba(20, 10, true), 20, 10);
    let animation = fx::animated(&[frame], 20, 10);
    let without: Vec<_> = fx::chunks_of(&animation)
        .into_iter()
        .filter(|(name, _)| name != b"VP8X")
        .collect();
    let why = probe_of("a.webp", &fx::from_chunks(&without)).unwrap_err();
    assert!(why.contains("no VP8X"), "{why}");
}

#[test]
fn an_svg_gives_no_size() {
    // Even with a width and a height attribute: the size is the browser's.
    for bytes in [
        SVG,
        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"/>"#,
    ] {
        assert_eq!(
            probe_of("a.svg", bytes),
            Ok(Probe {
                width: None,
                height: None
            })
        );
    }
}

// --- the limits and the refusals, with the optimiser's reasons -------------------

#[test]
fn a_file_over_max_pixels_is_refused_with_the_optimisers_reason() {
    let over = "image too large: 10000×10001 exceeds 100 megapixels";
    let cases = [
        ("a.png", png_header(10_000, 10_001)),
        ("a.jpg", grey_header(10_000, 10_001)),
        ("a.webp", webp_header(10_000, 10_001)),
    ];
    for (name, bytes) in cases {
        assert_eq!(probe_of(name, &bytes), Err(over.to_owned()), "{name}");
        assert_eq!(
            optimiser_reason(name, &bytes).as_deref(),
            Some(over),
            "{name}"
        );
    }
}

#[test]
fn a_file_at_max_pixels_is_accepted() {
    let cases = [
        ("a.png", png_header(10_000, 10_000)),
        ("a.jpg", grey_header(10_000, 10_000)),
        ("a.webp", webp_header(10_000, 10_000)),
    ];
    for (name, bytes) in cases {
        assert_eq!(probe_of(name, &bytes), size(10_000, 10_000), "{name}");
    }
}

#[test]
fn a_file_over_max_file_bytes_is_refused_with_the_planners_reason() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.png");
    // Sparse: nothing is written.
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    assert_eq!(probe(&path), Err(Skip::TooLarge.to_string()));
    assert_eq!(Skip::TooLarge.to_string(), "File is too large");
}

#[test]
fn a_file_of_exactly_max_file_bytes_is_refused_for_its_content_not_its_size() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.png");
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES)
        .unwrap();
    let why = probe(&path).unwrap_err();
    assert_ne!(why, Skip::TooLarge.to_string());
    assert!(why.contains("not a PNG"), "{why}");
}

#[test]
fn a_damaged_header_is_refused() {
    // Each damaged file is refused; its undamaged twin is answered.
    let mut jpg_garbage = vec![0xFF, 0xD8];
    jpg_garbage.extend_from_slice(&[0; 20]);
    let mut webp_cut = webp_of(16, 16);
    webp_cut.truncate(webp_cut.len() - 3);
    let mut png_cut = png_header(8, 8);
    png_cut.truncate(8 + 8 + 5);
    let cases = [
        ("a.png", PNG_SIGNATURE.to_vec(), png_of(8, 8)),
        ("a.png", png_cut, png_of(8, 8)),
        ("a.jpg", jpg_garbage, jpg_of(8, 8)),
        ("a.webp", webp_cut, webp_of(16, 16)),
    ];
    for (name, damaged, whole) in cases {
        assert!(probe_of(name, &damaged).is_err(), "{name} damaged");
        assert!(probe_of(name, &whole).is_ok(), "{name} whole");
    }
}

#[test]
fn every_header_check_the_optimiser_makes_gives_the_same_message() {
    let decls = format!("<!ENTITY a \"{}\">", "x".repeat(1000));
    let prolog = format!("<!DOCTYPE svg [{decls}]>");
    let refs = MAX_SVG_ENTITY_BYTES as usize / prolog.len() + 1;
    let bomb = format!(
        "{prolog}<svg xmlns=\"http://www.w3.org/2000/svg\"><desc>{}</desc></svg>",
        "&a;".repeat(refs)
    );
    // The root is level 1, so `levels - 1` groups make `levels` levels.
    let nested = |levels: usize| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}{}</svg>",
            "<g>".repeat(levels - 1),
            "</g>".repeat(levels - 1)
        )
    };
    let too_deep = nested(MAX_SVG_DEPTH + 1);
    let at_the_limit = nested(MAX_SVG_DEPTH);
    let mut two_ihdr = PNG_SIGNATURE.to_vec();
    two_ihdr.extend(ihdr(8, 8));
    two_ihdr.extend(ihdr(8, 8));
    two_ihdr.extend(chunk(b"IEND", &[]));
    let mut not_first = PNG_SIGNATURE.to_vec();
    not_first.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
    not_first.extend(ihdr(8, 8));
    not_first.extend(chunk(b"IEND", &[]));
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        ("not a png", "a.png", b"not a png at all".to_vec()),
        ("two ihdr", "a.png", two_ihdr),
        ("first chunk not ihdr", "a.png", not_first),
        ("not a jpeg", "a.jpg", b"not a jpeg at all".to_vec()),
        ("not a webp", "a.webp", b"RIFF".to_vec()),
        (
            "svg not utf-8",
            "a.svg",
            vec![0x89, b'P', b'N', b'G', 0xFF, 0xFE],
        ),
        ("svg not well-formed", "a.svg", b"<svg><g></svg>".to_vec()),
        (
            "svg root is not svg",
            "a.svg",
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><svg/></html>"#.to_vec(),
        ),
        ("svg too deep", "a.svg", too_deep.into_bytes()),
        ("svg entity bomb", "a.svg", bomb.into_bytes()),
    ];
    for (what, name, bytes) in cases {
        let theirs = optimiser_reason(name, &bytes)
            .unwrap_or_else(|| panic!("{what}: the optimiser should refuse it"));
        assert_eq!(probe_of(name, &bytes), Err(theirs), "{what}");
    }
    // The controls: files the optimiser takes are answered, and an SVG at
    // exactly the depth limit passes both.
    assert_eq!(optimiser_reason("a.svg", at_the_limit.as_bytes()), None);
    assert!(probe_of("a.svg", at_the_limit.as_bytes()).is_ok());
    for (name, bytes) in [
        ("a.png", png_of(8, 8)),
        ("a.jpg", jpg_of(8, 8)),
        ("a.webp", webp_of(8, 8)),
    ] {
        assert_eq!(optimiser_reason(name, &bytes), None, "{name}");
        assert!(probe_of(name, &bytes).is_ok(), "{name}");
    }
}

#[test]
fn a_non_image_name_is_refused_with_the_unsupported_text() {
    let unsupported = Skip::UnsupportedType.to_string();
    assert_eq!(unsupported, "Not a PNG, JPG, SVG or WebP file");
    let png = png_of(8, 8);
    for name in ["a.txt", "noextension", ".png", "a.png.bak", "a.gif"] {
        assert_eq!(probe_of(name, &png), Err(unsupported.clone()), "{name}");
    }
    // The same bytes under a supported name are answered.
    assert_eq!(probe_of("a.png", &png), size(8, 8));
}

#[test]
fn a_folder_is_refused() {
    let tmp = TempDir::new().unwrap();
    let not_a_file = Skip::NotAFile.to_string();
    let folder = tmp.path().join("pictures.png");
    fs::create_dir(&folder).unwrap();
    assert_eq!(probe(&folder), Err(not_a_file));
    // A folder with no image extension is refused for its name first, as the
    // planner does.
    let plain = tmp.path().join("pictures");
    fs::create_dir(&plain).unwrap();
    assert_eq!(probe(&plain), Err(Skip::UnsupportedType.to_string()));
    // The control: a file there is answered.
    assert!(probe(&write(tmp.path(), "a.png", &png_of(8, 8))).is_ok());
}

#[test]
fn a_missing_file_is_refused_as_not_readable() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        probe(&tmp.path().join("gone.png")),
        Err(Skip::NotAFile.to_string())
    );
}

/// A named pipe that nothing writes to: opening it for reading blocks.
#[cfg(unix)]
fn fifo(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let made = Command::new("mkfifo").arg(&path).status().unwrap();
    assert!(made.success(), "mkfifo failed");
    path
}

#[cfg(unix)]
#[test]
fn a_fifo_named_png_is_refused_and_never_opened() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let trap = fifo(tmp.path(), "a.png");
    let (tx, rx) = mpsc::channel();
    let path = trap.clone();
    thread::spawn(move || {
        let _ = tx.send(probe(&path));
    });
    let result = rx.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        // Release the reader stuck in the pipe so the test run ends.
        thread::spawn(move || drop(fs::OpenOptions::new().write(true).open(trap)));
    }
    let result = result.expect("probe blocked opening the FIFO");
    assert_eq!(result, Err(Skip::NotAFile.to_string()));
}

#[cfg(unix)]
#[test]
fn a_device_named_png_is_refused() {
    let tmp = TempDir::new().unwrap();
    let link = tmp.path().join("a.png");
    std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
    assert_eq!(probe(&link), Err(Skip::NotAFile.to_string()));
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_opened_is_refused_with_the_os_reason() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let path = write(tmp.path(), "a.png", &png_of(8, 8));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let Err(os_error) = fs::File::open(&path) else {
        // Running as root: the file opens whatever its mode says.
        return;
    };
    assert_eq!(probe(&path), Err(os_error.to_string()));
    // The pair: with its permissions back, the same file is answered.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(probe(&path), size(8, 8));
}

// --- names ----------------------------------------------------------------------

#[test]
fn a_file_name_with_spaces_and_e_acute_is_probed() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "Mobilis Logo – zwart é.png", &png_of(32, 16));
    assert_eq!(probe(&src), size(32, 16));
    assert_eq!(
        worker_stdout(&probe_worker(&src)),
        "lunaepix-probe 1 size 32 16\n"
    );
    assert_eq!(
        run_probe(WORKER.as_ref(), &src),
        ProbeAnswer::Size(probed(32, 16))
    );
    // A name like it that is not there is refused, not mistaken for a file.
    let missing = tmp.path().join("Mobilis Logo – zwart é.jpg");
    assert_eq!(probe(&missing), Err(Skip::NotAFile.to_string()));
}

#[cfg(unix)]
#[test]
fn a_non_utf8_file_name_is_probed() {
    use std::os::unix::ffi::OsStrExt;

    let tmp = TempDir::new().unwrap();
    // Byte 0xE9 is Latin-1 `é`, which is not valid UTF-8 on its own.
    let src = tmp.path().join(OsStr::from_bytes(b"caf\xE9.png"));
    fs::write(&src, png_of(32, 16)).unwrap();
    assert!(std::str::from_utf8(src.file_name().unwrap().as_bytes()).is_err());
    assert_eq!(probe(&src), size(32, 16));
    let output = probe_worker(&src);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(worker_stdout(&output), "lunaepix-probe 1 size 32 16\n");
    assert_eq!(
        run_probe(WORKER.as_ref(), &src),
        ProbeAnswer::Size(probed(32, 16))
    );
    // The control: the same name, missing, is a refusal.
    let missing = tmp.path().join(OsStr::from_bytes(b"caf\xE9.jpg"));
    assert_eq!(probe(&missing), Err(Skip::NotAFile.to_string()));
}

// --- the worker mode ----------------------------------------------------------------

fn probed(w: u32, h: u32) -> Probe {
    Probe {
        width: Some(w),
        height: Some(h),
    }
}

fn serve(args: &[&OsStr], dir: Option<&Path>) -> Output {
    let mut command = Command::new(WORKER);
    command.args(args).stdin(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    command.output().unwrap()
}

fn probe_worker(src: &Path) -> Output {
    serve(&[PROBE_FLAG.as_ref(), src.as_os_str()], None)
}

fn worker_stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn the_probe_flag_is_the_documented_one() {
    assert_eq!(PROBE_FLAG, "--lunaepix-probe");
}

#[test]
fn the_worker_answers_a_size_line_and_exits_0() {
    let tmp = TempDir::new().unwrap();
    let jpg = write(
        tmp.path(),
        "a.jpg",
        &jpg_with_orientation(300, 200, 6, false),
    );
    let output = probe_worker(&jpg);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(worker_stdout(&output), "lunaepix-probe 1 size 200 300\n");

    // An SVG has no size: two dashes.
    let svg = write(tmp.path(), "a.svg", SVG);
    let output = probe_worker(&svg);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(worker_stdout(&output), "lunaepix-probe 1 size - -\n");
}

#[test]
fn the_worker_answers_refused_with_an_escaped_message_and_exits_3() {
    let tmp = TempDir::new().unwrap();
    let cases = [
        write(tmp.path(), "a.png", b"just some text"),
        write(tmp.path(), "a.txt", b"just some text"),
        tmp.path().join("gone.png"),
    ];
    for src in cases {
        let output = probe_worker(&src);
        assert_eq!(output.status.code(), Some(3), "{src:?}: {output:?}");
        let reason = probe(&src).unwrap_err();
        // The message as `escape` writes it: this one has no character to escape.
        assert!(!reason.contains(['\\', '\n', '\r']), "{reason}");
        assert_eq!(
            worker_stdout(&output),
            format!("lunaepix-probe 1 refused {reason}\n"),
            "{src:?}"
        );
    }
}

#[test]
fn a_probe_with_two_arguments_is_a_usage_error() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_of(8, 8));
    let other = write(tmp.path(), "b.png", &png_of(8, 8));
    let flag: &OsStr = PROBE_FLAG.as_ref();

    let calls: [&[&OsStr]; 3] = [
        &[flag],
        &[flag, src.as_os_str(), other.as_os_str()],
        &[flag, "--bogus".as_ref(), src.as_os_str()],
    ];
    for args in calls {
        let output = serve(args, None);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    // The same source on its own is answered.
    assert_eq!(probe_worker(&src).status.code(), Some(0));
}

#[test]
fn a_path_that_looks_like_an_option_is_still_a_path() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "--not-an-option.png", &png_of(12, 6));
    let flag: &OsStr = PROBE_FLAG.as_ref();

    // Found by name from the folder it is in: an answer, not a usage error.
    let output = serve(&[flag, "--not-an-option.png".as_ref()], Some(tmp.path()));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(worker_stdout(&output), "lunaepix-probe 1 size 12 6\n");

    // The same word with no such file is a refusal (exit 3), never exit 2.
    let output = serve(&[flag, "--bogus".as_ref()], Some(tmp.path()));
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(
        worker_stdout(&output),
        format!("lunaepix-probe 1 refused {}\n", Skip::UnsupportedType)
    );
}

// --- run_probe -------------------------------------------------------------------------

#[test]
fn run_probe_reads_size_and_refused() {
    let tmp = TempDir::new().unwrap();
    let png = write(tmp.path(), "a.png", &png_of(300, 200));
    assert_eq!(
        run_probe(WORKER.as_ref(), &png),
        ProbeAnswer::Size(probed(300, 200))
    );

    let svg = write(tmp.path(), "a.svg", SVG);
    assert_eq!(
        run_probe(WORKER.as_ref(), &svg),
        ProbeAnswer::Size(Probe {
            width: None,
            height: None
        })
    );

    let text = write(tmp.path(), "b.png", b"just some text");
    let reason = probe(&text).unwrap_err();
    assert_eq!(
        run_probe(WORKER.as_ref(), &text),
        ProbeAnswer::Refused(reason)
    );
    let big = write(tmp.path(), "big.png", &png_header(10_000, 10_001));
    assert_eq!(
        run_probe(WORKER.as_ref(), &big),
        ProbeAnswer::Refused("image too large: 10000×10001 exceeds 100 megapixels".to_owned())
    );
}

#[test]
fn run_probe_with_a_program_that_is_not_there_is_failed() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_of(8, 8));
    assert_eq!(
        run_probe(&tmp.path().join("no-such-worker"), &src),
        ProbeAnswer::Failed
    );
}

#[cfg(unix)]
#[test]
fn run_probe_with_a_program_that_answers_nothing_is_failed() {
    // `true` and `false` ignore their arguments and print nothing: exit 0 and
    // exit 1 without a record are no answer.
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_of(8, 8));
    for program in ["/bin/true", "/bin/false"] {
        assert_eq!(
            run_probe(Path::new(program), &src),
            ProbeAnswer::Failed,
            "{program}"
        );
    }
}
