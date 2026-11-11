//! Worker mode: optimise one file in this process and report the result on
//! stdout.
//!
//! The batch runner ([`crate::run`]) starts a binary once per file as
//! `<exe> --lunaepix-worker [--lossy <0-100>] [--no-replace] <src> <dest>`,
//! so that a library that crashes the process (a stack overflow aborts, and
//! cannot be caught) takes down only that one file. The same binary also
//! makes a thumbnail, as `<exe> --lunaepix-thumb <src>` (see "The thumbnail
//! mode" below), and probes a file's shown size, as `<exe> --lunaepix-probe
//! <src>` (see "The probe mode" below), for the same reason: the decoders run
//! in a process that can die without the window. The CLI and the app call
//! [`serve_if_requested`] first thing in `main`, so both binaries answer all
//! three modes.
//!
//! # The arguments
//!
//! The last two arguments are always the source and the destination, so a
//! path that looks like an option is still a path. Before them come the run's
//! options ([`Options`]), in this order and each at most once: `--lossy`
//! with a quality that [`Quality::parse`] accepts, then `--no-replace`.
//! Anything else is a usage error: exit code 2, no record, nothing written.
//!
//! # The result record
//!
//! The worker writes exactly one line of UTF-8 to stdout, and nothing else:
//!
//! ```text
//! lunaepix-worker 2 ok <before> <after> <millis> <note-code or ->
//! lunaepix-worker 2 unsupported <role>
//! lunaepix-worker 2 invalid <role> <message>
//! lunaepix-worker 2 io <role> <os-error-code or -> <message>
//! ```
//!
//! `2` is the format version; the runner refuses any other, so a stale
//! worker counts as crashed. The note code is [`Note::code`], or `-` for no
//! note. The numbers are plain decimal digits. `<role>`
//! is `src` or `dest`: which of the two paths the error is about. Paths are
//! never written, because the parent already has them (exactly, even when
//! they are not UTF-8). In `<message>` a backslash is written `\\`, a line
//! feed `\n` and a carriage return `\r`, so the record stays one line; it is
//! cut to [`MAX_WORKER_MESSAGE_BYTES`] first.
//!
//! # Exit codes
//!
//! 0 with an `ok` record; 1 with an error record; 2 for a call with the wrong
//! arguments (no record); 3 if the record could not be written; 101 if the
//! optimiser panicked (no record; the panic message is on stderr). The runner
//! accepts only 0 with `ok` and 1 with an error record: anything else counts
//! as a crash.
//!
//! # The thumbnail mode
//!
//! `<exe> --lunaepix-thumb <src>` takes exactly one argument after the flag,
//! the source, which is a path even when it looks like an option; anything
//! else is a usage error (exit code 2, nothing on stdout). The worker sets
//! itself up as the optimiser does, makes the thumbnail ([`thumbnail`]) and
//! writes one of these answers to stdout, header line first and the bytes
//! right after it, then exits 0:
//!
//! ```text
//! lunaepix-thumb 1 png\n<the bytes of a PNG>
//! lunaepix-thumb 1 svg\n<the bytes of an SVG file>
//! ```
//!
//! If the file cannot be shown ([`CannotShow`]) it writes nothing and exits
//! 3. The exit codes 101 (a panic) and 1 (the answer could not be written)
//! are crashes to the runner ([`crate::run_thumb`]), which reads only exit 3
//! as "cannot be shown" and only exit 0 with a valid answer as a thumbnail.
//!
//! # The probe mode
//!
//! `<exe> --lunaepix-probe <src>` takes exactly one argument after the flag,
//! the source, which is a path even when it looks like an option; anything
//! else is a usage error (exit code 2, nothing on stdout). The worker sets
//! itself up as the optimiser does, probes the file ([`probe`]: the size it
//! is shown at, or the reason Batch would refuse it) and writes one line of
//! UTF-8 to stdout:
//!
//! ```text
//! lunaepix-probe 1 size <width> <height>      exit 0   (an SVG: "size - -")
//! lunaepix-probe 1 refused <message>          exit 3
//! ```
//!
//! `1` is the format version. The numbers are plain decimal digits. The
//! `<message>` is escaped and cut as the optimiser's record is (see "The
//! result record"). The exit codes 101 (a panic) and 1 (the record could not
//! be written, or the thread could not start) are crashes to the runner
//! ([`crate::run_probe`]), which reads only exit 0 with a `size` record and
//! exit 3 with a `refused` record; anything else is no answer.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;

use crate::{
    CannotShow, Compression, MAX_WORKER_MESSAGE_BYTES, Note, OptimizeError, Options, Outcome,
    Probe, Quality, Thumbnail, WriteMode, optimize_file, platform, probe, thumbnail,
};

/// The hidden first argument that starts a binary in worker mode.
pub const WORKER_FLAG: &str = "--lunaepix-worker";

/// The hidden first argument that starts a binary in thumbnail mode.
pub const THUMB_FLAG: &str = "--lunaepix-thumb";

/// The hidden first argument that starts a binary in probe mode.
pub const PROBE_FLAG: &str = "--lunaepix-probe";

/// Start of every probe answer: a magic word and the format version.
const PROBE_PREFIX: &str = "lunaepix-probe 1 ";

/// The line that starts a worker's answer when the thumbnail is a PNG.
const THUMB_PNG_HEADER: &[u8] = b"lunaepix-thumb 1 png
";

/// The line that starts a worker's answer when the thumbnail is an SVG.
const THUMB_SVG_HEADER: &[u8] = b"lunaepix-thumb 1 svg
";

/// The length of either header line, newline included.
pub(crate) const THUMB_HEADER_LEN: usize = THUMB_PNG_HEADER.len();

/// The first eight bytes of every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Start of every record: a magic word and the format version.
const RECORD_PREFIX: &str = "lunaepix-worker 2 ";

/// The option that makes a run lossy; the quality follows it.
const LOSSY_OPTION: &str = "--lossy";

/// The option that makes a run write without replacing anything.
const NO_REPLACE_OPTION: &str = "--no-replace";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_NO_RECORD: u8 = 3;
/// The thumbnail mode's own meaning of 3: the file cannot be shown.
const EXIT_NO_THUMB: u8 = 3;
/// The probe mode's own meaning of 3: the file is refused.
const EXIT_REFUSED: u8 = 3;
/// What Rust itself exits with after a panic on the main thread.
const EXIT_PANICKED: u8 = 101;

/// Stack for the thread that optimises: 8 MiB, the main-thread stack of
/// Linux and macOS. Windows gives its main thread 1 MiB, which an SVG at the
/// depth limit can exceed in a debug build (see `MAX_SVG_DEPTH`), so the
/// worker sets the size itself and every system gets the same.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

/// If this process was started as `<exe> --lunaepix-worker ...`,
/// `<exe> --lunaepix-thumb <src>` or `<exe> --lunaepix-probe <src>`, run that
/// worker and return its exit code;
/// `main` returns it at once. Otherwise `None`, and `main` carries on as
/// usual. The arguments are read with `args_os`, so paths that are not UTF-8
/// arrive intact.
pub fn serve_if_requested() -> Option<ExitCode> {
    let mut args = env::args_os().skip(1);
    let flag = args.next()?;
    if flag == OsStr::new(WORKER_FLAG) {
        Some(serve(args))
    } else if flag == OsStr::new(THUMB_FLAG) {
        Some(serve_thumb(args))
    } else if flag == OsStr::new(PROBE_FLAG) {
        Some(serve_probe(args))
    } else {
        None
    }
}

/// What every worker does before its job: the start-up for its parent. On
/// Linux it asks to be killed when the thread that started it ends (so
/// closing the window stops it), then raises its own OOM score, so that when
/// memory runs out the kernel kills the worker rather than the window; on
/// Windows it turns off the crash dialogs.
fn start_up() {
    // Before the OOM score: the tests read a score of 1000 as "ready".
    platform::die_with_parent();
    raise_oom_score();
    platform::suppress_crash_dialogs();
}

