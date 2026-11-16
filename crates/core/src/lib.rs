//! Lunaepix core: lossless and lossy PNG, JPG, SVG and WebP optimisation.
//!
//! All logic and all tests live here. The CLI and the Tauri app are thin
//! front ends over this crate.
//!
//! The `plan_*` functions never write to the filesystem; destination folders
//! are created only when a job runs. [`run`] runs the planned jobs, each file
//! in its own worker process ([`worker`]), so a library that crashes the
//! process fails only that file. [`run_thumb`] makes a thumbnail the same way,
//! and [`run_probe`] reads a file's shown size, or Batch's reason for
//! refusing it ([`probe`]), the same way.
//!
//! `unsafe` code is denied everywhere but in `jpg`, the libjpeg FFI, in
//! `platform`, the worker's two start-up system calls, and in `webp::codec`,
//! the libwebp FFI. The one other exception is two test helpers in `run`'s
//! unit tests, which ask `kill(pid, 0)` and `waitid` about a worker's process.

#![deny(unsafe_code)]

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod atomic;
#[allow(unsafe_code)]
mod jpg;
mod lossy;
mod plan;
#[allow(unsafe_code)]
mod platform;
mod png;
mod probe;
mod run;
mod svg;
mod thumb;
mod webp;
pub mod worker;

/// The JPEG fixture helpers of the integration tests, shared with the unit
/// tests (`jpg::decode`, `thumb::decode`).
#[cfg(test)]
#[path = "../tests/common/jpeg_fixtures.rs"]
mod jpeg_fixtures;

/// The check that a failure reason carries no path. `atomic`'s unit tests use
/// it through this module (`crate::no_path`); `tests/worker.rs` in the core and
/// `session_run.rs` in the app include the same file by `#[path]`.
#[cfg(test)]
#[path = "../tests/common/no_path.rs"]
mod no_path;

pub use atomic::{is_staging_name, save_new};
use lossy::Lossy;
pub use lossy::{DEFAULT_QUALITY, Note, Quality};
pub use plan::{
    MAX_ANIMATION_CANVAS_PIXELS, MAX_ANIMATION_PIXELS, MAX_FILE_BYTES, MAX_PIXELS, MAX_RUN_FILES,
    MAX_SVG_DEPTH, MAX_SVG_ENTITY_BYTES, MAX_SVG_NODES, MAX_WORKER_MESSAGE_BYTES,
    MAX_WORKER_OUTPUT_BYTES, WORKER_TIMEOUT, batch_output_name, first_free_name,
    has_supported_extension, images_in_folder, plan_files, plan_files_to, plan_folder,
    plan_folder_to,
};
pub use probe::{Probe, probe};
pub use run::{
    Crash, FileResult, MAX_THUMB_ANSWER_BYTES, ProbeAnswer, RunError, StopHandle, THUMB_TIMEOUT,
    ThumbAnswer, Worker, run, run_observed, run_probe, run_thumb, run_until_stopped,
};
pub use thumb::{CannotShow, MAX_SVG_THUMB_BYTES, THUMB_SIDE, Thumbnail, thumbnail};
pub use worker::{PROBE_FLAG, THUMB_FLAG, WORKER_FLAG};

/// How a run compresses its files: one setting for the whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Today's treatment: smaller files, not a pixel changed.
    Lossless,
    /// JPG, PNG and WebP are re-encoded at this quality; SVG gets the
    /// lossless treatment.
    Lossy(Quality),
}

/// Whether a run may replace an existing output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// The output replaces an earlier output of the same name. The output
    /// may only go to the source's folder or its `optimized/` folder.
    Replace,
    /// Nothing existing is overwritten.
    NoReplace,
}

/// The settings of a run, for every file in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub compression: Compression,
    pub write: WriteMode,
}

/// Result of optimising one file. Sizes are in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub before: u64,
    pub after: u64,
    pub millis: u64,
    /// Why a lossy run kept the file's bytes as they were, if it did.
    pub note: Option<Note>,
}

