//! The batch runner, through the public API only. Every optimised file goes
//! through a real worker process: the `lunaepix-worker` binary of this crate,
//! or a deliberately misbehaving program in its place. Fixtures are made here.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
// Only the tests that run a fake worker script (a shell script, so unix only)
// use these; on Windows they would be unused imports.
#[cfg(unix)]
use std::time::{Duration, Instant};

use lunaepix_core::{
    Compression, FileResult, Job, Note, OptimizeError, Options, Outcome, Quality, RunError, Skip,
    StopHandle, WORKER_FLAG, WORKER_TIMEOUT, Worker, WriteMode, plan_files, plan_folder, run,
    run_observed, run_until_stopped,
};
#[cfg(unix)]
use lunaepix_core::{Crash, MAX_WORKER_OUTPUT_BYTES};
use tempfile::TempDir;

// --- fixtures ---------------------------------------------------------------

fn worker() -> Worker {
    Worker::new(env!("CARGO_BIN_EXE_lunaepix-worker"))
}

/// A PNG with something to gain: a flat image, stored with fast compression.
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

/// A baseline JPEG with the standard Huffman tables.
fn jpg_bytes() -> Vec<u8> {
    let (w, h) = (64, 48);
    let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode(&pixels, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn svg_bytes() -> Vec<u8> {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
  <!-- a comment to remove -->
  <rect x="0.000" y="0.000" width="10.000" height="10.000" fill="#ff0000"/>
</svg>
"##
    .to_vec()
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

fn names(list: &[&str]) -> Vec<OsString> {
    let mut names: Vec<OsString> = list.iter().map(OsString::from).collect();
    names.sort();
    names
}

fn optimized(result: &FileResult) -> Outcome {
    match result {
        FileResult::Optimized(outcome) => *outcome,
        other => panic!("expected an optimised file, got {other:?}"),
    }
}

fn failure(result: &FileResult) -> &RunError {
    match result {
        FileResult::Failed(error) => error,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[cfg(unix)]
fn crash(result: &FileResult) -> Crash {
    match failure(result) {
        RunError::Crashed { cause, .. } => *cause,
        other => panic!("expected a crash, got {other:?}"),
    }
}

/// A shell script in the worker's place. It gets the source and the
/// destination as `$1` and `$2`, as the real worker would.
#[cfg(unix)]
fn script(body: &str, timeout: Duration) -> Worker {
    Worker {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), body.into(), "fake-worker".into()],
        timeout,
    }
}

/// One planned job for a file that exists, so only the worker decides.
fn one_png_job(dir: &Path) -> Vec<Job> {
    let src = write(dir, "a.png", &png_bytes());
    plan_files(&[src])
}

// --- order and isolation of failures ----------------------------------------

#[test]
fn gives_three_results_in_input_order_for_three_files() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "c.png", &png_bytes()),
        write(tmp.path(), "a.jpg", &jpg_bytes()),
        write(tmp.path(), "b.svg", &svg_bytes()),
    ];
    let jobs = plan_files(&srcs);

    let results = run(&jobs, &worker(), |_, _, _| {});

    assert_eq!(results.len(), 3);
    for (src, result) in srcs.iter().zip(&results) {
        let outcome = optimized(result);
        assert_eq!(outcome.before, fs::metadata(src).unwrap().len());
        let dest = tmp.path().join(format!(
            "optimized-{}",
            src.file_name().unwrap().to_str().unwrap()
        ));
        assert_eq!(outcome.after, fs::metadata(&dest).unwrap().len());
    }
    assert!(optimized(&results[0]).after < optimized(&results[0]).before);
}