/// Run `job` on a new thread with the fixed [`WORKER_STACK_BYTES`] stack and
/// wait for it: `Err` if the thread could not be started, and `Ok(Err(_))`
/// if the job panicked (the panic hook has already written the message to
/// stderr).
fn on_worker_stack<T: Send + 'static>(
    name: &str,
    job: impl FnOnce() -> T + Send + 'static,
) -> io::Result<thread::Result<T>> {
    on_stack(name, WORKER_STACK_BYTES, job)
}

/// [`on_worker_stack`] with the stack size given: the thumbnail and probe
/// tests pass a size the OS refuses, so that the thread cannot start.
fn on_stack<T: Send + 'static>(
    name: &str,
    stack_bytes: usize,
    job: impl FnOnce() -> T + Send + 'static,
) -> io::Result<thread::Result<T>> {
    thread::Builder::new()
        .name(name.to_owned())
        .stack_size(stack_bytes)
        .spawn(job)
        .map(thread::JoinHandle::join)
}

/// Optimise one file and write the result record to stdout. `args` are the
/// arguments after the flag: the options, then `<src> <dest>` (see the
/// module documentation).
///
/// First the worker sets itself up for its parent ([`start_up`]). The file
/// is optimised on a thread with a fixed 8 MiB stack.
pub fn serve(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    start_up();
    serve_with(
        args,
        |src: &Path, dest: &Path, options: Options| optimize_file(src, dest, &options),
        &mut io::stdout().lock(),
    )
}

/// [`serve`] without the start-up calls, with the optimiser and the output
/// given: the tests hand in an optimiser that panics, and a buffer for the
/// record, and keep their own process as it is.
pub(crate) fn serve_with<F>(
    args: impl IntoIterator<Item = OsString>,
    optimise: F,
    out: &mut dyn Write,
) -> ExitCode
where
    F: FnOnce(&Path, &Path, Options) -> Result<Outcome, OptimizeError> + Send + 'static,
{
    let Some((options, src, dest)) = parse_args(args.into_iter().collect()) else {
        eprintln!(
            "usage: <program> {WORKER_FLAG} [{LOSSY_OPTION} <0-100>] [{NO_REPLACE_OPTION}] <source> <destination>"
        );
        return ExitCode::from(EXIT_USAGE);
    };

    let job = {
        let (src, dest) = (src.clone(), dest.clone());
        move || optimise(&src, &dest, options)
    };
    let result = match on_worker_stack("optimize", job) {
        Ok(Ok(result)) => result,
        // The panic hook has already written the message to stderr.
        Ok(Err(_panic)) => return ExitCode::from(EXIT_PANICKED),
        Err(error) => {
            eprintln!("could not start the optimising thread: {error}");
            return ExitCode::from(EXIT_NO_RECORD);
        }
    };

    let record = format_record(&result, &src, &dest);
    if let Err(error) = out.write_all(record.as_bytes()).and_then(|()| out.flush()) {
        eprintln!("could not write the result: {error}");
        return ExitCode::from(EXIT_NO_RECORD);
    }
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(EXIT_FAILED),
    }
}

/// The options and the two paths of a worker call, or `None` for anything
/// but `[--lossy <quality>] [--no-replace] <src> <dest>`.
fn parse_args(mut args: Vec<OsString>) -> Option<(Options, PathBuf, PathBuf)> {
    let dest = PathBuf::from(args.pop()?);
    let src = PathBuf::from(args.pop()?);
    let mut flags = args.iter().map(|arg| arg.to_str());
    let mut next = flags.next();
    let mut options = Options {
        compression: Compression::Lossless,
        write: WriteMode::Replace,
    };
    if next == Some(Some(LOSSY_OPTION)) {
        let quality = Quality::parse(flags.next()??)?;
        options.compression = Compression::Lossy(quality);
        next = flags.next();
    }
    if next == Some(Some(NO_REPLACE_OPTION)) {
        options.write = WriteMode::NoReplace;
        next = flags.next();
    }
    // Nothing may follow, and nothing else may stand where an option was.
    next.is_none().then_some((options, src, dest))
}

/// The arguments that carry `options` to a worker, in the order
/// [`parse_args`] reads them.
pub(crate) fn option_args(options: &Options) -> Vec<OsString> {
    let mut args = Vec::new();
    if let Compression::Lossy(quality) = options.compression {
        args.push(LOSSY_OPTION.into());
        args.push(quality.get().to_string().into());
    }
    if options.write == WriteMode::NoReplace {
        args.push(NO_REPLACE_OPTION.into());
    }
    args
}

/// Make the thumbnail of one file and write the answer to stdout. `args` are
/// the arguments after the flag: exactly one, the source (see the module
/// documentation). Set up and bounded like [`serve`].
pub fn serve_thumb(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    start_up();
    serve_thumb_with(args, thumbnail, &mut io::stdout().lock())
}

/// [`serve_thumb`] without the start-up calls, with the thumbnailer and the
/// output given, as [`serve_with`] is for the optimiser.
///
/// Exit codes: 0 with the answer written; [`EXIT_NO_THUMB`] (nothing
/// written) when the file cannot be shown; 2 for anything but exactly one
/// argument; 101 if the thumbnailer panicked; 1 if the thread could not be
/// started or the answer could not be written. The last two must not be 3,
/// which the runner reads as "cannot be shown".
pub(crate) fn serve_thumb_with<F>(
    args: impl IntoIterator<Item = OsString>,
    make: F,
    out: &mut dyn Write,
) -> ExitCode
where
    F: FnOnce(&Path) -> Result<Thumbnail, CannotShow> + Send + 'static,
{
    serve_thumb_on_stack(args, make, out, WORKER_STACK_BYTES)
}