/// Why a file is left alone. The `Display` text is shown to the user in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Not a PNG, JPG, SVG or WebP file.
    UnsupportedType,
    /// Larger than the per-file size limit.
    TooLarge,
    /// The run already holds the maximum number of files to optimise.
    OverRunLimit,
    /// The file's output would be the file itself: it is already in the
    /// output folder, or its output name is a symlink back to it.
    IsOwnDestination,
    /// The input does not exist or is not a regular file (for example a
    /// socket), or it is a folder that could not be read.
    NotAFile,
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Skip::UnsupportedType => "Not a PNG, JPG, SVG or WebP file",
            Skip::TooLarge => "File is too large",
            Skip::OverRunLimit => {
                return write!(f, "Too many files in one run (max {MAX_RUN_FILES})");
            }
            Skip::IsOwnDestination => "Its output would overwrite the file itself",
            Skip::NotAFile => "Not a readable file",
        })
    }
}

/// One planned unit of work, or the reason a file will not be touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Optimise `src` and write the result to `dest`.
    Optimize { src: PathBuf, dest: PathBuf },
    /// Leave `src` alone, for the given reason.
    Skip { src: PathBuf, reason: Skip },
}

/// A file that failed while being optimised.
#[derive(Debug, thiserror::Error)]
pub enum OptimizeError {
    /// Reading or writing `path` failed. The cause is part of the message and
    /// deliberately not a `source()`, so an error chain does not print it twice.
    #[error("{path}: {cause}", path = path.display())]
    Io { path: PathBuf, cause: io::Error },
    /// The file is not a PNG, JPG, SVG or WebP.
    #[error("{path}: unsupported file type", path = path.display())]
    UnsupportedType { path: PathBuf },
    /// The file has a supported extension but its content could not be
    /// parsed, or the optimised result did not pass the lossless check.
    #[error("{path}: {message}", path = path.display())]
    Invalid { path: PathBuf, message: String },
}

impl OptimizeError {
    /// The reason alone, without the leading `"<path>: "` that `Display`
    /// adds, for a caller that already shows the path. `Display` is always
    /// `"<path>: <reason>"`.
    pub fn reason(&self) -> String {
        match self {
            OptimizeError::Io { cause, .. } => cause.to_string(),
            OptimizeError::UnsupportedType { .. } => "unsupported file type".to_owned(),
            OptimizeError::Invalid { message, .. } => message.clone(),
        }
    }
}

/// Optimise `src` with the run's `options` and write the result to `dest`.
///
/// With [`Compression::Lossless`] every format gets today's lossless
/// treatment, and no lossy path is ever called. With
/// [`Compression::Lossy`] a JPG, PNG or WebP goes to its format's lossy path,
/// which may keep the file lossless or keep the original bytes, with a
/// [`Note`] that says so; an SVG gets the lossless treatment without a note.
///
/// The format is chosen by the extension, case-insensitively, exactly as the
/// planner does. `src` is only ever read. `dest` is written atomically (the
/// `write_atomically` helper) and its folder is created if missing, only
/// when there is a result to write. If anything fails, `dest` is not touched
/// and no temporary file is left behind. The output gets the source's
/// permissions. A `dest` that is the same file as `src` (after resolving
/// symlinks and `.`/`..`) is refused. With [`WriteMode::Replace`] a `dest`
/// whose folder is not the source's folder or its `optimized/` subfolder is
/// refused (checked before anything is created). With
/// [`WriteMode::NoReplace`] `dest` may be in any folder: the output is
/// written as a new file and nothing existing is replaced. When `dest` is
/// taken by then (a file appeared after planning), the first free numbered
/// name beside it is used (`logo-1.png`, up to `logo-9999.png`), and past
/// that the file fails with "no free name in the output folder". A source of
/// more than [`MAX_FILE_BYTES`] when it is
/// read (it grew after planning) is refused with the planner's size reason,
/// having read one byte past the limit at most. On Windows the output does
/// not get the source's read-only attribute, so a later run can replace it.
/// `Outcome::millis` is the wall time of the whole call: read, optimise and
/// write.
pub fn optimize_file(src: &Path, dest: &Path, options: &Options) -> Result<Outcome, OptimizeError> {
    optimize_file_with(src, dest, options, &lossy_dispatch)
}

/// The formats that have a lossy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LossyFormat {
    Png,
    Jpg,
    Webp,
}

impl LossyFormat {
    /// The lossy path of `format`, if it has one: SVG has none.
    fn of(format: plan::Format) -> Option<LossyFormat> {
        match format {
            plan::Format::Png => Some(LossyFormat::Png),
            plan::Format::Jpg => Some(LossyFormat::Jpg),
            plan::Format::Webp => Some(LossyFormat::Webp),
            plan::Format::Svg => None,
        }
    }
}

