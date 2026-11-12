//! Single image mode's state: which view shows, the loaded image (a private
//! copy taken at load), the last drop that left items out, and the preview:
//! the latest request for a compressed version, its worker's stop handle and
//! the version kept. No Tauri here: the probe comes in as a closure and the
//! preview's worker result is handed in, so all of it is tested without a
//! window or a worker.
//!
//! The private folder is made once per launch, the first time an image is
//! loaded, with `tempfile` (mode 0700 on Unix, in the user's own temp folder
//! on Windows), and removed by [`Single::shutdown`] when the window closes. Each
//! image gets a subfolder `image-<key>` holding its copy, `original<.ext>`,
//! and its compressed versions, `v<n><.ext>`: the kept one and the one being
//! made. Keys and version numbers start at 1 and only go up during a launch.
//!
//! Every decision about shared state is acted on in the lock section that
//! makes it: a preview's outcome is sent there, `preview-started` too, a
//! load stores its image only if the window is still open, and a save takes
//! the image and its kept version's path together. Slow work (a worker run,
//! a file copy or read, a delete, a folder listing) happens outside the
//! lock. Preview workers and the clean-up of a version folder also take the
//! worker gate, so the clean-up never removes a running worker's staging
//! file.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use lunaepix_core::{
    Compression, Crash, FileResult, Job, MAX_FILE_BYTES, Options, ProbeAnswer, RunError, Skip,
    StopHandle, WriteMode, batch_output_name, first_free_name, has_supported_extension,
    is_staging_name,
};
use serde_json::Value;
use tempfile::TempDir;

use crate::events::{
    Event, ImageChanged, ImageInfo, NameSuggested, PreviewFailed, PreviewStarted, preview_ready,
};
use crate::session::{CommandError, lock};
use crate::thumbs::digits;

/// Which mode the window shows, so a drop goes there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum View {
    /// Every launch starts here.
    #[default]
    Single,
    Batch,
}

/// The view from `set_view`'s whole argument object: exactly
/// `{ "view": "single" }` or `{ "view": "batch" }`. Anything else (another
/// spelling, another type, `null`, a missing or an extra key, no JSON at all)
/// is [`CommandError::InvalidView`].
pub fn parse_view(args: Option<&Value>) -> Result<View, CommandError> {
    let args = args
        .and_then(Value::as_object)
        .filter(|args| args.len() == 1)
        .ok_or(CommandError::InvalidView)?;
    match args.get("view").and_then(Value::as_str) {
        Some("single") => Ok(View::Single),
        Some("batch") => Ok(View::Batch),
        _ => Err(CommandError::InvalidView),
    }
}

/// The notice of a choice or a drop with no PNG, JPG, SVG or WebP file in it.
pub const NOT_AN_IMAGE_NOTICE: &str = "Not a PNG, JPG, SVG or WebP file.";

/// The notice of a drop of only folders (and files that are not images).
pub const FOLDERS_GO_IN_BATCH_NOTICE: &str = "Folders go in Batch.";

/// The notice of a drop that loaded one image and left `count` (1 or more)
/// items out that Batch can take.
pub fn left_out_notice(count: usize) -> String {
    let others = if count == 1 {
        "1 other was".to_owned()
    } else {
        format!("{count} others were")
    };
    format!("Single image mode takes one image; {others} left out.")
}

/// Batch's reason for a worker that gave no valid answer, used when the probe
/// worker failed (crashed, timed out, could not start): the reason Batch
/// gives a file whose worker ended without a valid result.
pub fn probe_failed_reason() -> String {
    RunError::Crashed {
        path: PathBuf::new(),
        cause: Crash::NoValidResult,
        stderr: String::new(),
    }
    .reason()
}

/// The image Single image mode has loaded. Everything after the load works
/// from `copy`, so a file that changes on disk cannot mix versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedImage {
    /// 1 for the first image of a launch, then one more for each load.
    pub key: u64,
    /// The original's file name, its bytes as they are.
    pub file_name: OsString,
    /// The folder the original was in: where Save writes next to it.
    pub folder: PathBuf,
    /// The original's extension with its dot (`".jpg"`), as it is spelled.
    pub extension: String,
    /// The private copy, `<private folder>/image-<key>/original<.ext>`.
    pub copy: PathBuf,
    /// The original's permissions, for the saved file.
    pub permissions: fs::Permissions,
    /// The size of the copy in bytes.
    pub size: u64,
    /// The size it is shown at; `None` for an SVG.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl LoadedImage {
    /// The original's path: its folder and its file name.
    pub fn original(&self) -> PathBuf {
        self.folder.join(&self.file_name)
    }

    /// What the window hears about this image, with the name Save as starts
    /// with for the output folder `out` (`None`: next to the original).
    pub fn info(&self, out: Option<&Path>) -> ImageInfo {
        ImageInfo {
            name: self.file_name.to_string_lossy().into_owned(),
            size: self.size,
            width: self.width,
            height: self.height,
            key: self.key.to_string(),
            suggested: suggested_name(&self.original(), out),
            extension: self.extension.clone(),
        }
    }
}

/// The name Save as starts with, without the extension: the name Batch would
/// give the output of `original` ([`batch_output_name`]), or its first free
/// numbered form ([`first_free_name`]) in the folder it would be written to,
/// `out` or the original's own folder. When no number is free, the unnumbered
/// name. A name that is not valid UTF-8 is decoded lossily.
pub fn suggested_name(original: &Path, out: Option<&Path>) -> String {
    suggested_name_with(original, out, first_free_name)
}

/// [`suggested_name`] with the free-name finder given; the tests pass one
/// that finds none.
fn suggested_name_with(
    original: &Path,
    out: Option<&Path>,
    free: impl FnOnce(&Path, &OsStr) -> Option<OsString>,
) -> String {
    let folder = out.unwrap_or_else(|| original.parent().unwrap_or(Path::new("")));
    free_stem(folder, &batch_output_name(original, out.is_some()), free)
}

/// The name for the Save as box, without the extension: the first free name
/// for the file name `name` in `folder` as `free` finds it
/// ([`first_free_name`] outside the tests), or `name` itself when none is
/// free. Only the last extension goes (`beach.v2.jpg` gives `beach.v2`). A
/// name that is not valid UTF-8 is decoded lossily.
pub(crate) fn free_stem(
    folder: &Path,
    name: &OsStr,
    free: impl FnOnce(&Path, &OsStr) -> Option<OsString>,
) -> String {
    let chosen = free(folder, name).unwrap_or_else(|| name.to_owned());
    Path::new(&chosen)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// `save_image`'s argument: the name typed in Save as (without the
/// extension) and the key of the version the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveArgs {
    pub name: String,
    pub preview: String,
}

/// `save_image`'s whole argument object: exactly
/// `{ "name": <string>, "preview": <string> }`. Anything else (no JSON, not
/// an object, a missing or an extra key, a key of another type, `null`) is
/// [`CommandError::InvalidName`]. The strings themselves are checked later.
pub fn parse_save_args(args: Option<&Value>) -> Result<SaveArgs, CommandError> {
    let args = args
        .and_then(Value::as_object)
        .filter(|args| args.len() == 2)
        .ok_or(CommandError::InvalidName)?;
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(CommandError::InvalidName)
    };
    Ok(SaveArgs {
        name: text("name")?,
        preview: text("preview")?,
    })
}

/// What a save writes, as [`Single::save_source`] found it in one lock
/// section: the current image (its folder, extension and permissions) and
/// the path of the version the window named.
#[derive(Debug, Clone)]
pub(crate) struct SaveSource {
    pub image: LoadedImage,
    /// `image-<key>/v<n><.ext>`: written once by its own worker, never
    /// again, so it holds version `n`'s bytes for as long as it exists.
    pub version: PathBuf,
}

/// What a drop in Single image mode does with its items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropPlan {
    /// The first item that is not a folder and has a PNG, JPG, SVG or WebP
    /// extension: the one to load.
    pub load: Option<PathBuf>,
    /// The other items Batch could take: folders, and files with one of
    /// those extensions. Other files do not count; switching cannot help
    /// them.
    pub left_out: usize,
}

