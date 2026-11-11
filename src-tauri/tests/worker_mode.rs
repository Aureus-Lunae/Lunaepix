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
