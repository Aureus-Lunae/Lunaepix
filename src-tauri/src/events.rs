//! What the window receives: the event names and their JSON payloads.
//!
//! Every payload is plain data. A file is named by its file name only, never
//! by its path, the output folder by its folder name only, and a failure by
//! the core's bare reason, never by the worker's stderr. `INTERFACE.md`
//! documents the same shapes for the window.

use std::path::Path;

use lunaepix_core::{Compression, FileResult, MAX_RUN_FILES};
use serde::Serialize;

/// The compression of a run, as the window names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Lossless,
    Lossy,
}

/// A run has been planned and starts now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStarted {
    /// Number of `file-done` events that follow: one per planned file
    /// (skipped files included), except that the files the run limit left
    /// out share one row ([`over_limit_row`]).
    pub total: usize,
    /// The run's compression.
    pub mode: Mode,
    /// The lossy quality, 0 to 100; `null` for a lossless run.
    pub quality: Option<u8>,
}

impl RunStarted {
    /// The payload for a run of `total` rows with `compression`.
    pub fn new(total: usize, compression: Compression) -> Self {
        let (mode, quality) = match compression {
            Compression::Lossless => (Mode::Lossless, None),
            Compression::Lossy(quality) => (Mode::Lossy, Some(quality.get())),
        };
        RunStarted {
            total,
            mode,
            quality,
        }
    }
}

/// The file at `index` is being compressed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FileStarted {
    pub index: usize,
}

/// One row of the file list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListRow {
    /// The file name alone, lossily decoded.
    pub name: String,
    /// The size in bytes; `null` for a skipped row.
    pub size: Option<u64>,
    /// Why the row will be skipped, a sentence for the user; `null` otherwise.
    pub skip: Option<String>,
    /// The key of the row's thumbnail, `<list>-<index>` (see `thumbs`):
    /// never a path. `null` for a skipped row and for the over-limit row.
    pub thumb: Option<String>,
}

/// The whole file list, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListChanged {
    pub files: Vec<ListRow>,
    /// Why an add changed nothing, a sentence for the status line; or `null`.
    pub notice: Option<String>,
}

/// The output setting changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputChanged {
    /// The chosen folder's name, never its path; `null` for next to the
    /// originals.
    pub folder: Option<String>,
}

/// The `output-changed` payload for the chosen `folder`, or for none: the
/// folder's own name, lossily decoded, and empty for a folder without one
/// (a root such as `/`), never the path.
pub fn output_changed(folder: Option<&Path>) -> OutputChanged {
    OutputChanged {
        folder: folder.map(file_name_of),
    }
}

/// The last component of `path`, lossily decoded; empty if it has none.
pub fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
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
    /// How a `done` file was handled differently from the run's mode, a
    /// sentence for the user: only when there is such a note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

/// The image Single image mode shows, as the window may know it: names,
/// numbers and its key, never a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageInfo {
    /// The original's file name alone, lossily decoded.
    pub name: String,
    /// The size of the copy taken at load, in bytes.
    pub size: u64,
    /// The size it is shown at, in pixels; `null` for an SVG.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The image's key, a whole number as text (`"7"`); a new key is a new
    /// image.
    pub key: String,
    /// The name Save as starts with, without the extension.
    pub suggested: String,
    /// The original's extension with its dot (`".jpg"`), as it is spelled.
    pub extension: String,
}

/// The outcome of a load attempt in Single image mode: a choice, or a drop
/// while Single image shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageChanged {
    /// The image that shows now: the new one, or the one that was there
    /// when nothing was loaded; `null` when there is none.
    pub image: Option<ImageInfo>,
    /// How many dropped items were left out that Batch can take; above 0,
    /// Switch to Batch is offered.
    pub left_out: usize,
    /// A sentence for the status line, or `null`.
    pub notice: Option<String>,
}

/// The output setting changed while an image is loaded: the name Save as
/// starts with for it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NameSuggested {
    /// The image's key.
    pub image: String,
    /// The name, without the extension.
    pub suggested: String,
}

/// One event for the window: its name ([`Event::name`]) and its payload
/// (the `Serialize` output; the two unit events have `null`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Event {
    ListChanged(ListChanged),
    OutputChanged(OutputChanged),
    RunStarted(RunStarted),
    FileStarted(FileStarted),
    FileDone(FileDone),
    RunFinished(RunFinished),
    /// A drop or a command was refused because a run, or a dialog, is active.
    RunBusy,
    /// A file or folder dialog was closed without a choice.
    RunCancelled,
    DragState(DragState),
    ImageChanged(ImageChanged),
    NameSuggested(NameSuggested),
}

