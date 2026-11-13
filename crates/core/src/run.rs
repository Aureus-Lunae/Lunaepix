//! The batch runner: optimise every planned job, one file at a time, each in
//! its own worker process (see [`crate::worker`]).
//!
//! A worker that crashes, is killed (for example by the kernel when memory
//! runs out), hangs past its timeout, or prints anything but one valid record
//! fails only its own file; the run carries on with the next one.
//!
//! A thumbnail is made the same way, by [`run_thumb`]: one worker process per
//! file, with its own timeout and cap on the answer. So is a probe of a
//! file's shown size, by [`run_probe`].

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::worker::{
    PROBE_FLAG, THUMB_FLAG, THUMB_HEADER_LEN, WORKER_FLAG, option_args, parse_probe_answer,
    parse_record, parse_thumb_answer,
};
use crate::{
    Job, MAX_WORKER_OUTPUT_BYTES, OptimizeError, Options, Outcome, Probe, Skip, Thumbnail,
    WORKER_TIMEOUT,
};

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

    /// This worker with the arguments that carry `options` to it appended:
    /// `--lossy <quality>` for a lossy run, then `--no-replace` for a run
    /// that replaces nothing. A lossless run that replaces adds nothing, so
    /// its worker is called exactly as before.
    pub fn with_options(&self, options: &Options) -> Worker {
        let mut worker = self.clone();
        worker.args.extend(option_args(options));
        worker
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
    on_progress: impl FnMut(usize, &Path, &FileResult),
) -> Vec<FileResult> {
    run_observed(jobs, worker, stop, |_| {}, on_progress)
}

/// [`run_until_stopped`], with `on_start(index)` called right before each
/// [`Job::Optimize`]'s worker starts (so a window can show the file as in
/// progress), and never for a [`Job::Skip`]. `on_progress` follows as
/// before, once the job is done.
pub fn run_observed(
    jobs: &[Job],
    worker: &Worker,
    stop: &StopHandle,
    mut on_start: impl FnMut(usize),
    mut on_progress: impl FnMut(usize, &Path, &FileResult),
) -> Vec<FileResult> {
    let mut results = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        if stop.is_stopped() {
            break;
        }
        let (src, result) = match job {
            Job::Skip { src, reason } => (src, FileResult::Skipped(*reason)),
            Job::Optimize { src, dest } => {
                on_start(index);
                (
                    src,
                    match run_worker(worker, src, dest, stop) {
                        Ok(outcome) => FileResult::Optimized(outcome),
                        Err(error) => FileResult::Failed(error),
                    },
                )
            }
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

/// How long one thumbnail worker may take.
pub const THUMB_TIMEOUT: Duration = Duration::from_secs(10);

/// The most bytes of a thumbnail the runner accepts after the header line.
/// The largest SVG the worker passes through ([`crate::MAX_SVG_THUMB_BYTES`])
/// is exactly this long.
pub const MAX_THUMB_ANSWER_BYTES: usize = 2 * 1024 * 1024;

/// What a thumbnail worker answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbAnswer {
    /// A PNG: the bytes start with the PNG signature.
    Png(Vec<u8>),
    /// An SVG file's own bytes.
    Svg(Vec<u8>),
    /// The worker said the file cannot be shown (exit code 3).
    CannotShow,
    /// Anything else: the worker could not be started, timed out, crashed,
    /// exited with another code, wrote too much, or gave no valid answer.
    Failed,
}

/// Make the thumbnail of `src` in a worker process: `program` started as
/// `<program> --lunaepix-thumb <src>` (see [`crate::worker`]).
///
/// Started and waited for on the calling thread, with no shell and no stdin,
/// and with [`THUMB_TIMEOUT`] and [`MAX_THUMB_ANSWER_BYTES`]: a worker that
/// runs past the limit or writes more is killed and reaped. Only exit code 3
/// is [`ThumbAnswer::CannotShow`]; every other way of not giving a valid
/// answer is [`ThumbAnswer::Failed`].
pub fn run_thumb(program: &Path, src: &Path) -> ThumbAnswer {
    run_thumb_with(program, src, THUMB_TIMEOUT, MAX_THUMB_ANSWER_BYTES)
}

/// [`run_thumb`], with the timeout and the cap on the bytes after the header
/// given: the tests shorten them.
pub(crate) fn run_thumb_with(
    program: &Path,
    src: &Path,
    timeout: Duration,
    cap: usize,
) -> ThumbAnswer {
    let mut command = Command::new(program);
    command.arg(THUMB_FLAG).arg(src);
    run_thumb_command(&mut command, timeout, cap)
}

/// [`run_thumb_with`], with the whole command given: the tests put a shell
/// script in the worker's place.
pub(crate) fn run_thumb_command(
    command: &mut Command,
    timeout: Duration,
    cap: usize,
) -> ThumbAnswer {
    match run_short(command, timeout, cap + THUMB_HEADER_LEN) {
        Some(Short {
            code: 0,
            stdout: Some(answer),
        }) => match parse_thumb_answer(&answer) {
            Some(Thumbnail::Png(bytes)) => ThumbAnswer::Png(bytes),
            Some(Thumbnail::Svg(bytes)) => ThumbAnswer::Svg(bytes),
            None => ThumbAnswer::Failed,
        },
        Some(Short { code: 3, .. }) => ThumbAnswer::CannotShow,
        _ => ThumbAnswer::Failed,
    }
}

/// What a probe worker answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeAnswer {
    /// The size the file is shown at (an SVG has none).
    Size(Probe),
    /// The worker said the file is refused, with Batch's reason (exit code 3).
    Refused(String),
    /// Anything else: the worker could not be started, timed out, crashed,
    /// exited with another code, wrote too much, or gave no valid answer.
    Failed,
}