#[test]
fn a_corrupt_file_in_the_middle_fails_and_the_other_two_still_succeed() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "a.png", &png_bytes()),
        write(tmp.path(), "b.png", b"this is not a PNG"),
        write(tmp.path(), "c.jpg", &jpg_bytes()),
    ];
    let jobs = plan_files(&srcs);

    let results = run(&jobs, &worker(), |_, _, _| {});

    assert_eq!(results.len(), 3);
    optimized(&results[0]);
    match failure(&results[1]) {
        RunError::Optimize(OptimizeError::Invalid { path, message }) => {
            assert_eq!(path, &srcs[1]);
            assert!(message.contains("not a PNG"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    optimized(&results[2]);
    assert_eq!(
        listing(tmp.path()),
        names(&[
            "a.png",
            "b.png",
            "c.jpg",
            "optimized-a.png",
            "optimized-c.jpg"
        ])
    );
}

// --- progress ---------------------------------------------------------------

/// What one progress call reported, in a comparable form.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Optimized,
    Skipped(Skip),
    Failed,
}

fn seen(result: &FileResult) -> Seen {
    match result {
        FileResult::Optimized(_) => Seen::Optimized,
        FileResult::Skipped(reason) => Seen::Skipped(*reason),
        FileResult::Failed(_) => Seen::Failed,
    }
}

#[test]
fn reports_progress_once_per_file_with_its_index_source_and_result() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "a.png", &png_bytes()),
        write(tmp.path(), "b.txt", b"notes"),
        write(tmp.path(), "c.png", b"broken"),
    ];
    let jobs = plan_files(&srcs);
    let mut calls = Vec::new();

    let results = run(&jobs, &worker(), |index, src, result| {
        calls.push((index, src.to_path_buf(), seen(result)));
    });

    assert_eq!(
        calls,
        vec![
            (0, srcs[0].clone(), Seen::Optimized),
            (1, srcs[1].clone(), Seen::Skipped(Skip::UnsupportedType)),
            (2, srcs[2].clone(), Seen::Failed),
        ]
    );
    let returned: Vec<Seen> = results.iter().map(seen).collect();
    let reported: Vec<Seen> = calls.into_iter().map(|(_, _, s)| s).collect();
    assert_eq!(returned, reported);
}

#[test]
fn an_empty_job_list_reports_nothing_and_creates_no_output_folder() {
    let tmp = TempDir::new().unwrap();
    // A folder with nothing to optimise plans to no jobs.
    let jobs = plan_folder(tmp.path());
    assert!(jobs.is_empty());
    let mut calls = 0;

    let results = run(&jobs, &worker(), |_, _, _| calls += 1);

    assert!(results.is_empty());
    assert_eq!(calls, 0);
    assert!(!tmp.path().join("optimized").exists());
    assert!(listing(tmp.path()).is_empty());
}

#[test]
fn a_folder_run_writes_into_its_optimized_folder() {
    // The positive counterpart of the test above: with a file to optimise,
    // the same folder run does create `optimized/`.
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.png", &png_bytes());
    let jobs = plan_folder(tmp.path());

    let results = run(&jobs, &worker(), |_, _, _| {});

    optimized(&results[0]);
    assert!(tmp.path().join("optimized").join("a.png").is_file());
}

// --- cancelled dialogs and skips ---------------------------------------------

#[test]
fn a_cancelled_dialog_does_nothing() {
    let jobs = plan_files(&[]);
    let mut calls = 0;

    let results = run(&jobs, &worker(), |_, _, _| calls += 1);

    assert!(results.is_empty());
    assert_eq!(calls, 0);
}

#[cfg(unix)]
#[test]
fn a_worker_that_panicked_and_exited_101_counts_as_crashed() {
    // 101 is what the worker exits with when the optimiser panics: no
    // record, the panic message on stderr.
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let body = "echo \"thread 'optimize' panicked at src/svg.rs\" >&2; exit 101";

    let results = run(&jobs, &script(body, WORKER_TIMEOUT), |_, _, _| {});

    assert_eq!(crash(&results[0]), Crash::ExitCode(101));
    let reason = failure(&results[0]).reason();
    assert_eq!(reason, "the optimiser crashed on this file (exit code 101)");
    // The panic text stays out of the reason the window shows.
    assert!(!reason.contains("panicked"), "{reason}");
    assert!(!tmp.path().join("optimized-a.png").exists());
}

// --- stopping a run (the window closes) ----------------------------------------

/// Wait up to five seconds for `path` to hold `lines` lines.
#[cfg(target_os = "linux")]
fn wait_for_lines(path: &Path, lines: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(path).map_or(0, |text| text.lines().count()) < lines
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn stopping_a_run_kills_and_reaps_its_worker_and_starts_no_further_file() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "a.png", &png_bytes()),
        write(tmp.path(), "b.png", &png_bytes()),
    ];
    let jobs = plan_files(&srcs);
    let pids = tmp.path().join("pids");
    // Every worker started appends its PID; `exec` keeps the PID.
    let body = format!("echo $$ >> '{}'; exec sleep 60", pids.display());
    let worker = script(&body, WORKER_TIMEOUT);
    let stop = StopHandle::new();

    let started = Instant::now();
    let results = std::thread::scope(|scope| {
        let running = scope.spawn(|| run_until_stopped(&jobs, &worker, &stop, |_, _, _| {}));
        wait_for_lines(&pids, 1);
        // What the window's close handler does.
        stop.stop();
        running.join().unwrap()
    });

    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
    assert!(stop.is_stopped());
    let started_pids = fs::read_to_string(&pids).unwrap();
    assert_eq!(started_pids.lines().count(), 1, "b.png must not start");
    let pid = started_pids.trim();
    assert!(
        !Path::new("/proc").join(pid).exists(),
        "worker {pid} still exists"
    );
    assert_eq!(results.len(), 1, "{results:?}");
    match failure(&results[0]) {
        RunError::Stopped { path } => assert_eq!(path, &srcs[0]),
        other => panic!("{other:?}"),
    }
    assert_eq!(failure(&results[0]).reason(), "the run was stopped");
    assert!(!tmp.path().join("optimized-a.png").exists());
}

