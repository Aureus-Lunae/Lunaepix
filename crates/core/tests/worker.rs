//! The worker mode, run directly as a process: `lunaepix-worker
//! --lunaepix-worker <src> <dest>`, the way the runner starts it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use lunaepix_core::WORKER_FLAG;
use tempfile::TempDir;

const WORKER: &str = env!("CARGO_BIN_EXE_lunaepix-worker");

fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 32, 32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[200; 32 * 32]).unwrap();
    writer.finish().unwrap();
    out
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn serve(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(WORKER)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn exits_0_with_an_ok_record_when_the_file_is_optimised() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_bytes());
    let dest = tmp.path().join("optimized-a.png");

    let output = serve(&[WORKER_FLAG.as_ref(), src.as_os_str(), dest.as_os_str()]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let record = stdout(&output);
    assert!(record.starts_with("lunaepix-worker 2 ok "), "{record:?}");
    assert_eq!(record.lines().count(), 1, "{record:?}");
    let before = fs::metadata(&src).unwrap().len();
    let after = fs::metadata(&dest).unwrap().len();
    assert!(
        record.starts_with(&format!("lunaepix-worker 2 ok {before} {after} ")),
        "{record:?}"
    );
}

#[test]
fn exits_1_with_an_error_record_when_the_file_cannot_be_optimised() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", b"not a png at all");
    let dest = tmp.path().join("optimized-a.png");

    let output = serve(&[WORKER_FLAG.as_ref(), src.as_os_str(), dest.as_os_str()]);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let record = stdout(&output);
    assert!(
        record.starts_with("lunaepix-worker 2 invalid src "),
        "{record:?}"
    );
    assert!(!dest.exists());
}

#[test]
fn exits_1_with_an_io_record_when_the_source_is_missing() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("gone.png");
    let dest = tmp.path().join("optimized-gone.png");

    let output = serve(&[WORKER_FLAG.as_ref(), src.as_os_str(), dest.as_os_str()]);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let record = stdout(&output);
    assert!(
        record.starts_with("lunaepix-worker 2 io src "),
        "{record:?}"
    );
}

#[test]
fn refuses_a_call_without_the_flag_or_with_the_wrong_arguments_and_prints_no_record() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &png_bytes());
    let dest = tmp.path().join("optimized-a.png");
    let flag: &std::ffi::OsStr = WORKER_FLAG.as_ref();

    let calls: [&[&std::ffi::OsStr]; 4] = [
        &[],
        &[src.as_os_str(), dest.as_os_str()],
        &[flag, src.as_os_str()],
        &[flag, src.as_os_str(), dest.as_os_str(), dest.as_os_str()],
    ];
    for args in calls {
        let output = serve(args);

        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!dest.exists(), "{args:?}");
    }
}

/// A 64x64 RGBA gradient with noise: thousands of colours, so a lossless run
/// keeps it a true-colour PNG and only a lossy run makes it indexed.
fn gradient_png() -> Vec<u8> {
    let (w, h) = (64u32, 64u32);
    let mut pixels = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let noise = ((x * 31 + y * 17) % 13) as u8;
            pixels.extend_from_slice(&[
                (x * 4) as u8 ^ noise,
                (y * 4) as u8,
                ((x + y) * 2) as u8 ^ noise,
                255,
            ]);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

/// The colour type in a PNG's IHDR: 3 is indexed.
fn colour_type(png: &[u8]) -> u8 {
    assert_eq!(&png[12..16], b"IHDR");
    png[25]
}

#[test]
fn the_worker_accepts_lossy_0_75_and_100() {
    let tmp = TempDir::new().unwrap();
    let original = gradient_png();
    let src = write(tmp.path(), "a.png", &original);
    // The control: the same file in a lossless run stays true colour.
    let lossless = tmp.path().join("optimized/a.png");
    let output = serve(&[WORKER_FLAG.as_ref(), src.as_os_str(), lossless.as_os_str()]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_ne!(colour_type(&fs::read(&lossless).unwrap()), 3);

    for quality in ["0", "75", "100"] {
        let dest = tmp.path().join(format!("optimized-{quality}.png"));

        let output = serve(&[
            WORKER_FLAG.as_ref(),
            "--lossy".as_ref(),
            quality.as_ref(),
            src.as_os_str(),
            dest.as_os_str(),
        ]);

        assert_eq!(output.status.code(), Some(0), "{quality}: {output:?}");
        let record = stdout(&output);
        let after = fs::metadata(&dest).unwrap().len();
        assert!(
            record.starts_with(&format!("lunaepix-worker 2 ok {} {after} ", original.len())),
            "{quality}: {record:?}"
        );
        assert!(record.ends_with(" -\n"), "{quality}: {record:?}");
        let written = fs::read(&dest).unwrap();
        assert_eq!(
            colour_type(&written),
            3,
            "{quality}: lossy output is indexed"
        );
        let decoded = image::load_from_memory(&written).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 64), "{quality}");
    }
    assert_eq!(fs::read(&src).unwrap(), original);
}

#[test]
fn the_worker_refuses_a_missing_quality_minus_1_101_75_5_abc_an_unknown_option_and_a_repeat() {
    let tmp = TempDir::new().unwrap();
    let src = write(tmp.path(), "a.png", &gradient_png());
    let dest = tmp.path().join("optimized-a.png");

    let calls: [&[&str]; 9] = [
        &["--lossy"],
        &["--lossy", "-1"],
        &["--lossy", "101"],
        &["--lossy", "75.5"],
        &["--lossy", "abc"],
        &["--fast"],
        &["--lossy", "75", "--lossy", "75"],
        &["--no-replace", "--no-replace"],
        &["--no-replace", "--lossy", "75"],
    ];
    for flags in calls {
        let mut args: Vec<&std::ffi::OsStr> = vec![WORKER_FLAG.as_ref()];
        args.extend(flags.iter().map(std::ffi::OsStr::new));
        args.extend([src.as_os_str(), dest.as_os_str()]);

        let output = serve(&args);

        assert_eq!(output.status.code(), Some(2), "{flags:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{flags:?}");
        assert!(!dest.exists(), "{flags:?}");
    }
    assert_eq!(listing_names(tmp.path()), ["a.png"]);
}