/// Which item of a drop to load, and how many are left out.
pub fn plan_drop(paths: &[PathBuf]) -> DropPlan {
    let mut plan = DropPlan {
        load: None,
        left_out: 0,
    };
    for path in paths {
        let is_folder = path.is_dir();
        let is_image = !is_folder && has_supported_extension(path);
        if is_image && plan.load.is_none() {
            plan.load = Some(path.clone());
        } else if is_folder || is_image {
            plan.left_out += 1;
        }
    }
    plan
}

/// A reason for the window from an I/O error, never with a path in it: the
/// operating system's own text for an OS error, else only the kind's text
/// (a wrapped error such as `tempfile`'s carries the path in its message).
pub(crate) fn io_reason(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(_) => error.to_string(),
        None => error.kind().to_string(),
    }
}

/// Copy `src` to a new file `dest`, at most `limit` bytes: the copy's size
/// and `src`'s permissions. What is opened must be a regular file, checked
/// on the open file itself: something else put at `src` since the caller
/// checked it (a FIFO, a folder) is refused as "Not a readable file". On
/// Windows a folder cannot be opened as a file at all, so there the open
/// itself fails and the reason is the OS's own access-denied text. On
/// Unix it is opened with `O_NONBLOCK`, so a FIFO does not block the open
/// waiting for a writer (the flag changes nothing for a regular file). A
/// file that grew past `limit` since its size was checked is refused with
/// Batch's size reason, having read one byte past it at most; on any
/// failure no copy is left behind. Every reason is free of paths.
pub(crate) fn copy_capped(
    src: &Path,
    dest: &Path,
    limit: u64,
) -> Result<(u64, fs::Permissions), String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = options.open(src).map_err(|error| io_reason(&error))?;
    let metadata = file.metadata().map_err(|error| io_reason(&error))?;
    if !metadata.is_file() {
        return Err(Skip::NotAFile.to_string());
    }
    let permissions = metadata.permissions();
    let mut copy = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|error| io_reason(&error))?;
    let copied = io::copy(&mut file.take(limit.saturating_add(1)), &mut copy);
    drop(copy);
    let failure = match copied {
        Ok(size) if size <= limit => return Ok((size, permissions)),
        Ok(_) => Skip::TooLarge.to_string(),
        Err(error) => io_reason(&error),
    };
    // The copy was made by this call, in a folder only this user can use.
    if let Err(error) = fs::remove_file(dest) {
        eprintln!("lunaepix: could not remove a partial copy: {error}");
    }
    Err(failure)
}

/// Remove the folder at `path` and everything in it; a failure is logged and
/// otherwise ignored. A folder that is not there counts as removed. Whether
/// it is gone.
fn remove_folder(path: &Path) -> bool {
    match fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            eprintln!("lunaepix: could not remove a private image folder: {error}");
            false
        }
    }
}

/// One request for a compressed version: version `version` of image `key`,
/// made from the copy `src` into `dest` (`image-<key>/v<version><.ext>`),
/// stopped through `stop`.
#[derive(Debug, Clone)]
pub struct PreviewJob {
    pub key: u64,
    pub version: u64,
    pub src: PathBuf,
    pub dest: PathBuf,
    pub compression: Compression,
    pub stop: StopHandle,
}

impl PreviewJob {
    /// The one job the worker runs: the copy into the version file.
    pub fn job(&self) -> Job {
        Job::Optimize {
            src: self.src.clone(),
            dest: self.dest.clone(),
        }
    }

    /// The worker's options: the request's compression, never replacing.
    pub fn options(&self) -> Options {
        Options {
            compression: self.compression,
            write: WriteMode::NoReplace,
        }
    }
}

/// The most digits of a version number in a file name, as in a key.
const VERSION_DIGITS: usize = 10;

/// The version number in a version file's name, `v<n>` or `v<n>.<ext>`
/// with `n` 1 to 10 ASCII digits and an extension without a further dot;
/// `None` for any other name.
pub(crate) fn version_of(name: &OsStr) -> Option<u64> {
    let rest = name.to_str()?.strip_prefix('v')?;
    let number = match rest.split_once('.') {
        Some((_, extension)) if extension.contains('.') => return None,
        Some((number, _)) => number,
        None => rest,
    };
    digits(number, VERSION_DIGITS)
}

/// The kept version: the latest request whose worker succeeded.
#[derive(Debug)]
struct Ready {
    key: u64,
    version: u64,
    path: PathBuf,
}

/// The log of the clean-up: standard error, as the crate's other warnings.
fn warn(line: String) {
    eprintln!("{line}");
}

/// Remove the file at `path`; a file that is not there counts as removed,
/// any other failure goes to `log` and is otherwise ignored.
fn remove_file_or_log(path: &Path, log: &mut dyn FnMut(String)) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => log(format!(
            "lunaepix: could not remove a file of a compressed version: {error}"
        )),
    }
}

/// Where [`Single::settle_preview_with`] lets a test step in: before the
/// result is weighed, and after the outcome was sent but before the files
/// are cleaned up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettleSeam {
    BeforeDecision,
    AfterSend,
}

/// Remove every version file in `kept`'s folder numbered below `version`,
/// and every staging file ([`is_staging_name`]) a killed worker left there;
/// other files (the copy, `kept`, a newer version) stay. Run only while the
/// worker gate is held, so no staging file of a running worker is taken. A
/// folder that is gone (a new load or the window closing removed it, with
/// everything in it) is fine; any other failure goes to `log`.
fn clean_version_folder(kept: &Path, version: u64, log: &mut dyn FnMut(String)) {
    // `kept` is always `image-<key>/v<n><.ext>`; a path without a folder
    // has nothing to clean.
    let Some(folder) = kept.parent() else {
        return;
    };
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            log(format!(
                "lunaepix: could not list a private image folder: {error}"
            ));
            return;
        }
    };
    // An entry that cannot be read is skipped; the next load or the window
    // closing removes the whole folder anyway.
    for entry in entries.flatten() {
        let name = entry.file_name();
        if version_of(&name).is_some_and(|older| older < version) || is_staging_name(&name) {
            remove_file_or_log(&entry.path(), log);
        }
    }
}

/// The reason a load is refused once the window has closed. The window is
/// gone by then, so nobody reads it.
const CLOSED: &str = "the window has closed";

/// Single image mode's state, behind one lock that is never held while a
/// file is copied or probed.
#[derive(Debug, Default)]
pub struct Single {
    state: Mutex<State>,
    /// Held across every preview worker run and across the clean-up of a
    /// version folder, so the clean-up never runs while a worker writes.
    gate: Mutex<()>,
    /// Where the private folder is made; `None` for the system's temp folder.
    /// The tests give one they can break.
    root: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct State {
    view: View,
    /// The private folder, once made.
    folder: Option<TempDir>,
    /// The window has closed: no private folder is made again.
    closed: bool,
    image: Option<LoadedImage>,
    /// The key of the last load attempt; 0 before the first.
    last_key: u64,
    /// Every item of the last drop that left something out, for Switch to
    /// Batch; gone at the next load attempt and at a switch by hand.
    offer: Option<Vec<PathBuf>>,
    /// Stops the preview being made: the latest request's worker. Taken and
    /// used when a newer request comes, the view goes to Batch, a run
    /// starts, a new image is loaded and the window closes; from that lock
    /// section on, the stopped job sends nothing.
    preview: Option<StopHandle>,
    /// The number of the last version asked for; 0 before the first. One
    /// count for the whole launch, so a `(key, version)` pair is never
    /// given twice.
    last_version: u64,
    /// The latest request, `(key, version)`, while its result may still be
    /// sent: only its result is. Cleared whenever the preview is stopped.
    latest: Option<(u64, u64)>,
    /// The kept version of the current image, if one is ready.
    ready: Option<Ready>,
}

impl State {
    /// Take the preview's stop handle and forget the latest request. The
    /// job it stops sends nothing from now on: its outcome is either sent
    /// already (decided in an earlier lock section) or never.
    fn take_preview(&mut self) -> Option<StopHandle> {
        self.latest = None;
        self.preview.take()
    }
}

impl Single {
    pub fn new() -> Self {
        Self::default()
    }

    /// A state whose current image is `image`, as no load would give it (an
    /// extension the loader refuses): for the scheme's tests.
    #[cfg(test)]
    pub(crate) fn with_image(image: LoadedImage) -> Self {
        let single = Single::default();
        lock(&single.state).image = Some(image);
        single
    }

