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
/// receives a path. Used on every platform: the probe test that is not
/// Unix-only calls it too.
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

// --- Single image mode: the preview through the real worker -----------------

use std::sync::Mutex;
// Only `shell` and its tests and `mod running` (all Unix) use it, so on
// Windows it would be an unused import.
#[cfg(unix)]
use std::time::Duration;

use lunaepix_app_lib::events::{FileDone, PreviewFailed, PreviewReady};
use lunaepix_app_lib::previews::{self, PreviewResponse};
use lunaepix_app_lib::single::PreviewJob;

fn lossy(quality: u8) -> Value {
    json!({ "mode": "lossy", "quality": quality })
}

fn lossless() -> Value {
    json!({ "mode": "lossless", "quality": 75 })
}

/// What the window hears, from any thread.
#[derive(Default)]
struct Heard(Mutex<Vec<Event>>);

impl Heard {
    fn emit(&self) -> impl FnMut(Event) + '_ {
        |event| self.0.lock().unwrap().push(event)
    }

    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// `preview_image` with `settings`, which must start: the job.
fn request(session: &Session, settings: &Value, heard: &Heard) -> PreviewJob {
    let mut job = None;
    session
        .preview_image(Some(settings), &mut heard.emit(), |started| {
            job = Some(started)
        })
        .unwrap();
    job.expect("the preview started")
}

/// A preview with `settings`, made by `worker` on this thread: every event
/// the window hears.
fn preview_with(session: &Session, settings: &Value, worker: &Worker) -> Vec<Event> {
    let heard = Heard::default();
    let job = request(session, settings, &heard);
    session.run_preview(job, worker, &mut heard.emit());
    heard.take()
}

fn names(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(Event::name).collect()
}

/// The `preview-ready` of a preview that sent exactly started and ready.
fn only_ready(events: &[Event]) -> PreviewReady {
    assert_eq!(
        names(events),
        ["preview-started", "preview-ready"],
        "{events:?}"
    );
    match &events[1] {
        Event::PreviewReady(ready) => ready.clone(),
        _ => unreachable!(),
    }
}

/// The `preview-failed` of a preview that sent exactly started and failed.
fn only_failed(events: &[Event]) -> PreviewFailed {
    assert_eq!(
        names(events),
        ["preview-started", "preview-failed"],
        "{events:?}"
    );
    match &events[1] {
        Event::PreviewFailed(failed) => failed.clone(),
        _ => unreachable!(),
    }
}

/// What the `preview` scheme answers for `path`, with no query.
fn served(session: &Session, path: &str) -> PreviewResponse {
    previews::serve(session.single(), path, None)
}

/// `name` holding `bytes` in a new folder, loaded through the real probe
/// worker: the folder, the session and the `image-changed` payload.
fn loaded(name: &str, bytes: &[u8]) -> (TempDir, Session, Value) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    let session = Session::new();
    let changed = choose_image(&session, &worker(), &path);
    assert_eq!(changed["notice"], Value::Null, "{changed}");
    (dir, session, changed)
}

/// The private folder of the current image.
fn image_folder(session: &Session) -> PathBuf {
    let copy = session.single().current().expect("an image").copy;
    copy.parent().unwrap().to_path_buf()
}

// The sizes are the served bytes.