#[test]
fn a_run_whose_stop_handle_is_never_used_runs_every_file() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "a.png", &png_bytes()),
        write(tmp.path(), "b.svg", &svg_bytes()),
    ];
    let jobs = plan_files(&srcs);
    let stop = StopHandle::new();

    let results = run_until_stopped(&jobs, &worker(), &stop, |_, _, _| {});

    assert_eq!(results.len(), 2);
    optimized(&results[0]);
    optimized(&results[1]);
    assert!(!stop.is_stopped());
}

#[test]
fn a_run_stopped_before_it_starts_runs_nothing_and_reports_nothing() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let stop = StopHandle::new();
    stop.stop();
    let mut reported = 0;

    let results = run_until_stopped(&jobs, &worker(), &stop, |_, _, _| reported += 1);

    assert!(results.is_empty(), "{results:?}");
    assert_eq!(reported, 0);
    assert!(!tmp.path().join("optimized-a.png").exists());
}

#[test]
fn a_clone_of_a_stop_handle_stops_the_same_run() {
    let stop = StopHandle::new();
    let clone = stop.clone();
    assert!(!stop.is_stopped());
    clone.stop();
    assert!(stop.is_stopped());
    // Stopping twice is harmless.
    stop.stop();
    assert!(clone.is_stopped());
}

/// A worker that cannot be started: any attempt to spawn it fails.
fn missing_worker(tmp: &TempDir) -> Worker {
    Worker::new(tmp.path().join("no-such-worker"))
}

#[test]
fn a_skipped_file_gets_no_output_and_starts_no_worker() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "notes.txt", b"hello"),
        tmp.path().join("missing.png"),
    ];
    let jobs = plan_files(&srcs);
    let before = listing(tmp.path());

    let results = run(&jobs, &missing_worker(&tmp), |_, _, _| {});

    // Had a worker been started, its spawn would have failed.
    assert_eq!(results.len(), 2);
    assert!(matches!(
        results[0],
        FileResult::Skipped(Skip::UnsupportedType)
    ));
    assert!(matches!(results[1], FileResult::Skipped(Skip::NotAFile)));
    assert_eq!(listing(tmp.path()), before);
}

#[test]
fn a_file_to_optimise_does_start_the_worker() {
    // The proof that the missing worker above would have been noticed.
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());

    let results = run(&jobs, &missing_worker(&tmp), |_, _, _| {});

    match failure(&results[0]) {
        RunError::Spawn { path, .. } => assert_eq!(path, &tmp.path().join("a.png")),
        other => panic!("{other:?}"),
    }
    let message = failure(&results[0]).to_string();
    assert!(message.contains("could not be started"), "{message}");
    assert!(!tmp.path().join("optimized-a.png").exists());
}

// --- the worker's result record ----------------------------------------------

#[cfg(unix)]
#[test]
fn takes_a_well_formed_record_from_any_worker_exactly() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let fake = script(
        "printf 'lunaepix-worker 2 ok 4000 3000 12 -\\n'",
        WORKER_TIMEOUT,
    );

    let results = run(&jobs, &fake, |_, _, _| {});

    assert_eq!(
        optimized(&results[0]),
        Outcome {
            before: 4000,
            after: 3000,
            millis: 12,
            note: None,
        }
    );
}