    /// A new request for a compressed version of the current image with
    /// `compression` ([`CommandError::NoImage`] without one, and nothing
    /// sent): the next version number, the latest request from now on.
    /// `preview-started` goes to `emit` before the lock is let go, so an
    /// older request's outcome can never follow it. The previous request's
    /// worker is stopped after the lock is let go; its outcome is never
    /// sent.
    pub(crate) fn begin_preview(
        &self,
        compression: Compression,
        emit: &mut impl FnMut(Event),
    ) -> Result<PreviewJob, CommandError> {
        let (job, previous) = {
            let mut state = lock(&self.state);
            let image = state.image.as_ref().ok_or(CommandError::NoImage)?;
            let (key, src) = (image.key, image.copy.clone());
            let version = state.last_version + 1;
            let dest = src.with_file_name(format!("v{version}{}", image.extension));
            state.last_version = version;
            state.latest = Some((key, version));
            let stop = StopHandle::new();
            let previous = state.preview.replace(stop.clone());
            let job = PreviewJob {
                key,
                version,
                src,
                dest,
                compression,
                stop,
            };
            // Sent before the lock is let go (see `settle_preview_with`).
            // Safe only while tauri's `tracing` feature is off: with it,
            // `eval_script` blocks until the main thread answers.
            emit(Event::PreviewStarted(PreviewStarted {
                image: key.to_string(),
                version,
            }));
            (job, previous)
        };
        if let Some(previous) = previous {
            previous.stop();
        }
        Ok(job)
    }

    /// What `job`'s worker gave (`None`: it was stopped before it started)
    /// becomes the window's event, but only while `job` is the latest
    /// request. Then a success is the kept version and `preview-ready`
    /// goes out; any failure gives `preview-failed` with Batch's reason and
    /// keeps the version kept before. A job that is no longer the latest (a
    /// newer request, a new image, a stop) sends nothing. The decision, the
    /// kept version and the event happen in one lock section, so a load, a
    /// stop or a newer request either comes first (and nothing is sent) or
    /// comes after the event. The files are cleaned up after the lock is
    /// let go: a stale job's own file is removed; after a success, the
    /// older versions and the staging files killed workers left
    /// ([`clean_version_folder`], under the worker gate).
    pub(crate) fn settle_preview(
        &self,
        job: &PreviewJob,
        result: Option<FileResult>,
        emit: &mut impl FnMut(Event),
    ) {
        self.settle_preview_with(job, result, emit, &mut |_| {});
    }

    /// [`Single::settle_preview`] with `seam` called at each [`SettleSeam`].
    pub(crate) fn settle_preview_with(
        &self,
        job: &PreviewJob,
        result: Option<FileResult>,
        emit: &mut impl FnMut(Event),
        seam: &mut dyn FnMut(SettleSeam),
    ) {
        /// What is left to do once the lock is let go.
        enum CleanUp {
            OwnFile,
            OlderFiles,
            Nothing,
        }
        seam(SettleSeam::BeforeDecision);
        let clean_up = {
            let mut state = lock(&self.state);
            if state.latest == Some((job.key, job.version)) {
                let failed = |reason: String| {
                    Event::PreviewFailed(PreviewFailed {
                        image: job.key.to_string(),
                        version: job.version,
                        reason,
                    })
                };
                let (event, clean_up) = match result {
                    Some(FileResult::Optimized(outcome)) => {
                        state.ready = Some(Ready {
                            key: job.key,
                            version: job.version,
                            path: job.dest.clone(),
                        });
                        let ready = preview_ready(job.key, job.version, job.compression, &outcome);
                        (Event::PreviewReady(ready), CleanUp::OlderFiles)
                    }
                    Some(FileResult::Failed(error)) => (failed(error.reason()), CleanUp::Nothing),
                    // The runner never skips an `Optimize` job; its text if
                    // it did.
                    Some(FileResult::Skipped(skip)) => (failed(skip.to_string()), CleanUp::Nothing),
                    None => {
                        let stopped = RunError::Stopped {
                            path: PathBuf::new(),
                        };
                        (failed(stopped.reason()), CleanUp::Nothing)
                    }
                };
                // Sent before the lock is let go, so nothing that makes this
                // job stale can be sent between the decision and the event
                // (the order of the `emit` calls; the window's own check on
                // `image` and `version` covers delivery). `emit` only queues
                // the event for the window; it never calls back into this
                // state, which would deadlock.
                // Safe only while tauri's `tracing` feature is off: with it,
                // `eval_script` blocks until the main thread answers.
                emit(event);
                clean_up
            } else {
                CleanUp::OwnFile
            }
        };
        seam(SettleSeam::AfterSend);
        match clean_up {
            // Only this job's worker wrote that file, and it has ended.
            CleanUp::OwnFile => remove_file_or_log(&job.dest, &mut warn),
            CleanUp::OlderFiles => {
                let _gate = self.worker_gate();
                clean_version_folder(&job.dest, job.version, &mut warn);
            }
            CleanUp::Nothing => {}
        }
    }

    /// Wait until no preview worker runs and no version folder is being
    /// cleaned up, and keep it so while the guard lives.
    pub(crate) fn worker_gate(&self) -> MutexGuard<'_, ()> {
        lock(&self.gate)
    }

    /// The kept version, `(key, version)`: the version a save may write.
    pub fn latest_ready(&self) -> Option<(u64, u64)> {
        lock(&self.state)
            .ready
            .as_ref()
            .map(|ready| (ready.key, ready.version))
    }

    /// What a save of the version `preview` writes, decided in one lock
    /// section: [`CommandError::NoImage`] without an image,
    /// [`CommandError::NotReady`] unless `preview` is exactly
    /// `"<key>-<n>"` of the kept version (not an older or a running one, no
    /// other spelling); otherwise the image and that version's path, taken
    /// together. The file is read after the lock is let go: a newer ready
    /// version or the window closing may delete it by then, but never
    /// change it (see [`SaveSource::version`]).
    pub(crate) fn save_source(&self, preview: &str) -> Result<SaveSource, CommandError> {
        let state = lock(&self.state);
        let image = state.image.as_ref().ok_or(CommandError::NoImage)?;
        let ready = state
            .ready
            .as_ref()
            .filter(|ready| {
                ready.key == image.key && format!("{}-{}", ready.key, ready.version) == preview
            })
            .ok_or(CommandError::NotReady)?;
        Ok(SaveSource {
            image: image.clone(),
            version: ready.path.clone(),
        })
    }

    /// The file the `preview` scheme serves: for `version` `None` the copy
    /// of the current image when it has `key`, else the kept version when
    /// it is `(key, version)`. `None` for anything else.
    pub(crate) fn served_file(&self, key: u64, version: Option<u64>) -> Option<PathBuf> {
        let state = lock(&self.state);
        let image = state.image.as_ref().filter(|image| image.key == key)?;
        match version {
            None => Some(image.copy.clone()),
            Some(version) => state
                .ready
                .as_ref()
                .filter(|ready| (ready.key, ready.version) == (key, version))
                .map(|ready| ready.path.clone()),
        }
    }

    /// A state whose private folder is made inside `root`.
    #[cfg(test)]
    pub(crate) fn with_root(root: PathBuf) -> Self {
        Single {
            root: Some(root),
            ..Single::default()
        }
    }

    /// The view that shows.
    pub fn view(&self) -> View {
        lock(&self.state).view
    }

    /// Show `view`. The Switch to Batch offer goes; a switch to Batch stops
    /// the preview being made, which sends nothing after this.
    pub(crate) fn set_view(&self, view: View) {
        let preview = {
            let mut state = lock(&self.state);
            state.view = view;
            state.offer = None;
            match view {
                View::Batch => state.take_preview(),
                View::Single => None,
            }
        };
        if let Some(preview) = preview {
            preview.stop();
        }
    }

    /// The loaded image, if any.
    pub fn current(&self) -> Option<LoadedImage> {
        lock(&self.state).image.clone()
    }