/// A gradient PNG previewed with `settings`: the sizes in `preview-ready`
/// are the lengths of what the scheme serves. The version's bytes.
fn sizes_match(settings: Value, mode: &str, quality: Value) -> (Vec<u8>, Vec<u8>) {
    let (_dir, session, changed) = loaded("a.png", &gradient_png());
    let ready = only_ready(&preview_with(&session, &settings, &worker()));
    assert_eq!((ready.image.as_str(), ready.version), ("1", 1));
    assert_eq!(ready.key, "1-1");
    let payload = serde_json::to_value(&ready).unwrap();
    assert_eq!(payload["mode"], json!(mode));
    assert_eq!(payload["quality"], quality);
    assert_eq!(payload["note"], Value::Null);

    let original = served(&session, "/1");
    assert_eq!(original.status, 200);
    assert_eq!(ready.before, original.body.len() as u64);
    assert_eq!(json!(ready.before), changed["image"]["size"]);
    let version = served(&session, "/1-1");
    assert_eq!(version.status, 200);
    assert_eq!(version.content_type, Some("image/png"));
    assert_eq!(ready.after, version.body.len() as u64);
    let file = image_folder(&session).join("v1.png");
    assert_eq!(fs::read(&file).unwrap(), version.body);
    (original.body, version.body)
}

#[test]
fn preview_ready_sizes_equal_the_served_bytes_length_and_the_original_size_at_quality_0() {
    let (original, version) = sizes_match(lossy(0), "lossy", json!(0));
    // The sizes follow the version, not the original.
    assert_ne!(original, version);
}

#[test]
fn preview_ready_sizes_equal_the_served_bytes_length_and_the_original_size_at_quality_75() {
    sizes_match(lossy(75), "lossy", json!(75));
}

#[test]
fn preview_ready_sizes_equal_the_served_bytes_length_and_the_original_size_at_quality_100() {
    sizes_match(lossy(100), "lossy", json!(100));
}

#[test]
fn preview_ready_sizes_equal_the_served_bytes_length_and_the_original_size_lossless() {
    sizes_match(lossless(), "lossless", Value::Null);
}

// The kept version.

#[test]
fn a_failed_preview_keeps_the_previous_ready_version() {
    let (_dir, session, _) = loaded("a.png", &gradient_png());
    only_ready(&preview_with(&session, &lossy(40), &worker()));
    let kept = served(&session, "/1-1");
    assert_eq!(kept.status, 200);
    // The copy goes away, so the next version cannot be made.
    fs::remove_file(session.single().current().unwrap().copy).unwrap();
    let failed = only_failed(&preview_with(&session, &lossy(60), &worker()));
    assert_eq!(failed.version, 2);
    assert_eq!(session.single().latest_ready(), Some((1, 1)));
    assert_eq!(served(&session, "/1-1"), kept);
    assert_eq!(served(&session, "/1-2").status, 404);
}

#[test]
fn a_later_ready_preview_replaces_the_kept_version() {
    let (_dir, session, _) = loaded("a.png", &gradient_png());
    only_ready(&preview_with(&session, &lossy(40), &worker()));
    let second = only_ready(&preview_with(&session, &lossy(60), &worker()));
    assert_eq!(second.key, "1-2");
    assert_eq!(session.single().latest_ready(), Some((1, 2)));
    assert_eq!(served(&session, "/1-1").status, 404);
    assert_eq!(served(&session, "/1-2").status, 200);
    assert!(!image_folder(&session).join("v1.png").exists());
}

// Saving the shown version.

#[test]
fn save_writes_exactly_the_served_versions_bytes_through_the_real_worker() {
    let (dir, session, _) = loaded("a.png", &gradient_png());
    let ready = only_ready(&preview_with(&session, &lossy(40), &worker()));
    let shown = served(&session, &format!("/{}", ready.key));
    assert_eq!(shown.status, 200);
    let heard = Heard::default();
    session
        .save_image(
            Some(&json!({ "name": "beach", "preview": ready.key })),
            &mut heard.emit(),
        )
        .unwrap();
    let saved = fs::read(dir.path().join("beach.png")).unwrap();
    assert_eq!(saved, shown.body);
    assert_eq!(saved.len() as u64, ready.after);
    // The version, not the original.
    assert_ne!(saved, gradient_png());
    assert_eq!(names(&heard.take()), ["image-saved"]);
}

// Failures.

