//! The app binary answers the runner's worker mode before it touches Tauri.
//!
//! Every run here has no display (`DISPLAY` and `WAYLAND_DISPLAY` removed,
//! GTK forced to X11), so a binary that started its window instead would
//! fail, not show one, and a timeout stops it if it hangs.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const WORKER_FLAG: &str = "--lunaepix-worker";
const THUMB_FLAG: &str = "--lunaepix-thumb";
const TIMEOUT: Duration = Duration::from_secs(60);

/// A PNG with something to gain: flat, stored with fast compression.
fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 64, 64);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&vec![90; 64 * 64 * 3]).unwrap();
    writer.finish().unwrap();
    out
}

/// Run the app binary with `args`, headless, and stop it after [`TIMEOUT`].
fn app(args: &[&std::ffi::OsStr]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lunaepix-app"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("GDK_BACKEND", "x11")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > TIMEOUT {
            child.kill().unwrap();
            panic!("the app binary did not exit within {TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

fn worker(src: &Path, dest: &Path) -> Output {
    app(&[WORKER_FLAG.as_ref(), src.as_os_str(), dest.as_os_str()])
}

#[test]
fn the_app_binary_optimises_one_file_in_worker_mode_without_a_window() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("hero image.png");
    let dest = dir.path().join("optimized-hero image.png");
    let original = png_bytes();
    fs::write(&src, &original).unwrap();

    let output = worker(&src, &dest);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert!(stdout.starts_with("lunaepix-worker 2 ok "), "{stdout:?}");
    let written = fs::read(&dest).unwrap();
    assert!(written.len() < original.len());
    assert_eq!(fs::read(&src).unwrap(), original);
}

#[test]
fn the_app_binary_reports_a_corrupt_file_in_worker_mode_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("broken.png");
    let dest = dir.path().join("optimized-broken.png");
    fs::write(&src, b"this is not a PNG").unwrap();

    let output = worker(&src, &dest);

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert!(
        stdout.starts_with("lunaepix-worker 2 invalid src "),
        "{stdout:?}"
    );
    assert!(!dest.exists());
}

#[test]
fn the_app_binary_refuses_a_worker_call_without_a_destination() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    fs::write(&src, png_bytes()).unwrap();

    let output = app(&[WORKER_FLAG.as_ref(), src.as_os_str()]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let mut names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["a.png"]);
}

/// A 200 x 100 RGB gradient: scaled to 64 x 32 by the thumbnail mode.
fn wide_png_bytes() -> Vec<u8> {
    let (w, h) = (200u32, 100u32);
    let pixels: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % w) as u8, (i / w) as u8, 90])
        .collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

#[test]
fn thumb_flag_answers_a_png_with_the_header_and_a_decodable_thumbnail() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("hero image.png");
    fs::write(&src, wide_png_bytes()).unwrap();

    let output = app(&[THUMB_FLAG.as_ref(), src.as_os_str()]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let header = b"lunaepix-thumb 1 png\n";
    assert!(output.stdout.starts_with(header), "{:?}", output.stdout);
    let mut reader = png::Decoder::new(&output.stdout[header.len()..])
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).unwrap();
    assert_eq!((frame.width, frame.height), (64, 32));
}

#[test]
fn thumb_flag_exits_3_for_a_text_file_and_prints_nothing() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("notes.txt");
    fs::write(&src, b"just some text").unwrap();

    let output = app(&[THUMB_FLAG.as_ref(), src.as_os_str()]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
}

// --- the `thumb` handler's answer, with the app binary as the worker --------

use lunaepix_app_lib::thumbs::{self, Thumbs};

/// A GET request for `address`, as the webview sends it.
fn request_for(address: &str) -> tauri::http::Request<Vec<u8>> {
    tauri::http::Request::builder()
        .uri(address)
        .body(Vec::new())
        .expect("a request")
}