/// How long one probe worker may take: a thumbnail's, since a probe reads a
/// file's header the way a thumbnail does and is waited for by the window
/// the same way. Not [`crate::WORKER_TIMEOUT`], which is for optimising.
pub(crate) const PROBE_TIMEOUT: Duration = THUMB_TIMEOUT;

/// The most bytes the runner reads from a probe worker's stdout (and, as for
/// every worker, from its stderr): a record is at most a few KiB.
pub(crate) const PROBE_OUTPUT_CAP: usize = MAX_WORKER_OUTPUT_BYTES;

/// Probe `src` in a worker process: `program` started as
/// `<program> --lunaepix-probe <src>` (see [`crate::worker`]).
///
/// Started and waited for on the calling thread, with no shell and no stdin,
/// and with [`PROBE_TIMEOUT`] and [`PROBE_OUTPUT_CAP`]: a worker that
/// runs past the limit or writes more is killed and reaped. Only exit code 0
/// with a `size` record is [`ProbeAnswer::Size`] and only exit code 3 with a
/// `refused` record is [`ProbeAnswer::Refused`]; every other way of not
/// giving a valid answer is [`ProbeAnswer::Failed`].
pub fn run_probe(program: &Path, src: &Path) -> ProbeAnswer {
    run_probe_with(program, src, PROBE_TIMEOUT, PROBE_OUTPUT_CAP)
}

/// [`run_probe`], with the timeout and the cap on the answer given: the
/// tests shorten them.
pub(crate) fn run_probe_with(
    program: &Path,
    src: &Path,
    timeout: Duration,
    cap: usize,
) -> ProbeAnswer {
    let mut command = Command::new(program);
    command.arg(PROBE_FLAG).arg(src);
    run_probe_command(&mut command, timeout, cap)
}

/// [`run_probe_with`], with the whole command given: the tests put a shell
/// script in the worker's place.
pub(crate) fn run_probe_command(
    command: &mut Command,
    timeout: Duration,
    cap: usize,
) -> ProbeAnswer {
    match run_short(command, timeout, cap) {
        Some(Short {
            code: 0,
            stdout: Some(answer),
        }) => match parse_probe_answer(&answer) {
            Some(Ok(probe)) => ProbeAnswer::Size(probe),
            _ => ProbeAnswer::Failed,
        },
        Some(Short {
            code: 3,
            stdout: Some(answer),
        }) => match parse_probe_answer(&answer) {
            Some(Err(message)) => ProbeAnswer::Refused(message),
            _ => ProbeAnswer::Failed,
        },
        _ => ProbeAnswer::Failed,
    }
}