/// A gradient PNG previewed by `worker`, which fails: the `preview-failed`.
fn failing(worker: &Worker) -> PreviewFailed {
    let (dir, session, _) = loaded("a.png", &gradient_png());
    let failed = only_failed(&preview_with(&session, &lossy(40), worker));
    assert_eq!((failed.image.as_str(), failed.version), ("1", 1));
    assert_eq!(session.single().latest_ready(), None);
    assert_no_path_in(&failed.reason, dir.path());
    failed
}

/// `reason` holds no part of the temporary folder's or the private folder's
/// path.
fn assert_no_path_in(reason: &str, dir: &Path) {
    let name = dir.file_name().unwrap().to_string_lossy();
    assert!(!reason.contains(&*name), "{reason:?}");
    assert!(!reason.contains("image-1"), "{reason:?}");
    assert!(!reason.contains("original"), "{reason:?}");
}

/// A worker that runs `script` in a shell, with `timeout`.
#[cfg(unix)]
fn shell(script: &str, timeout: Duration) -> Worker {
    Worker {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), script.into(), "fake-worker".into()],
        timeout,
    }
}

#[cfg(unix)]
#[test]
fn a_worker_crash_sends_preview_failed() {
    let failed = failing(&shell("exit 0", Duration::from_secs(60)));
    assert_eq!(
        failed.reason,
        "the optimiser crashed on this file (it gave no valid result)"
    );
}

#[cfg(unix)]
#[test]
fn a_worker_exit_code_sends_preview_failed() {
    let failed = failing(&shell("exit 7", Duration::from_secs(60)));
    assert_eq!(
        failed.reason,
        "the optimiser crashed on this file (exit code 7)"
    );
}

#[cfg(unix)]
#[test]
fn a_worker_timeout_sends_preview_failed() {
    let failed = failing(&shell("exec sleep 5", Duration::from_millis(300)));
    assert_eq!(
        failed.reason,
        "the optimiser timed out after 300ms and was stopped"
    );
}

#[test]
fn a_worker_that_cannot_start_sends_preview_failed() {
    let nowhere = TempDir::new().unwrap();
    let failed = failing(&Worker::new(nowhere.path().join("no-such-worker")));
    assert_eq!(
        failed.reason,
        format!(
            "the optimiser could not be started: {}",
            std::io::Error::from_raw_os_error(2)
        )
    );
}

#[test]
fn an_unreadable_copy_sends_preview_failed() {
    let (dir, session, _) = loaded("a.png", &gradient_png());
    fs::remove_file(session.single().current().unwrap().copy).unwrap();
    let failed = only_failed(&preview_with(&session, &lossy(40), &worker()));
    assert_eq!(
        failed.reason,
        std::io::Error::from_raw_os_error(2).to_string()
    );
    assert_no_path_in(&failed.reason, dir.path());
    assert!(!image_folder(&session).join("v1.png").exists());
}

// Batch's behaviour and notes.

/// `src` run by Batch with `settings` into a new output folder: its one
/// `file-done` row and the bytes it wrote.
fn batch_output(src: &Path, settings: &Value) -> (FileDone, Vec<u8>) {
    let out = TempDir::new().unwrap();
    let session = Session::new();
    add(&session, vec![src.to_path_buf()]);
    session
        .choose_output_folder(&mut |_| {}, |ticket| {
            session.set_output(ticket, Some(out.path().to_path_buf()), &mut |_| {});
        })
        .unwrap();
    let events = run(&session, settings);
    let done = events
        .iter()
        .find_map(|event| match event {
            Event::FileDone(done) => Some(done.clone()),
            _ => None,
        })
        .expect("one file-done");
    assert_eq!(done.status, Status::Done, "{done:?}");
    let bytes = fs::read(out.path().join(src.file_name().unwrap())).unwrap();
    (done, bytes)
}