#[cfg(unix)]
#[test]
fn takes_a_well_formed_error_record_and_names_the_planned_path() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let fake = script(
        "printf 'lunaepix-worker 2 invalid dest no room\\\\nat all\\n'; exit 1",
        WORKER_TIMEOUT,
    );

    let results = run(&jobs, &fake, |_, _, _| {});

    match failure(&results[0]) {
        RunError::Optimize(OptimizeError::Invalid { path, message }) => {
            assert_eq!(path, &tmp.path().join("optimized-a.png"));
            assert_eq!(message, "no room\nat all");
        }
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_worker_that_prints_garbage_and_exits_0_counts_as_crashed() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let fake = script("echo 'all good, trust me'", WORKER_TIMEOUT);

    let results = run(&jobs, &fake, |_, _, _| {});

    assert_eq!(crash(&results[0]), Crash::NoValidResult);
    let message = failure(&results[0]).to_string();
    assert!(
        message.contains("the optimiser crashed on this file"),
        "{message}"
    );
}

#[cfg(unix)]
#[test]
fn a_record_that_disagrees_with_the_exit_code_counts_as_crashed() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let ok_but_failed = script(
        "printf 'lunaepix-worker 2 ok 4000 3000 12 -\\n'; exit 1",
        WORKER_TIMEOUT,
    );
    let failed_but_ok = script(
        "printf 'lunaepix-worker 2 invalid src nope\\n'; exit 0",
        WORKER_TIMEOUT,
    );

    let first = run(&jobs, &ok_but_failed, |_, _, _| {});
    let second = run(&jobs, &failed_but_ok, |_, _, _| {});

    assert_eq!(crash(&first[0]), Crash::NoValidResult);
    assert_eq!(crash(&second[0]), Crash::NoValidResult);
}

#[cfg(unix)]
#[test]
fn a_worker_that_prints_far_more_than_the_cap_and_exits_0_counts_as_crashed() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    // 16 MiB, 256 times the cap.
    let fake = script("exec head -c 16777216 /dev/zero", WORKER_TIMEOUT);

    let results = run(&jobs, &fake, |_, _, _| {});

    assert_eq!(crash(&results[0]), Crash::OutputTooLarge);
}

#[cfg(unix)]
#[test]
fn a_worker_that_floods_stdout_or_stderr_forever_is_stopped_long_before_the_timeout() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    for body in ["exec cat /dev/zero", "exec cat /dev/zero >&2"] {
        let started = Instant::now();

        let results = run(&jobs, &script(body, Duration::from_secs(120)), |_, _, _| {});

        assert_eq!(crash(&results[0]), Crash::OutputTooLarge, "{body}");
        assert!(started.elapsed() < Duration::from_secs(30), "{body}");
    }
}

#[cfg(unix)]
#[test]
fn keeps_at_most_the_cap_of_a_crashed_workers_stderr() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    // Exactly the cap of stderr, then a crash: kept, but never more.
    let body = format!("head -c {MAX_WORKER_OUTPUT_BYTES} /dev/zero | tr '\\0' e >&2; exit 7");

    let results = run(&jobs, &script(&body, WORKER_TIMEOUT), |_, _, _| {});

    match failure(&results[0]) {
        RunError::Crashed { cause, stderr, .. } => {
            assert_eq!(*cause, Crash::ExitCode(7));
            assert!(stderr.len() <= MAX_WORKER_OUTPUT_BYTES, "{}", stderr.len());
            assert!(stderr.starts_with("eeee"));
        }
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn names_the_signal_that_killed_a_worker() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());

    let results = run(
        &jobs,
        &script("kill -SEGV $$", WORKER_TIMEOUT),
        |_, _, _| {},
    );

    assert_eq!(crash(&results[0]), Crash::Signal(11));
    let message = failure(&results[0]).to_string();
    assert!(message.contains("signal 11"), "{message}");
    assert!(message.contains("SIGSEGV"), "{message}");
}

// --- timeout -----------------------------------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn stops_and_reaps_a_worker_that_runs_past_its_timeout() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let pid_file = tmp.path().join("worker.pid");
    // `exec` keeps the PID: the process that sleeps is the one that was started.
    let body = format!("echo $$ > '{}'; exec sleep 60", pid_file.display());
    let limit = Duration::from_millis(500);
    let started = Instant::now();

    let results = run(&jobs, &script(&body, limit), |_, _, _| {});

    let elapsed = started.elapsed();
    match failure(&results[0]) {
        RunError::TimedOut { path, limit: got } => {
            assert_eq!(path, &tmp.path().join("a.png"));
            assert_eq!(*got, limit);
        }
        other => panic!("{other:?}"),
    }
    assert!(elapsed >= limit, "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
    let pid = fs::read_to_string(&pid_file).unwrap();
    // Gone from the process table: killed and reaped, not left a zombie.
    assert!(
        !Path::new("/proc").join(pid.trim()).exists(),
        "process {} still exists",
        pid.trim()
    );
}