    /// A new key and the private folder to put its image folder in, making
    /// the private folder the first time. Refused after [`Single::shutdown`].
    fn next_key(&self) -> Result<(u64, PathBuf), String> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(CLOSED.to_owned());
        }
        let folder = match &state.folder {
            Some(folder) => folder.path().to_path_buf(),
            None => {
                let mut builder = tempfile::Builder::new();
                builder.prefix("lunaepix-");
                // tempfile makes a folder with the process's default mode
                // (0755 with the usual umask); only this user may read it.
                #[cfg(unix)]
                builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
                let made = match &self.root {
                    Some(root) => builder.tempdir_in(root),
                    None => builder.tempdir(),
                }
                .map_err(|error| io_reason(&error))?;
                let path = made.path().to_path_buf();
                state.folder = Some(made);
                path
            }
        };
        state.last_key += 1;
        Ok((state.last_key, folder))
    }

    /// Give up a load attempt: remove its image folder `folder`. Once the
    /// window has closed, also remove the private folder `private` when it
    /// is empty: the attempt's folder may have been made while shutdown was
    /// removing it, and kept it there.
    fn discard_attempt(&self, folder: &Path, private: &Path) {
        remove_folder(folder);
        // `closed` never goes back to false, so this cannot go stale.
        let closed = lock(&self.state).closed;
        if closed {
            match fs::remove_dir(private) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => eprintln!("lunaepix: could not remove the private folder: {error}"),
            }
        }
    }

    /// Load `src`: check its extension and size, copy it into the private
    /// folder and ask `prober` about the copy. On success it is the current
    /// image, the old image's preview is stopped (it sends nothing after
    /// the image is stored),
    /// its kept version forgotten and its folder (copy and versions)
    /// removed. On failure the current image, its preview and its kept
    /// version stay, the attempt's folder is removed, and the reason
    /// (Batch's) is returned. A load that finishes after
    /// [`Single::shutdown`] keeps nothing: the image is not stored and the
    /// attempt's folder is removed.
    pub(crate) fn load(
        &self,
        src: &Path,
        prober: impl Fn(&Path) -> ProbeAnswer,
    ) -> Result<(), String> {
        self.load_with(src, prober, || {})
    }

    /// [`Single::load`] with `after_key` called between taking the new key
    /// and making the image's folder; the tests close the window there.
    pub(crate) fn load_with(
        &self,
        src: &Path,
        prober: impl Fn(&Path) -> ProbeAnswer,
        after_key: impl FnOnce(),
    ) -> Result<(), String> {
        if !has_supported_extension(src) {
            return Err(NOT_AN_IMAGE_NOTICE.to_owned());
        }
        // Checked before anything is copied, as the planner checks: a path
        // that is not a regular file is not opened (a FIFO would block).
        // `copy_capped` checks the open file again, for a path swapped since.
        let metadata = fs::metadata(src).map_err(|_| Skip::NotAFile.to_string())?;
        if !metadata.is_file() {
            return Err(Skip::NotAFile.to_string());
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Skip::TooLarge.to_string());
        }
        let (key, private) = self.next_key()?;
        after_key();
        let folder = private.join(format!("image-{key}"));
        // A supported extension is ASCII, so nothing is lost here.
        let extension = format!(".{}", src.extension().unwrap_or_default().to_string_lossy());
        let copy = folder.join(format!("original{extension}"));
        let loaded = fs::create_dir(&folder)
            .map_err(|error| io_reason(&error))
            .and_then(|()| copy_capped(src, &copy, MAX_FILE_BYTES))
            .and_then(|(size, permissions)| {
                let probe = match prober(&copy) {
                    ProbeAnswer::Size(probe) => probe,
                    ProbeAnswer::Refused(reason) => return Err(reason),
                    ProbeAnswer::Failed => return Err(probe_failed_reason()),
                };
                Ok(LoadedImage {
                    key,
                    file_name: src.file_name().unwrap_or_default().to_owned(),
                    folder: src.parent().unwrap_or(Path::new("")).to_path_buf(),
                    extension,
                    copy: copy.clone(),
                    permissions,
                    size,
                    width: probe.width,
                    height: probe.height,
                })
            });
        let image = match loaded {
            Ok(image) => image,
            Err(reason) => {
                self.discard_attempt(&folder, &private);
                return Err(reason);
            }
        };
        // `next_key` found the window open, but it may have closed since:
        // looked at again in the section that stores the image.
        let committed = {
            let mut state = lock(&self.state);
            if state.closed {
                None
            } else {
                state.ready = None;
                Some((state.image.replace(image), state.take_preview()))
            }
        };
        let Some((old, preview)) = committed else {
            self.discard_attempt(&folder, &private);
            return Err(CLOSED.to_owned());
        };
        if let Some(preview) = preview {
            preview.stop();
        }
        if let Some(old) = old
            && let Some(old_folder) = old.copy.parent()
        {
            remove_folder(old_folder);
        }
        Ok(())
    }

    /// The `image-changed` payload: the current image (if any) with the name
    /// suggested for `out`, `left_out` and `notice`.
    pub(crate) fn image_changed(
        &self,
        left_out: usize,
        notice: Option<String>,
        out: Option<&Path>,
    ) -> ImageChanged {
        // `current` clones the image, so the folder listing behind the
        // suggested name is read without holding the lock.
        ImageChanged {
            image: self.current().map(|image| image.info(out)),
            left_out,
            notice,
        }
    }

    /// The `name-suggested` payload for `out`, when an image is loaded.
    pub(crate) fn name_suggested(&self, out: Option<&Path>) -> Option<NameSuggested> {
        self.current().map(|image| NameSuggested {
            image: image.key.to_string(),
            suggested: suggested_name(&image.original(), out),
        })
    }

    /// Keep `offer` (the items of a drop that left something out), or none.
    pub(crate) fn set_offer(&self, offer: Option<Vec<PathBuf>>) {
        lock(&self.state).offer = offer;
    }

    /// The offer, taken: a second call gives `None`.
    pub(crate) fn take_offer(&self) -> Option<Vec<PathBuf>> {
        lock(&self.state).offer.take()
    }

    /// Stop the preview being made, if one is; it sends nothing after this.
    pub(crate) fn stop_preview(&self) {
        let preview = lock(&self.state).take_preview();
        if let Some(preview) = preview {
            preview.stop();
        }
    }

    /// The window closes: stop the preview (it sends nothing after this),
    /// forget the image, its kept version and the offer, and remove the
    /// private folder. No private folder is made after this, and a load
    /// still going keeps nothing.
    pub fn shutdown(&self) {
        let (folder, preview) = {
            let mut state = lock(&self.state);
            state.closed = true;
            state.image = None;
            state.ready = None;
            state.offer = None;
            (state.folder.take(), state.take_preview())
        };
        if let Some(preview) = preview {
            preview.stop();
        }
        if let Some(folder) = folder
            && let Err(error) = folder.close()
        {
            eprintln!("lunaepix: could not remove the private folder: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use lunaepix_core::Probe;
    use tempfile::TempDir;

    use super::*;

    fn one_pixel(_: &Path) -> ProbeAnswer {
        ProbeAnswer::Size(Probe {
            width: Some(1),
            height: Some(1),
        })
    }

    /// The private folder of `single`, found from its current image's copy.
    fn private_folder(single: &Single) -> PathBuf {
        let image = single.current().expect("an image is loaded");
        image.copy.parent().unwrap().parent().unwrap().to_path_buf()
    }

    // --- picking the image out of a drop ----------------------------------------

    #[test]
    fn plan_drop_counts_folders_and_images_only() {
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("photos");
        fs::create_dir(&folder).unwrap();
        // A folder whose name looks like an image is a folder.
        let png_folder = dir.path().join("looks.png");
        fs::create_dir(&png_folder).unwrap();
        let text = dir.path().join("notes.txt");
        let first = dir.path().join("FIRST.PNG");
        let second = dir.path().join("second.webp");
        for file in [&text, &first, &second] {
            fs::write(file, b"x").unwrap();
        }

        let plan = plan_drop(&[
            text.clone(),
            png_folder.clone(),
            first.clone(),
            folder.clone(),
            second.clone(),
        ]);
        // The first image file, in any case of the extension; the folders and
        // the other image count, the text file does not.
        assert_eq!(
            plan,
            DropPlan {
                load: Some(first),
                left_out: 3
            }
        );

        // No image file: nothing to load, the folders still count.
        assert_eq!(
            plan_drop(&[text.clone(), folder]),
            DropPlan {
                load: None,
                left_out: 1
            }
        );
        assert_eq!(
            plan_drop(&[text]),
            DropPlan {
                load: None,
                left_out: 0
            }
        );
    }

    // --- the copy -----------------------------------------------------------------

    #[test]
    fn copy_capped_copies_a_file_of_exactly_the_limit() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"12345").unwrap();
        let dest = dir.path().join("copy.png");
        let (size, permissions) = copy_capped(&src, &dest, 5).unwrap();
        assert_eq!(size, 5);
        assert_eq!(fs::read(&dest).unwrap(), b"12345");
        assert_eq!(permissions, fs::metadata(&src).unwrap().permissions());
    }

    #[test]
    fn copy_capped_refuses_a_file_over_the_limit_and_leaves_no_copy() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"123456").unwrap();
        let dest = dir.path().join("copy.png");
        assert_eq!(
            copy_capped(&src, &dest, 5),
            Err("File is too large".to_owned())
        );
        assert!(!dest.exists());
    }

    #[test]
    fn copy_capped_reports_a_write_error_for_a_missing_folder() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"png").unwrap();
        let dest = dir.path().join("gone").join("copy.png");
        let reason = copy_capped(&src, &dest, 5).unwrap_err();
        assert!(!reason.is_empty());
        assert!(!reason.contains(dir.path().to_str().unwrap()), "{reason}");
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_capped_reports_a_read_error_without_a_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"png").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&src).is_ok() {
            // Run as root, which reads it anyway: nothing to check here.
            return;
        }
        let dest = dir.path().join("copy.png");
        let reason = copy_capped(&src, &dest, 5).unwrap_err();
        assert_eq!(
            reason,
            io::Error::from_raw_os_error(13).to_string(),
            "the OS's own text"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn io_reason_never_carries_a_path() {
        // An OS error: its own text, which has no path.
        let os = io::Error::from_raw_os_error(2);
        assert_eq!(io_reason(&os), os.to_string());
        // A wrapped error with the path in its message: only the kind's text.
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing");
        let wrapped = tempfile::Builder::new()
            .tempdir_in(&missing)
            .expect_err("no such folder");
        assert!(
            wrapped.to_string().contains(missing.to_str().unwrap()),
            "the fixture carries the path: {wrapped}"
        );
        let reason = io_reason(&wrapped);
        assert!(!reason.contains(missing.to_str().unwrap()), "{reason}");
        assert_eq!(reason, wrapped.kind().to_string());
    }

    #[test]
    fn a_folder_that_cannot_be_removed_is_logged_and_skipped() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a file, not a folder");
        fs::write(&file, b"x").unwrap();
        assert!(!remove_folder(&file));
        assert!(file.exists());
        // A folder that does not exist (any more) is not a failure.
        assert!(remove_folder(&dir.path().join("gone")));
        let folder = dir.path().join("image-1");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("original.png"), b"x").unwrap();
        assert!(remove_folder(&folder));
        assert!(!folder.exists());
    }

    // --- the suggested name ---------------------------------------------------------

    #[test]
    fn suggested_falls_back_to_the_plain_stem_when_no_number_is_free() {
        let asked = Cell::new(false);
        let original = Path::new("/srv/in/holiday.jpg");
        let suggested = suggested_name_with(original, None, |folder, name| {
            assert_eq!(folder, Path::new("/srv/in"));
            assert_eq!(name, OsStr::new("optimized-holiday.jpg"));
            asked.set(true);
            None
        });
        assert!(asked.get());
        assert_eq!(suggested, "optimized-holiday");
        // When a free name is found, that one.
        let found = suggested_name_with(original, None, |_, _| {
            Some(OsString::from("optimized-holiday-7.jpg"))
        });
        assert_eq!(found, "optimized-holiday-7");
    }

    #[test]
    fn free_stem_falls_back_to_the_name_when_no_number_is_free() {
        let asked = Cell::new(false);
        let stem = free_stem(
            Path::new("/srv/out"),
            OsStr::new("beach.v2.jpg"),
            |folder, name| {
                assert_eq!(folder, Path::new("/srv/out"));
                assert_eq!(name, OsStr::new("beach.v2.jpg"));
                asked.set(true);
                None
            },
        );
        assert!(asked.get());
        // The stem: not empty, without the extension, the dot inside kept.
        assert_eq!(stem, "beach.v2");
        // When a free name is found, its stem.
        let found = free_stem(Path::new("/srv/out"), OsStr::new("beach.jpg"), |_, _| {
            Some(OsString::from("beach-3.jpg"))
        });
        assert_eq!(found, "beach-3");
    }

    // --- loading ------------------------------------------------------------------

    #[test]
    fn a_missing_file_or_a_folder_is_not_a_readable_file() {
        let dir = TempDir::new().unwrap();
        let single = Single::with_root(dir.path().to_path_buf());
        let called = Cell::new(0);
        let prober = |path: &Path| {
            called.set(called.get() + 1);
            one_pixel(path)
        };
        let folder = dir.path().join("folder.png");
        fs::create_dir(&folder).unwrap();
        for src in [dir.path().join("missing.png"), folder] {
            assert_eq!(
                single.load(&src, prober),
                Err("Not a readable file".to_owned()),
                "{src:?}"
            );
        }
        assert_eq!(called.get(), 0);
        assert_eq!(single.current(), None);
        // A real file is loaded.
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        assert_eq!(single.load(&png, prober), Ok(()));
        assert_eq!(called.get(), 1);
    }

    #[test]
    fn a_private_folder_that_cannot_be_made_refuses_the_load_without_a_path() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("not a folder");
        fs::write(&root, b"x").unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root.clone());
        let reason = single.load(&png, one_pixel).unwrap_err();
        assert!(!reason.is_empty());
        assert!(!reason.contains(dir.path().to_str().unwrap()), "{reason}");
        assert_eq!(single.current(), None);

        // With a root that is a folder, the same file loads.
        let single = Single::with_root(dir.path().to_path_buf());
        assert_eq!(single.load(&png, one_pixel), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn the_private_folder_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::new();
        single.load(&png, one_pixel).unwrap();
        let mode = fs::metadata(private_folder(&single))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        single.shutdown();
    }

    // --- clean-up -----------------------------------------------------------------

    #[test]
    fn shutdown_removes_the_private_folder() {
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let single = Single::with_root(root);
        single.load(&png, one_pixel).unwrap();
        let private = private_folder(&single);
        assert!(private.is_dir());
        single.set_offer(Some(vec![png.clone()]));

        single.shutdown();
        assert!(!private.exists());
        assert_eq!(single.current(), None);
        assert_eq!(single.take_offer(), None);
        // The original is never touched.
        assert_eq!(fs::read(&png).unwrap(), b"png");
    }

    #[test]
    fn a_load_after_shutdown_makes_no_new_folder() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root.clone());
        single.shutdown();
        assert!(single.load(&png, one_pixel).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(single.current(), None);
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_logs_a_folder_it_cannot_remove_and_forgets_the_image() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root);
        single.load(&png, one_pixel).unwrap();
        let image_folder = single
            .current()
            .unwrap()
            .copy
            .parent()
            .unwrap()
            .to_path_buf();
        fs::set_permissions(&image_folder, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(image_folder.join("probe"), b"x").is_ok() {
            // Run as root, which removes it anyway: nothing to check here.
            single.shutdown();
            return;
        }

        single.shutdown();
        assert_eq!(single.current(), None);
        assert!(image_folder.exists(), "the folder could not be removed");
        // No second private folder is made either.
        assert!(single.load(&png, one_pixel).is_err());

        fs::set_permissions(&image_folder, fs::Permissions::from_mode(0o700)).unwrap();
    }

    // --- previews: versions, settling, clean-up ---------------------------------

    use std::time::Duration;

    use lunaepix_core::{OptimizeError, Outcome, Quality};

    use crate::events::{Mode, PreviewFailed, PreviewReady};

    fn lossy(quality: u8) -> Compression {
        Compression::Lossy(Quality::new(quality).unwrap())
    }

    /// A state in a fresh folder with `a.png` loaded as image 1, and that
    /// folder (which also holds `b.png` and `notes.txt`).
    fn with_an_image() -> (TempDir, Single) {
        let dir = TempDir::new().unwrap();
        for name in ["a.png", "b.png", "notes.txt"] {
            fs::write(dir.path().join(name), b"png").unwrap();
        }
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let single = Single::with_root(root);
        single.load(&dir.path().join("a.png"), one_pixel).unwrap();
        (dir, single)
    }

    fn load_b(dir: &TempDir, single: &Single) {
        single.load(&dir.path().join("b.png"), one_pixel).unwrap();
    }

    fn success() -> FileResult {
        FileResult::Optimized(Outcome {
            before: 3,
            after: 2,
            millis: 5,
            note: None,
        })
    }

    fn src() -> PathBuf {
        PathBuf::from("/private/image-1/original.png")
    }

    fn optimiser_error() -> FileResult {
        FileResult::Failed(RunError::Optimize(OptimizeError::Invalid {
            path: src(),
            message: "damaged".to_owned(),
        }))
    }

    fn crash() -> FileResult {
        FileResult::Failed(RunError::Crashed {
            path: src(),
            cause: Crash::NoValidResult,
            stderr: "secret stderr".to_owned(),
        })
    }

    fn timeout() -> FileResult {
        FileResult::Failed(RunError::TimedOut {
            path: src(),
            limit: Duration::from_millis(300),
        })
    }

    fn stopped() -> FileResult {
        FileResult::Failed(RunError::Stopped { path: src() })
    }

    fn skipped() -> FileResult {
        FileResult::Skipped(Skip::TooLarge)
    }

    /// The version file `job` writes, written as its worker would.
    fn write_version(job: &PreviewJob) {
        fs::write(&job.dest, b"v").unwrap();
    }

    /// A ready version 1 of image 1: its job.
    fn ready_first(single: &Single) -> PreviewJob {
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        write_version(&job);
        assert!(settle(single, &job, Some(success())).is_some());
        job
    }

    /// `job` settles with `result`: the one event it sent, if any.
    fn settle(single: &Single, job: &PreviewJob, result: Option<FileResult>) -> Option<Event> {
        let mut sent = Vec::new();
        single.settle_preview(job, result, &mut |event| sent.push(event));
        assert!(sent.len() <= 1, "{sent:?}");
        sent.pop()
    }

    fn failed(key: u64, version: u64, reason: &str) -> Option<Event> {
        Some(Event::PreviewFailed(PreviewFailed {
            image: key.to_string(),
            version,
            reason: reason.to_owned(),
        }))
    }

    #[test]
    fn versions_start_at_1_and_only_go_up_across_images() {
        let (dir, single) = with_an_image();
        let first = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let second = single
            .begin_preview(Compression::Lossless, &mut |_| {})
            .unwrap();
        load_b(&dir, &single);
        let third = single.begin_preview(lossy(75), &mut |_| {}).unwrap();
        let pairs: Vec<_> = [&first, &second, &third]
            .iter()
            .map(|job| (job.key, job.version))
            .collect();
        assert_eq!(pairs, [(1, 1), (1, 2), (2, 3)]);
        // No version file is named twice.
        assert_ne!(first.dest, second.dest);
        assert_ne!(second.dest, third.dest);
    }

    #[test]
    fn a_job_reads_the_copy_and_writes_its_version_beside_it() {
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("Holiday.PNG");
        fs::write(&png, b"png").unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let single = Single::with_root(root);
        single.load(&png, one_pixel).unwrap();
        let copy = single.current().unwrap().copy;
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        assert_eq!(job.src, copy);
        assert_eq!(job.dest, copy.parent().unwrap().join("v1.PNG"));
        assert_eq!(
            job.job(),
            Job::Optimize {
                src: copy.clone(),
                dest: job.dest.clone()
            }
        );
        assert_eq!(
            job.options(),
            Options {
                compression: lossy(40),
                write: WriteMode::NoReplace
            }
        );
        assert!(!job.stop.is_stopped());
    }

    // The latest request: settled whatever its result.

    #[test]
    fn the_latest_success_sends_ready_and_is_kept() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        write_version(&job);
        let event = settle(&single, &job, Some(success()));
        assert_eq!(
            event,
            Some(Event::PreviewReady(PreviewReady {
                image: "1".to_owned(),
                version: 1,
                key: "1-1".to_owned(),
                mode: Mode::Lossy,
                quality: Some(40),
                before: 3,
                after: 2,
                note: None,
            }))
        );
        assert_eq!(single.latest_ready(), Some((1, 1)));
        assert!(job.dest.exists());
    }

    #[test]
    fn the_latest_optimiser_error_sends_failed_with_its_reason() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let event = settle(&single, &job, Some(optimiser_error()));
        assert_eq!(event, failed(1, 1, "damaged"));
        assert_eq!(single.latest_ready(), None);
    }

    #[test]
    fn the_latest_crash_sends_failed_with_its_reason() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let event = settle(&single, &job, Some(crash()));
        assert_eq!(
            event,
            failed(
                1,
                1,
                "the optimiser crashed on this file (it gave no valid result)"
            )
        );
    }

    #[test]
    fn the_latest_timeout_sends_failed_with_its_reason() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let event = settle(&single, &job, Some(timeout()));
        assert_eq!(
            event,
            failed(1, 1, "the optimiser timed out after 300ms and was stopped")
        );
    }

    #[test]
    fn the_latest_skip_sends_failed_with_the_skip_text() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let event = settle(&single, &job, Some(skipped()));
        assert_eq!(event, failed(1, 1, "File is too large"));
    }

    #[test]
    fn the_latest_empty_result_sends_failed_stopped() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let event = settle(&single, &job, None);
        assert_eq!(event, failed(1, 1, "the run was stopped"));
    }

    // A stale result for an older version: discarded, its file deleted, the
    // kept version untouched.

    /// Ready version 1, then requests 2 and 3; request 2 settles with
    /// `result` (its file written first when `wrote` is set).
    fn stale_by_version(result: Option<FileResult>, wrote: bool) {
        let (_dir, single) = with_an_image();
        let kept = ready_first(&single);
        let old = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let _new = single.begin_preview(lossy(60), &mut |_| {}).unwrap();
        if wrote {
            write_version(&old);
        }
        assert_eq!(settle(&single, &old, result), None);
        assert!(!old.dest.exists(), "the stale file is deleted");
        assert_eq!(single.latest_ready(), Some((1, 1)));
        assert!(kept.dest.exists(), "the kept version stays");
    }

    #[test]
    fn a_stale_success_for_an_older_version_is_discarded_and_its_file_deleted() {
        stale_by_version(Some(success()), true);
    }

    #[test]
    fn a_stale_optimiser_error_for_an_older_version_is_discarded() {
        stale_by_version(Some(optimiser_error()), false);
    }

    #[test]
    fn a_stale_crash_for_an_older_version_is_discarded() {
        stale_by_version(Some(crash()), false);
    }

    #[test]
    fn a_stale_timeout_for_an_older_version_is_discarded() {
        stale_by_version(Some(timeout()), false);
    }

    #[test]
    fn a_stale_stop_for_an_older_version_is_discarded() {
        stale_by_version(Some(stopped()), false);
    }

    #[test]
    fn a_stale_skip_for_an_older_version_is_discarded() {
        stale_by_version(Some(skipped()), false);
    }

    #[test]
    fn a_stale_empty_result_for_an_older_version_is_discarded() {
        stale_by_version(None, false);
    }

    // A stale result for an older image.

    /// A request on image 1, then image 2 loaded (with a request of its own
    /// when `request_on_new`); image 1's job settles with `result`.
    fn stale_by_image(result: Option<FileResult>, wrote: bool, request_on_new: bool) {
        let (dir, single) = with_an_image();
        let old = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        load_b(&dir, &single);
        if request_on_new {
            let new = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
            assert_eq!(new.key, 2);
        }
        if wrote {
            // As a worker that outlived the load would leave it.
            fs::create_dir_all(old.dest.parent().unwrap()).unwrap();
            write_version(&old);
        }
        assert_eq!(settle(&single, &old, result), None);
        assert!(!old.dest.exists());
        assert_eq!(single.latest_ready(), None);
    }

    #[test]
    fn a_stale_success_for_an_older_image_is_discarded_and_its_file_deleted() {
        stale_by_image(Some(success()), true, false);
    }

    #[test]
    fn a_stale_failure_for_an_older_image_is_discarded() {
        stale_by_image(Some(crash()), false, false);
    }

    #[test]
    fn a_stale_empty_result_for_an_older_image_is_discarded() {
        stale_by_image(None, false, false);
    }

    #[test]
    fn a_stale_success_for_an_older_image_with_a_request_on_the_new_one_is_discarded() {
        stale_by_image(Some(success()), true, true);
    }

    // Every stop makes the running job stale; other calls do not.

    /// A request on image 1; `trigger` runs; then the job settles as a
    /// killed worker would. What the window hears, and whether the job's
    /// stop handle was used.
    fn after(trigger: impl FnOnce(&TempDir, &Single)) -> (Option<Event>, bool) {
        let (dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        trigger(&dir, &single);
        let used = job.stop.is_stopped();
        (settle(&single, &job, Some(stopped())), used)
    }

    #[test]
    fn set_view_batch_makes_the_running_preview_stale() {
        assert_eq!(
            after(|_, single| single.set_view(View::Batch)),
            (None, true)
        );
    }

    #[test]
    fn stop_preview_makes_the_running_preview_stale() {
        assert_eq!(after(|_, single| single.stop_preview()), (None, true));
    }

    #[test]
    fn a_new_load_makes_the_running_preview_stale() {
        assert_eq!(after(load_b), (None, true));
    }

    #[test]
    fn shutdown_makes_the_running_preview_stale() {
        assert_eq!(after(|_, single| single.shutdown()), (None, true));
    }

    #[test]
    fn set_view_single_keeps_the_running_preview_latest() {
        assert_eq!(
            after(|_, single| single.set_view(View::Single)),
            (failed(1, 1, "the run was stopped"), false)
        );
    }

    #[test]
    fn a_refused_load_keeps_the_running_preview_latest() {
        let refused = |dir: &TempDir, single: &Single| {
            assert!(
                single
                    .load(&dir.path().join("notes.txt"), one_pixel)
                    .is_err()
            );
        };
        assert_eq!(after(refused), (failed(1, 1, "the run was stopped"), false));
    }

    // Clean-up of version files.

    #[test]
    fn a_ready_version_deletes_every_older_version_file_and_keeps_the_rest() {
        let (_dir, single) = with_an_image();
        let folder = single
            .current()
            .unwrap()
            .copy
            .parent()
            .unwrap()
            .to_path_buf();
        let jobs: Vec<_> = (0..3)
            .map(|_| single.begin_preview(lossy(40), &mut |_| {}).unwrap())
            .collect();
        for name in [
            "v1.png",
            "v2.png",
            "v4.png",
            "v.png",
            "vx.png",
            "v1.png.tmp",
            "notes",
        ] {
            fs::write(folder.join(name), b"x").unwrap();
        }
        write_version(&jobs[2]);
        assert!(settle(&single, &jobs[2], Some(success())).is_some());
        let mut left: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "notes",
                "original.png",
                "v.png",
                "v1.png.tmp",
                "v3.png",
                "v4.png",
                "vx.png"
            ]
        );
    }

    #[test]
    fn version_of_reads_v_and_digits_names() {
        for (name, version) in [
            ("v1.png", 1),
            ("v10.PNG", 10),
            ("v7", 7),
            ("v9999999999.jpg", 9_999_999_999),
        ] {
            assert_eq!(version_of(OsStr::new(name)), Some(version), "{name}");
        }
    }

    #[test]
    fn version_of_is_none_for_other_names() {
        for name in [
            "v",
            "v.png",
            "vx.png",
            "v1x.png",
            "V1.png",
            "v1.png.tmp",
            "original.png",
            "v12345678901.png",
            "v-1.png",
            "v+1.png",
            " v1.png",
            "v1 .png",
            "v\u{661}.png",
        ] {
            assert_eq!(version_of(OsStr::new(name)), None, "{name}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert_eq!(version_of(OsStr::from_bytes(b"v1\xff.png")), None);
        }
    }

    #[test]
    fn a_stale_file_that_cannot_be_deleted_is_logged_and_skipped() {
        let (_dir, single) = with_an_image();
        let old = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let _new = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        // A folder with something in it where the file should be.
        fs::create_dir(&old.dest).unwrap();
        fs::write(old.dest.join("x"), b"x").unwrap();
        assert_eq!(settle(&single, &old, Some(success())), None);
        assert!(old.dest.is_dir());
    }

    #[test]
    fn an_older_version_that_cannot_be_deleted_is_logged_and_skipped() {
        let (_dir, single) = with_an_image();
        let old = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let new = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        fs::create_dir(&old.dest).unwrap();
        fs::write(old.dest.join("x"), b"x").unwrap();
        write_version(&new);
        assert!(matches!(
            settle(&single, &new, Some(success())),
            Some(Event::PreviewReady(_))
        ));
        assert!(old.dest.is_dir());
        assert_eq!(single.latest_ready(), Some((1, 2)));
    }

    #[test]
    fn a_removed_image_folder_is_not_logged_and_the_version_still_kept() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        fs::remove_dir_all(job.dest.parent().unwrap()).unwrap();
        assert!(matches!(
            settle(&single, &job, Some(success())),
            Some(Event::PreviewReady(_))
        ));
        assert_eq!(single.latest_ready(), Some((1, 1)));
    }

    #[test]
    fn a_failed_latest_keeps_the_previous_ready_version() {
        let (_dir, single) = with_an_image();
        let kept = ready_first(&single);
        let next = single.begin_preview(lossy(60), &mut |_| {}).unwrap();
        assert_eq!(
            settle(&single, &next, Some(crash())),
            failed(
                1,
                2,
                "the optimiser crashed on this file (it gave no valid result)"
            )
        );
        assert_eq!(single.latest_ready(), Some((1, 1)));
        assert!(kept.dest.exists());
    }

    #[test]
    fn a_new_load_forgets_the_ready_version() {
        let (dir, single) = with_an_image();
        ready_first(&single);
        assert_eq!(single.latest_ready(), Some((1, 1)));
        load_b(&dir, &single);
        assert_eq!(single.latest_ready(), None);
    }

    #[test]
    fn shutdown_forgets_the_ready_version() {
        let (_dir, single) = with_an_image();
        ready_first(&single);
        assert_eq!(single.latest_ready(), Some((1, 1)));
        single.shutdown();
        assert_eq!(single.latest_ready(), None);
    }

    // --- fix round 1: one section per decision, the sweep, the load's commit --

    use std::sync::mpsc;

    /// `settle_preview_with` with no seam: what it sent.
    fn settled(single: &Single, job: &PreviewJob, result: Option<FileResult>) -> Vec<Event> {
        let mut sent = Vec::new();
        single.settle_preview_with(job, result, &mut |event| sent.push(event), &mut |_| {});
        sent
    }

    /// A recording log: every line handed to it.
    fn recorded(run: impl FnOnce(&mut dyn FnMut(String))) -> Vec<String> {
        let mut lines = Vec::new();
        run(&mut |line| lines.push(line));
        lines
    }

    fn names_in(folder: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn begin_preview_sends_preview_started_in_its_section() {
        let (_dir, single) = with_an_image();
        let mut sent = Vec::new();
        let mut locked = Vec::new();
        let job = single
            .begin_preview(lossy(40), &mut |event| {
                locked.push(single.state.try_lock().is_err());
                sent.push(event);
            })
            .unwrap();
        assert_eq!(
            sent,
            [Event::PreviewStarted(PreviewStarted {
                image: "1".to_owned(),
                version: job.version,
            })]
        );
        assert_eq!(locked, [true], "sent while the state was locked");
    }

    #[test]
    fn begin_preview_without_an_image_sends_nothing() {
        let single = Single::new();
        let mut sent = Vec::new();
        assert!(matches!(
            single.begin_preview(lossy(40), &mut |event| sent.push(event)),
            Err(CommandError::NoImage)
        ));
        assert!(sent.is_empty());
    }

    /// The latest request settles with `result`: its one outcome is `name`,
    /// sent while the state was locked.
    fn sent_inside_the_section(result: FileResult, name: &str) {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        write_version(&job);
        let mut locked = Vec::new();
        let mut names = Vec::new();
        single.settle_preview_with(
            &job,
            Some(result),
            &mut |event| {
                locked.push(single.state.try_lock().is_err());
                names.push(event.name());
            },
            &mut |_| {},
        );
        assert_eq!(names, [name]);
        assert_eq!(locked, [true]);
    }

    #[test]
    fn the_ready_outcome_is_sent_inside_the_section() {
        sent_inside_the_section(success(), "preview-ready");
    }

    #[test]
    fn the_failed_outcome_is_sent_inside_the_section() {
        sent_inside_the_section(crash(), "preview-failed");
    }

    #[test]
    fn the_seams_come_before_the_decision_and_after_the_send() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        write_version(&job);
        let order = std::cell::RefCell::new(Vec::new());
        single.settle_preview_with(
            &job,
            Some(success()),
            &mut |event| order.borrow_mut().push(event.name().to_owned()),
            &mut |seam| order.borrow_mut().push(format!("{seam:?}")),
        );
        assert_eq!(
            order.into_inner(),
            ["BeforeDecision", "preview-ready", "AfterSend"]
        );
    }

    // The sweep of staging files a killed worker left.

    #[test]
    fn a_ready_version_sweeps_leftover_staging_files_and_keeps_other_names() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let folder = job.dest.parent().unwrap().to_path_buf();
        for name in [
            ".tmpAbC123",
            ".tmp000000",
            ".tmpAbC12",
            ".tmpAbC1234",
            "notes.tmp",
            "v1.png.tmp",
        ] {
            fs::write(folder.join(name), b"x").unwrap();
        }
        write_version(&job);
        assert_eq!(settled(&single, &job, Some(success())).len(), 1);
        assert_eq!(
            names_in(&folder),
            [
                ".tmpAbC12",
                ".tmpAbC1234",
                "notes.tmp",
                "original.png",
                "v1.png",
                "v1.png.tmp"
            ]
        );
    }

    #[test]
    fn a_stale_result_sweeps_no_staging_files() {
        let (_dir, single) = with_an_image();
        let old = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let _new = single.begin_preview(lossy(60), &mut |_| {}).unwrap();
        let staging = old.dest.parent().unwrap().join(".tmpAbC123");
        fs::write(&staging, b"x").unwrap();
        write_version(&old);
        assert!(settled(&single, &old, Some(success())).is_empty());
        assert!(staging.exists());
    }

    #[test]
    fn a_failed_latest_sweeps_no_staging_files() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        let staging = job.dest.parent().unwrap().join(".tmpAbC123");
        fs::write(&staging, b"x").unwrap();
        assert_eq!(settled(&single, &job, Some(crash())).len(), 1);
        assert!(staging.exists());
    }

    #[test]
    fn a_worker_running_during_the_sweep_keeps_its_staging_file() {
        let (_dir, single) = with_an_image();
        let job = single.begin_preview(lossy(40), &mut |_| {}).unwrap();
        write_version(&job);
        let folder = job.dest.parent().unwrap().to_path_buf();
        let leftover = folder.join(".tmpDead00");
        fs::write(&leftover, b"x").unwrap();
        let (staged_tx, staged_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let single = &single;
            let folder = &folder;
            single.settle_preview_with(&job, Some(success()), &mut |_| {}, &mut |seam| {
                if seam != SettleSeam::AfterSend {
                    return;
                }
                // A newer worker starts as the sweep is due: it holds the
                // gate, stages its file, and persists it a moment later.
                let staged_tx = staged_tx.clone();
                scope.spawn(move || {
                    let _gate = single.worker_gate();
                    let staged = folder.join(".tmpLive99");
                    fs::write(&staged, b"new").unwrap();
                    staged_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                    fs::rename(&staged, folder.join("v2.png"))
                        .expect("the staging file was still there to persist");
                });
                staged_rx.recv().unwrap();
            });
        });
        assert!(folder.join("v2.png").exists());
        assert!(!leftover.exists(), "the dead worker's file is swept");
    }

    #[test]
    fn a_staging_file_that_cannot_be_removed_is_logged_and_skipped() {
        let dir = TempDir::new().unwrap();
        let stuck = dir.path().join(".tmpAAAAAA");
        fs::create_dir(&stuck).unwrap();
        fs::write(stuck.join("x"), b"x").unwrap();
        let loose = dir.path().join(".tmpBBBBBB");
        fs::write(&loose, b"x").unwrap();
        let lines = recorded(|log| clean_version_folder(&dir.path().join("v2.png"), 2, log));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(stuck.is_dir());
        assert!(!loose.exists());
    }

    #[test]
    fn listing_a_removed_image_folder_logs_nothing() {
        let dir = TempDir::new().unwrap();
        let kept = dir.path().join("image-1").join("v2.png");
        let lines = recorded(|log| clean_version_folder(&kept, 2, log));
        assert!(lines.is_empty(), "{lines:?}");
    }

    // Unix only: listing a file as a folder is `ENOTDIR` there. Windows
    // does not document which error it gives (it could be "path not found",
    // which counts as a removed folder); the code under test is the same on
    // both.
    #[cfg(unix)]
    #[test]
    fn an_image_folder_that_cannot_be_listed_is_logged() {
        let dir = TempDir::new().unwrap();
        let not_a_folder = dir.path().join("image-1");
        fs::write(&not_a_folder, b"x").unwrap();
        let lines = recorded(|log| clean_version_folder(&not_a_folder.join("v2.png"), 2, log));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            !lines[0].contains(dir.path().to_str().unwrap()),
            "{lines:?}"
        );
    }

    // A path that is not a regular file when it is opened.

    /// `run` on a thread of its own: its answer, or a panic after 5 s. Only
    /// the FIFO tests use it, so it is Unix-only as they are.
    #[cfg(unix)]
    fn within_5s<T: Send + 'static>(run: impl FnOnce() -> T + Send + 'static) -> T {
        let (answer_tx, answer_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = answer_tx.send(run());
        });
        answer_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("answered without blocking")
    }

    #[cfg(unix)]
    fn fifo(path: &Path) {
        let made = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(made.success());
    }

    #[cfg(unix)]
    #[test]
    fn copy_capped_refuses_a_fifo_without_blocking() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fifo(&src);
        let dest = dir.path().join("copy.png");
        let (from, to) = (src.clone(), dest.clone());
        let answer = within_5s(move || copy_capped(&from, &to, 5));
        assert_eq!(answer, Err("Not a readable file".to_owned()));
        assert!(!dest.exists());
    }

    #[test]
    fn copy_capped_refuses_a_folder_as_not_a_readable_file() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::create_dir(&src).unwrap();
        let dest = dir.path().join("copy.png");
        // Unix opens a folder for reading, and the check on the open file
        // refuses it. Windows refuses to open a folder as a file at all
        // (`ERROR_ACCESS_DENIED`), so its own text is the reason there.
        #[cfg(unix)]
        let reason = "Not a readable file".to_owned();
        #[cfg(windows)]
        let reason = io::Error::from_raw_os_error(5).to_string();
        assert_eq!(copy_capped(&src, &dest, 5), Err(reason));
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_load_path_is_refused_without_blocking() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fifo(&src);
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let single = Single::with_root(root.clone());
        let answer = within_5s(move || single.load(&src, one_pixel));
        assert_eq!(answer, Err("Not a readable file".to_owned()));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    // A load that finishes after the window closed.

    #[test]
    fn a_load_whose_probe_outlives_shutdown_is_refused_and_leaves_nothing() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root.clone());
        let answer = single.load(&png, |copy: &Path| {
            single.shutdown();
            // A copy that lands after the private folder was removed.
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::write(copy, b"png").unwrap();
            one_pixel(copy)
        });
        assert_eq!(answer, Err(CLOSED.to_owned()));
        assert_eq!(single.current(), None);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0, "nothing left");
    }

    #[test]
    fn a_load_whose_folder_comes_after_shutdown_is_refused_and_leaves_nothing() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root.clone());
        let answer = single.load_with(&png, one_pixel, || single.shutdown());
        assert!(answer.is_err());
        assert_eq!(single.current(), None);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0, "nothing left");
    }

    #[test]
    fn load_with_a_seam_that_does_nothing_loads() {
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(dir.path().to_path_buf());
        let called = Cell::new(false);
        assert_eq!(
            single.load_with(&png, one_pixel, || called.set(true)),
            Ok(())
        );
        assert!(called.get());
        assert_eq!(single.current().unwrap().key, 1);
    }
}