const SVG: &[u8] = br##"<?xml version="1.0" encoding="UTF-8"?>
<!-- a comment the optimiser removes -->
<svg xmlns="http://www.w3.org/2000/svg"   width="100"  height="50">
    <rect x="0" y="0"   width="100" height="50" fill="#ff0000"/>
    <circle cx="50.000" cy="25.000" r="10.000" fill="#00ff00"/>
</svg>
"##;

#[test]
fn a_lossy_preview_of_an_svg_has_batchs_behaviour() {
    let (dir, session, _) = loaded("logo.svg", SVG);
    let lossy_ready = only_ready(&preview_with(&session, &lossy(40), &worker()));
    let lossy_bytes = served(&session, "/1-1");
    assert_eq!(lossy_bytes.content_type, Some("image/svg+xml"));
    let lossless_ready = only_ready(&preview_with(&session, &lossless(), &worker()));
    let lossless_bytes = served(&session, "/1-2");
    // An SVG gets the lossless treatment either way, without a note.
    assert_eq!(lossy_bytes.body, lossless_bytes.body);
    assert_eq!(lossy_ready.note, None);
    assert_eq!(lossless_ready.note, None);
    assert!(lossy_ready.after < lossy_ready.before);

    let (done, written) = batch_output(&dir.path().join("logo.svg"), &lossy(40));
    assert_eq!(done.after, Some(lossy_ready.after));
    assert_eq!(done.note, None);
    assert_eq!(written, lossy_bytes.body);
}

#[test]
fn lossy_and_lossless_previews_of_a_png_differ() {
    let (_dir, session, _) = loaded("a.png", &gradient_png());
    only_ready(&preview_with(&session, &lossy(40), &worker()));
    let lossy_bytes = served(&session, "/1-1").body;
    only_ready(&preview_with(&session, &lossless(), &worker()));
    let lossless_bytes = served(&session, "/1-2").body;
    assert_ne!(lossy_bytes, lossless_bytes);
}

/// A 16 × 16 animated PNG of two frames.
fn apng() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 16, 16);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    for frame in 0..2u8 {
        let pixels: Vec<u8> = (0..16 * 16 * 4)
            .map(|i: u32| (i as u8).wrapping_mul(7).wrapping_add(frame * 90))
            .collect();
        writer.write_image_data(&pixels).unwrap();
    }
    writer.finish().unwrap();
    out
}

#[test]
fn a_lossy_preview_of_an_animated_png_has_the_kept_lossless_note() {
    let (dir, session, _) = loaded("anim.png", &apng());
    let ready = only_ready(&preview_with(&session, &lossy(40), &worker()));
    assert_eq!(ready.note.as_deref(), Some("kept lossless: animated PNG"));
    let (done, written) = batch_output(&dir.path().join("anim.png"), &lossy(40));
    assert_eq!(done.note, ready.note);
    assert_eq!(done.after, Some(ready.after));
    assert_eq!(written, served(&session, "/1-1").body);
}

#[test]
fn a_lossy_preview_of_a_still_png_has_no_note() {
    let (_dir, session, _) = loaded("a.png", &gradient_png());
    let ready = only_ready(&preview_with(&session, &lossy(40), &worker()));
    assert_eq!(ready.note, None);
}

// The EXIF orientation of a lossy JPG (Review Focus 1).

/// A noisy RGB JPEG at quality 95, so a lossy version is smaller.
fn noisy_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut seed: u32 = 0x1234_5678;
    let pixels: Vec<u8> = (0..width * height * 3)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        })
        .collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
        .encode(&pixels, width, height, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// `jpeg` with an EXIF APP1 segment holding only Orientation, after its
/// JFIF APP0 (or right after SOI when it has none).
fn with_exif(jpeg: &[u8], orientation: u16, big_endian: bool) -> Vec<u8> {
    let u16_bytes = |v: u16| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let u32_bytes = |v: u32| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(if big_endian { b"MM" } else { b"II" });
    payload.extend(u16_bytes(42));
    payload.extend(u32_bytes(8));
    payload.extend(u16_bytes(1));
    payload.extend(u16_bytes(0x0112));
    payload.extend(u16_bytes(3));
    payload.extend(u32_bytes(1));
    payload.extend(u16_bytes(orientation));
    payload.extend([0, 0]);
    payload.extend(u32_bytes(0));
    let mut segment = vec![0xFF, 0xE1];
    segment.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    segment.extend(payload);
    let at = if jpeg[2..4] == [0xFF, 0xE0] {
        4 + usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]))
    } else {
        2
    };
    [&jpeg[..at], &segment[..], &jpeg[at..]].concat()
}

