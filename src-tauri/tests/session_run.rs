//! A whole session through the real worker: the app binary in worker mode,
//! as the release app's sidecar runs it. This reaches the core's planner,
//! `Worker::with_options` and `run_observed` through the app's own commands,
//! on every platform.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use lunaepix_app_lib::events::{Event, Status};
use lunaepix_app_lib::session::{PreparedRun, Session};
use lunaepix_core::{StopHandle, Worker};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A PNG with more than 256 colours, so a lossy run has to change pixels.
fn gradient_png() -> Vec<u8> {
    let mut pixels = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u8 {
        for x in 0..64u8 {
            pixels.extend_from_slice(&[x * 4, y * 4, x.wrapping_add(y).wrapping_mul(2)]);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 64, 64);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

/// The RGB(A) pixels and size of a PNG file.
fn decoded(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    pixels.truncate(info.buffer_size());
    (info.width, info.height, pixels)
}

fn worker() -> Worker {
    Worker::new(env!("CARGO_BIN_EXE_lunaepix-app"))
}

/// Run the list of `session` with `settings` through the real worker: the
/// events the window would hear.
fn run(session: &Session, settings: &Value) -> Vec<Event> {
    let events = RefCell::new(Vec::new());
    let mut prepared: Option<PreparedRun> = None;
    session
        .start_run(
            Some(settings),
            &mut |event| events.borrow_mut().push(event),
            |run| prepared = Some(run),
        )
        .unwrap();
    session.run_with_worker(
        prepared.expect("the run started"),
        &worker(),
        &StopHandle::new(),
        &mut |event| events.borrow_mut().push(event),
    );
    events.into_inner()
}

fn add(session: &Session, paths: Vec<PathBuf>) {
    session
        .choose_files(&mut |_| {}, |ticket| {
            session.add_paths(ticket, Some(paths), &mut |_| {});
        })
        .unwrap();
}

fn done_rows(events: &[Event]) -> Vec<(Status, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::FileDone(done) => Some((done.status, done.reason.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_lossy_run_into_an_output_folder_through_the_real_worker_numbers_a_taken_name() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    let original = gradient_png();
    fs::write(&src, &original).unwrap();
    let out = dir.path().join("exports");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("a.png"), b"keep me").unwrap();

    let session = Session::new();
    add(&session, vec![src.clone()]);
    session
        .choose_output_folder(&mut |_| {}, |ticket| {
            session.set_output(ticket, Some(out.clone()), &mut |_| {});
        })
        .unwrap();
    let events = run(&session, &json!({ "mode": "lossy", "quality": 40 }));

    assert_eq!(
        serde_json::to_value(&events[0]).unwrap(),
        json!({ "total": 1, "mode": "lossy", "quality": 40 })
    );
    assert_eq!(done_rows(&events), [(Status::Done, None)]);
    // Nothing in the output folder is replaced; the output takes a number.
    assert_eq!(fs::read(out.join("a.png")).unwrap(), b"keep me");
    let (width, height, _) = decoded(&out.join("a-2.png"));
    assert_eq!((width, height), (64, 64));
    // The original is untouched, and nothing is written beside it.
    assert_eq!(fs::read(&src).unwrap(), original);
    assert!(!dir.path().join("optimized-a.png").exists());
    assert!(!dir.path().join("optimized").exists());
}

#[test]
fn a_lossless_run_through_the_real_worker_writes_next_to_the_original() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    let original = gradient_png();
    fs::write(&src, &original).unwrap();
    // An earlier output next to the original is replaced, as always.
    let dest = dir.path().join("optimized-a.png");
    fs::write(&dest, b"an earlier output").unwrap();

    let session = Session::new();
    add(&session, vec![src.clone()]);
    let events = run(&session, &json!({ "mode": "lossless", "quality": 75 }));

    assert_eq!(
        serde_json::to_value(&events[0]).unwrap(),
        json!({ "total": 1, "mode": "lossless", "quality": null })
    );
    assert_eq!(done_rows(&events), [(Status::Done, None)]);
    let note = events.iter().find_map(|event| match event {
        Event::FileDone(done) => Some(done.note.clone()),
        _ => None,
    });
    assert_eq!(note, Some(None));
    // Not a pixel changed, and the original is untouched.
    assert_eq!(decoded(&dest), decoded(&src));
    assert_eq!(fs::read(&src).unwrap(), original);
}
