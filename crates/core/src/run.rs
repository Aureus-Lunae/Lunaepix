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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
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
/// The CLI and the app use [`Worker::current_exe`]: their own binary, in
/// worker mode. Tests put another program in its place.
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
    mut on_progress: impl FnMut(usize, &Path, &FileResult),
) -> Vec<FileResult> {
    let mut results = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        let (src, result) = match job {
            Job::Skip { src, reason } => (src, FileResult::Skipped(*reason)),
            Job::Optimize { src, dest } => (
                src,
                match run_worker(worker, src, dest) {
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

/// Optimise one file in a worker process.
///
/// The worker gets no shell and no stdin; the paths are separate arguments.
/// Its stdout and stderr are read on two threads, each up to
/// [`MAX_WORKER_OUTPUT_BYTES`]; if either goes past that, or the timeout
/// passes, the worker is killed through its own handle and reaped.
fn run_worker(worker: &Worker, src: &Path, dest: &Path) -> Result<Outcome, RunError> {
    let spawn_error = |cause| RunError::Spawn {
        path: src.to_path_buf(),
        cause,
    };
    let mut child = Command::new(&worker.program)
        .args(&worker.args)
        .arg(src)
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;

    let overflow = Arc::new(AtomicBool::new(false));
    let readers = match (child.stdout.take(), child.stderr.take()) {
        (Some(stdout), Some(stderr)) => read_capped(stdout, &overflow)
            .and_then(|out| Ok((out, read_capped(stderr, &overflow)?))),
        _ => Err(io::Error::other("the worker's output pipes are missing")),
    };
    let (stdout, stderr) = match readers {
        Ok(readers) => readers,
        Err(cause) => {
            stop(&mut child);
            return Err(spawn_error(cause));
        }
    };

    let ended = wait(&mut child, worker.timeout, &overflow);
    // A reader sets the overflow flag before it sends, so once both have
    // been received the flag is final.
    let stdout = stdout.recv_timeout(PIPE_GRACE).ok();
    let stderr = stderr.recv_timeout(PIPE_GRACE).unwrap_or_default();
    let crashed = |cause| RunError::Crashed {
        path: src.to_path_buf(),
        cause,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };

    if let Ended::TimedOut = ended {
        return Err(RunError::TimedOut {
            path: src.to_path_buf(),
            limit: worker.timeout,
        });
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
enum Ended {
    Exited(ExitStatus),
    /// Killed and reaped after the timeout.
    TimedOut,
    /// Waiting failed, or the output overflowed (told apart by its flag);
    /// the worker was killed and reaped.
    Unknown,
}

/// Wait for `child` to exit, polling, and stop it if it runs past `timeout`
/// or its output overflows.
fn wait(child: &mut Child, timeout: Duration, overflow: &AtomicBool) -> Ended {
    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ended::Exited(status),
            Ok(None) => {}
            Err(_) => {
                stop(child);
                return Ended::Unknown;
            }
        }
        if overflow.load(Ordering::SeqCst) {
            stop(child);
            return Ended::Unknown;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            stop(child);
            return Ended::TimedOut;
        }
        thread::sleep(pause.min(timeout - elapsed));
        pause = (pause * 2).min(MAX_POLL);
    }
}

/// Kill this one child, through its handle, and reap it. Either step may
/// fail only because it has already exited, which is what is wanted.
fn stop(child: &mut Child) {
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
