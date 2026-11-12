//! A whole session through the real worker: the app binary in worker mode,
//! as the release app's sidecar runs it. This reaches the core's planner,
//! `Worker::with_options` and `run_observed` through the app's own commands,
//! on every platform.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use lunaepix_app_lib::events::{Event, Status};
use lunaepix_app_lib::session::{PreparedRun, Session, worker_prober};
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
    run_after_start(session, settings, || {})
}

/// [`run`], with `between` called after `start_run` accepted the run and
/// before any file runs.
fn run_after_start(session: &Session, settings: &Value, between: impl FnOnce()) -> Vec<Event> {
    let events = RefCell::new(Vec::new());
    let mut prepared: Option<PreparedRun> = None;
    session
        .start_run(
            Some(settings),
            &mut |event| events.borrow_mut().push(event),
            |run| prepared = Some(run),
        )
        .unwrap();
    between();
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

fn listing(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
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
    let (width, height, _) = decoded(&out.join("a-1.png"));
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
    let dest = dir.path().join("optimized-a.png");
    assert_eq!(decoded(&dest), decoded(&src));
    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(listing(dir.path()), ["a.png", "optimized-a.png"]);
}

#[test]
fn a_lossless_run_through_the_real_worker_keeps_an_earlier_output_next_to_the_original() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    let original = gradient_png();
    fs::write(&src, &original).unwrap();
    let earlier = dir.path().join("optimized-a.png");
    fs::write(&earlier, b"an earlier output").unwrap();

    let session = Session::new();
    add(&session, vec![src.clone()]);
    let events = run(&session, &json!({ "mode": "lossless", "quality": 75 }));

    assert_eq!(done_rows(&events), [(Status::Done, None)]);
    assert_eq!(fs::read(&earlier).unwrap(), b"an earlier output");
    assert_eq!(
        decoded(&dir.path().join("optimized-a-1.png")),
        decoded(&src)
    );
    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(
        listing(dir.path()),
        ["a.png", "optimized-a-1.png", "optimized-a.png"]
    );
}

#[test]
fn two_runs_on_a_png_with_no_output_folder_leave_optimized_a_and_optimized_a_1() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    let original = gradient_png();
    fs::write(&src, &original).unwrap();
    let first = dir.path().join("optimized-a.png");
    let second = dir.path().join("optimized-a-1.png");

    // The first run is lossless, the second lossy: the bytes tell them apart.
    let session = Session::new();
    add(&session, vec![src.clone()]);
    let events = run(&session, &json!({ "mode": "lossless", "quality": 75 }));
    assert_eq!(done_rows(&events), [(Status::Done, None)]);
    let first_bytes = fs::read(&first).unwrap();
    assert_eq!(listing(dir.path()), ["a.png", "optimized-a.png"]);

    let session = Session::new();
    add(&session, vec![src.clone()]);
    let events = run(&session, &json!({ "mode": "lossy", "quality": 40 }));
    assert_eq!(done_rows(&events), [(Status::Done, None)]);

    // The first run's output is as it was, the second run's is beside it.
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    assert_eq!(decoded(&first), decoded(&src));
    assert_ne!(decoded(&second), decoded(&src));
    assert_eq!(
        listing(dir.path()),
        ["a.png", "optimized-a-1.png", "optimized-a.png"]
    );

    // A third run takes the next number.
    let session = Session::new();
    add(&session, vec![src.clone()]);
    run(&session, &json!({ "mode": "lossless", "quality": 75 }));
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    assert!(dir.path().join("optimized-a-2.png").exists());
    assert!(!dir.path().join("optimized-a-0.png").exists());
    assert_eq!(fs::read(&src).unwrap(), original);
}

// --- an output that cannot be written: the reason carries no path ----------

/// Make `folder` read-only. `false` when running as root, where it stays
/// writable and the test has nothing to check.
#[cfg(unix)]
fn lock(folder: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(folder.join("probe"), b"").is_ok() {
        eprintln!("skipped: the folder stays writable (running as root)");
        return false;
    }
    true
}

