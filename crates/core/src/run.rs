//! The batch runner: optimise every planned job, one file at a time, each in
//! its own worker process (see [`crate::worker`]).
//!
//! A worker that crashes, is killed (for example by the kernel when memory
//! runs out), hangs past its timeout, or prints anything but one valid record
//! fails only its own file; the run carries on with the next one.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::worker::{WORKER_FLAG, parse_record};
use crate::{Job, MAX_WORKER_OUTPUT_BYTES, OptimizeError, Outcome, Skip, WORKER_TIMEOUT};

/// Longest pause between two checks on a running worker. The pause starts at
/// 1 ms and doubles, so a quick file costs a millisecond or two of waiting.
const MAX_POLL: Duration = Duration::from_millis(50);

/// How long the runner waits, once a worker has exited, for its output pipes
/// to reach end-of-file. Only a process the worker itself started could hold
/// them open longer; the real worker starts none.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// The program the runner starts for each file, as
/// `<program> <args...> <src> <dest>`.
///
/// The CLI uses [`Worker::current_exe`]: its own binary, in worker mode. A
/// release app uses the `lunaepix-worker` sidecar beside it, and falls back
/// to its own binary. Tests put another program in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub program: PathBuf,
    /// The arguments before the source and the destination.
    pub args: Vec<OsString>,
    /// How long one file may take before its worker is stopped.
    pub timeout: Duration,
}

impl Worker {
    /// `program` in worker mode ([`WORKER_FLAG`]), with [`WORKER_TIMEOUT`].
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Worker {
            program: program.into(),
            args: vec![WORKER_FLAG.into()],
            timeout: WORKER_TIMEOUT,
        }
    }

    /// The running binary in worker mode. Its `main` must call
    /// [`crate::worker::serve_if_requested`] first.
    pub fn current_exe() -> io::Result<Self> {
        Ok(Self::new(std::env::current_exe()?))
    }
}

/// What happened to one job of a run.
#[derive(Debug)]
pub enum FileResult {
    /// The file was optimised and the output written.
    Optimized(Outcome),
    /// The planner left the file alone; no worker was started.
    Skipped(Skip),
    /// The file was meant to be optimised, but was not. No output was written.
    Failed(RunError),
}

/// Why a file that was meant to be optimised was not. The `Display` text is
/// shown to the user.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The optimiser refused the file or could not read or write it: the same
    /// error `optimize_file` gives, passed back by the worker.
    #[error(transparent)]
    Optimize(#[from] OptimizeError),
    /// The worker ended without a valid result: it crashed, was killed, or
    /// printed something else. `stderr` holds the start of what it wrote
    /// there (at most [`MAX_WORKER_OUTPUT_BYTES`], lossily decoded), for a
    /// log or a details view.
    #[error("{path}: the optimiser crashed on this file ({cause})", path = path.display())]
    Crashed {
        path: PathBuf,
        cause: Crash,
        stderr: String,
    },
    /// The worker ran longer than `limit` and was stopped.
    #[error("{path}: the optimiser timed out after {limit:?} and was stopped", path = path.display())]
    TimedOut { path: PathBuf, limit: Duration },
    /// The worker program could not be started.
    #[error("{path}: the optimiser could not be started: {cause}", path = path.display())]
    Spawn { path: PathBuf, cause: io::Error },
    /// The run was stopped (the window closed) while this file was being
    /// optimised; its worker was killed. No output was written.
    #[error("{path}: the run was stopped", path = path.display())]
    Stopped { path: PathBuf },
}

impl RunError {
    /// The reason alone, without the leading `"<path>: "` that `Display`
    /// adds, for a caller that already shows the path. `Display` is always
    /// `"<path>: <reason>"`, where the path is the one in the error (for an
    /// optimiser error that can be the output file rather than the source).
    pub fn reason(&self) -> String {
        match self {
            RunError::Optimize(error) => error.reason(),
            RunError::Crashed { cause, .. } => {
                format!("the optimiser crashed on this file ({cause})")
            }
            RunError::TimedOut { limit, .. } => {
                format!("the optimiser timed out after {limit:?} and was stopped")
            }
            RunError::Spawn { cause, .. } => {
                format!("the optimiser could not be started: {cause}")
            }
            RunError::Stopped { .. } => "the run was stopped".to_owned(),
        }
    }
}

