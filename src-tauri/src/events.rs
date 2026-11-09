//! What the window receives: the event names and their JSON payloads.
//!
//! Every payload is plain data. A file is named by its file name only, never
//! by its path, and a failure by the core's bare reason, never by the
//! worker's stderr. `INTERFACE.md` documents the same shapes for the window.

use std::path::Path;

use lunaepix_core::{FileResult, MAX_RUN_FILES};
use serde::Serialize;

/// A run has been planned and starts now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStarted {
    /// Number of `file-done` events that follow: one per planned file
    /// (skipped files included), except that the files the run limit left
    /// out share one row ([`over_limit_row`]).
    pub total: usize,
}

/// How one file ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Done,
    Skipped,
    Failed,
}

/// One file of the run is finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDone {
    /// Position in the run, from 0 to `total - 1`, in run order.
    pub index: usize,
    /// The file name alone, lossily decoded. Empty if the path has none.
    pub name: String,
    pub status: Status,
    /// Sizes in bytes and the time in ms: only when `status` is `done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
    /// Why the file was skipped or failed: only when `status` is not `done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The run is over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunFinished {
    /// Number of files optimised and written.
    pub optimised: usize,
    /// Bytes saved over the optimised files. A file that grew counts 0.
    pub saved_bytes: u64,
    /// Whether `open_output_folder` now has a folder to open: the same
    /// output folder the backend stores (see `session::Tally`).
    pub has_output: bool,
}

/// Whether files are being dragged over the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DragState {
    pub over: bool,
}

/// One event for the window: its name ([`Event::name`]) and its payload
/// (the `Serialize` output; the two unit events have `null`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Event {
    RunStarted(RunStarted),
    FileDone(FileDone),
    RunFinished(RunFinished),
    /// A drop or a pick was refused because a run is active.
    RunBusy,
    /// A file or folder dialog was closed without a choice.
    RunCancelled,
    DragState(DragState),
}

impl Event {
    /// The event name the window listens to.
    pub fn name(&self) -> &'static str {
        match self {
            Event::RunStarted(_) => "run-started",
            Event::FileDone(_) => "file-done",
            Event::RunFinished(_) => "run-finished",
            Event::RunBusy => "run-busy",
            Event::RunCancelled => "run-cancelled",
            Event::DragState(_) => "drag-state",
        }
    }
}

/// The name of the one row that stands for the files the run limit left out:
/// a placeholder in the style of `(no name)`, so that it is never taken for a
/// file name.
pub const OVER_LIMIT_NAME: &str = "(over the limit)";

/// The `file-done` payload of row `index`, the one row that stands for the
/// `count` files the run limit left out, instead of a row for each.
pub fn over_limit_row(index: usize, count: usize) -> FileDone {
    let files = if count == 1 { "file" } else { "files" };
    FileDone {
        index,
        name: OVER_LIMIT_NAME.to_owned(),
        status: Status::Skipped,
        before: None,
        after: None,
        millis: None,
        reason: Some(format!(
            "{count} more {files} not optimised: over the {MAX_RUN_FILES}-file limit"
        )),
    }
}