/// [`serve_thumb_with`] with the stack size of the thumbnail thread given,
/// which production sets to [`WORKER_STACK_BYTES`]: a test passes a size the
/// OS refuses to reach the exit code of a thread that cannot start.
pub(crate) fn serve_thumb_on_stack<F>(
    args: impl IntoIterator<Item = OsString>,
    make: F,
    out: &mut dyn Write,
    stack_bytes: usize,
) -> ExitCode
where
    F: FnOnce(&Path) -> Result<Thumbnail, CannotShow> + Send + 'static,
{
    let Ok([src]) = <[OsString; 1]>::try_from(args.into_iter().collect::<Vec<_>>()) else {
        eprintln!("usage: <program> {THUMB_FLAG} <source>");
        return ExitCode::from(EXIT_USAGE);
    };
    let src = PathBuf::from(src);

    let thumbnail = match on_stack("thumbnail", stack_bytes, move || make(&src)) {
        Ok(Ok(result)) => result,
        // The panic hook has already written the message to stderr.
        Ok(Err(_panic)) => return ExitCode::from(EXIT_PANICKED),
        Err(error) => {
            eprintln!("could not start the thumbnail thread: {error}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let Ok(thumbnail) = thumbnail else {
        return ExitCode::from(EXIT_NO_THUMB);
    };

    let (header, bytes) = match &thumbnail {
        Thumbnail::Png(bytes) => (THUMB_PNG_HEADER, bytes),
        Thumbnail::Svg(bytes) => (THUMB_SVG_HEADER, bytes),
    };
    let written = out
        .write_all(header)
        .and_then(|()| out.write_all(bytes))
        .and_then(|()| out.flush());
    if let Err(error) = written {
        eprintln!("could not write the thumbnail: {error}");
        return ExitCode::from(EXIT_FAILED);
    }
    ExitCode::SUCCESS
}

/// The thumbnail in a worker's answer, or `None` unless `answer` is exactly a
/// header ([`THUMB_PNG_HEADER`] or [`THUMB_SVG_HEADER`]) and the bytes after
/// it: for `png` the bytes must start with the PNG signature, for `svg`
/// there must be some. Never panics, whatever the bytes.
pub(crate) fn parse_thumb_answer(answer: &[u8]) -> Option<Thumbnail> {
    let (header, bytes) = answer.split_at_checked(THUMB_HEADER_LEN)?;
    if header == THUMB_PNG_HEADER && bytes.starts_with(&PNG_SIGNATURE) {
        Some(Thumbnail::Png(bytes.to_vec()))
    } else if header == THUMB_SVG_HEADER && !bytes.is_empty() {
        Some(Thumbnail::Svg(bytes.to_vec()))
    } else {
        None
    }
}

/// Probe one file and write the answer to stdout. `args` are the arguments
/// after the flag: exactly one, the source (see the module documentation).
/// Set up and bounded like [`serve`].
pub fn serve_probe(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    start_up();
    serve_probe_with(args, probe, &mut io::stdout().lock())
}

/// [`serve_probe`] without the start-up calls, with the prober and the
/// output given, as [`serve_thumb_with`] is for the thumbnailer.
///
/// Exit codes: 0 with a size record written; [`EXIT_REFUSED`] with a refusal
/// record written; 2 for anything but exactly one argument (nothing
/// written); 101 if the prober panicked; 1 if the thread could not be
/// started or the record could not be written. The last two must not be 3,
/// which the runner reads as "refused".
pub(crate) fn serve_probe_with<F>(
    args: impl IntoIterator<Item = OsString>,
    probe: F,
    out: &mut dyn Write,
) -> ExitCode
where
    F: FnOnce(&Path) -> Result<Probe, String> + Send + 'static,
{
    serve_probe_on_stack(args, probe, out, WORKER_STACK_BYTES)
}

/// [`serve_probe_with`] with the stack size of the probing thread given,
/// which production sets to [`WORKER_STACK_BYTES`]: a test passes a size the
/// OS refuses to reach the exit code of a thread that cannot start.
pub(crate) fn serve_probe_on_stack<F>(
    args: impl IntoIterator<Item = OsString>,
    probe: F,
    out: &mut dyn Write,
    stack_bytes: usize,
) -> ExitCode
where
    F: FnOnce(&Path) -> Result<Probe, String> + Send + 'static,
{
    let Ok([src]) = <[OsString; 1]>::try_from(args.into_iter().collect::<Vec<_>>()) else {
        eprintln!("usage: <program> {PROBE_FLAG} <source>");
        return ExitCode::from(EXIT_USAGE);
    };
    let src = PathBuf::from(src);

    let answer = match on_stack("probe", stack_bytes, move || probe(&src)) {
        Ok(Ok(answer)) => answer,
        // The panic hook has already written the message to stderr.
        Ok(Err(_panic)) => return ExitCode::from(EXIT_PANICKED),
        Err(error) => {
            eprintln!("could not start the probing thread: {error}");
            return ExitCode::from(EXIT_FAILED);
        }
    };

    let record = format_probe_answer(&answer);
    if let Err(error) = out.write_all(record.as_bytes()).and_then(|()| out.flush()) {
        eprintln!("could not write the answer: {error}");
        return ExitCode::from(EXIT_FAILED);
    }
    match answer {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(EXIT_REFUSED),
    }
}

/// The record for a probe's answer; see the module documentation for the
/// format. A refusal's message is cut and escaped as the optimiser's record
/// is ([`escape`]).
pub(crate) fn format_probe_answer(answer: &Result<Probe, String>) -> String {
    let body = match answer {
        Ok(Probe { width, height }) => {
            let field = |side: &Option<u32>| side.map_or("-".to_owned(), |side| side.to_string());
            format!("size {} {}", field(width), field(height))
        }
        Err(message) => format!("refused {}", escape(message)),
    };
    format!("{PROBE_PREFIX}{body}\n")
}

/// Read a worker's whole stdout as one probe record: `Ok` for a size
/// record, `Err` with the message for a refusal. `None` unless it is exactly
/// one well-formed record, which the runner then counts as no answer. Never
/// panics, whatever the bytes.
pub(crate) fn parse_probe_answer(answer: &[u8]) -> Option<Result<Probe, String>> {
    let text = std::str::from_utf8(answer).ok()?;
    let line = text.strip_suffix('\n')?;
    if line.contains(['\n', '\r']) {
        return None;
    }
    let body = line.strip_prefix(PROBE_PREFIX)?;
    let (kind, rest) = body.split_once(' ')?;
    match kind {
        "size" => {
            let (width, height) = rest.split_once(' ')?;
            let side = |field: &str| match field {
                "-" => Some(None),
                digits => u32::try_from(number(digits)?).ok().map(Some),
            };
            let (width, height) = (side(width)?, side(height)?);
            // Both or neither: a size has a width and a height, an SVG none.
            (width.is_some() == height.is_some()).then_some(Ok(Probe { width, height }))
        }
        "refused" => Some(Err(unescape(rest).filter(|message| !message.is_empty())?)),
        _ => None,
    }
}

/// Ask the kernel to pick this process first when memory runs out. Best
/// effort: raising one's own score needs no privilege, and a failure only
/// means the default choice applies.
#[cfg(target_os = "linux")]
fn raise_oom_score() {
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
}

#[cfg(not(target_os = "linux"))]
fn raise_oom_score() {}

/// The record for `result`; see the module documentation for the format.
/// An error about a path other than `dest` is reported as being about `src`.
pub(crate) fn format_record(
    result: &Result<Outcome, OptimizeError>,
    src: &Path,
    dest: &Path,
) -> String {
    let role = |path: &Path| {
        if path == dest && path != src {
            "dest"
        } else {
            "src"
        }
    };
    let body = match result {
        Ok(outcome) => format!(
            "ok {} {} {} {}",
            outcome.before,
            outcome.after,
            outcome.millis,
            outcome.note.map_or("-", Note::code)
        ),
        Err(OptimizeError::UnsupportedType { path }) => format!("unsupported {}", role(path)),
        Err(OptimizeError::Invalid { path, message }) => {
            format!("invalid {} {}", role(path), escape(message))
        }
        Err(OptimizeError::Io { path, cause }) => {
            let code = match cause.raw_os_error() {
                Some(code) if code >= 0 => code.to_string(),
                _ => "-".to_owned(),
            };
            format!("io {} {code} {}", role(path), escape(&cause.to_string()))
        }
    };
    format!("{RECORD_PREFIX}{body}\n")
}

/// Read a worker's whole stdout as one record. `None` unless it is exactly
/// one well-formed record: the runner then counts the file as crashed.
/// Never panics, whatever the bytes.
pub(crate) fn parse_record(
    bytes: &[u8],
    src: &Path,
    dest: &Path,
) -> Option<Result<Outcome, OptimizeError>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let line = text.strip_suffix('\n')?;
    if line.contains(['\n', '\r']) {
        return None;
    }
    let body = line.strip_prefix(RECORD_PREFIX)?;
    let path = |role: &str| match role {
        "src" => Some(src.to_path_buf()),
        "dest" => Some(dest.to_path_buf()),
        _ => None,
    };
    let (kind, rest) = body.split_once(' ')?;
    let parsed = match kind {
        "ok" => {
            let mut fields = rest.split(' ');
            let outcome = Outcome {
                before: number(fields.next()?)?,
                after: number(fields.next()?)?,
                millis: number(fields.next()?)?,
                note: match fields.next()? {
                    "-" => None,
                    code => Some(Note::from_code(code)?),
                },
            };
            if fields.next().is_some() {
                return None;
            }
            Ok(outcome)
        }
        "unsupported" => Err(OptimizeError::UnsupportedType { path: path(rest)? }),
        "invalid" => {
            let (role, message) = rest.split_once(' ')?;
            Err(OptimizeError::Invalid {
                path: path(role)?,
                message: unescape(message)?,
            })
        }
        "io" => {
            let mut fields = rest.splitn(3, ' ');
            let path = path(fields.next()?)?;
            let code = fields.next()?;
            let message = unescape(fields.next()?)?;
            let cause = match code {
                "-" => io::Error::other(message),
                // Same system as the worker, so the code means the same.
                digits => io::Error::from_raw_os_error(i32::try_from(number(digits)?).ok()?),
            };
            Err(OptimizeError::Io { path, cause })
        }
        _ => return None,
    };
    Some(parsed)
}

/// A non-empty run of ASCII digits that fits in a `u64`; no sign, no spaces.
fn number(field: &str) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// `message`, cut to [`MAX_WORKER_MESSAGE_BYTES`] at a character boundary
/// (ending in `…` when cut), with `\`, line feed and carriage return escaped.
fn escape(message: &str) -> String {
    let message = if message.len() > MAX_WORKER_MESSAGE_BYTES {
        let mut end = MAX_WORKER_MESSAGE_BYTES - '…'.len_utf8();
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &message[..end])
    } else {
        message.to_owned()
    };
    let mut out = String::with_capacity(message.len());
    for c in message.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// The inverse of [`escape`]. `None` for any other backslash sequence.
fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            '\\' => '\\',
            'n' => '\n',
            'r' => '\r',
            _ => return None,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_WORKER_MESSAGE_BYTES;
    use std::io;
    use std::path::PathBuf;

    fn paths() -> (PathBuf, PathBuf) {
        (
            PathBuf::from("/photos/a b.png"),
            PathBuf::from("/photos/optimized-a b.png"),
        )
    }

    /// Format `result`, parse it back, and return both.
    fn round_trip(
        result: Result<Outcome, OptimizeError>,
    ) -> (String, Result<Outcome, OptimizeError>) {
        let (src, dest) = paths();
        let record = format_record(&result, &src, &dest);
        let parsed = parse_record(record.as_bytes(), &src, &dest)
            .unwrap_or_else(|| panic!("a record the worker writes must parse: {record:?}"));
        (record, parsed)
    }

    #[test]
    fn writes_an_outcome_as_one_exact_line() {
        let (src, dest) = paths();
        let outcome = Outcome {
            before: 2048,
            after: 1024,
            millis: 7,
            note: None,
        };
        assert_eq!(
            format_record(&Ok(outcome), &src, &dest),
            "lunaepix-worker 2 ok 2048 1024 7 -\n"
        );
    }

    #[test]
    fn parses_an_outcome_back_exactly() {
        let outcome = Outcome {
            before: u64::MAX,
            after: 0,
            millis: 123_456,
            note: None,
        };
        let (_, parsed) = round_trip(Ok(outcome));
        assert_eq!(parsed.unwrap(), outcome);
    }

    #[test]
    fn parses_an_invalid_error_back_with_its_path_and_message() {
        let (src, dest) = paths();
        let message = "line one\nline two \\ with a backslash\r and a return".to_owned();
        let (record, parsed) = round_trip(Err(OptimizeError::Invalid {
            path: dest.clone(),
            message: message.clone(),
        }));
        // One line: the newline inside the message is escaped.
        assert_eq!(record.matches('\n').count(), 1, "{record:?}");
        match parsed.unwrap_err() {
            OptimizeError::Invalid { path, message: got } => {
                assert_eq!(path, dest);
                assert_ne!(path, src);
                assert_eq!(got, message);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_an_unsupported_type_error_back_with_the_source_path() {
        let (src, _) = paths();
        let (_, parsed) = round_trip(Err(OptimizeError::UnsupportedType { path: src.clone() }));
        assert!(
            matches!(parsed.unwrap_err(), OptimizeError::UnsupportedType { path } if path == src)
        );
    }

    #[test]
    fn parses_an_io_error_back_with_its_os_error_and_its_text() {
        // The OS's own "access denied" code: EACCES on unix,
        // ERROR_ACCESS_DENIED on Windows (where 13 is ERROR_INVALID_DATA).
        #[cfg(unix)]
        const ACCESS_DENIED: i32 = 13;
        #[cfg(windows)]
        const ACCESS_DENIED: i32 = 5;

        let (src, _) = paths();
        let cause = io::Error::from_raw_os_error(ACCESS_DENIED);
        let text = cause.to_string();
        let (_, parsed) = round_trip(Err(OptimizeError::Io {
            path: src.clone(),
            cause,
        }));
        match parsed.unwrap_err() {
            OptimizeError::Io { path, cause } => {
                assert_eq!(path, src);
                assert_eq!(cause.raw_os_error(), Some(ACCESS_DENIED));
                assert_eq!(cause.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(cause.to_string(), text);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_an_io_error_without_an_os_code_back_with_its_text() {
        let (_, dest) = paths();
        let (_, parsed) = round_trip(Err(OptimizeError::Io {
            path: dest.clone(),
            cause: io::Error::other("disk on fire"),
        }));
        match parsed.unwrap_err() {
            OptimizeError::Io { path, cause } => {
                assert_eq!(path, dest);
                assert_eq!(cause.raw_os_error(), None);
                assert_eq!(cause.to_string(), "disk on fire");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keeps_a_message_within_the_limit_whole() {
        let (src, _) = paths();
        let message = "é".repeat(MAX_WORKER_MESSAGE_BYTES / 2);
        let (_, parsed) = round_trip(Err(OptimizeError::Invalid {
            path: src,
            message: message.clone(),
        }));
        assert!(
            matches!(parsed.unwrap_err(), OptimizeError::Invalid { message: m, .. } if m == message)
        );
    }

    #[test]
    fn cuts_a_message_over_the_limit_at_a_character_boundary() {
        let (src, _) = paths();
        // Two-byte characters, offset by one byte, so a cut at the byte limit
        // would land inside a character.
        let message = format!("x{}", "é".repeat(MAX_WORKER_MESSAGE_BYTES));
        let (record, parsed) = round_trip(Err(OptimizeError::Invalid { path: src, message }));
        assert!(
            record.len() < MAX_WORKER_MESSAGE_BYTES + 64,
            "{}",
            record.len()
        );
        match parsed.unwrap_err() {
            OptimizeError::Invalid { message, .. } => {
                assert!(
                    message.len() <= MAX_WORKER_MESSAGE_BYTES,
                    "{}",
                    message.len()
                );
                assert!(message.starts_with("xé"));
                assert!(message.ends_with('…'), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    fn worker_args() -> Vec<OsString> {
        let (src, dest) = paths();
        vec![src.into_os_string(), dest.into_os_string()]
    }

    #[test]
    fn exits_101_without_a_record_when_the_optimiser_panics() {
        let mut out = Vec::new();
        let code = serve_with(
            worker_args(),
            |_: &Path, _: &Path, _: Options| -> Result<Outcome, OptimizeError> {
                panic!("the optimiser hit a bug")
            },
            &mut out,
        );
        assert_eq!(code, ExitCode::from(101));
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn exits_0_with_exactly_the_record_when_the_optimiser_returns() {
        // The counterpart: the same call without a panic writes its record.
        let outcome = Outcome {
            before: 10,
            after: 4,
            millis: 2,
            note: None,
        };
        let mut out = Vec::new();
        let code = serve_with(
            worker_args(),
            move |_: &Path, _: &Path, _: Options| Ok(outcome),
            &mut out,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"lunaepix-worker 2 ok 10 4 2 -\n");
    }

    #[test]
    fn exits_1_with_the_error_record_when_the_optimiser_refuses_the_file() {
        let mut out = Vec::new();
        let code = serve_with(
            worker_args(),
            |src: &Path, _: &Path, _: Options| {
                Err(OptimizeError::UnsupportedType {
                    path: src.to_path_buf(),
                })
            },
            &mut out,
        );
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(out, b"lunaepix-worker 2 unsupported src\n");
    }

    #[test]
    fn exits_3_when_the_record_cannot_be_written() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let code = serve_with(
            worker_args(),
            |_: &Path, _: &Path, _: Options| {
                Ok(Outcome {
                    before: 1,
                    after: 1,
                    millis: 0,
                    note: None,
                })
            },
            &mut Closed,
        );
        assert_eq!(code, ExitCode::from(3));
    }

    #[test]
    fn rejects_anything_that_is_not_exactly_one_well_formed_record() {
        let (src, dest) = paths();
        let ok = "lunaepix-worker 2 ok 10 5 3 -\n";
        assert!(
            parse_record(ok.as_bytes(), &src, &dest).is_some(),
            "control"
        );

        let bad: &[&[u8]] = &[
            b"",
            b"\n",
            b"garbage\n",
            b"lunaepix-worker 2 ok 10 5 3 -", // no newline
            b"lunaepix-worker 2 ok 10 5 3 -\n\n",
            b"lunaepix-worker 2 ok 10 5 3 -\nlunaepix-worker 2 ok 10 5 3 -\n",
            b"hello\nlunaepix-worker 2 ok 10 5 3 -\n",
            b"lunaepix-worker 3 ok 10 5 3 -\n", // other version
            b"lunaepix-worker 2 ok 10 5 -\n",
            b"lunaepix-worker 2 ok 10 5 3 - 4\n",
            b"lunaepix-worker 2 ok +10 5 3 -\n",
            b"lunaepix-worker 2 ok -10 5 3 -\n",
            b"lunaepix-worker 2 ok 10  5 3 -\n",
            b"lunaepix-worker 2 ok 1e3 5 3 -\n",
            b"lunaepix-worker 2 ok 18446744073709551616 5 3 -\n", // u64::MAX + 1
            b"lunaepix-worker 2 ok 10 5 3 -\r\n",
            b"lunaepix-worker 2 maybe 10 5 3 -\n",
            b"lunaepix-worker 2 unsupported\n",
            b"lunaepix-worker 2 unsupported elsewhere\n",
            b"lunaepix-worker 2 unsupported src extra\n",
            b"lunaepix-worker 2 invalid\n",
            b"lunaepix-worker 2 invalid nowhere oops\n",
            b"lunaepix-worker 2 invalid src bad \\x escape\n",
            b"lunaepix-worker 2 invalid src trailing backslash \\\n",
            b"lunaepix-worker 2 io src\n",
            b"lunaepix-worker 2 io src 13\n",
            b"lunaepix-worker 2 io src x13 denied\n",
            b"lunaepix-worker 2 invalid src \xff\xfe not UTF-8\n",
        ];
        for bytes in bad {
            assert!(
                parse_record(bytes, &src, &dest).is_none(),
                "accepted {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    // --- record version 2: the note ---------------------------------------------

    use crate::Note;

    const NOTES: [Note; 4] = [
        Note::KeptLosslessAnimatedPng,
        Note::KeptLosslessCmyk,
        Note::KeptLosslessAnimationTooLarge,
        Note::OriginalKept,
    ];

    #[test]
    fn a_v2_ok_record_with_a_note_round_trips() {
        let (src, dest) = paths();
        let outcome = Outcome {
            before: 2048,
            after: 2048,
            millis: 7,
            note: Some(Note::OriginalKept),
        };
        assert_eq!(
            format_record(&Ok(outcome), &src, &dest),
            "lunaepix-worker 2 ok 2048 2048 7 original-kept\n"
        );
        for note in NOTES.map(Some).into_iter().chain([None]) {
            let outcome = Outcome {
                before: 300,
                after: 200,
                millis: 9,
                note,
            };
            let (record, parsed) = round_trip(Ok(outcome));
            assert_eq!(parsed.unwrap(), outcome, "{record:?}");
            let code = note.map_or("-", Note::code);
            assert_eq!(record, format!("lunaepix-worker 2 ok 300 200 9 {code}\n"));
        }
    }

    #[test]
    fn a_v1_record_is_refused() {
        let (src, dest) = paths();
        for record in [
            &b"lunaepix-worker 1 ok 10 5 3\n"[..],
            b"lunaepix-worker 1 ok 10 5 3 -\n",
            b"lunaepix-worker 1 ok 10 5 3 original-kept\n",
            b"lunaepix-worker 1 unsupported src\n",
            b"lunaepix-worker 1 invalid src nope\n",
            b"lunaepix-worker 1 io src - gone\n",
        ] {
            assert!(
                parse_record(record, &src, &dest).is_none(),
                "accepted {:?}",
                String::from_utf8_lossy(record)
            );
        }
        // The same records in version 2 are taken.
        for record in [
            &b"lunaepix-worker 2 ok 10 5 3 -\n"[..],
            b"lunaepix-worker 2 unsupported src\n",
            b"lunaepix-worker 2 invalid src nope\n",
            b"lunaepix-worker 2 io src - gone\n",
        ] {
            assert!(
                parse_record(record, &src, &dest).is_some(),
                "refused {:?}",
                String::from_utf8_lossy(record)
            );
        }
    }

    #[test]
    fn an_unknown_note_code_is_refused() {
        let (src, dest) = paths();
        for record in [
            &b"lunaepix-worker 2 ok 10 5 3 bogus\n"[..],
            b"lunaepix-worker 2 ok 10 5 3 APNG\n",
            b"lunaepix-worker 2 ok 10 5 3 original kept\n",
            b"lunaepix-worker 2 ok 10 5 3 --\n",
            b"lunaepix-worker 2 ok 10 5 3 \n",
            // The note field is not optional.
            b"lunaepix-worker 2 ok 10 5 3\n",
        ] {
            assert!(
                parse_record(record, &src, &dest).is_none(),
                "accepted {:?}",
                String::from_utf8_lossy(record)
            );
        }
        for note in NOTES {
            let record = format!("lunaepix-worker 2 ok 10 5 3 {}\n", note.code());
            let parsed = parse_record(record.as_bytes(), &src, &dest).unwrap();
            assert_eq!(parsed.unwrap().note, Some(note));
        }
    }

    // --- the worker's arguments ---------------------------------------------------

    use std::sync::{Arc, Mutex};

    use crate::{Compression, Quality, WriteMode};

    /// What the optimiser double was handed.
    type Handed = (PathBuf, PathBuf, Options);

    /// Run the worker with `args` and an optimiser double; the exit code, the
    /// record and what the double was handed.
    fn serve_recorded(args: &[&str]) -> (ExitCode, Vec<u8>, Vec<Handed>) {
        let handed = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&handed);
        let mut out = Vec::new();
        let code = serve_with(
            args.iter().map(OsString::from),
            move |src: &Path, dest: &Path, options: Options| {
                recorder
                    .lock()
                    .unwrap()
                    .push((src.to_path_buf(), dest.to_path_buf(), options));
                Ok(Outcome {
                    before: 10,
                    after: 4,
                    millis: 2,
                    note: None,
                })
            },
            &mut out,
        );
        let handed = handed.lock().unwrap().clone();
        (code, out, handed)
    }

    fn options(quality: Option<u8>, no_replace: bool) -> Options {
        Options {
            compression: match quality {
                Some(value) => Compression::Lossy(Quality::new(value).unwrap()),
                None => Compression::Lossless,
            },
            write: if no_replace {
                WriteMode::NoReplace
            } else {
                WriteMode::Replace
            },
        }
    }

    #[test]
    fn the_worker_hands_the_parsed_options_to_the_optimiser() {
        let cases: [(&[&str], Options); 7] = [
            (&[], options(None, false)),
            (&["--lossy", "0"], options(Some(0), false)),
            (&["--lossy", "75"], options(Some(75), false)),
            (&["--lossy", "100"], options(Some(100), false)),
            (&["--no-replace"], options(None, true)),
            (&["--lossy", "40", "--no-replace"], options(Some(40), true)),
            (&["--lossy", "075"], options(Some(75), false)),
        ];
        for (flags, expected) in cases {
            let mut args = flags.to_vec();
            args.extend(["/photos/a.png", "/photos/optimized-a.png"]);

            let (code, out, handed) = serve_recorded(&args);

            assert_eq!(code, ExitCode::SUCCESS, "{flags:?}");
            assert_eq!(out, b"lunaepix-worker 2 ok 10 4 2 -\n", "{flags:?}");
            assert_eq!(
                handed,
                [(
                    PathBuf::from("/photos/a.png"),
                    PathBuf::from("/photos/optimized-a.png"),
                    expected
                )],
                "{flags:?}"
            );
        }
        // The last two arguments are the paths, even when they look like
        // options.
        let (code, _, handed) = serve_recorded(&["--lossy", "50", "--no-replace", "--lossy"]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            handed,
            [(
                PathBuf::from("--no-replace"),
                PathBuf::from("--lossy"),
                options(Some(50), false)
            )]
        );
    }

    #[test]
    fn the_worker_refuses_options_out_of_order_and_never_calls_the_optimiser() {
        let calls: [&[&str]; 13] = [
            &["--no-replace", "--lossy", "75"],
            &["--lossy"],
            &["--lossy", "-1"],
            &["--lossy", "101"],
            &["--lossy", "75.5"],
            &["--lossy", "abc"],
            &["--lossy", ""],
            &["--quality", "75"],
            &["--lossy", "75", "--lossy", "75"],
            &["--no-replace", "--no-replace"],
            &["--lossy=75"],
            &["--lossy", "75", "--fast"],
            &["--fast"],
        ];
        for flags in calls {
            let mut args = flags.to_vec();
            args.extend(["/photos/a.png", "/photos/optimized-a.png"]);

            let (code, out, handed) = serve_recorded(&args);

            assert_eq!(code, ExitCode::from(EXIT_USAGE), "{flags:?}");
            assert!(out.is_empty(), "{flags:?}");
            assert!(handed.is_empty(), "{flags:?}");
        }
        // Fewer than two paths.
        for args in [
            &[][..],
            &["/photos/a.png"],
            &["--lossy", "75", "/photos/a.png"],
        ] {
            let (code, out, handed) = serve_recorded(args);
            assert_eq!(code, ExitCode::from(EXIT_USAGE), "{args:?}");
            assert!(out.is_empty() && handed.is_empty(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_worker_refuses_a_quality_that_is_not_utf8() {
        use std::os::unix::ffi::OsStringExt;

        let calls = Arc::new(Mutex::new(0));
        let recorder = Arc::clone(&calls);
        let mut out = Vec::new();
        let args = vec![
            OsString::from("--lossy"),
            OsString::from_vec(b"7\xff".to_vec()),
            OsString::from("/photos/a.png"),
            OsString::from("/photos/optimized-a.png"),
        ];
        let code = serve_with(
            args,
            move |_: &Path, _: &Path, _: Options| {
                *recorder.lock().unwrap() += 1;
                Err(OptimizeError::UnsupportedType {
                    path: PathBuf::new(),
                })
            },
            &mut out,
        );
        assert_eq!(code, ExitCode::from(EXIT_USAGE));
        assert!(out.is_empty());
        assert_eq!(*calls.lock().unwrap(), 0);
        // The same call with a UTF-8 quality reaches the optimiser.
        let (code, _, handed) =
            serve_recorded(&["--lossy", "7", "/photos/a.png", "/photos/optimized-a.png"]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(handed.len(), 1);
    }

    // --- the thumbnail mode -----------------------------------------------------

    const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    /// A thumbnail that starts like a PNG (the worker does not look inside).
    fn png_thumb() -> Vec<u8> {
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend_from_slice(b"pretend pixels");
        bytes
    }

    /// Run the thumbnail mode with `args` and a thumbnailer double that
    /// answers `answer`; the exit code, what was written, and the paths the
    /// double was handed.
    fn serve_thumb_recorded(
        args: Vec<OsString>,
        answer: Result<Thumbnail, CannotShow>,
    ) -> (ExitCode, Vec<u8>, Vec<PathBuf>) {
        let handed = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&handed);
        let mut out = Vec::new();
        let code = serve_thumb_with(
            args,
            move |src: &Path| {
                recorder.lock().unwrap().push(src.to_path_buf());
                answer
            },
            &mut out,
        );
        let handed = handed.lock().unwrap().clone();
        (code, out, handed)
    }

    fn one_source() -> Vec<OsString> {
        vec![OsString::from("/photos/a b.png")]
    }

    #[test]
    fn thumb_serves_a_png_as_the_png_header_and_the_bytes() {
        let bytes = png_thumb();
        let (code, out, _) = serve_thumb_recorded(one_source(), Ok(Thumbnail::Png(bytes.clone())));
        assert_eq!(code, ExitCode::SUCCESS);
        let mut expected = b"lunaepix-thumb 1 png\n".to_vec();
        expected.extend_from_slice(&bytes);
        assert_eq!(out, expected);
    }

    #[test]
    fn thumb_serves_an_svg_under_the_svg_header_not_the_png_one() {
        let bytes = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
        let (code, out, _) = serve_thumb_recorded(one_source(), Ok(Thumbnail::Svg(bytes.clone())));
        assert_eq!(code, ExitCode::SUCCESS);
        let mut expected = b"lunaepix-thumb 1 svg\n".to_vec();
        expected.extend_from_slice(&bytes);
        assert_eq!(out, expected);
        assert!(!out.starts_with(b"lunaepix-thumb 1 png"));
    }

    #[test]
    fn thumb_exits_3_and_writes_nothing_when_the_file_cannot_be_shown() {
        let (code, out, handed) =
            serve_thumb_recorded(one_source(), Err(CannotShow("not a picture".to_owned())));
        assert_eq!(code, ExitCode::from(3));
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
        assert_eq!(handed.len(), 1, "it was asked, and said no");
    }

    #[test]
    fn thumb_hands_the_one_argument_to_the_thumbnailer_exactly() {
        let mut sources = vec![
            OsString::from("--looks-like-an-option"),
            OsString::from("/photos/Mobilis Logo – zwart é.png"),
        ];
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            sources.push(OsString::from_vec(b"/photos/caf\xE9.png".to_vec()));
        }
        // The Windows counterpart: a lone UTF-16 surrogate is not Unicode.
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let mut wide: Vec<u16> = "C:\\photos\\caf".encode_utf16().collect();
            wide.push(0xD800);
            wide.extend(".png".encode_utf16());
            sources.push(OsString::from_wide(&wide));
        }
        for source in sources {
            let (code, _, handed) =
                serve_thumb_recorded(vec![source.clone()], Ok(Thumbnail::Png(png_thumb())));
            assert_eq!(code, ExitCode::SUCCESS, "{source:?}");
            assert_eq!(handed, [PathBuf::from(&source)], "{source:?}");
        }
    }

    #[test]
    fn thumb_refuses_no_source_two_sources_and_an_option_before_the_source_with_exit_2() {
        let calls: [Vec<OsString>; 3] = [
            vec![],
            vec![
                OsString::from("/photos/a.png"),
                OsString::from("/photos/b.png"),
            ],
            vec![OsString::from("--bogus"), OsString::from("/photos/a.png")],
        ];
        for args in calls {
            let (code, out, handed) =
                serve_thumb_recorded(args.clone(), Ok(Thumbnail::Png(png_thumb())));
            assert_eq!(code, ExitCode::from(EXIT_USAGE), "{args:?}");
            assert!(out.is_empty(), "{args:?}");
            assert!(handed.is_empty(), "{args:?}: the thumbnailer must not run");
        }
    }

    #[test]
    fn thumb_exits_101_without_an_answer_when_the_thumbnailer_panics() {
        let mut out = Vec::new();
        let code = serve_thumb_with(
            one_source(),
            |_: &Path| -> Result<Thumbnail, CannotShow> { panic!("the decoder hit a bug") },
            &mut out,
        );
        assert_eq!(code, ExitCode::from(EXIT_PANICKED));
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
    }

    /// A writer that fails on every write.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn thumb_exits_1_not_3_when_the_answer_cannot_be_written() {
        // Exit 3 means "cannot be shown" to the runner, so a failed write
        // must not look like it.
        let code = serve_thumb_with(
            one_source(),
            |_: &Path| Ok(Thumbnail::Png(png_thumb())),
            &mut ClosedPipe,
        );
        assert_eq!(code, ExitCode::from(EXIT_FAILED));
        assert_ne!(code, ExitCode::from(3));
    }

    /// `usize::MAX` is a stack size Linux refuses (`EINVAL`), so the thread
    /// cannot start; this is checked to give an `Err`, not a panic.
    #[cfg(target_os = "linux")]
    #[test]
    fn thumb_exits_1_not_3_and_writes_nothing_when_its_thread_cannot_start() {
        let calls = Arc::new(Mutex::new(0));
        let recorder = Arc::clone(&calls);
        let mut out = Vec::new();
        let code = serve_thumb_on_stack(
            one_source(),
            move |_: &Path| {
                *recorder.lock().unwrap() += 1;
                Ok(Thumbnail::Png(png_thumb()))
            },
            &mut out,
            usize::MAX,
        );
        assert_eq!(code, ExitCode::from(EXIT_FAILED));
        assert_ne!(code, ExitCode::from(3));
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
        assert_eq!(*calls.lock().unwrap(), 0, "the thumbnailer must not run");

        // The pair: the same call on the normal stack answers.
        let mut out = Vec::new();
        let code = serve_thumb_on_stack(
            one_source(),
            |_: &Path| Ok(Thumbnail::Png(png_thumb())),
            &mut out,
            WORKER_STACK_BYTES,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(out.starts_with(b"lunaepix-thumb 1 png\n"));
    }

    #[test]
    fn parses_a_png_answer_and_an_svg_answer() {
        let png = png_thumb();
        let mut answer = b"lunaepix-thumb 1 png\n".to_vec();
        answer.extend_from_slice(&png);
        assert_eq!(parse_thumb_answer(&answer), Some(Thumbnail::Png(png)));

        let svg = b"<svg/>".to_vec();
        let mut answer = b"lunaepix-thumb 1 svg\n".to_vec();
        answer.extend_from_slice(&svg);
        assert_eq!(parse_thumb_answer(&answer), Some(Thumbnail::Svg(svg)));
    }

    #[test]
    fn rejects_a_wrong_version_no_header_an_unknown_kind_and_a_png_header_with_other_bytes() {
        let png = png_thumb();
        let with = |header: &[u8], bytes: &[u8]| [header, bytes].concat();
        // Control: the right header and bytes parse.
        assert!(parse_thumb_answer(&with(b"lunaepix-thumb 1 png\n", &png)).is_some());

        let bad: Vec<(&str, Vec<u8>)> = vec![
            ("empty", vec![]),
            ("no header", png.clone()),
            ("version 2", with(b"lunaepix-thumb 2 png\n", &png)),
            ("version 0", with(b"lunaepix-thumb 0 png\n", &png)),
            ("unknown kind", with(b"lunaepix-thumb 1 gif\n", &png)),
            ("upper-case kind", with(b"lunaepix-thumb 1 PNG\n", &png)),
            ("no newline", with(b"lunaepix-thumb 1 png", &png)),
            ("carriage return", with(b"lunaepix-thumb 1 png\r\n", &png)),
            ("header only", b"lunaepix-thumb 1 png\n".to_vec()),
            (
                "png header, text bytes",
                with(b"lunaepix-thumb 1 png\n", b"<svg/>"),
            ),
            (
                "png header, a truncated signature",
                with(b"lunaepix-thumb 1 png\n", &PNG_SIGNATURE[..7]),
            ),
            (
                "svg header, nothing after it",
                b"lunaepix-thumb 1 svg\n".to_vec(),
            ),
        ];
        for (what, bytes) in bad {
            assert_eq!(parse_thumb_answer(&bytes), None, "{what}");
        }
    }

    #[test]
    fn the_thumbnail_header_length_is_that_of_both_headers() {
        assert_eq!(b"lunaepix-thumb 1 png\n".len(), THUMB_HEADER_LEN);
        assert_eq!(b"lunaepix-thumb 1 svg\n".len(), THUMB_HEADER_LEN);
    }

    // --- the probe mode -----------------------------------------------------------

    fn size_of(width: u32, height: u32) -> Result<Probe, String> {
        Ok(Probe {
            width: Some(width),
            height: Some(height),
        })
    }

    /// Run the probe mode with `args` and a prober double that answers
    /// `answer`; the exit code, what was written, and the paths the double
    /// was handed.
    fn serve_probe_recorded(
        args: Vec<OsString>,
        answer: Result<Probe, String>,
    ) -> (ExitCode, Vec<u8>, Vec<PathBuf>) {
        let handed = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&handed);
        let mut out = Vec::new();
        let code = serve_probe_with(
            args,
            move |src: &Path| {
                recorder.lock().unwrap().push(src.to_path_buf());
                answer
            },
            &mut out,
        );
        let handed = handed.lock().unwrap().clone();
        (code, out, handed)
    }

    #[test]
    fn the_probe_flag_is_the_documented_one() {
        assert_eq!(PROBE_FLAG, "--lunaepix-probe");
    }

    #[test]
    fn probe_serves_a_size_as_one_record_and_exits_0() {
        let (code, out, _) = serve_probe_recorded(one_source(), size_of(300, 200));
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"lunaepix-probe 1 size 300 200\n");
        // An SVG has no size: two dashes.
        let none = Probe {
            width: None,
            height: None,
        };
        let (code, out, _) = serve_probe_recorded(one_source(), Ok(none));
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"lunaepix-probe 1 size - -\n");
    }

    #[test]
    fn probe_serves_a_refusal_as_one_record_and_exits_3() {
        let (code, out, handed) =
            serve_probe_recorded(one_source(), Err("File is too large".to_owned()));
        assert_eq!(code, ExitCode::from(3));
        assert_eq!(out, b"lunaepix-probe 1 refused File is too large\n");
        assert_eq!(handed.len(), 1);
    }

    #[test]
    fn probe_escapes_backslash_and_line_breaks_in_a_refusal() {
        let (code, out, _) = serve_probe_recorded(one_source(), Err("a\\b\nc\rd".to_owned()));
        assert_eq!(code, ExitCode::from(3));
        assert_eq!(out, b"lunaepix-probe 1 refused a\\\\b\\nc\\rd\n");
        // One line: the only line feed is the last byte.
        assert_eq!(out.iter().filter(|&&byte| byte == b'\n').count(), 1);
        assert!(!out.contains(&b'\r'));
    }

    #[test]
    fn probe_cuts_a_long_refusal_at_max_worker_message_bytes() {
        let prefix = "lunaepix-probe 1 refused ";
        let long = "é".repeat(MAX_WORKER_MESSAGE_BYTES); // twice the cap in bytes
        let (_, out, _) = serve_probe_recorded(one_source(), Err(long));
        let record = String::from_utf8(out).unwrap();
        let message = record
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix('\n'))
            .unwrap();
        assert!(
            message.len() <= MAX_WORKER_MESSAGE_BYTES,
            "{}",
            message.len()
        );
        assert!(message.ends_with('…'), "{message}");
        // Exactly at the cap nothing is cut.
        let exact = "x".repeat(MAX_WORKER_MESSAGE_BYTES);
        let (_, out, _) = serve_probe_recorded(one_source(), Err(exact.clone()));
        assert_eq!(out, format!("{prefix}{exact}\n").into_bytes());
    }

    #[test]
    fn probe_hands_the_one_argument_to_the_prober_exactly() {
        let mut sources = vec![
            OsString::from("--looks-like-an-option"),
            OsString::from("/photos/Mobilis Logo – zwart é.png"),
        ];
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            sources.push(OsString::from_vec(b"/photos/caf\xE9.png".to_vec()));
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let mut wide: Vec<u16> = "C:\\photos\\caf".encode_utf16().collect();
            wide.push(0xD800);
            wide.extend(".png".encode_utf16());
            sources.push(OsString::from_wide(&wide));
        }
        for source in sources {
            let (code, _, handed) = serve_probe_recorded(vec![source.clone()], size_of(1, 1));
            assert_eq!(code, ExitCode::SUCCESS, "{source:?}");
            assert_eq!(handed, [PathBuf::from(&source)], "{source:?}");
        }
    }

    #[test]
    fn probe_refuses_no_source_two_sources_and_an_option_before_the_source_with_exit_2() {
        let calls: [Vec<OsString>; 3] = [
            vec![],
            vec![
                OsString::from("/photos/a.png"),
                OsString::from("/photos/b.png"),
            ],
            vec![OsString::from("--bogus"), OsString::from("/photos/a.png")],
        ];
        for args in calls {
            let (code, out, handed) = serve_probe_recorded(args.clone(), size_of(1, 1));
            assert_eq!(code, ExitCode::from(EXIT_USAGE), "{args:?}");
            assert!(out.is_empty(), "{args:?}");
            assert!(handed.is_empty(), "{args:?}: the prober must not run");
        }
    }

    #[test]
    fn probe_exits_101_without_an_answer_when_the_prober_panics() {
        let mut out = Vec::new();
        let code = serve_probe_with(
            one_source(),
            |_: &Path| -> Result<Probe, String> { panic!("the decoder hit a bug") },
            &mut out,
        );
        assert_eq!(code, ExitCode::from(EXIT_PANICKED));
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn probe_exits_1_not_3_when_the_answer_cannot_be_written() {
        // Exit 3 means "refused" to the runner, so a failed write must not
        // look like it; the same goes for a refusal that cannot be written.
        for answer in [size_of(3, 2), Err("File is too large".to_owned())] {
            let code = serve_probe_with(one_source(), move |_: &Path| answer, &mut ClosedPipe);
            assert_eq!(code, ExitCode::from(EXIT_FAILED));
            assert_ne!(code, ExitCode::from(3));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_exits_1_not_3_and_writes_nothing_when_its_thread_cannot_start() {
        let calls = Arc::new(Mutex::new(0));
        let recorder = Arc::clone(&calls);
        let mut out = Vec::new();
        let code = serve_probe_on_stack(
            one_source(),
            move |_: &Path| {
                *recorder.lock().unwrap() += 1;
                size_of(3, 2)
            },
            &mut out,
            usize::MAX,
        );
        assert_eq!(code, ExitCode::from(EXIT_FAILED));
        assert_ne!(code, ExitCode::from(3));
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
        assert_eq!(*calls.lock().unwrap(), 0, "the prober must not run");

        // The pair: the same call on the normal stack answers.
        let mut out = Vec::new();
        let code = serve_probe_on_stack(
            one_source(),
            |_: &Path| size_of(3, 2),
            &mut out,
            WORKER_STACK_BYTES,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"lunaepix-probe 1 size 3 2\n");
    }

    #[test]
    fn parse_probe_answer_reads_a_size_and_a_refusal() {
        let size = parse_probe_answer(b"lunaepix-probe 1 size 300 200\n");
        assert_eq!(size, Some(size_of(300, 200)));
        let none = parse_probe_answer(b"lunaepix-probe 1 size - -\n");
        assert_eq!(
            none,
            Some(Ok(Probe {
                width: None,
                height: None
            }))
        );
        let refused = parse_probe_answer(b"lunaepix-probe 1 refused File is too large\n");
        assert_eq!(refused, Some(Err("File is too large".to_owned())));
        // The escapes are undone.
        let escaped = parse_probe_answer(b"lunaepix-probe 1 refused a\\\\b\\nc\\rd\n");
        assert_eq!(escaped, Some(Err("a\\b\nc\rd".to_owned())));
    }

    #[test]
    fn a_record_the_worker_writes_parses_back_to_what_it_was_given() {
        let answers = [
            size_of(4032, 3024),
            size_of(u32::MAX, 1),
            Ok(Probe {
                width: None,
                height: None,
            }),
            Err("could not read the PNG data: it has more than one IHDR chunk".to_owned()),
            Err("a\\b\nc\rd … é".to_owned()),
        ];
        for answer in answers {
            let record = format_probe_answer(&answer);
            assert_eq!(
                parse_probe_answer(record.as_bytes()),
                Some(answer.clone()),
                "{record:?}"
            );
        }
    }

    #[test]
    fn parse_probe_answer_rejects_what_is_not_exactly_one_valid_record() {
        // Control: the valid ones parse.
        assert!(parse_probe_answer(b"lunaepix-probe 1 size 3 2\n").is_some());
        assert!(parse_probe_answer(b"lunaepix-probe 1 refused x\n").is_some());
        let bad: [(&str, &[u8]); 21] = [
            ("empty", b""),
            ("version 2", b"lunaepix-probe 2 size 3 2\n"),
            ("version 0", b"lunaepix-probe 0 size 3 2\n"),
            ("another magic word", b"lunaepix-thumb 1 size 3 2\n"),
            ("no newline", b"lunaepix-probe 1 size 3 2"),
            ("carriage return", b"lunaepix-probe 1 size 3 2\r\n"),
            (
                "two lines",
                b"lunaepix-probe 1 size 3 2\nlunaepix-probe 1 size 3 2\n",
            ),
            ("trailing bytes", b"lunaepix-probe 1 size 3 2\nx"),
            ("unknown kind", b"lunaepix-probe 1 sizes 3 2\n"),
            ("upper-case kind", b"lunaepix-probe 1 SIZE 3 2\n"),
            ("one number", b"lunaepix-probe 1 size 3\n"),
            ("three numbers", b"lunaepix-probe 1 size 3 2 1\n"),
            ("one dash", b"lunaepix-probe 1 size - 2\n"),
            ("one dash second", b"lunaepix-probe 1 size 3 -\n"),
            ("a sign", b"lunaepix-probe 1 size +3 2\n"),
            ("a minus", b"lunaepix-probe 1 size -3 2\n"),
            ("letters", b"lunaepix-probe 1 size x 2\n"),
            ("over u32", b"lunaepix-probe 1 size 4294967296 2\n"),
            ("no message", b"lunaepix-probe 1 refused\n"),
            ("an empty message", b"lunaepix-probe 1 refused \n"),
            ("a bad escape", b"lunaepix-probe 1 refused a\\x\n"),
        ];
        for (what, bytes) in bad {
            assert_eq!(parse_probe_answer(bytes), None, "{what}");
        }
        // Not UTF-8.
        assert_eq!(
            parse_probe_answer(b"lunaepix-probe 1 refused \xFF\xFE\n"),
            None
        );
    }
}