/// How a worker ended without a valid result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crash {
    /// Killed by this signal (unix only). A stack overflow aborts
    /// (`SIGABRT`); the kernel's out-of-memory killer sends `SIGKILL`.
    Signal(i32),
    /// Exited with a code other than the two the worker uses (on Windows a
    /// crash shows up here too, as an NTSTATUS such as `0xC00000FD`).
    ExitCode(i32),
    /// Exited normally, but stdout was not exactly one valid record that
    /// agrees with the exit code.
    NoValidResult,
    /// Wrote more than [`MAX_WORKER_OUTPUT_BYTES`] to stdout or to stderr,
    /// and was stopped.
    OutputTooLarge,
}

impl fmt::Display for Crash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Crash::Signal(signal) => {
                write!(f, "killed by signal {signal}")?;
                match signal_name(signal) {
                    Some(name) => write!(f, ", {name}"),
                    None => Ok(()),
                }
            }
            Crash::ExitCode(code) if cfg!(windows) && code < 0 => {
                write!(f, "exit code {:#010X}", code as u32)
            }
            Crash::ExitCode(code) => write!(f, "exit code {code}"),
            Crash::NoValidResult => f.write_str("it gave no valid result"),
            Crash::OutputTooLarge => f.write_str("it wrote too much output"),
        }
    }
}

/// The usual name of a signal that ends a crashing process, with a hint.
fn signal_name(signal: i32) -> Option<&'static str> {
    // The same numbers on Linux and macOS.
    Some(match signal {
        4 => "SIGILL",
        6 => "SIGABRT, for example a stack overflow",
        8 => "SIGFPE",
        9 => "SIGKILL, for example out of memory",
        11 => "SIGSEGV",
        _ => return None,
    })
}

/// Run every job, one at a time, in input order, on the calling thread (the
/// caller chooses which thread that is).
///
/// - Returns one result per job, in the order of `jobs`.
/// - A [`Job::Skip`] gives [`FileResult::Skipped`] without starting anything.
/// - A [`Job::Optimize`] starts `worker` once and waits for it; whatever the
///   worker does, the run goes on with the next job.
/// - `on_progress(index, src, result)` is called once per job, right after
///   it finishes.
///
/// An empty job list does nothing at all: no call, no folder.
pub fn run(
    jobs: &[Job],
    worker: &Worker,
    on_progress: impl FnMut(usize, &Path, &FileResult),
) -> Vec<FileResult> {
    run_until_stopped(jobs, worker, &StopHandle::new(), on_progress)
}

/// [`run`], until `stop` is used. From then on no worker is started, and no
/// further job is reported: the run returns at once with the results so
/// far, the last being [`RunError::Stopped`] for the file whose worker was
/// killed (if one was running).
pub fn run_until_stopped(
    jobs: &[Job],
    worker: &Worker,
    stop: &StopHandle,
    mut on_progress: impl FnMut(usize, &Path, &FileResult),
) -> Vec<FileResult> {
    let mut results = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        if stop.is_stopped() {
            break;
        }
        let (src, result) = match job {
            Job::Skip { src, reason } => (src, FileResult::Skipped(*reason)),
            Job::Optimize { src, dest } => (
                src,
                match run_worker(worker, src, dest, stop) {
                    Ok(outcome) => FileResult::Optimized(outcome),
                    Err(error) => FileResult::Failed(error),
                },
            ),
        };
        on_progress(index, src, &result);
        results.push(result);
    }
    results
}

/// Stops a run from another thread: the window's close handler uses it.
/// Clones share one state.
#[derive(Debug, Clone, Default)]
pub struct StopHandle(Arc<StopState>);

#[derive(Debug, Default)]
struct StopState {
    stopped: AtomicBool,
    /// The worker running now, if any. Its runner and [`StopHandle::stop`]
    /// take turns on it; the lock is held only for a call on the handle.
    active: Mutex<Option<Child>>,
}