/// The request for `key`, on the Linux and macOS address.
fn uri(key: &Option<String>) -> tauri::http::Request<Vec<u8>> {
    request_for(&format!(
        "thumb://localhost/{}",
        key.as_deref().expect("a key")
    ))
}

fn app_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_lunaepix-app"))
}

#[test]
fn serve_with_the_app_binary_answers_a_png_and_an_svg() {
    let dir = TempDir::new().unwrap();
    let png = dir.path().join("hero image.png");
    fs::write(&png, wide_png_bytes()).unwrap();
    let svg = dir.path().join("logo.svg");
    let svg_bytes = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
    fs::write(&svg, svg_bytes).unwrap();
    let thumbs = Thumbs::new();
    let keys = thumbs.snapshot(&[Some(png), Some(svg)]);

    let answer = thumbs::serve(&thumbs, app_binary(), &uri(&keys[0]));
    assert_eq!(answer.status, 200);
    assert_eq!(answer.content_type, Some("image/png"));
    let mut reader = png::Decoder::new(answer.body.as_slice())
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).unwrap();
    assert_eq!((frame.width, frame.height), (64, 32));

    let answer = thumbs::serve(&thumbs, app_binary(), &uri(&keys[1]));
    assert_eq!(answer.status, 200);
    assert_eq!(answer.content_type, Some("image/svg+xml"));
    assert_eq!(answer.body, svg_bytes);
}

#[test]
fn serve_with_the_app_binary_is_404_for_a_text_file_and_remembers_it() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("notes.png");
    fs::write(&src, b"just some text").unwrap();
    let thumbs = Thumbs::new();
    let keys = thumbs.snapshot(&[Some(src.clone())]);

    let answer = thumbs::serve(&thumbs, app_binary(), &uri(&keys[0]));
    assert_eq!(
        (answer.status, answer.content_type, answer.body.len()),
        (404, None, 0)
    );
    // Remembered: even once the file is a picture, the key stays 404.
    fs::write(&src, wide_png_bytes()).unwrap();
    assert_eq!(
        thumbs::serve(&thumbs, app_binary(), &uri(&keys[0])).status,
        404
    );
}

#[test]
fn serve_with_a_program_that_is_not_there_is_404_and_tries_again() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    fs::write(&src, wide_png_bytes()).unwrap();
    let thumbs = Thumbs::new();
    let keys = thumbs.snapshot(&[Some(src)]);

    let missing = dir.path().join("no-such-worker");
    let answer = thumbs::serve(&thumbs, &missing, &uri(&keys[0]));
    assert_eq!(
        (answer.status, answer.content_type, answer.body.len()),
        (404, None, 0)
    );
    // Not remembered: with a worker that is there, the same key is made.
    let answer = thumbs::serve(&thumbs, app_binary(), &uri(&keys[0]));
    assert_eq!(answer.status, 200);
    assert_eq!(answer.content_type, Some("image/png"));
}

#[test]
fn serve_with_the_app_binary_is_404_for_a_query_and_starts_no_worker() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    fs::write(&src, wide_png_bytes()).unwrap();
    let thumbs = Thumbs::new();
    let keys = thumbs.snapshot(&[Some(src)]);
    let key = keys[0].as_deref().expect("a key");

    // The key is current (the plain request below is answered), so the
    // query alone is the reason for the 404.
    for address in [
        format!("thumb://localhost/{key}?x=1"),
        format!("http://thumb.localhost/{key}?"),
    ] {
        let answer = thumbs::serve(&thumbs, app_binary(), &request_for(&address));
        assert_eq!(
            (answer.status, answer.content_type, answer.body.len()),
            (404, None, 0),
            "{address}"
        );
    }
    let answer = thumbs::serve(&thumbs, app_binary(), &uri(&keys[0]));
    assert_eq!(answer.status, 200);
    assert_eq!(answer.content_type, Some("image/png"));
    // The Windows address answers the same key.
    let windows = request_for(&format!("http://thumb.localhost/{key}"));
    assert_eq!(thumbs::serve(&thumbs, app_binary(), &windows).status, 200);
}