/// The lossy path of each format.
fn lossy_dispatch(format: LossyFormat, bytes: &[u8], quality: Quality) -> Result<Lossy, String> {
    match format {
        LossyFormat::Png => png::optimize_lossy(bytes, quality),
        LossyFormat::Jpg => jpg::optimize_lossy(bytes, quality),
        LossyFormat::Webp => webp::optimize_lossy(bytes, quality),
    }
}

/// Today's lossless treatment of `format`.
fn lossless(format: plan::Format, bytes: &[u8]) -> Result<Vec<u8>, String> {
    match format {
        plan::Format::Png => png::optimize(bytes),
        plan::Format::Jpg => jpg::optimize(bytes),
        plan::Format::Svg => svg::optimize(bytes),
        plan::Format::Webp => webp::optimize(bytes),
    }
}

/// The lossy paths, as [`optimize_file_with`] takes them: [`lossy_dispatch`],
/// or a double in the tests.
pub(crate) type LossyPaths<'a> = &'a dyn Fn(LossyFormat, &[u8], Quality) -> Result<Lossy, String>;

/// [`optimize_file`] with the lossy paths given: the tests pass a double that
/// records what it is handed.
pub(crate) fn optimize_file_with(
    src: &Path,
    dest: &Path,
    options: &Options,
    lossy: LossyPaths<'_>,
) -> Result<Outcome, OptimizeError> {
    let started = Instant::now();
    let format = plan::format_of(src).ok_or_else(|| OptimizeError::UnsupportedType {
        path: src.to_path_buf(),
    })?;
    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |cause| OptimizeError::Io { path, cause }
    };
    // Writing the output over the source would destroy the original.
    if plan::resolve(src) == plan::resolve(dest) {
        return Err(OptimizeError::Invalid {
            path: src.to_path_buf(),
            message: "the source and the destination are the same file".to_owned(),
        });
    }
    // Opened read-only; the same handle gives the size, the permissions and
    // the bytes, so they all describe one file.
    let file = fs::File::open(src).map_err(io_error(src))?;
    let permissions = file.metadata().map_err(io_error(src))?.permissions();
    // The planner skipped a larger file, but it may have grown since.
    let original = read_at_most(file, MAX_FILE_BYTES)
        .map_err(io_error(src))?
        .ok_or_else(|| OptimizeError::Invalid {
            path: src.to_path_buf(),
            message: Skip::TooLarge.to_string(),
        })?;
    let optimized = match (options.compression, LossyFormat::of(format)) {
        (Compression::Lossy(quality), Some(lossy_format)) => {
            lossy(lossy_format, &original, quality)
        }
        (Compression::Lossy(_), None) | (Compression::Lossless, _) => {
            lossless(format, &original).map(|bytes| Lossy { bytes, note: None })
        }
    }
    .map_err(|message| OptimizeError::Invalid {
        path: src.to_path_buf(),
        message,
    })?;
    match options.write {
        WriteMode::Replace => {
            ensure_destination_folder(src, dest)?;
            atomic::write_atomically(dest, &optimized.bytes, permissions)
                .map_err(io_error(dest))?;
        }
        WriteMode::NoReplace => {
            // Any folder: nothing existing can be overwritten. The name
            // actually written is not reported back (plan handoff).
            atomic::write_new(dest, &optimized.bytes, permissions).map_err(io_error(dest))?;
        }
    }

    Ok(Outcome {
        before: original.len() as u64,
        after: optimized.bytes.len() as u64,
        millis: started.elapsed().as_millis() as u64,
        note: optimized.note,
    })
}

/// Output may only go to the source's own folder (`optimized-<name>` beside
/// it) or to its `optimized/` subfolder, which are the two places the planner
/// chooses. Both folders are compared after resolving symlinks, so the user's
/// own folder may itself be reached through a link, but an `optimized` that is
/// a link to anywhere else is refused. On Windows and macOS an existing
/// `Optimized` (any letter case) is the output folder. Nothing is created or
/// written here: a
/// folder that does not exist yet is resolved through its nearest existing
/// parent.
///
/// The check and the write are two steps, so a link swapped in between them is
/// not caught; the folders are the user's own.
fn ensure_destination_folder(src: &Path, dest: &Path) -> Result<(), OptimizeError> {
    let source_folder = plan::resolve(atomic::folder_of(src));
    let dest_folder = plan::resolve(atomic::folder_of(dest));
    if is_allowed_destination_folder(&dest_folder, &source_folder, NAMES_IGNORE_CASE) {
        return Ok(());
    }
    Err(OptimizeError::Invalid {
        path: dest.to_path_buf(),
        message: format!(
            "the destination must be the source's folder or its '{}' folder",
            plan::OUTPUT_DIR
        ),
    })
}