impl StopHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop the run: no worker starts after this, and the one running now is
    /// killed through its own handle and reaped before this returns. Harmless
    /// to call twice, or when nothing runs.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        if let Some(child) = self.active().as_mut() {
            kill_and_reap(child);
        }
    }

    /// Whether [`stop`](Self::stop) has been called.
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }

    fn active(&self) -> MutexGuard<'_, Option<Child>> {
        self.0.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `call` on the active worker; `None` if there is none.
    fn with_active<T>(&self, call: impl FnOnce(&mut Child) -> T) -> Option<T> {
        self.active().as_mut().map(call)
    }
}

/// Windows' `CREATE_NO_WINDOW` process creation flag.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// On Windows, the worker is a console program: without this flag, every
/// file would flash a console window when the app (a window program)
/// starts its worker. Elsewhere the command is left as it is.
fn without_console_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Optimise one file in a worker process.
///
/// The worker gets no shell and no stdin; the paths are separate arguments.
/// Its stdout and stderr are read on two threads, each up to
/// [`MAX_WORKER_OUTPUT_BYTES`]; if either goes past that, or the timeout
/// passes, the worker is killed through its own handle and reaped. While it
/// runs, the worker sits in `stop`, so that a stop from another thread can
/// kill it.
fn run_worker(
    worker: &Worker,
    src: &Path,
    dest: &Path,
    stop: &StopHandle,
) -> Result<Outcome, RunError> {
    let spawn_error = |cause| RunError::Spawn {
        path: src.to_path_buf(),
        cause,
    };
    let stopped = || RunError::Stopped {
        path: src.to_path_buf(),
    };
    let (pipes, overflow) = {
        // The flag is checked, and the worker started and stored, under the
        // lock that `stop` takes after setting the flag: either `stop` sees
        // the worker, or the worker is never started.
        let mut active = stop.active();
        if stop.is_stopped() {
            return Err(stopped());
        }
        let mut command = Command::new(&worker.program);
        command
            .args(&worker.args)
            .arg(src)
            .arg(dest)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = without_console_window(&mut command)
            .spawn()
            .map_err(spawn_error)?;
        let overflow = Arc::new(AtomicBool::new(false));
        let pipes = match (child.stdout.take(), child.stderr.take()) {
            (Some(stdout), Some(stderr)) => read_capped(stdout, &overflow)
                .and_then(|out| Ok((out, read_capped(stderr, &overflow)?))),
            _ => Err(io::Error::other("the worker's output pipes are missing")),
        };
        if pipes.is_err() {
            kill_and_reap(&mut child);
        } else {
            *active = Some(child);
        }
        (pipes, overflow)
    };
    let (stdout, stderr) = pipes.map_err(spawn_error)?;

    let ended = wait(stop, worker.timeout, &overflow);
    // Reaped by now; the next file starts with an empty slot.
    stop.active().take();
    // A reader sets the overflow flag before it sends, so once both have
    // been received the flag is final.
    let stdout = stdout.recv_timeout(PIPE_GRACE).ok();
    let stderr = stderr.recv_timeout(PIPE_GRACE).unwrap_or_default();
    let crashed = |cause| RunError::Crashed {
        path: src.to_path_buf(),
        cause,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };

    match ended {
        Ended::Stopped => return Err(stopped()),
        Ended::TimedOut => {
            return Err(RunError::TimedOut {
                path: src.to_path_buf(),
                limit: worker.timeout,
            });
        }
        Ended::Exited(_) | Ended::Unknown => {}
    }
    // Checked before the exit status: a worker whose pipe was closed on it
    // may have died of that (`SIGPIPE`).
    if overflow.load(Ordering::SeqCst) {
        return Err(crashed(Crash::OutputTooLarge));
    }
    let Ended::Exited(status) = ended else {
        return Err(crashed(Crash::NoValidResult));
    };
    if let Some(signal) = signal_of(status) {
        return Err(crashed(Crash::Signal(signal)));
    }
    let record = stdout.and_then(|bytes| parse_record(&bytes, src, dest));
    match (status.code(), record) {
        (Some(0), Some(Ok(outcome))) => Ok(outcome),
        (Some(1), Some(Err(error))) => Err(RunError::Optimize(error)),
        (Some(0 | 1), _) => Err(crashed(Crash::NoValidResult)),
        (Some(code), _) => Err(crashed(Crash::ExitCode(code))),
        (None, _) => Err(crashed(Crash::NoValidResult)),
    }
}

