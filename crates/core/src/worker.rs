//! Worker mode: optimise one file in this process and report the result on
//! stdout.
//!
//! The batch runner ([`crate::run`]) starts a binary once per file as
//! `<exe> --lunaepix-worker <src> <dest>`, so that a library that crashes the
//! process (a stack overflow aborts, and cannot be caught) takes down only
//! that one file. The CLI and the app call [`serve_if_requested`] first thing
//! in `main`, so both binaries answer the worker mode.
//!
//! # The result record
//!
//! The worker writes exactly one line of UTF-8 to stdout, and nothing else:
//!
//! ```text
//! lunaepix-worker 1 ok <before> <after> <millis>
//! lunaepix-worker 1 unsupported <role>
//! lunaepix-worker 1 invalid <role> <message>
//! lunaepix-worker 1 io <role> <os-error-code or -> <message>
//! ```
//!
//! `1` is the format version. The numbers are plain decimal digits. `<role>`
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

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;

use crate::{MAX_WORKER_MESSAGE_BYTES, OptimizeError, Outcome, optimize_file, platform};

/// The hidden first argument that starts a binary in worker mode.
pub const WORKER_FLAG: &str = "--lunaepix-worker";

/// Start of every record: a magic word and the format version.
const RECORD_PREFIX: &str = "lunaepix-worker 1 ";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_NO_RECORD: u8 = 3;
/// What Rust itself exits with after a panic on the main thread.
const EXIT_PANICKED: u8 = 101;

/// Stack for the thread that optimises: 8 MiB, the main-thread stack of
/// Linux and macOS. Windows gives its main thread 1 MiB, which an SVG at the
/// depth limit can exceed in a debug build (see `MAX_SVG_DEPTH`), so the
/// worker sets the size itself and every system gets the same.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

/// If this process was started as `<exe> --lunaepix-worker ...`, run the
/// worker and return its exit code; `main` returns it at once. Otherwise
/// `None`, and `main` carries on as usual. The arguments are read with
/// `args_os`, so paths that are not UTF-8 arrive intact.
pub fn serve_if_requested() -> Option<ExitCode> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(WORKER_FLAG)) {
        return None;
    }
    Some(serve(args))
}