#[cfg(unix)]
#[test]
fn a_slow_worker_within_its_timeout_is_not_stopped() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let body = "sleep 1; printf 'lunaepix-worker 2 ok 4000 3000 1000 -\\n'";

    let results = run(&jobs, &script(body, Duration::from_secs(30)), |_, _, _| {});

    assert_eq!(optimized(&results[0]).millis, 1000);
}

#[test]
fn a_normal_file_finishes_within_the_default_timeout() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let worker = worker();
    assert_eq!(worker.timeout, WORKER_TIMEOUT);

    let results = run(&jobs, &worker, |_, _, _| {});

    optimized(&results[0]);
}

#[test]
fn the_default_worker_is_this_binary_in_worker_mode() {
    let worker = Worker::current_exe().unwrap();
    assert_eq!(worker.program, std::env::current_exe().unwrap());
    assert_eq!(worker.args, vec![OsString::from(WORKER_FLAG)]);
    assert_eq!(worker.timeout, WORKER_TIMEOUT);
}

// --- paths ---------------------------------------------------------------------

#[test]
fn a_name_with_spaces_and_non_ascii_characters_reaches_the_right_output() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "my phöto – ✓ 日本.png", &png_bytes());
    let jobs = plan_files(&[src]);

    let results = run(&jobs, &worker(), |_, _, _| {});

    optimized(&results[0]);
    assert_eq!(
        listing(tmp.path()),
        names(&["my phöto – ✓ 日本.png", "optimized-my phöto – ✓ 日本.png"])
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_name_that_is_not_utf8_reaches_the_right_output() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let tmp = TempDir::new().unwrap();
    let name = OsStr::from_bytes(b"caf\xe9 \xff.png");
    let src = tmp.path().join(name);
    fs::write(&src, png_bytes()).unwrap();
    let jobs = plan_files(&[src]);

    let results = run(&jobs, &worker(), |_, _, _| {});

    optimized(&results[0]);
    let mut expected = vec![
        name.to_os_string(),
        OsStr::from_bytes(b"optimized-caf\xe9 \xff.png").to_os_string(),
    ];
    expected.sort();
    // Exactly these two: no output under a lossy (U+FFFD) spelling of the name.
    assert_eq!(listing(tmp.path()), expected);
}

// --- run options -----------------------------------------------------------------

fn lossy(quality: u8) -> Compression {
    Compression::Lossy(Quality::new(quality).unwrap())
}

fn options(compression: Compression, write: WriteMode) -> Options {
    Options { compression, write }
}