/// How the wait for a worker ended.
#[derive(Debug)]
enum Ended {
    Exited(ExitStatus),
    /// Killed and reaped after the timeout.
    TimedOut,
    /// Killed and reaped by [`StopHandle::stop`].
    Stopped,
    /// Waiting failed, or the output overflowed (told apart by its flag);
    /// the worker was killed and reaped.
    Unknown,
}

/// Wait for the worker in `stop` to exit, polling, and kill it if it runs
/// past `timeout` or its output overflows. A stop from another thread has
/// killed and reaped it already, and wins over whatever status it left.
fn wait(stop: &StopHandle, timeout: Duration, overflow: &AtomicBool) -> Ended {
    wait_with(stop, timeout, overflow, Child::try_wait)
}

/// [`wait`], with the poll of the worker given: the tests use it to put a
/// stop between the flag check and the poll, which a real run reaches only
/// by a race.
fn wait_with(
    stop: &StopHandle,
    timeout: Duration,
    overflow: &AtomicBool,
    mut poll: impl FnMut(&mut Child) -> io::Result<Option<ExitStatus>>,
) -> Ended {
    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    loop {
        if stop.is_stopped() {
            // `stop()` has killed and reaped it already; this makes sure
            // that a stopped wait never leaves a worker behind.
            stop.with_active(kill_and_reap);
            return Ended::Stopped;
        }
        match stop.with_active(&mut poll) {
            // A stop that came after the check above killed the worker
            // itself: the status it left is that kill, not a crash.
            Some(Ok(Some(_))) if stop.is_stopped() => return Ended::Stopped,
            Some(Ok(Some(status))) => return Ended::Exited(status),
            Some(Ok(None)) => {}
            Some(Err(_)) | None => {
                stop.with_active(kill_and_reap);
                return Ended::Unknown;
            }
        }
        if overflow.load(Ordering::SeqCst) {
            stop.with_active(kill_and_reap);
            return Ended::Unknown;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            stop.with_active(kill_and_reap);
            return Ended::TimedOut;
        }
        thread::sleep(pause.min(timeout - elapsed));
        pause = (pause * 2).min(MAX_POLL);
    }
}

/// Kill this one child, through its handle, and reap it. Either step may
/// fail only because it has already exited, which is what is wanted.
fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Read `pipe` to its end on a new thread, keeping at most
/// [`MAX_WORKER_OUTPUT_BYTES`]. One byte more sets `overflow` and stops the
/// reading; closing the pipe then makes the worker's next write fail.
fn read_capped(
    pipe: impl Read + Send + 'static,
    overflow: &Arc<AtomicBool>,
) -> io::Result<mpsc::Receiver<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let overflow = Arc::clone(overflow);
    thread::Builder::new()
        .name("worker-output".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            // A read error ends the output early; whatever came is kept.
            let _ = pipe
                .take(MAX_WORKER_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut bytes);
            if bytes.len() > MAX_WORKER_OUTPUT_BYTES {
                bytes.truncate(MAX_WORKER_OUTPUT_BYTES);
                overflow.store(true, Ordering::SeqCst);
            }
            let _ = sender.send(bytes);
        })?;
    Ok(receiver)
}