/// The EXIF Orientation of `jpeg`: read from its first `Exif` APP1 segment
/// before the scan, from IFD0. `None` without one.
fn exif_orientation(jpeg: &[u8]) -> Option<u16> {
    let mut at = 2;
    while at + 4 <= jpeg.len() && jpeg[at] == 0xFF && jpeg[at + 1] != 0xDA {
        let length = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        let payload = jpeg.get(at + 4..at + 2 + length)?;
        if jpeg[at + 1] == 0xE1
            && let Some(tiff) = payload.strip_prefix(b"Exif\0\0")
        {
            return orientation_in(tiff);
        }
        at += 2 + length;
    }
    None
}

fn orientation_in(tiff: &[u8]) -> Option<u16> {
    let big_endian = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(i..i + 2)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32_at = |i: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(i..i + 4)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    (0..usize::from(u16_at(ifd)?))
        .map(|entry| ifd + 2 + 12 * entry)
        .find(|&entry| u16_at(entry) == Some(0x0112))
        .and_then(|entry| u16_at(entry + 8))
}

/// A 96 × 64 JPG with `orientation` previewed lossy: the version is really
/// re-encoded and keeps the Orientation and the upright size.
fn keeps_orientation(orientation: u16, big_endian: bool) {
    let original = with_exif(&noisy_jpeg(96, 64), orientation, big_endian);
    assert_eq!(
        exif_orientation(&original),
        Some(orientation),
        "the fixture"
    );
    let (_dir, session, _) = loaded("photo.jpg", &original);
    let ready = only_ready(&preview_with(&session, &lossy(40), &worker()));
    assert_eq!(ready.note, None, "re-encoded, not kept");
    assert!(ready.after < ready.before);
    let version = served(&session, "/1-1");
    assert_eq!(version.content_type, Some("image/jpeg"));
    assert_ne!(version.body, original);
    assert_eq!(exif_orientation(&version.body), Some(orientation));
    let folder = image_folder(&session);
    assert_eq!(
        lunaepix_core::probe(&folder.join("v1.jpg")),
        lunaepix_core::probe(&folder.join("original.jpg"))
    );
}

#[test]
fn an_exif_rotated_jpg_keeps_its_orientation_in_the_lossy_version() {
    keeps_orientation(6, false);
}

#[test]
fn an_upside_down_jpg_keeps_orientation_3_in_the_lossy_version() {
    keeps_orientation(3, false);
}

#[test]
fn a_big_endian_exif_keeps_orientation_8_in_the_lossy_version() {
    keeps_orientation(8, true);
}

#[test]
fn the_orientation_reader_finds_none_in_a_jpg_without_exif() {
    let plain = noisy_jpeg(16, 16);
    assert_eq!(exif_orientation(&plain), None);
    assert_eq!(exif_orientation(&with_exif(&plain, 6, false)), Some(6));
    assert_eq!(exif_orientation(&with_exif(&plain, 8, true)), Some(8));
}

// --- Whether a process is gone -------------------------------------------------