fn names_in_order(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn with_options_appends_lossy_and_no_replace_in_order() {
    let base = Worker::new("/opt/lunaepix-worker");
    let cases = [
        (
            options(lossy(75), WriteMode::Replace),
            vec![WORKER_FLAG, "--lossy", "75"],
        ),
        (
            options(lossy(0), WriteMode::NoReplace),
            vec![WORKER_FLAG, "--lossy", "0", "--no-replace"],
        ),
        (
            options(lossy(100), WriteMode::Replace),
            vec![WORKER_FLAG, "--lossy", "100"],
        ),
        (
            options(Compression::Lossless, WriteMode::NoReplace),
            vec![WORKER_FLAG, "--no-replace"],
        ),
    ];
    for (options, expected) in cases {
        let worker = base.with_options(&options);

        assert_eq!(worker.args, names_in_order(&expected), "{options:?}");
        assert_eq!(worker.program, base.program);
        assert_eq!(worker.timeout, base.timeout);
    }
    // The worker it was made from keeps its own arguments.
    assert_eq!(base.args, [OsString::from(WORKER_FLAG)]);
}

#[test]
fn with_options_for_a_lossless_replace_run_adds_nothing() {
    let base = Worker::new("/opt/lunaepix-worker");

    let worker = base.with_options(&options(Compression::Lossless, WriteMode::Replace));

    assert_eq!(worker, base);
}

/// A small lossless WebP: in a lossy run its lossy form is not smaller, so
/// the original is kept, with the note.
fn small_webp() -> Vec<u8> {
    let pixels: Vec<u8> = (0..32 * 32 * 4).map(|i| (i * 7 % 251) as u8).collect();
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(&pixels, 32, 32, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

#[test]
fn a_lossy_run_reports_the_note_the_worker_gives() {
    let tmp = TempDir::new().unwrap();
    let original = small_webp();
    let src = write(tmp.path(), "a.webp", &original);
    let jobs = plan_files(&[src]);
    let worker = worker().with_options(&options(lossy(75), WriteMode::Replace));

    let results = run(&jobs, &worker, |_, _, _| {});

    let outcome = optimized(&results[0]);
    assert_eq!(outcome.note, Some(Note::OriginalKept));
    assert_eq!(outcome.after, original.len() as u64);
    assert_eq!(
        fs::read(tmp.path().join("optimized-a.webp")).unwrap(),
        original
    );
}

#[test]
fn a_lossless_run_reports_no_note() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.webp", &small_webp());
    let jobs = plan_files(&[src]);
    let worker = worker().with_options(&options(Compression::Lossless, WriteMode::Replace));

    let results = run(&jobs, &worker, |_, _, _| {});

    assert_eq!(optimized(&results[0]).note, None);
}

#[cfg(unix)]
#[test]
fn takes_a_note_from_any_workers_record_exactly() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let fake = script(
        "printf 'lunaepix-worker 2 ok 4000 4000 12 apng\\n'",
        WORKER_TIMEOUT,
    );

    let results = run(&jobs, &fake, |_, _, _| {});

    assert_eq!(
        optimized(&results[0]),
        Outcome {
            before: 4000,
            after: 4000,
            millis: 12,
            note: Some(Note::KeptLosslessAnimatedPng),
        }
    );
}

#[cfg(unix)]
#[test]
fn a_record_of_version_1_counts_as_crashed() {
    let tmp = TempDir::new().unwrap();
    let jobs = one_png_job(tmp.path());
    let stale = script(
        "printf 'lunaepix-worker 1 ok 4000 3000 12\\n'",
        WORKER_TIMEOUT,
    );

    let results = run(&jobs, &stale, |_, _, _| {});

    assert_eq!(crash(&results[0]), Crash::NoValidResult);
}

// --- the on-start hook --------------------------------------------------------------

/// One call of a hook, in a comparable form. A start records whether the
/// job's output existed at that moment.
#[derive(Debug, PartialEq, Eq)]
enum Event {
    Start { index: usize, output_existed: bool },
    Progress { index: usize, seen: Seen },
}

/// Run `jobs` with both hooks, and give every call in order.
fn observed(jobs: &[Job], worker: &Worker) -> Vec<Event> {
    let events = std::cell::RefCell::new(Vec::new());
    run_observed(
        jobs,
        worker,
        &StopHandle::new(),
        |index| {
            let output_existed = match &jobs[index] {
                Job::Optimize { dest, .. } => dest.exists(),
                Job::Skip { .. } => panic!("on_start for a skip at {index}"),
            };
            events.borrow_mut().push(Event::Start {
                index,
                output_existed,
            });
        },
        |index, _, result| {
            events.borrow_mut().push(Event::Progress {
                index,
                seen: seen(result),
            });
        },
    );
    events.into_inner()
}

#[test]
fn on_start_fires_once_per_optimize_job_before_its_progress() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "a.png", &png_bytes()),
        write(tmp.path(), "b.txt", b"notes"),
        write(tmp.path(), "c.svg", &svg_bytes()),
    ];
    let jobs = plan_files(&srcs);

    let events = observed(&jobs, &worker());

    assert_eq!(
        events,
        [
            Event::Start {
                index: 0,
                output_existed: false
            },
            Event::Progress {
                index: 0,
                seen: Seen::Optimized
            },
            Event::Progress {
                index: 1,
                seen: Seen::Skipped(Skip::UnsupportedType)
            },
            Event::Start {
                index: 2,
                output_existed: false
            },
            Event::Progress {
                index: 2,
                seen: Seen::Optimized
            },
        ]
    );
    assert!(tmp.path().join("optimized-a.png").exists());
    assert!(tmp.path().join("optimized-c.svg").exists());
}

#[test]
fn on_start_never_fires_for_a_skip() {
    let tmp = TempDir::new().unwrap();
    let srcs = [
        write(tmp.path(), "notes.txt", b"hello"),
        tmp.path().join("missing.png"),
    ];
    let jobs = plan_files(&srcs);

    let events = observed(&jobs, &missing_worker(&tmp));

    assert_eq!(
        events,
        [
            Event::Progress {
                index: 0,
                seen: Seen::Skipped(Skip::UnsupportedType)
            },
            Event::Progress {
                index: 1,
                seen: Seen::Skipped(Skip::NotAFile)
            },
        ]
    );
}