/// Optimise one file and write the result record to stdout. `args` are the
/// arguments after the flag, which must be exactly `<src> <dest>`.
///
/// First the worker sets itself up for its parent: on Linux it asks to be
/// killed when the thread that started it ends (so closing the window stops
/// it), then raises its own OOM score, so that when memory runs out the
/// kernel kills the worker rather than the window; on Windows it turns off
/// the crash dialogs. The file is optimised on a thread with a fixed 8 MiB
/// stack.
pub fn serve(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    // Before the OOM score: the tests read a score of 1000 as "ready".
    platform::die_with_parent();
    raise_oom_score();
    platform::suppress_crash_dialogs();
    serve_with(
        args,
        |src: &Path, dest: &Path| optimize_file(src, dest),
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
    F: FnOnce(&Path, &Path) -> Result<Outcome, OptimizeError> + Send + 'static,
{
    let mut args = args.into_iter();
    let (Some(src), Some(dest), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: <program> {WORKER_FLAG} <source> <destination>");
        return ExitCode::from(EXIT_USAGE);
    };
    let (src, dest) = (PathBuf::from(src), PathBuf::from(dest));

    let job = {
        let (src, dest) = (src.clone(), dest.clone());
        move || optimise(&src, &dest)
    };
    let spawned = thread::Builder::new()
        .name("optimize".to_owned())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(job);
    let result = match spawned.map(thread::JoinHandle::join) {
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
        Ok(outcome) => format!("ok {} {} {}", outcome.before, outcome.after, outcome.millis),
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
        };
        assert_eq!(
            format_record(&Ok(outcome), &src, &dest),
            "lunaepix-worker 1 ok 2048 1024 7\n"
        );
    }

    #[test]
    fn parses_an_outcome_back_exactly() {
        let outcome = Outcome {
            before: u64::MAX,
            after: 0,
            millis: 123_456,
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
        let (src, _) = paths();
        let cause = io::Error::from_raw_os_error(13);
        let text = cause.to_string();
        let (_, parsed) = round_trip(Err(OptimizeError::Io {
            path: src.clone(),
            cause,
        }));
        match parsed.unwrap_err() {
            OptimizeError::Io { path, cause } => {
                assert_eq!(path, src);
                assert_eq!(cause.raw_os_error(), Some(13));
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
            |_: &Path, _: &Path| -> Result<Outcome, OptimizeError> {
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
        };
        let mut out = Vec::new();
        let code = serve_with(
            worker_args(),
            move |_: &Path, _: &Path| Ok(outcome),
            &mut out,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"lunaepix-worker 1 ok 10 4 2\n");
    }

    #[test]
    fn exits_1_with_the_error_record_when_the_optimiser_refuses_the_file() {
        let mut out = Vec::new();
        let code = serve_with(
            worker_args(),
            |src: &Path, _: &Path| {
                Err(OptimizeError::UnsupportedType {
                    path: src.to_path_buf(),
                })
            },
            &mut out,
        );
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(out, b"lunaepix-worker 1 unsupported src\n");
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
            |_: &Path, _: &Path| {
                Ok(Outcome {
                    before: 1,
                    after: 1,
                    millis: 0,
                })
            },
            &mut Closed,
        );
        assert_eq!(code, ExitCode::from(3));
    }

    #[test]
    fn rejects_anything_that_is_not_exactly_one_well_formed_record() {
        let (src, dest) = paths();
        let ok = "lunaepix-worker 1 ok 10 5 3\n";
        assert!(
            parse_record(ok.as_bytes(), &src, &dest).is_some(),
            "control"
        );

        let bad: &[&[u8]] = &[
            b"",
            b"\n",
            b"garbage\n",
            b"lunaepix-worker 1 ok 10 5 3", // no newline
            b"lunaepix-worker 1 ok 10 5 3\n\n",
            b"lunaepix-worker 1 ok 10 5 3\nlunaepix-worker 1 ok 10 5 3\n",
            b"hello\nlunaepix-worker 1 ok 10 5 3\n",
            b"lunaepix-worker 2 ok 10 5 3\n", // other version
            b"lunaepix-worker 1 ok 10 5\n",
            b"lunaepix-worker 1 ok 10 5 3 4\n",
            b"lunaepix-worker 1 ok +10 5 3\n",
            b"lunaepix-worker 1 ok -10 5 3\n",
            b"lunaepix-worker 1 ok 10  5 3\n",
            b"lunaepix-worker 1 ok 1e3 5 3\n",
            b"lunaepix-worker 1 ok 18446744073709551616 5 3\n", // u64::MAX + 1
            b"lunaepix-worker 1 ok 10 5 3\r\n",
            b"lunaepix-worker 1 maybe 10 5 3\n",
            b"lunaepix-worker 1 unsupported\n",
            b"lunaepix-worker 1 unsupported elsewhere\n",
            b"lunaepix-worker 1 unsupported src extra\n",
            b"lunaepix-worker 1 invalid\n",
            b"lunaepix-worker 1 invalid nowhere oops\n",
            b"lunaepix-worker 1 invalid src bad \\x escape\n",
            b"lunaepix-worker 1 invalid src trailing backslash \\\n",
            b"lunaepix-worker 1 io src\n",
            b"lunaepix-worker 1 io src 13\n",
            b"lunaepix-worker 1 io src x13 denied\n",
            b"lunaepix-worker 1 invalid src \xff\xfe not UTF-8\n",
        ];
        for bytes in bad {
            assert!(
                parse_record(bytes, &src, &dest).is_none(),
                "accepted {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }
}