impl Event {
    /// The event name the window listens to.
    pub fn name(&self) -> &'static str {
        match self {
            Event::ListChanged(_) => "list-changed",
            Event::OutputChanged(_) => "output-changed",
            Event::RunStarted(_) => "run-started",
            Event::FileStarted(_) => "file-started",
            Event::FileDone(_) => "file-done",
            Event::RunFinished(_) => "run-finished",
            Event::RunBusy => "run-busy",
            Event::RunCancelled => "run-cancelled",
            Event::DragState(_) => "drag-state",
            Event::ImageChanged(_) => "image-changed",
            Event::NameSuggested(_) => "name-suggested",
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
    FileDone {
        index,
        name: OVER_LIMIT_NAME.to_owned(),
        status: Status::Skipped,
        before: None,
        after: None,
        millis: None,
        reason: Some(over_limit_reason(count)),
        note: None,
    }
}

/// Why the `count` files the run limit left out are not optimised: the
/// reason on the one row that stands for them, in the list and in the run.
pub fn over_limit_reason(count: usize) -> String {
    let files = if count == 1 { "file" } else { "files" };
    format!("{count} more {files} not optimised: over the {MAX_RUN_FILES}-file limit")
}

/// The `file-done` payload for job `index`, whose source is `src`.
///
/// A failure gives [`RunError::reason`](lunaepix_core::RunError::reason),
/// which carries neither a path nor the worker's stderr.
pub fn file_done(index: usize, src: &Path, result: &FileResult) -> FileDone {
    let mut payload = FileDone {
        index,
        name: file_name_of(src),
        status: Status::Done,
        before: None,
        after: None,
        millis: None,
        reason: None,
        note: None,
    };
    match result {
        FileResult::Optimized(outcome) => {
            payload.before = Some(outcome.before);
            payload.after = Some(outcome.after);
            payload.millis = Some(outcome.millis);
            payload.note = outcome.note.map(|note| note.to_string());
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
                Event::RunStarted(RunStarted {
                    total: 4,
                    mode: Mode::Lossless,
                    quality: None,
                }),
                "run-started",
                r#"{"total":4,"mode":"lossless","quality":null}"#,
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

    /// `text` parsed as JSON, so that a payload is compared as data.
    fn parsed(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap()
    }

    fn value(event: &impl Serialize) -> serde_json::Value {
        serde_json::to_value(event).unwrap()
    }

    #[test]
    fn run_started_names_mode_and_quality_and_null_quality_for_lossless() {
        let lossy = |q| Compression::Lossy(lunaepix_core::Quality::new(q).unwrap());
        let cases = [
            (
                RunStarted::new(3, lossy(75)),
                r#"{ "total": 3, "mode": "lossy", "quality": 75 }"#,
            ),
            (
                RunStarted::new(3, Compression::Lossless),
                r#"{ "total": 3, "mode": "lossless", "quality": null }"#,
            ),
            // Quality 0 is a quality, not "none".
            (
                RunStarted::new(1, lossy(0)),
                r#"{ "total": 1, "mode": "lossy", "quality": 0 }"#,
            ),
            (
                RunStarted::new(2001, lossy(100)),
                r#"{ "total": 2001, "mode": "lossy", "quality": 100 }"#,
            ),
        ];
        for (payload, expected) in &cases {
            assert_eq!(value(&Event::RunStarted(payload.clone())), parsed(expected));
        }
    }

    #[test]
    fn file_done_serialises_note_only_when_present() {
        let src = Path::new("/srv/photos/anim.png");
        let noted = FileResult::Optimized(Outcome {
            before: 512000,
            after: 498000,
            millis: 380,
            note: Some(lunaepix_core::Note::KeptLosslessAnimatedPng),
        });
        // The spec's example, compared as JSON.
        assert_eq!(
            value(&file_done(1, src, &noted)),
            parsed(
                r#"{ "index": 1, "name": "anim.png", "status": "done", "before": 512000, "after": 498000, "millis": 380, "note": "kept lossless: animated PNG" }"#
            )
        );
        // Every note reaches the window as its sentence.
        let notes = [
            (
                lunaepix_core::Note::KeptLosslessCmyk,
                "kept lossless: CMYK JPG",
            ),
            (
                lunaepix_core::Note::KeptLosslessAnimationTooLarge,
                "kept lossless: animation too large for lossy",
            ),
            (
                lunaepix_core::Note::OriginalKept,
                "original kept: lossy was not smaller",
            ),
        ];
        for (note, text) in notes {
            let result = FileResult::Optimized(Outcome {
                before: 2,
                after: 2,
                millis: 1,
                note: Some(note),
            });
            assert_eq!(file_done(0, src, &result).note.as_deref(), Some(text));
        }
        // Without a note there is no `note` key at all, not a `null`.
        let plain = value(&file_done(0, src, &optimized(100, 60)));
        assert!(plain.get("note").is_none(), "{plain}");
    }

    #[test]
    fn the_new_events_carry_the_specs_json() {
        let events = [
            (
                Event::ListChanged(ListChanged {
                    files: vec![
                        ListRow {
                            name: "holiday.jpg".to_owned(),
                            size: Some(2400000),
                            skip: None,
                            thumb: Some("3-0".to_owned()),
                        },
                        ListRow {
                            name: "notes.txt".to_owned(),
                            size: None,
                            skip: Some("Not a PNG, JPG, SVG or WebP file".to_owned()),
                            thumb: None,
                        },
                    ],
                    notice: None,
                }),
                "list-changed",
                r#"{ "files": [
                    { "name": "holiday.jpg", "size": 2400000, "skip": null, "thumb": "3-0" },
                    { "name": "notes.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file", "thumb": null }
                ], "notice": null }"#,
            ),
            (
                Event::OutputChanged(output_changed(Some(Path::new("/home/someone/exports")))),
                "output-changed",
                r#"{ "folder": "exports" }"#,
            ),
            (
                Event::OutputChanged(output_changed(None)),
                "output-changed",
                r#"{ "folder": null }"#,
            ),
            (
                Event::FileStarted(FileStarted { index: 1 }),
                "file-started",
                r#"{ "index": 1 }"#,
            ),
        ];
        for (event, name, payload) in &events {
            assert_eq!(event.name(), *name);
            assert_eq!(value(event), parsed(payload));
        }
    }

    // --- Single image ---------------------------------------------------------

    fn holiday(key: &str) -> ImageInfo {
        ImageInfo {
            name: "holiday.jpg".to_owned(),
            size: 2_400_000,
            width: Some(4032),
            height: Some(3024),
            key: key.to_owned(),
            suggested: "optimized-holiday".to_owned(),
            extension: ".jpg".to_owned(),
        }
    }

    #[test]
    fn image_changed_serialises_exactly_as_the_spec_examples() {
        let events = [
            (
                Event::ImageChanged(ImageChanged {
                    image: Some(holiday("7")),
                    left_out: 0,
                    notice: None,
                }),
                r#"{ "image": { "name": "holiday.jpg", "size": 2400000, "width": 4032, "height": 3024,
                     "key": "7", "suggested": "optimized-holiday", "extension": ".jpg" },
                     "left_out": 0, "notice": null }"#,
            ),
            (
                Event::ImageChanged(ImageChanged {
                    image: Some(holiday("8")),
                    left_out: 3,
                    notice: Some(
                        "Single image mode takes one image; 3 others were left out.".to_owned(),
                    ),
                }),
                r#"{ "image": { "name": "holiday.jpg", "size": 2400000, "width": 4032, "height": 3024,
                     "key": "8", "suggested": "optimized-holiday", "extension": ".jpg" },
                     "left_out": 3, "notice": "Single image mode takes one image; 3 others were left out." }"#,
            ),
            (
                Event::ImageChanged(ImageChanged {
                    image: None,
                    left_out: 1,
                    notice: Some("Folders go in Batch.".to_owned()),
                }),
                r#"{ "image": null, "left_out": 1, "notice": "Folders go in Batch." }"#,
            ),
            (
                Event::ImageChanged(ImageChanged {
                    image: Some(ImageInfo {
                        name: "logo.svg".to_owned(),
                        size: 4100,
                        width: None,
                        height: None,
                        key: "9".to_owned(),
                        suggested: "optimized-logo".to_owned(),
                        extension: ".svg".to_owned(),
                    }),
                    left_out: 0,
                    notice: None,
                }),
                r#"{ "image": { "name": "logo.svg", "size": 4100, "width": null, "height": null,
                     "key": "9", "suggested": "optimized-logo", "extension": ".svg" },
                     "left_out": 0, "notice": null }"#,
            ),
        ];
        for (event, payload) in &events {
            assert_eq!(event.name(), "image-changed");
            assert_eq!(value(event), parsed(payload));
        }
    }

    #[test]
    fn name_suggested_serialises_as_image_and_suggested_only() {
        let event = Event::NameSuggested(NameSuggested {
            image: "7".to_owned(),
            suggested: "holiday".to_owned(),
        });
        assert_eq!(event.name(), "name-suggested");
        assert_eq!(
            value(&event),
            parsed(r#"{ "image": "7", "suggested": "holiday" }"#)
        );
        // Not the image-changed shape: no other key, and not a nested image.
        assert_ne!(
            value(&event),
            parsed(r#"{ "image": { "key": "7" }, "suggested": "holiday" }"#)
        );
    }
}