/// Whether process `pid` is gone from the process table: killed and reaped,
/// not even a zombie left. `kill(pid, 0)` sends nothing and only asks whether
/// the pid exists, which a zombie still does. It works on every Unix; macOS
/// has no `/proc` to look in.
#[cfg(unix)]
fn process_is_gone(pid: u32) -> bool {
    let pid = libc::pid_t::try_from(pid).expect("pid fits pid_t");
    // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
    let answer = unsafe { libc::kill(pid, 0) };
    // `EPERM` would mean the process exists, so only `ESRCH` is "gone".
    answer == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Block until child `pid` has exited, and leave it unreaped (a zombie).
/// `waitid` with `WNOWAIT` does that on Linux and on macOS alike.
#[cfg(unix)]
fn wait_until_exited_without_reaping(pid: u32) {
    // SAFETY: `info` is a zeroed `siginfo_t` that the call fills in.
    let waited = unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(waited, 0, "{}", std::io::Error::last_os_error());
}

#[cfg(unix)]
fn sleeper() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn a_reaped_child_is_gone() {
    let mut child = sleeper();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(process_is_gone(child.id()));
}

#[cfg(unix)]
#[test]
fn a_running_child_is_not_gone() {
    let mut child = sleeper();
    let gone = process_is_gone(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!gone);
}

#[cfg(unix)]
#[test]
fn a_killed_child_that_was_not_waited_for_is_not_gone() {
    let mut child = sleeper();
    child.kill().unwrap();
    wait_until_exited_without_reaping(child.id());
    let zombie_is_gone = process_is_gone(child.id());
    child.wait().unwrap();
    assert!(!zombie_is_gone, "a zombie is not reaped");
    assert!(process_is_gone(child.id()), "now it is reaped");
}

// A running preview, stopped or not (the worker's PID is checked with
// `process_is_gone`). The worker is the real one behind a gate: a shell that
// records its PID, waits until the gate opens, then becomes the real worker.

#[cfg(unix)]
mod running {
    use std::time::Instant;

    use super::*;

    struct Gate(TempDir);

    impl Gate {
        fn new() -> Self {
            Gate(TempDir::new().unwrap())
        }

        fn worker(&self) -> Worker {
            let script = format!(
                "echo $$ >> '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; exec '{}' \"$@\"",
                self.0.path().join("pids").display(),
                self.0.path().join("go").display(),
                env!("CARGO_BIN_EXE_lunaepix-app"),
            );
            Worker {
                program: PathBuf::from("/bin/sh"),
                args: vec![
                    "-c".into(),
                    script.into(),
                    "gated-worker".into(),
                    "--lunaepix-worker".into(),
                ],
                timeout: Duration::from_secs(600),
            }
        }

        fn pids(&self) -> Vec<String> {
            fs::read_to_string(self.0.path().join("pids"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// The PIDs once `count` workers have started.
        fn wait_for(&self, count: usize) -> Vec<String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.pids().len() < count {
                assert!(Instant::now() < deadline, "no worker started");
                std::thread::sleep(Duration::from_millis(5));
            }
            self.pids()
        }

        fn open(&self) {
            fs::write(self.0.path().join("go"), b"").unwrap();
        }
    }

    /// The process is still in the process table (a zombie counts, so
    /// "not alive" means killed and reaped).
    fn alive(pid: &str) -> bool {
        !process_is_gone(pid.parse().unwrap())
    }

    /// Start a lossy 40 preview of the current image through a gated worker
    /// on a thread; once its worker runs, `between(pid)`; then open the
    /// gate and wait for the job. The preview's events.
    fn during_a_preview(session: &Session, between: impl FnOnce(&str)) -> Vec<Event> {
        let gate = Gate::new();
        let worker = &gate.worker();
        let heard = &Heard::default();
        let job = request(session, &lossy(40), heard);
        std::thread::scope(|scope| {
            let running = scope.spawn(move || session.run_preview(job, worker, &mut heard.emit()));
            let pid = gate.wait_for(1).remove(0);
            assert!(alive(&pid), "the worker runs");
            between(&pid);
            gate.open();
            running.join().unwrap();
        });
        heard.take()
    }

    fn set_view(session: &Session, view: &str) {
        session
            .set_view(Some(&json!({ "view": view })), &mut |_| {})
            .unwrap();
    }

    fn folder_names(folder: &Path) -> Vec<std::ffi::OsString> {
        listing(folder)
    }

    #[test]
    fn set_view_batch_stops_a_running_preview_and_no_ready_follows() {
        let (_dir, session, _) = loaded("a.png", &gradient_png());
        let events = during_a_preview(&session, |pid| {
            set_view(&session, "batch");
            assert!(!alive(pid), "the worker is stopped");
        });
        assert_eq!(names(&events), ["preview-started"]);
        assert_eq!(folder_names(&image_folder(&session)), ["original.png"]);
    }

    #[test]
    fn set_view_single_leaves_a_running_preview_to_send_ready() {
        let (_dir, session, _) = loaded("a.png", &gradient_png());
        let events = during_a_preview(&session, |pid| {
            set_view(&session, "single");
            assert!(alive(pid));
        });
        assert_eq!(only_ready(&events).version, 1);
    }

    #[test]
    fn move_drop_to_batch_stops_a_running_preview() {
        let dir = TempDir::new().unwrap();
        let paths = [dir.path().join("a.png"), dir.path().join("b.png")];
        for path in &paths {
            fs::write(path, gradient_png()).unwrap();
        }
        let session = Session::new();
        let probe_worker = worker();
        session.drop_paths(paths.to_vec(), &mut |_| {}, |ticket, dropped| {
            session.receive_drop(ticket, dropped, worker_prober(&probe_worker), &mut |_| {});
        });
        assert!(session.single().current().is_some());
        let events = during_a_preview(&session, |pid| {
            session.move_drop_to_batch(&mut |_| {}).unwrap();
            assert!(!alive(pid));
        });
        assert_eq!(names(&events), ["preview-started"]);
    }

    #[test]
    fn start_run_stops_a_running_preview() {
        let (dir, session, _) = loaded("a.png", &gradient_png());
        add(&session, vec![dir.path().join("a.png")]);
        let events = during_a_preview(&session, |pid| {
            let mut started = None;
            session
                .start_run(Some(&lossless()), &mut |_| {}, |run| started = Some(run))
                .unwrap();
            assert!(started.is_some());
            assert!(!alive(pid));
        });
        assert_eq!(names(&events), ["preview-started"]);
    }

    #[test]
    fn a_refused_start_run_leaves_a_running_preview_to_send_ready() {
        let (_dir, session, _) = loaded("a.png", &gradient_png());
        let events = during_a_preview(&session, |pid| {
            let answer = session.start_run(Some(&lossless()), &mut |_| {}, |_| {});
            assert_eq!(
                serde_json::to_value(answer.unwrap_err()).unwrap(),
                json!("empty-list")
            );
            assert!(alive(pid));
        });
        assert_eq!(only_ready(&events).version, 1);
    }

    #[test]
    fn a_new_load_stops_the_old_images_preview() {
        let (dir, session, _) = loaded("a.png", &gradient_png());
        let old_folder = image_folder(&session);
        let b = dir.path().join("b.png");
        fs::write(&b, gradient_png()).unwrap();
        let events = during_a_preview(&session, |pid| {
            let changed = choose_image(&session, &worker(), &b);
            assert_eq!(changed["image"]["key"], json!("2"));
            assert!(!alive(pid));
        });
        assert_eq!(names(&events), ["preview-started"]);
        assert!(!old_folder.exists());
    }

    #[test]
    fn a_drop_that_loads_stops_the_old_images_preview() {
        let (dir, session, _) = loaded("a.png", &gradient_png());
        let b = dir.path().join("b.png");
        fs::write(&b, gradient_png()).unwrap();
        let probe_worker = worker();
        let events = during_a_preview(&session, |pid| {
            session.drop_paths(vec![b.clone()], &mut |_| {}, |ticket, dropped| {
                session.receive_drop(ticket, dropped, worker_prober(&probe_worker), &mut |_| {});
            });
            assert_eq!(session.single().current().unwrap().key, 2);
            assert!(!alive(pid));
        });
        assert_eq!(names(&events), ["preview-started"]);
    }

    #[test]
    fn a_refused_load_leaves_the_running_preview_to_send_ready() {
        let (dir, session, _) = loaded("a.png", &gradient_png());
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, b"not an image").unwrap();
        let events = during_a_preview(&session, |pid| {
            let changed = choose_image(&session, &worker(), &notes);
            assert_eq!(changed["image"]["key"], json!("1"));
            assert!(alive(pid));
        });
        assert_eq!(only_ready(&events).version, 1);
    }

    #[test]
    fn shutdown_during_a_preview_kills_the_worker_and_removes_the_folder() {
        let (_dir, session, _) = loaded("a.png", &gradient_png());
        let private = image_folder(&session).parent().unwrap().to_path_buf();
        let events = during_a_preview(&session, |pid| {
            assert!(private.is_dir(), "the folder is there before");
            session.shutdown();
            assert!(!alive(pid));
            assert!(!private.exists());
        });
        assert_eq!(names(&events), ["preview-started"]);
        assert!(!private.exists());
    }

    #[test]
    fn a_newer_request_stops_the_older_worker_and_only_the_newer_sends_ready() {
        let (_dir, session, _) = loaded("a.png", &gradient_png());
        let gate = Gate::new();
        let worker = &gate.worker();
        let heard = &Heard::default();
        let session = &session;
        std::thread::scope(|scope| {
            let first = request(session, &lossy(40), heard);
            let one = scope.spawn(move || session.run_preview(first, worker, &mut heard.emit()));
            let first_pid = gate.wait_for(1).remove(0);
            let second = request(session, &lossy(60), heard);
            assert!(!alive(&first_pid), "the older worker is stopped");
            let two = scope.spawn(move || session.run_preview(second, worker, &mut heard.emit()));
            gate.wait_for(2);
            gate.open();
            one.join().unwrap();
            two.join().unwrap();
        });
        let events = heard.take();
        assert_eq!(
            names(&events),
            ["preview-started", "preview-started", "preview-ready"]
        );
        assert_eq!(only_ready(&events[1..]).version, 2);
        assert_eq!(
            folder_names(&image_folder(session)),
            ["original.png", "v2.png"]
        );
    }

    #[test]
    fn ten_requests_in_a_row_leave_one_version_file_and_no_running_worker() {
        let (_dir, session, _) = loaded("a.png", &gradient_png());
        let gate = Gate::new();
        let worker = &gate.worker();
        let heard = &Heard::default();
        let session = &session;
        std::thread::scope(|scope| {
            let running: Vec<_> = (0..10u8)
                .map(|n| {
                    let job = request(session, &lossy(40 + n), heard);
                    scope.spawn(move || session.run_preview(job, worker, &mut heard.emit()))
                })
                .collect();
            gate.open();
            for job in running {
                job.join().unwrap();
            }
        });
        let events = heard.take();
        let started = names(&events)
            .iter()
            .filter(|name| **name == "preview-started")
            .count();
        assert_eq!(started, 10);
        let ready: Vec<u64> = events
            .iter()
            .filter_map(|event| match event {
                Event::PreviewReady(ready) => Some(ready.version),
                _ => None,
            })
            .collect();
        assert_eq!(ready, [10], "only the last result is sent");
        assert!(!names(&events).contains(&"preview-failed"), "{events:?}");
        for pid in gate.pids() {
            assert!(!alive(&pid), "worker {pid} runs on");
        }
        assert_eq!(
            folder_names(&image_folder(session)),
            ["original.png", "v10.png"]
        );
    }
}