fn listing_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Start `program`, which blocks opening the FIFO `fifo`, and read its OOM
/// score, waiting up to `patience` for it to become 1000; then stop it.
#[cfg(target_os = "linux")]
fn oom_score_of_a_blocked(
    program: &str,
    args: &[&std::ffi::OsStr],
    fifo: &Path,
    patience: Duration,
) -> String {
    use std::os::unix::fs::OpenOptionsExt;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let score_file = format!("/proc/{}/oom_score_adj", child.id());
    // The worker sets its score at start-up, before it opens the source.
    let deadline = Instant::now() + patience;
    let mut score = fs::read_to_string(&score_file).unwrap();
    while score.trim() != "1000" && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        score = fs::read_to_string(&score_file).unwrap();
    }
    // Opening the FIFO for writing releases a reader blocked on it.
    // O_NONBLOCK: do not wait for a reader if the child is already gone.
    let _ = fs::OpenOptions::new()
        .write(true)
        .custom_flags(0o4000)
        .open(fifo);
    child.kill().ok();
    child.wait().unwrap();
    score.trim().to_owned()
}

#[cfg(target_os = "linux")]
fn fifo(dir: &Path) -> PathBuf {
    let path = dir.join("slow.png");
    let status = Command::new("mkfifo").arg(&path).status().unwrap();
    assert!(status.success());
    path
}

#[cfg(target_os = "linux")]
#[test]
fn raises_its_own_oom_score_so_the_kernel_kills_it_before_the_window() {
    let tmp = TempDir::new().unwrap();
    let src = fifo(tmp.path());
    let dest = tmp.path().join("optimized-slow.png");

    let score = oom_score_of_a_blocked(
        WORKER,
        &[WORKER_FLAG.as_ref(), src.as_os_str(), dest.as_os_str()],
        &src,
        Duration::from_secs(5),
    );

    assert_eq!(score, "1000");
    // The parent, which stands in for the window, keeps its own score.
    let own = fs::read_to_string("/proc/self/oom_score_adj").unwrap();
    assert_ne!(own.trim(), "1000");
}

#[cfg(target_os = "linux")]
#[test]
fn a_child_that_is_not_the_worker_keeps_the_inherited_oom_score() {
    // The proof that the score above comes from the worker, not from the way
    // the test starts processes: `cat` blocked on the same kind of FIFO.
    let tmp = TempDir::new().unwrap();
    let src = fifo(tmp.path());

    let score = oom_score_of_a_blocked("cat", &[src.as_os_str()], &src, Duration::from_millis(300));

    assert_ne!(score, "1000");
    let own = fs::read_to_string("/proc/self/oom_score_adj").unwrap();
    assert_eq!(score, own.trim());
}

/// Whether process `pid` still runs: it exists and is not a zombie.
#[cfg(target_os = "linux")]
fn is_running(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state is the first field after the parenthesised name.
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with('Z')),
        Err(_) => false,
    }
}

/// Start `program args...` in the background of a shell that then waits on
/// its stdin, wait for `ready(pid)`, end the shell, and report whether the
/// program still runs `grace` later. The program is stopped either way.
#[cfg(target_os = "linux")]
fn outlives_its_parent(
    program: &str,
    args: &[&std::ffi::OsStr],
    ready: impl Fn(&str) -> bool,
    grace: Duration,
) -> bool {
    use std::io::{BufRead, BufReader};

    // `read` ends when the test closes the shell's stdin.
    let mut shell = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#""$0" "$@" </dev/null >/dev/null 2>&1 & echo $!; read _"#)
        .arg(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid = line.trim().to_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(shell.stdin.take());
    shell.wait().unwrap();
    let deadline = Instant::now() + grace;
    while is_running(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let survived = is_running(&pid);
    if survived {
        // Started by this test (through the shell): stop exactly that PID.
        let _ = Command::new("kill").arg("-9").arg(&pid).status();
    }
    survived
}

#[cfg(target_os = "linux")]
#[test]
fn dies_when_the_process_that_started_it_exits() {
    // Stands in for the window closing or crashing mid-file: the worker,
    // blocked on its source, must not carry on alone.
    let tmp = TempDir::new().unwrap();
    let src = fifo(tmp.path());
    let dest = tmp.path().join("optimized-slow.png");
    // The worker asks for the signal before it sets its OOM score, so a
    // score of 1000 says it is ready.
    let ready = |pid: &str| {
        fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))
            .is_ok_and(|score| score.trim() == "1000")
    };

    let survived = outlives_its_parent(
        WORKER,
        &[WORKER_FLAG.as_ref(), src.as_os_str(), dest.as_os_str()],
        ready,
        Duration::from_secs(5),
    );

    assert!(!survived, "the worker kept running after its parent exited");
    assert!(!dest.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn a_child_that_is_not_the_worker_outlives_the_process_that_started_it() {
    // The proof that the test above can see a survivor: `cat`, blocked on the
    // same kind of FIFO, keeps running after the same shell exits.
    let tmp = TempDir::new().unwrap();
    let src = fifo(tmp.path());

    let survived = outlives_its_parent(
        "cat",
        &[src.as_os_str()],
        is_running,
        Duration::from_millis(500),
    );

    assert!(survived, "cat should outlive the shell");
}