/// How a short worker (a thumbnail or a probe) ended, when it ended on its
/// own with an exit code.
struct Short {
    code: i32,
    /// Everything it wrote to stdout, or `None` if the reader gave none.
    stdout: Option<Vec<u8>>,
}

/// Run `command` to its end on the calling thread, with no stdin: stdout
/// read up to `stdout_limit` bytes and stderr up to
/// [`MAX_WORKER_OUTPUT_BYTES`], and a worker that runs past `timeout` or
/// writes more is killed and reaped. `None` unless it ended on its own with
/// an exit code and no output went past its limit: it could not be started,
/// timed out, crashed on a signal, or overflowed.
fn run_short(command: &mut Command, timeout: Duration, stdout_limit: usize) -> Option<Short> {
    // Nobody else holds this handle, so nothing stops the worker but its
    // own end, the timeout, or an overflow.
    let stop = StopHandle::new();
    let Started {
        stdout,
        stderr,
        overflow,
    } = start(command, stdout_limit, &mut stop.active()).ok()?;

    let ended = wait(&stop, timeout, &overflow);
    // Reaped by now.
    stop.active().take();
    // A reader sets the overflow flag before it sends, so once both have
    // been received the flag is final.
    let stdout = stdout.recv_timeout(PIPE_GRACE).ok();
    let _ = stderr.recv_timeout(PIPE_GRACE);

    // A timeout, a failed wait and a stop (which cannot happen here) are all
    // "no answer", like a crash.
    let Ended::Exited(status) = ended else {
        return None;
    };
    if overflow.load(Ordering::SeqCst) {
        return None;
    }
    // A worker killed by a signal has no exit code.
    Some(Short {
        code: status.code()?,
        stdout,
    })
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
    let Started {
        stdout,
        stderr,
        overflow,
    } = {
        // The flag is checked, and the worker started and stored, under the
        // lock that `stop` takes after setting the flag: either `stop` sees
        // the worker, or the worker is never started.
        let mut active = stop.active();
        if stop.is_stopped() {
            return Err(stopped());
        }
        let mut command = Command::new(&worker.program);
        command.args(&worker.args).arg(src).arg(dest);
        start(&mut command, MAX_WORKER_OUTPUT_BYTES, &mut active).map_err(spawn_error)?
    };

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

/// A worker that has been started: the readers of its two pipes, and the
/// flag they set when one of them goes past its limit.
struct Started {
    stdout: mpsc::Receiver<Vec<u8>>,
    stderr: mpsc::Receiver<Vec<u8>>,
    overflow: Arc<AtomicBool>,
}

/// Start `command` with no stdin and both output pipes read on their own
/// threads (stdout up to `stdout_limit` bytes, stderr up to
/// [`MAX_WORKER_OUTPUT_BYTES`]), and put the child in `active`. If the pipes
/// cannot be set up, the child is killed and reaped and `active` stays as it
/// was.
fn start(
    command: &mut Command,
    stdout_limit: usize,
    active: &mut Option<Child>,
) -> io::Result<Started> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = without_console_window(command).spawn()?;
    let overflow = Arc::new(AtomicBool::new(false));
    let pipes = match (child.stdout.take(), child.stderr.take()) {
        (Some(stdout), Some(stderr)) => {
            read_capped(stdout, stdout_limit, &overflow).and_then(|out| {
                Ok((
                    out,
                    read_capped(stderr, MAX_WORKER_OUTPUT_BYTES, &overflow)?,
                ))
            })
        }
        _ => Err(io::Error::other("the worker's output pipes are missing")),
    };
    match pipes {
        Ok((stdout, stderr)) => {
            *active = Some(child);
            Ok(Started {
                stdout,
                stderr,
                overflow,
            })
        }
        Err(error) => {
            kill_and_reap(&mut child);
            Err(error)
        }
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

/// Read `pipe` to its end on a new thread, keeping at most `limit` bytes.
/// One byte more sets `overflow` and stops the reading; closing the pipe
/// then makes the worker's next write fail.
fn read_capped(
    pipe: impl Read + Send + 'static,
    limit: usize,
    overflow: &Arc<AtomicBool>,
) -> io::Result<mpsc::Receiver<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let overflow = Arc::clone(overflow);
    thread::Builder::new()
        .name("worker-output".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            // A read error ends the output early; whatever came is kept.
            let _ = pipe.take(limit as u64 + 1).read_to_end(&mut bytes);
            if bytes.len() > limit {
                bytes.truncate(limit);
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

    /// Whether process `pid` is gone from the process table: killed and reaped,
    /// not even a zombie left. `kill(pid, 0)` sends nothing and only asks whether
    /// the pid exists, which a zombie still does. It works on every Unix; macOS
    /// has no `/proc` to look in.
    #[cfg(unix)]
    #[allow(unsafe_code)]
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
    #[allow(unsafe_code)]
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
    fn sleeper() -> Child {
        Command::new("sleep").arg("60").spawn().unwrap()
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

    /// Pid 1 exists and is not ours: `kill(1, 0)` answers `EPERM` to a user and
    /// 0 to root. Either way the process is there, so it is not gone.
    #[cfg(unix)]
    #[test]
    fn pid_1_is_not_gone() {
        assert!(!process_is_gone(1));
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

    #[test]
    fn the_thumbnail_limits_are_the_documented_ones() {
        assert_eq!(THUMB_TIMEOUT, Duration::from_secs(10));
        assert_eq!(MAX_THUMB_ANSWER_BYTES, 2 * 1024 * 1024);
        // The largest SVG the worker passes through must fit in an answer.
        assert!(MAX_THUMB_ANSWER_BYTES as u64 >= crate::MAX_SVG_THUMB_BYTES);
    }

    #[test]
    fn the_probe_limits_are_the_thumbnail_limits_and_not_a_batch_files() {
        // A probe reads a header: it gets a thumbnail's ten seconds, not the
        // ten minutes a file's optimising gets, and the cap a record needs.
        assert_eq!(PROBE_TIMEOUT, THUMB_TIMEOUT);
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(10));
        assert_ne!(PROBE_TIMEOUT, crate::WORKER_TIMEOUT);
        assert_eq!(PROBE_OUTPUT_CAP, MAX_WORKER_OUTPUT_BYTES);
        assert_eq!(PROBE_OUTPUT_CAP, 64 * 1024);
        // Not the thumbnail's 2 MiB: a record is at most a few KiB.
        assert_ne!(PROBE_OUTPUT_CAP, MAX_THUMB_ANSWER_BYTES);
        // A longest record (the message cut at its cap, every byte escaped
        // to two) fits.
        let longest = "lunaepix-probe 1 refused ".len() + 2 * crate::MAX_WORKER_MESSAGE_BYTES + 1;
        assert!(longest <= PROBE_OUTPUT_CAP);
    }

    /// The thumbnail runner against doubles: shell scripts that print what a
    /// worker would, or misbehave.
    #[cfg(unix)]
    mod thumb {
        use std::fs;

        use tempfile::TempDir;

        use super::*;

        const LONG: Duration = Duration::from_secs(30);
        /// A small cap, so the boundary tests stay small.
        const CAP: usize = 100;
        const PNG_HEADER: &str = "printf 'lunaepix-thumb 1 png\\n'";
        /// The PNG signature, as `printf` octal escapes.
        const PNG_SIGNATURE: &str = "printf '\\211PNG\\r\\n\\032\\n'";

        fn answer_of(script: &str) -> ThumbAnswer {
            answer_with(script, LONG, CAP)
        }

        fn answer_with(script: &str, timeout: Duration, cap: usize) -> ThumbAnswer {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            run_thumb_command(&mut command, timeout, cap)
        }

        /// A script that answers `png` with the signature and `extra` more
        /// bytes of zero.
        fn png_answer(extra: usize) -> String {
            format!("{PNG_HEADER}; {PNG_SIGNATURE}; head -c {extra} /dev/zero")
        }

        fn png_bytes(extra: usize) -> Vec<u8> {
            let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            bytes.resize(8 + extra, 0);
            bytes
        }

        #[test]
        fn a_valid_png_answer_is_png() {
            assert_eq!(answer_of(&png_answer(20)), ThumbAnswer::Png(png_bytes(20)));
        }

        #[test]
        fn a_valid_svg_answer_is_svg() {
            let script = "printf 'lunaepix-thumb 1 svg\\n<svg/>'";
            assert_eq!(answer_of(script), ThumbAnswer::Svg(b"<svg/>".to_vec()));
        }

        #[test]
        fn a_png_header_with_non_png_bytes_is_failed() {
            let script = "printf 'lunaepix-thumb 1 png\\n<svg/>'";
            assert_eq!(answer_of(script), ThumbAnswer::Failed);
        }

        #[test]
        fn a_header_of_version_2_none_or_another_kind_is_failed() {
            let sig = PNG_SIGNATURE;
            for header in [
                "printf 'lunaepix-thumb 2 png\\n'",
                "printf 'lunaepix-thumb 1 gif\\n'",
                "printf 'lunaepix-thumb 1 png'", // no newline
                "true",                          // no header at all
            ] {
                let script = format!("{header}; {sig}; head -c 20 /dev/zero");
                assert_eq!(answer_of(&script), ThumbAnswer::Failed, "{header}");
            }
        }

        #[test]
        fn exit_3_is_cannot_show() {
            assert_eq!(answer_of("exit 3"), ThumbAnswer::CannotShow);
        }

        #[test]
        fn exit_1_other_exit_codes_and_a_crash_by_signal_are_failed() {
            // Each of these prints a valid answer first, so only the way the
            // worker ended can make it `Failed`.
            let valid = png_answer(20);
            for ending in [
                "exit 1",
                "exit 2",
                "exit 4",
                "exit 101",
                "kill -SEGV $$",
                "kill -KILL $$",
            ] {
                let script = format!("{valid}; {ending}");
                assert_eq!(answer_of(&script), ThumbAnswer::Failed, "{ending}");
            }
            // And with nothing printed.
            for ending in ["exit 1", "exit 2", "exit 101", "kill -SEGV $$"] {
                assert_eq!(answer_of(ending), ThumbAnswer::Failed, "{ending}");
            }
        }

        #[test]
        fn a_program_that_does_not_exist_is_failed() {
            let missing = Path::new("/nonexistent/lunaepix-worker");
            let src = Path::new("/photos/a.png");
            assert_eq!(run_thumb_with(missing, src, LONG, CAP), ThumbAnswer::Failed);
            assert_eq!(run_thumb(missing, src), ThumbAnswer::Failed);
        }

        #[test]
        fn an_answer_of_exactly_the_cap_is_png() {
            // The cap counts the bytes after the header: the signature and
            // the rest make exactly `CAP`.
            assert_eq!(
                answer_with(&png_answer(CAP - 8), LONG, CAP),
                ThumbAnswer::Png(png_bytes(CAP - 8))
            );
        }

        #[test]
        fn an_answer_one_byte_over_the_cap_is_failed() {
            assert_eq!(
                answer_with(&png_answer(CAP - 8 + 1), LONG, CAP),
                ThumbAnswer::Failed
            );
        }

        #[test]
        fn a_worker_that_also_writes_to_stderr_still_answers() {
            let script = format!("echo some noise >&2; {}", png_answer(20));
            assert_eq!(answer_of(&script), ThumbAnswer::Png(png_bytes(20)));
        }

        #[test]
        fn a_worker_whose_stderr_overflows_is_failed() {
            let script = format!(
                "head -c {} /dev/zero >&2; {}",
                MAX_WORKER_OUTPUT_BYTES + 1,
                png_answer(20)
            );
            assert_eq!(answer_of(&script), ThumbAnswer::Failed);
        }

        #[test]
        fn a_worker_past_the_timeout_is_failed_and_reaped() {
            let tmp = TempDir::new().unwrap();
            let pid_file = tmp.path().join("worker.pid");
            // `exec` keeps the PID: the process that sleeps is the one started.
            let script = format!("echo $$ > '{}'; exec sleep 60", pid_file.display());
            let limit = Duration::from_millis(500);
            let started = Instant::now();

            let answer = answer_with(&script, limit, CAP);

            let elapsed = started.elapsed();
            assert_eq!(answer, ThumbAnswer::Failed);
            assert!(elapsed >= limit, "{elapsed:?}");
            assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
            let pid = fs::read_to_string(&pid_file).unwrap();
            // Gone from the process table: killed and reaped, no zombie.
            assert!(
                process_is_gone(pid.trim().parse().unwrap()),
                "process {} still exists",
                pid.trim()
            );
        }

        #[test]
        fn a_slow_worker_within_the_timeout_is_not_stopped() {
            let script = format!("sleep 1; {}", png_answer(20));
            assert_eq!(
                answer_with(&script, LONG, CAP),
                ThumbAnswer::Png(png_bytes(20))
            );
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

    /// The probe runner against doubles: shell scripts that print what a
    /// worker would, or misbehave.
    #[cfg(unix)]
    mod probe {
        use std::fs;

        use tempfile::TempDir;

        use super::*;

        const LONG: Duration = Duration::from_secs(30);
        /// A small cap, so the boundary tests stay small.
        const CAP: usize = 100;
        /// `lunaepix-probe 1 refused `, the start of a refusal's record.
        const REFUSED_PREFIX: &str = "lunaepix-probe 1 refused ";

        fn answer_of(script: &str) -> ProbeAnswer {
            answer_with(script, LONG, CAP)
        }

        fn answer_with(script: &str, timeout: Duration, cap: usize) -> ProbeAnswer {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            run_probe_command(&mut command, timeout, cap)
        }

        fn size(width: u32, height: u32) -> ProbeAnswer {
            ProbeAnswer::Size(Probe {
                width: Some(width),
                height: Some(height),
            })
        }

        /// A script that prints a refusal record whose message is `length`
        /// bytes of `x`, then exits 3.
        fn refusal_of(length: usize) -> String {
            format!(
                "printf '{REFUSED_PREFIX}'; head -c {length} /dev/zero | tr '\\0' x; echo; exit 3"
            )
        }

        #[test]
        fn a_valid_size_answer_with_exit_0_is_size() {
            assert_eq!(
                answer_of("printf 'lunaepix-probe 1 size 300 200\\n'"),
                size(300, 200)
            );
            assert_eq!(
                answer_of("printf 'lunaepix-probe 1 size - -\\n'"),
                ProbeAnswer::Size(Probe {
                    width: None,
                    height: None
                })
            );
        }

        #[test]
        fn a_valid_refused_answer_with_exit_3_is_refused_with_its_message_unescaped() {
            let script = "printf 'lunaepix-probe 1 refused File is too large\\n'; exit 3";
            assert_eq!(
                answer_of(script),
                ProbeAnswer::Refused("File is too large".to_owned())
            );
            // The escapes the worker wrote are undone.
            let escaped = "printf '%s\\n' 'lunaepix-probe 1 refused a\\\\b\\nc'; exit 3";
            assert_eq!(
                answer_of(escaped),
                ProbeAnswer::Refused("a\\b\nc".to_owned())
            );
        }

        #[test]
        fn a_record_with_the_wrong_exit_code_is_failed() {
            // Each prints a valid record first, so only the way the worker
            // ended can make it `Failed`.
            let size_record = "printf 'lunaepix-probe 1 size 3 2\\n'";
            let refused_record = "printf 'lunaepix-probe 1 refused File is too large\\n'";
            for ending in ["exit 1", "exit 2", "exit 3", "exit 4", "exit 101"] {
                let script = format!("{size_record}; {ending}");
                // A size is only a size with exit 0; exit 3 reads the record as
                // a refusal, and it is not one.
                assert_eq!(answer_of(&script), ProbeAnswer::Failed, "size, {ending}");
            }
            for ending in ["exit 0", "exit 1", "exit 2", "exit 4", "exit 101"] {
                let script = format!("{refused_record}; {ending}");
                assert_eq!(answer_of(&script), ProbeAnswer::Failed, "refused, {ending}");
            }
            for ending in ["kill -SEGV $$", "kill -KILL $$"] {
                for record in [size_record, refused_record] {
                    let script = format!("{record}; {ending}");
                    assert_eq!(answer_of(&script), ProbeAnswer::Failed, "{script}");
                }
            }
            // And with nothing printed.
            for ending in ["exit 0", "exit 1", "exit 2", "exit 3", "exit 101"] {
                assert_eq!(answer_of(ending), ProbeAnswer::Failed, "{ending}");
            }
        }

        #[test]
        fn a_wrong_version_or_a_garbled_record_is_failed_even_with_the_right_exit_code() {
            // Control: the same shape, valid, is an answer.
            assert_eq!(
                answer_of("printf 'lunaepix-probe 1 size 3 2\\n'"),
                size(3, 2)
            );
            for record in [
                "lunaepix-probe 2 size 3 2\\n",
                "lunaepix-probe 1 size 3 2",
                "lunaepix-probe 1 size 3\\n",
                "lunaepix-probe 1 size 3 -\\n",
                "lunaepix-probe 1 size x 2\\n",
                "lunaepix-probe 1 size 4294967296 2\\n",
                "lunaepix-probe 1 size 3 2\\nlunaepix-probe 1 size 3 2\\n",
                "lunaepix-thumb 1 size 3 2\\n",
            ] {
                let script = format!("printf '{record}'");
                assert_eq!(answer_of(&script), ProbeAnswer::Failed, "{record}");
            }
            for record in [
                "lunaepix-probe 2 refused x\\n",
                "lunaepix-probe 1 refused x",
                "lunaepix-probe 1 refused\\n",
                "lunaepix-probe 1 refused \\n",
                "lunaepix-probe 1 refused a\\\\x\\n",
            ] {
                let script = format!("printf '{record}'; exit 3");
                assert_eq!(answer_of(&script), ProbeAnswer::Failed, "{record}");
            }
        }

        #[test]
        fn an_answer_of_exactly_the_cap_is_read_and_one_byte_over_it_is_failed() {
            // The cap counts the whole line: prefix, message and newline.
            let message = CAP - REFUSED_PREFIX.len() - 1;
            assert_eq!(
                answer_with(&refusal_of(message), LONG, CAP),
                ProbeAnswer::Refused("x".repeat(message))
            );
            assert_eq!(
                answer_with(&refusal_of(message + 1), LONG, CAP),
                ProbeAnswer::Failed
            );
        }

        #[test]
        fn a_worker_that_also_writes_to_stderr_still_answers() {
            let script = "echo some noise >&2; printf 'lunaepix-probe 1 size 3 2\\n'";
            assert_eq!(answer_of(script), size(3, 2));
        }

        #[test]
        fn a_worker_whose_stderr_overflows_is_failed() {
            let script = format!(
                "head -c {} /dev/zero >&2; printf 'lunaepix-probe 1 size 3 2\\n'",
                MAX_WORKER_OUTPUT_BYTES + 1
            );
            assert_eq!(answer_of(&script), ProbeAnswer::Failed);
        }

        #[test]
        fn a_worker_past_the_timeout_is_failed_and_reaped() {
            let tmp = TempDir::new().unwrap();
            let pid_file = tmp.path().join("worker.pid");
            // `exec` keeps the PID: the process that sleeps is the one started.
            let script = format!("echo $$ > '{}'; exec sleep 60", pid_file.display());
            let limit = Duration::from_millis(500);
            let started = Instant::now();

            let answer = answer_with(&script, limit, CAP);

            let elapsed = started.elapsed();
            assert_eq!(answer, ProbeAnswer::Failed);
            assert!(elapsed >= limit, "{elapsed:?}");
            assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
            let pid = fs::read_to_string(&pid_file).unwrap();
            assert!(
                process_is_gone(pid.trim().parse().unwrap()),
                "process {} still exists",
                pid.trim()
            );
        }

        #[test]
        fn a_slow_worker_within_the_timeout_is_not_stopped() {
            let script = "sleep 1; printf 'lunaepix-probe 1 size 3 2\\n'";
            assert_eq!(answer_with(script, LONG, CAP), size(3, 2));
        }

        #[test]
        fn a_worker_that_floods_stdout_is_stopped_and_failed() {
            // `yes` writes without end; the cap stops it at once.
            let started = Instant::now();
            assert_eq!(answer_with("exec yes", LONG, CAP), ProbeAnswer::Failed);
            assert!(started.elapsed() < Duration::from_secs(20));
        }

        #[test]
        fn a_program_that_does_not_exist_is_failed() {
            let missing = Path::new("/nonexistent/lunaepix-worker");
            let src = Path::new("/photos/a.png");
            assert_eq!(run_probe_with(missing, src, LONG, CAP), ProbeAnswer::Failed);
            assert_eq!(run_probe(missing, src), ProbeAnswer::Failed);
        }
    }
}