#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: ExitStatus) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker's start and its wait, driven directly: the stop paths that
    /// a whole run can reach only through a timing race.
    #[cfg(unix)]
    mod stop_paths {
        use std::fs;
        use std::os::unix::process::ExitStatusExt;

        use tempfile::TempDir;

        use super::*;

        const LONG: Duration = Duration::from_secs(30);

        fn sh(script: &str) -> Command {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            command
        }

        /// A worker that writes its PID to `pid_file`, and nothing else.
        fn pid_worker(pid_file: &Path) -> Worker {
            let script = format!("echo $$ > '{}'", pid_file.display());
            Worker {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into(), "worker".into()],
                timeout: LONG,
            }
        }

        /// A worker whose program does not exist, so it can never start.
        fn missing_worker() -> Worker {
            Worker::new("/nonexistent/lunaepix-worker")
        }

        /// A handle with `script` running in its slot.
        fn running(script: &str) -> StopHandle {
            let stop = StopHandle::new();
            *stop.active() = Some(sh(script).spawn().unwrap());
            stop
        }

        /// Sets the flag only, as the first step of `stop()` does, without
        /// the kill that follows it.
        fn flag_only(stop: &StopHandle) {
            stop.0.stopped.store(true, Ordering::SeqCst);
        }

        /// The status of the child in the slot, once it has been reaped.
        fn reaped_status(stop: &StopHandle) -> Option<ExitStatus> {
            stop.with_active(|child| child.try_wait().unwrap())
                .flatten()
        }

        #[test]
        fn a_stopped_handle_does_not_even_try_to_start_the_worker() {
            let tmp = TempDir::new().unwrap();
            let stop = StopHandle::new();
            stop.stop();
            let (src, dest) = (tmp.path().join("a.png"), tmp.path().join("b.png"));

            // A program that cannot start: only the check under the lock can
            // turn this into `Stopped`. Without it the spawn is tried and
            // fails, which is `Spawn`, every time.
            let result = run_worker(&missing_worker(), &src, &dest, &stop);

            assert!(
                matches!(result, Err(RunError::Stopped { .. })),
                "{result:?}"
            );
        }

        #[test]
        fn a_fresh_handle_does_try_to_start_a_missing_worker() {
            let tmp = TempDir::new().unwrap();
            let (src, dest) = (tmp.path().join("a.png"), tmp.path().join("b.png"));

            let result = run_worker(&missing_worker(), &src, &dest, &StopHandle::new());

            assert!(matches!(result, Err(RunError::Spawn { .. })), "{result:?}");
        }

        #[test]
        fn a_fresh_handle_does_start_the_worker() {
            let tmp = TempDir::new().unwrap();
            let pid = tmp.path().join("pid");
            let (src, dest) = (tmp.path().join("a.png"), tmp.path().join("b.png"));

            let result = run_worker(&pid_worker(&pid), &src, &dest, &StopHandle::new());

            assert!(
                !matches!(result, Err(RunError::Stopped { .. })),
                "{result:?}"
            );
            let written = fs::read_to_string(&pid).expect("the worker ran");
            assert!(written.trim().parse::<u32>().is_ok(), "{written:?}");
        }

        #[test]
        fn a_stopped_wait_kills_and_reaps_a_worker_that_is_still_running() {
            let stop = running("exec sleep 30");
            flag_only(&stop);

            let ended = wait(&stop, LONG, &AtomicBool::new(false));

            assert!(matches!(ended, Ended::Stopped), "{ended:?}");
            let status = reaped_status(&stop).expect("killed and reaped");
            assert_eq!(status.signal(), Some(9));
        }

        #[test]
        fn a_wait_that_is_not_stopped_lets_the_worker_exit_on_its_own() {
            let stop = running("exit 0");

            let ended = wait(&stop, LONG, &AtomicBool::new(false));

            let Ended::Exited(status) = ended else {
                panic!("expected an exit, got {ended:?}");
            };
            assert_eq!((status.code(), status.signal()), (Some(0), None));
        }

        #[test]
        fn an_exit_seen_after_a_stop_counts_as_stopped() {
            let stop = running("exec sleep 30");

            // What `stop()` does in the race: it lands after the loop's
            // flag check and kills the worker before `try_wait` looks.
            let ended = wait_with(&stop, LONG, &AtomicBool::new(false), |child| {
                kill_and_reap(child);
                flag_only(&stop);
                child.try_wait()
            });

            assert!(matches!(ended, Ended::Stopped), "{ended:?}");
        }

        #[test]
        fn an_exit_seen_without_a_stop_counts_as_an_exit() {
            let stop = running("exec sleep 30");

            // The same kill without a stop, as the kernel's OOM killer would.
            let ended = wait_with(&stop, LONG, &AtomicBool::new(false), |child| {
                kill_and_reap(child);
                child.try_wait()
            });

            let Ended::Exited(status) = ended else {
                panic!("expected an exit, got {ended:?}");
            };
            assert_eq!(status.signal(), Some(9));
        }

        #[test]
        fn a_poll_error_kills_the_worker_and_ends_unknown() {
            let stop = running("exec sleep 30");

            let ended = wait_with(&stop, LONG, &AtomicBool::new(false), |_| {
                Err(io::Error::other("the poll failed"))
            });

            assert!(matches!(ended, Ended::Unknown), "{ended:?}");
            let status = reaped_status(&stop).expect("killed and reaped");
            assert_eq!(status.signal(), Some(9));
        }

        #[test]
        fn a_wait_with_no_worker_in_the_slot_ends_unknown() {
            let ended = wait(&StopHandle::new(), LONG, &AtomicBool::new(false));
            assert!(matches!(ended, Ended::Unknown), "{ended:?}");
        }
    }

    /// Only Windows CI runs this. std has no getter for the creation flags,
    /// so the test pins the constant to the Windows SDK's value and shows
    /// that a command with the flag still starts and hands back its output;
    /// whether a window flashes only the manual Windows test can show.
    #[cfg(windows)]
    #[test]
    fn the_worker_is_started_without_a_console_window() {
        assert_eq!(
            CREATE_NO_WINDOW,
            windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
        );
        let mut command = Command::new("cmd");
        command
            .args(["/C", "echo", "started"])
            .stdout(Stdio::piped());
        let output = without_console_window(&mut command).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("started"));
    }

    #[test]
    fn names_the_signals_a_crash_usually_ends_in() {
        assert_eq!(
            Crash::Signal(6).to_string(),
            "killed by signal 6, SIGABRT, for example a stack overflow"
        );
        assert_eq!(
            Crash::Signal(9).to_string(),
            "killed by signal 9, SIGKILL, for example out of memory"
        );
    }

    #[test]
    fn gives_only_the_number_of_a_signal_it_does_not_know() {
        assert_eq!(Crash::Signal(15).to_string(), "killed by signal 15");
    }

    fn run_errors() -> Vec<RunError> {
        vec![
            RunError::Optimize(OptimizeError::Invalid {
                path: PathBuf::from("dir/a.png"),
                message: "not a PNG".to_owned(),
            }),
            RunError::Crashed {
                path: PathBuf::from("dir/b.svg"),
                cause: Crash::Signal(6),
                stderr: "dir/b.svg overflowed".to_owned(),
            },
            RunError::TimedOut {
                path: PathBuf::from("dir/c.png"),
                limit: Duration::from_secs(600),
            },
            RunError::Spawn {
                path: PathBuf::from("dir/d.jpg"),
                cause: io::Error::other("no such program"),
            },
            RunError::Stopped {
                path: PathBuf::from("dir/e.svg"),
            },
        ]
    }

    #[test]
    fn a_run_error_gives_its_reason_without_the_path() {
        let reasons: Vec<String> = run_errors().iter().map(RunError::reason).collect();
        assert_eq!(
            reasons,
            [
                "not a PNG",
                "the optimiser crashed on this file (killed by signal 6, SIGABRT, for example a stack overflow)",
                "the optimiser timed out after 600s and was stopped",
                "the optimiser could not be started: no such program",
                "the run was stopped",
            ]
        );
    }

    #[test]
    fn a_run_errors_display_is_its_path_then_its_reason() {
        for error in run_errors() {
            let displayed = error.to_string();
            let reason = error.reason();
            assert!(displayed.ends_with(&format!(": {reason}")), "{displayed}");
            let path = displayed.strip_suffix(&format!(": {reason}")).unwrap();
            assert!(
                path.starts_with("dir/") && !path.contains(": "),
                "{displayed}"
            );
            // The reason alone does not carry the path, nor the worker's stderr.
            assert!(!reason.contains("dir/"), "{reason}");
        }
    }

    #[test]
    fn says_what_crashed_and_why_in_the_users_message() {
        let error = RunError::Crashed {
            path: PathBuf::from("logo.svg"),
            cause: Crash::NoValidResult,
            stderr: String::new(),
        };
        assert_eq!(
            error.to_string(),
            "logo.svg: the optimiser crashed on this file (it gave no valid result)"
        );
        let timed_out = RunError::TimedOut {
            path: PathBuf::from("big.png"),
            limit: Duration::from_secs(600),
        };
        assert_eq!(
            timed_out.to_string(),
            "big.png: the optimiser timed out after 600s and was stopped"
        );
    }
}