/// `reason` names no part of `path` and no temporary file: the window never
/// receives a path.
#[cfg(unix)]
fn assert_no_path(reason: &str, path: &Path) {
    assert!(!reason.contains("at path"), "{reason:?}");
    assert!(!reason.contains(".tmp"), "{reason:?}");
    for part in path.components() {
        if let std::path::Component::Normal(name) = part {
            let name = name.to_string_lossy();
            assert!(!reason.contains(&*name), "{name:?} in {reason:?}");
        }
    }
}

/// What the system calls a refused write: `EACCES` is 13 on Linux and macOS.
#[cfg(unix)]
fn permission_denied() -> String {
    std::io::Error::from_raw_os_error(13).to_string()
}

#[cfg(unix)]
#[test]
fn a_lossless_run_into_a_read_only_folder_fails_with_a_reason_without_a_path() {
    // The output goes beside the original, in a folder that cannot be
    // written to.
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("locked-folder");
    fs::create_dir(&folder).unwrap();
    let src = folder.join("a.png");
    fs::write(&src, gradient_png()).unwrap();
    if !lock(&folder) {
        return;
    }

    let session = Session::new();
    add(&session, vec![src.clone()]);
    let events = run(&session, &json!({ "mode": "lossless", "quality": 75 }));

    let rows = done_rows(&events);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, Status::Failed, "{rows:?}");
    let reason = rows[0].1.clone().expect("a reason");
    assert_eq!(reason, permission_denied());
    assert_no_path(&reason, &folder.join("optimized-a.png"));
}

#[cfg(unix)]
#[test]
fn a_run_into_an_output_folder_made_read_only_fails_with_a_reason_without_a_path() {
    // The folder was writable when the run started; it is locked before the
    // file runs.
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("a.png");
    fs::write(&src, gradient_png()).unwrap();
    let out = dir.path().join("locked-folder");
    fs::create_dir(&out).unwrap();

    let session = Session::new();
    add(&session, vec![src.clone()]);
    session
        .choose_output_folder(&mut |_| {}, |ticket| {
            session.set_output(ticket, Some(out.clone()), &mut |_| {});
        })
        .unwrap();
    let mut locked = true;
    let events = run_after_start(&session, &json!({ "mode": "lossy", "quality": 40 }), || {
        locked = lock(&out);
    });
    if !locked {
        return;
    }

    let rows = done_rows(&events);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, Status::Failed, "{rows:?}");
    let reason = rows[0].1.clone().expect("a reason");
    assert_eq!(reason, permission_denied());
    assert_no_path(&reason, &out.join("a.png"));
}

// --- Single image mode: loading through the real probe worker ---------------

/// `choose_image` answered with `path`, probed by the real worker: the
/// `image-changed` payload, as JSON.
fn choose_image(session: &Session, worker: &Worker, path: &Path) -> Value {
    let events = RefCell::new(Vec::new());
    session
        .choose_image(&mut |event| events.borrow_mut().push(event), |ticket| {
            session.load_image(
                ticket,
                Some(path.to_path_buf()),
                worker_prober(worker),
                &mut |event| events.borrow_mut().push(event),
            );
        })
        .unwrap();
    let events = events.into_inner();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].name(), "image-changed");
    serde_json::to_value(&events[0]).unwrap()
}

#[test]
fn a_choice_loads_through_the_real_probe_worker_and_a_damaged_png_is_refused() {
    let dir = TempDir::new().unwrap();
    let good = dir.path().join("a.png");
    let bytes = gradient_png();
    fs::write(&good, &bytes).unwrap();
    let damaged = dir.path().join("b.png");
    fs::write(&damaged, b"\x89PNG\r\n\x1a\nnot a png after all").unwrap();
    let worker = worker();
    let session = Session::new();

    let changed = choose_image(&session, &worker, &good);
    assert_eq!(
        changed,
        json!({ "image": { "name": "a.png", "size": bytes.len(), "width": 64, "height": 64,
                           "key": "1", "suggested": "optimized-a", "extension": ".png" },
                "left_out": 0, "notice": null })
    );

    // The worker's refusal is the core's own reason for that file.
    let reason = lunaepix_core::probe(&damaged).expect_err("a damaged PNG is refused");
    let changed = choose_image(&session, &worker, &damaged);
    assert_eq!(changed["image"]["key"], json!("1"), "the image stays");
    assert_eq!(changed["notice"], json!(reason));
    assert_no_path(&reason, &damaged);
}