/// The `file-done` payload for job `index`, whose source is `src`.
///
/// A failure gives [`RunError::reason`](lunaepix_core::RunError::reason),
/// which carries neither a path nor the worker's stderr.
pub fn file_done(index: usize, src: &Path, result: &FileResult) -> FileDone {
    let mut payload = FileDone {
        index,
        name: src
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        status: Status::Done,
        before: None,
        after: None,
        millis: None,
        reason: None,
    };
    match result {
        FileResult::Optimized(outcome) => {
            payload.before = Some(outcome.before);
            payload.after = Some(outcome.after);
            payload.millis = Some(outcome.millis);
        }
        FileResult::Skipped(skip) => {
            payload.status = Status::Skipped;
            payload.reason = Some(skip.to_string());
        }
        FileResult::Failed(error) => {
            payload.status = Status::Failed;
            payload.reason = Some(error.reason());
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use lunaepix_core::{Crash, OptimizeError, Outcome, RunError, Skip};

    use super::*;

    fn optimized(before: u64, after: u64) -> FileResult {
        FileResult::Optimized(Outcome {
            before,
            after,
            millis: 7,
            note: None,
        })
    }

    fn json(value: &impl Serialize) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn a_done_file_carries_its_sizes_and_time_and_no_reason() {
        let src = Path::new("/home/someone/Pictures/logo black.svg");
        let payload = file_done(3, src, &optimized(100, 60));
        assert_eq!(
            json(&payload),
            r#"{"index":3,"name":"logo black.svg","status":"done","before":100,"after":60,"millis":7}"#
        );
    }

    #[test]
    fn a_failed_file_carries_the_bare_reason_and_no_stderr_path_or_sizes() {
        let folder = "/home/someone/private-folder";
        let src = PathBuf::from(format!("{folder}/broken.svg"));
        let result = FileResult::Failed(RunError::Crashed {
            path: src.clone(),
            cause: Crash::Signal(6),
            stderr: format!("thread panicked while reading {folder}/broken.svg"),
        });
        let payload = file_done(0, &src, &result);
        assert_eq!(payload.status, Status::Failed);
        assert_eq!(
            payload.reason.as_deref(),
            Some(
                "the optimiser crashed on this file (killed by signal 6, SIGABRT, for example a stack overflow)"
            )
        );
        let text = json(&payload);
        assert!(!text.contains(folder), "{text}");
        assert!(!text.contains("panicked"), "{text}");
        assert!(
            !text.contains("before") && !text.contains("after"),
            "{text}"
        );
        assert!(!text.contains("millis"), "{text}");
    }

    #[test]
    fn an_optimiser_error_about_the_destination_does_not_leak_its_path() {
        let src = Path::new("/srv/photos/a.png");
        let result = FileResult::Failed(RunError::Optimize(OptimizeError::Io {
            path: PathBuf::from("/srv/photos/optimized/a.png"),
            cause: io::Error::other("disk full"),
        }));
        let text = json(&file_done(1, src, &result));
        assert_eq!(
            text,
            r#"{"index":1,"name":"a.png","status":"failed","reason":"disk full"}"#
        );
    }

    #[test]
    fn a_skipped_file_carries_the_skip_reason_and_no_sizes() {
        let src = Path::new("/srv/photos/notes.txt");
        let text = json(&file_done(
            2,
            src,
            &FileResult::Skipped(Skip::UnsupportedType),
        ));
        assert_eq!(
            text,
            r#"{"index":2,"name":"notes.txt","status":"skipped","reason":"Not a PNG, JPG, SVG or WebP file"}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_decoded_lossily() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let src = Path::new(OsStr::from_bytes(b"/tmp/caf\xe9.png"));
        assert_eq!(file_done(0, src, &optimized(2, 1)).name, "caf\u{fffd}.png");
    }

    #[test]
    fn each_event_has_its_own_name_and_a_data_only_payload() {
        let events = [
            (
                Event::RunStarted(RunStarted { total: 4 }),
                "run-started",
                r#"{"total":4}"#,
            ),
            (Event::RunBusy, "run-busy", "null"),
            (Event::RunCancelled, "run-cancelled", "null"),
            (
                Event::DragState(DragState { over: true }),
                "drag-state",
                r#"{"over":true}"#,
            ),
            (
                Event::RunFinished(RunFinished {
                    optimised: 1,
                    saved_bytes: 9,
                    has_output: true,
                }),
                "run-finished",
                r#"{"optimised":1,"saved_bytes":9,"has_output":true}"#,
            ),
        ];
        for (event, name, payload) in &events {
            assert_eq!(event.name(), *name);
            assert_eq!(json(event), *payload);
        }
    }
}