/// Whether the file systems of this target usually treat `Optimized` and
/// `optimized` as one name: Windows (NTFS) and macOS (APFS) by default.
const NAMES_IGNORE_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// Whether output may go to `dest_folder`: `source_folder` itself, or its
/// `optimized` subfolder. Both are resolved paths. The parent is compared
/// exactly; with `ignore_case`, the last name is compared without regard to
/// ASCII letter case, because resolving an existing `Optimized` folder on a
/// case-insensitive file system gives that spelling back.
fn is_allowed_destination_folder(
    dest_folder: &Path,
    source_folder: &Path,
    ignore_case: bool,
) -> bool {
    if dest_folder == source_folder {
        return true;
    }
    let (Some(parent), Some(name)) = (dest_folder.parent(), dest_folder.file_name()) else {
        return false;
    };
    let output = OsStr::new(plan::OUTPUT_DIR);
    parent == source_folder
        && (name == output || (ignore_case && name.eq_ignore_ascii_case(output)))
}

/// All of `reader`, or `None` if it holds more than `limit` bytes. At most
/// one byte past the limit is read.
fn read_at_most(reader: impl Read, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

/// The first of the smallest candidates, if it is smaller than `original`;
/// otherwise `original`. On a tie between candidates the first one wins
/// (for JPG, candidate A, jpegoptim's result); a candidate of the same size
/// as the original does not count as smaller.
pub(crate) fn smallest<'a>(original: &'a [u8], candidates: &'a [Vec<u8>]) -> &'a [u8] {
    candidates
        .iter()
        .map(Vec::as_slice)
        .fold(original, |best, candidate| {
            if candidate.len() < best.len() {
                candidate
            } else {
                best
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the smallest of the candidates, or the original ----------------------

    #[test]
    fn smallest_returns_the_smallest_candidate() {
        let original = vec![0u8; 10];
        let candidates = [vec![1u8; 8], vec![2u8; 6]];
        assert_eq!(smallest(&original, &candidates), &[2u8; 6]);
        let reversed = [vec![2u8; 6], vec![1u8; 8]];
        assert_eq!(smallest(&original, &reversed), &[2u8; 6]);
    }

    #[test]
    fn smallest_returns_the_original_when_no_candidate_is_smaller() {
        let original = vec![0u8; 10];
        // Equal size is not smaller: the original wins.
        let candidates = [vec![1u8; 10], vec![2u8; 12]];
        let chosen = smallest(&original, &candidates);
        assert_eq!(chosen.as_ptr(), original.as_ptr());
        assert_eq!(chosen, original.as_slice());
        assert_eq!(smallest(&original, &[]).as_ptr(), original.as_ptr());
    }

    #[test]
    fn smallest_keeps_the_first_candidate_when_two_tie() {
        let original = vec![0u8; 10];
        let candidates = [vec![1u8; 7], vec![2u8; 7]];
        assert_eq!(smallest(&original, &candidates), &[1u8; 7]);
    }

    #[test]
    fn skip_reasons_are_human_readable() {
        assert_eq!(
            Skip::UnsupportedType.to_string(),
            "Not a PNG, JPG, SVG or WebP file"
        );
        assert_eq!(Skip::NotAFile.to_string(), "Not a readable file");
        assert_ne!(Skip::TooLarge.to_string(), Skip::OverRunLimit.to_string());
    }

    fn optimize_errors() -> Vec<OptimizeError> {
        vec![
            OptimizeError::Io {
                path: PathBuf::from("dir/a.png"),
                cause: io::Error::other("disk full"),
            },
            OptimizeError::UnsupportedType {
                path: PathBuf::from("dir/a.txt"),
            },
            OptimizeError::Invalid {
                path: PathBuf::from("dir/b.svg"),
                message: "nested too deeply".to_owned(),
            },
        ]
    }

    #[test]
    fn an_optimize_error_gives_its_reason_without_the_path() {
        let reasons: Vec<String> = optimize_errors().iter().map(|e| e.reason()).collect();
        assert_eq!(
            reasons,
            ["disk full", "unsupported file type", "nested too deeply"]
        );
    }

    #[test]
    fn an_optimize_errors_display_is_its_path_then_its_reason() {
        for error in optimize_errors() {
            let path = match &error {
                OptimizeError::Io { path, .. }
                | OptimizeError::UnsupportedType { path }
                | OptimizeError::Invalid { path, .. } => path.display().to_string(),
            };
            assert_eq!(error.to_string(), format!("{path}: {}", error.reason()));
            // The reason alone does not carry the path.
            assert!(!error.reason().contains("dir/"), "{}", error.reason());
        }
    }

    #[test]
    fn the_own_destination_skip_says_the_output_would_overwrite_the_file() {
        assert_eq!(
            Skip::IsOwnDestination.to_string(),
            "Its output would overwrite the file itself"
        );
        // The old text named an output folder, which a symlink back to the
        // source is not in.
        assert!(!Skip::IsOwnDestination.to_string().contains("output folder"));
    }

    // --- the read cap -------------------------------------------------------

    #[test]
    fn reads_a_source_of_exactly_the_limit_whole() {
        let bytes = [7u8; 10];
        assert_eq!(read_at_most(&bytes[..], 10).unwrap(), Some(bytes.to_vec()));
        assert_eq!(read_at_most(&[][..], 10).unwrap(), Some(Vec::new()));
    }

    #[test]
    fn refuses_a_source_one_byte_over_the_limit() {
        assert_eq!(read_at_most(&[7u8; 11][..], 10).unwrap(), None);
        assert_eq!(read_at_most(&[7u8; 1000][..], 10).unwrap(), None);
    }

    #[test]
    fn passes_on_a_read_error_of_the_source() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("disk gone"))
            }
        }
        let error = read_at_most(Broken, 10).unwrap_err();
        assert_eq!(error.to_string(), "disk gone");
    }

    // --- where output may go -------------------------------------------------

    #[test]
    fn accepts_the_source_folder_and_its_optimized_folder_exactly() {
        let source = Path::new("/photos/trip");
        for ignore_case in [false, true] {
            assert!(is_allowed_destination_folder(source, source, ignore_case));
            assert!(is_allowed_destination_folder(
                &source.join("optimized"),
                source,
                ignore_case
            ));
        }
    }

    #[test]
    fn accepts_an_optimized_folder_in_another_letter_case_only_when_names_ignore_case() {
        let source = Path::new("/photos/trip");
        for name in ["Optimized", "OPTIMIZED", "optimizeD"] {
            let folder = source.join(name);
            assert!(
                is_allowed_destination_folder(&folder, source, true),
                "{name}"
            );
            assert!(
                !is_allowed_destination_folder(&folder, source, false),
                "{name}"
            );
        }
    }

    #[test]
    fn refuses_another_parent_or_another_name_whatever_the_case_rule() {
        let source = Path::new("/photos/trip");
        let refused = [
            PathBuf::from("/photos/other/optimized"),
            PathBuf::from("/photos/optimized"),
            // The parent is compared exactly, also where names ignore case.
            PathBuf::from("/photos/Trip/optimized"),
            PathBuf::from("/photos/trip/optimizedx"),
            PathBuf::from("/photos/trip/optimised"),
            PathBuf::from("/photos/trip/optimized/deeper"),
            PathBuf::from("/photos"),
        ];
        for folder in &refused {
            for ignore_case in [false, true] {
                assert!(
                    !is_allowed_destination_folder(folder, source, ignore_case),
                    "{} ({ignore_case})",
                    folder.display()
                );
            }
        }
    }

    #[test]
    fn names_ignore_case_exactly_on_windows_and_macos() {
        assert_eq!(NAMES_IGNORE_CASE, cfg!(any(windows, target_os = "macos")));
    }

    // --- run options: which path a file takes -----------------------------------

    use std::cell::RefCell;

    use tempfile::TempDir;

    const LOSSLESS: Options = Options {
        compression: Compression::Lossless,
        write: WriteMode::Replace,
    };

    fn lossy(quality: u8) -> Options {
        Options {
            compression: Compression::Lossy(Quality::new(quality).unwrap()),
            write: WriteMode::Replace,
        }
    }

    /// What the double for the lossy paths was handed: the format, the bytes
    /// and the quality, per call.
    type LossyCall = (LossyFormat, Vec<u8>, Quality);

    /// A double for the lossy paths: it records every call and gives `answer`.
    struct LossyDouble {
        calls: RefCell<Vec<LossyCall>>,
        answer: Result<Lossy, String>,
    }

    impl LossyDouble {
        fn answering(answer: Result<Lossy, String>) -> Self {
            LossyDouble {
                calls: RefCell::new(Vec::new()),
                answer,
            }
        }

        fn call(
            &self,
            format: LossyFormat,
            bytes: &[u8],
            quality: Quality,
        ) -> Result<Lossy, String> {
            self.calls
                .borrow_mut()
                .push((format, bytes.to_vec(), quality));
            self.answer.clone()
        }

        fn calls(&self) -> Vec<LossyCall> {
            self.calls.borrow().clone()
        }
    }

    /// A double whose answer would be visible in the output if it were used.
    fn marked_double() -> LossyDouble {
        LossyDouble::answering(Ok(Lossy {
            bytes: b"written by the lossy double".to_vec(),
            note: Some(Note::OriginalKept),
        }))
    }

    fn png_fixture() -> Vec<u8> {
        let (w, h) = (48u32, 32u32);
        let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = Vec::new();
        let mut encoder = ::png::Encoder::new(&mut out, w, h);
        encoder.set_color(::png::ColorType::Rgba);
        encoder.set_depth(::png::BitDepth::Eight);
        encoder.set_compression(::png::Compression::Fast);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
        writer.finish().unwrap();
        out
    }

    fn jpg_fixture() -> Vec<u8> {
        let (w, h) = (48u32, 32u32);
        let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 241) as u8).collect();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode(&pixels, w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    fn webp_fixture() -> Vec<u8> {
        let pixels: Vec<u8> = (0..32 * 32 * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(&pixels, 32, 32, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn svg_fixture() -> Vec<u8> {
        br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
  <!-- a comment to remove -->
  <rect x="0.000" y="0.000" width="10.000" height="10.000" fill="#ff0000"/>
</svg>
"##
        .to_vec()
    }

    /// Every format: its file name, its bytes, and today's lossless output.
    fn every_format() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
        let png = png_fixture();
        let jpg = jpg_fixture();
        let svg = svg_fixture();
        let webp = webp_fixture();
        vec![
            ("a.png", png.clone(), crate::png::optimize(&png).unwrap()),
            ("a.jpg", jpg.clone(), crate::jpg::optimize(&jpg).unwrap()),
            ("a.svg", svg.clone(), crate::svg::optimize(&svg).unwrap()),
            (
                "a.webp",
                webp.clone(),
                crate::webp::optimize(&webp).unwrap(),
            ),
        ]
    }

    /// Write `bytes` as `name` in `dir` and optimise it to `optimized-<name>`
    /// with `options` and `double` as the lossy paths.
    fn optimise_with(
        dir: &Path,
        name: &str,
        bytes: &[u8],
        options: &Options,
        double: &LossyDouble,
    ) -> (PathBuf, PathBuf, Result<Outcome, OptimizeError>) {
        let src = dir.join(name);
        fs::write(&src, bytes).unwrap();
        let dest = dir.join(format!("optimized-{name}"));
        let result = optimize_file_with(&src, &dest, options, &|format, bytes, quality| {
            double.call(format, bytes, quality)
        });
        (src, dest, result)
    }

    #[test]
    fn a_lossless_run_gives_todays_bytes() {
        for (name, original, expected) in every_format() {
            let tmp = TempDir::new().unwrap();
            let src = tmp.path().join(name);
            fs::write(&src, &original).unwrap();
            let dest = tmp.path().join(format!("optimized-{name}"));

            let outcome = optimize_file(&src, &dest, &LOSSLESS).unwrap();

            assert_eq!(fs::read(&dest).unwrap(), expected, "{name}");
            assert_eq!(outcome.after, expected.len() as u64, "{name}");
            assert_eq!(outcome.note, None, "{name}");
            assert_eq!(fs::read(&src).unwrap(), original, "{name}");
        }
    }

    #[test]
    fn a_lossless_run_never_reaches_a_lossy_path() {
        for (name, original, expected) in every_format() {
            let tmp = TempDir::new().unwrap();
            let double = marked_double();

            let (_, dest, result) = optimise_with(tmp.path(), name, &original, &LOSSLESS, &double);

            assert_eq!(result.unwrap().note, None, "{name}");
            assert_eq!(double.calls(), Vec::<LossyCall>::new(), "{name}");
            assert_eq!(fs::read(&dest).unwrap(), expected, "{name}");
        }
    }

    #[test]
    fn a_lossy_run_hands_each_format_its_bytes_and_quality_and_writes_what_comes_back() {
        let cases = [
            ("a.png", png_fixture(), LossyFormat::Png),
            ("a.jpg", jpg_fixture(), LossyFormat::Jpg),
            ("a.webp", webp_fixture(), LossyFormat::Webp),
        ];
        for (name, original, format) in cases {
            for quality in [0, 75, 100] {
                let tmp = TempDir::new().unwrap();
                let double = marked_double();

                let (src, dest, result) =
                    optimise_with(tmp.path(), name, &original, &lossy(quality), &double);

                let outcome = result.unwrap();
                assert_eq!(
                    double.calls(),
                    [(format, original.clone(), Quality::new(quality).unwrap())],
                    "{name} at {quality}"
                );
                assert_eq!(fs::read(&dest).unwrap(), b"written by the lossy double");
                assert_eq!(outcome.before, original.len() as u64);
                assert_eq!(outcome.after, b"written by the lossy double".len() as u64);
                assert_eq!(outcome.note, Some(Note::OriginalKept), "{name}");
                assert_eq!(fs::read(&src).unwrap(), original, "{name}");
            }
        }
    }

    #[test]
    fn a_lossy_path_error_fails_the_file_and_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let double = LossyDouble::answering(Err("the double refused it".to_owned()));

        let (src, dest, result) =
            optimise_with(tmp.path(), "a.png", &png_fixture(), &lossy(75), &double);

        match result.unwrap_err() {
            OptimizeError::Invalid { path, message } => {
                assert_eq!(path, src);
                assert_eq!(message, "the double refused it");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(double.calls().len(), 1);
        assert!(!dest.exists());
        let mut names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["a.png"]);
    }

    #[test]
    fn the_lossy_dispatch_double_records_no_call_for_svg() {
        let tmp = TempDir::new().unwrap();
        let double = marked_double();
        let original = svg_fixture();

        let (_, dest, result) = optimise_with(tmp.path(), "a.svg", &original, &lossy(75), &double);

        assert_eq!(result.unwrap().note, None);
        assert_eq!(double.calls(), Vec::<LossyCall>::new());
        assert_eq!(
            fs::read(&dest).unwrap(),
            crate::svg::optimize(&original).unwrap()
        );
    }

    #[test]
    fn a_no_replace_run_writes_beside_an_existing_file_without_replacing_it() {
        let original = png_fixture();
        let cases = [
            (
                Compression::Lossless,
                crate::png::optimize(&original).unwrap(),
            ),
            (
                lossy(75).compression,
                b"written by the lossy double".to_vec(),
            ),
        ];
        for (compression, expected) in cases {
            let tmp = TempDir::new().unwrap();
            let existing = tmp.path().join("optimized-a.png");
            fs::write(&existing, b"an earlier output").unwrap();
            let options = Options {
                compression,
                write: WriteMode::NoReplace,
            };
            let double = marked_double();

            let (src, dest, result) =
                optimise_with(tmp.path(), "a.png", &original, &options, &double);

            let outcome = result.unwrap();
            assert_eq!(dest, existing);
            assert_eq!(fs::read(&dest).unwrap(), b"an earlier output");
            let numbered = tmp.path().join("optimized-a-1.png");
            assert_eq!(fs::read(&numbered).unwrap(), expected, "{compression:?}");
            assert_eq!(outcome.after, expected.len() as u64);
            assert_eq!(fs::read(&src).unwrap(), original);
            let mut names: Vec<_> = fs::read_dir(tmp.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            names.sort();
            assert_eq!(names, ["a.png", "optimized-a-1.png", "optimized-a.png"]);
        }
    }

    #[test]
    fn the_run_limit_skip_names_the_file_count_limit() {
        assert_eq!(
            Skip::OverRunLimit.to_string(),
            "Too many files in one run (max 2000)"
        );
        // The old text spoke of a size limit; the limit is a file count.
        assert!(!Skip::OverRunLimit.to_string().contains("size"));
    }
}
