//! The backend's state and what each command does with it: the file list,
//! the output setting, the busy guard, the run itself with its events, the
//! output folder a run or a save leaves behind, and Single image mode's
//! view, loading, drops and save (its state in `single`). No Tauri here: the
//! side effects (the window's events, the dialogs, the run thread, the
//! system opener, the runner, the probe, the save's writer) come in as
//! closures, so all of it is tested without a window, failures included.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lunaepix_core::{
    Compression, FileResult, Job, Options, ProbeAnswer, Quality, Skip, StopHandle, Worker,
    WriteMode, first_free_name, images_in_folder, plan_files_to, run_probe, run_until_stopped,
    save_new,
};
use serde::Serialize;
use serde_json::Value;

use crate::events::{
    Event, FileStarted, ImageSaved, ListChanged, ListRow, OVER_LIMIT_NAME, RunFinished, RunStarted,
    file_done, file_name_of, output_changed, over_limit_reason, over_limit_row,
};
use crate::names::check_save_name;
use crate::single::{
    FOLDERS_GO_IN_BATCH_NOTICE, NOT_AN_IMAGE_NOTICE, PreviewJob, SaveArgs, Single, View, free_stem,
    left_out_notice, parse_save_args, parse_view, plan_drop,
};
use crate::thumbs::Thumbs;

/// Why a command did nothing. Serialised as a bare string: `"busy"`,
/// `"no-output"`, `"open-failed"`, `"invalid-settings"`, `"empty-list"`,
/// `"output-missing"`, `"invalid-view"`, `"no-image"`, `"nothing-to-move"`,
/// `"not-ready"`, `"invalid-name"`, `"name-taken"` or `"save-failed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandError {
    /// The guard ([`RunGuard`]) is held: a run, a dialog (for files, a
    /// folder or an image), a load of a chosen or dropped image, or a save
    /// is active, or another command is in its short checked section (a
    /// preview request, a list or output change).
    Busy,
    /// `open_output_folder` has no folder: nothing was saved and no
    /// completed run wrote output yet, or the last completed run (after any
    /// save) wrote nothing. A save sets the folder as a run does.
    NoOutput,
    /// The system could not open the folder (for example, it was removed).
    OpenFailed,
    /// `start_run` was given settings that are not exactly
    /// `{ "mode": "lossless" | "lossy", "quality": <integer 0 to 100> }`.
    InvalidSettings,
    /// No file in the list can be compressed.
    EmptyList,
    /// The chosen output folder is gone or cannot be written to. For a save
    /// with no chosen folder it is the original's own folder; for a save it
    /// is also a folder that went away just before the write.
    OutputMissing,
    /// `set_view` was given anything but `{ "view": "single" }` or
    /// `{ "view": "batch" }`.
    InvalidView,
    /// Single image mode has no image loaded.
    NoImage,
    /// `move_drop_to_batch` with no drop to move: none left items out, or
    /// its offer is gone (a later load attempt, or a switch by hand).
    NothingToMove,
    /// `save_image` named a version that is not the kept version of the
    /// current image: none is ready yet, the last attempt failed, a newer
    /// one has replaced it, or the key is another image's. Also when a newer
    /// version or the window closing removed the named version's file
    /// before the save read it (after the read the save goes on).
    NotReady,
    /// `save_image` was given a name the rules refuse
    /// ([`crate::names::check_save_name`]), or an argument that is not
    /// exactly `{ "name": <string>, "preview": <string> }`.
    InvalidName,
    /// `save_image`'s name is taken (by a file or a folder) in the folder it
    /// would write to. Nothing is replaced, and the name is not numbered.
    NameTaken,
    /// `save_image`'s write failed for another reason (a full disk, a
    /// folder that refused it); nothing was written.
    SaveFailed,
}

/// Where [`Session::save_image_with`] lets a test step in: after every
/// check passed (the version's path taken in [`Single::save_source`]'s
/// section) and before the version is read; and after it was read, before
/// the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveSeam {
    AfterCheck,
    BeforeWrite,
}

/// The folder a save writes to: the chosen output folder `out`, or else
/// `original_folder`, the one the original is in.
fn save_folder(out: Option<&Path>, original_folder: &Path) -> PathBuf {
    out.unwrap_or(original_folder).to_path_buf()
}

/// The bytes of the version file at `path`. A file that is gone (a newer
/// version became ready and removed it, or the window closed) is
/// [`CommandError::NotReady`]; any other failure is
/// [`CommandError::SaveFailed`].
fn read_version(path: &Path) -> Result<Vec<u8>, CommandError> {
    fs::read(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => CommandError::NotReady,
        _ => CommandError::SaveFailed,
    })
}

/// What a failed save write ([`save_new`]) means for the window: a taken
/// name is [`CommandError::NameTaken`]; a folder that is gone (`save_new`
/// creates none: `NotFound`), also one removed after [`can_write_in`]
/// passed, is [`CommandError::OutputMissing`]; anything else is
/// [`CommandError::SaveFailed`].
fn save_error(error: &io::Error) -> CommandError {
    match error.kind() {
        io::ErrorKind::AlreadyExists => CommandError::NameTaken,
        io::ErrorKind::NotFound => CommandError::OutputMissing,
        _ => CommandError::SaveFailed,
    }
}

/// The `notice` of an add that changed nothing because a dropped or picked
/// folder held no image.
pub const EMPTY_FOLDER_NOTICE: &str =
    "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)";

/// The run's compression from the window's settings: exactly the two keys
/// `mode` (`"lossless"` or `"lossy"`) and `quality` (a JSON integer from 0 to
/// 100, also for a lossless run, where it is not used). Anything else, such
/// as a float, a string, `null`, a missing or an extra key, is
/// [`CommandError::InvalidSettings`].
pub fn parse_settings(value: &Value) -> Result<Compression, CommandError> {
    let settings = value.as_object().ok_or(CommandError::InvalidSettings)?;
    if settings.len() != 2 {
        return Err(CommandError::InvalidSettings);
    }
    // `as_u64` is `None` for a float (also `75.0`), a negative number, a
    // string, `null` or anything else that is not a whole JSON number.
    let quality = settings
        .get("quality")
        .and_then(Value::as_u64)
        .and_then(|quality| u8::try_from(quality).ok())
        .and_then(Quality::new)
        .ok_or(CommandError::InvalidSettings)?;
    match settings.get("mode").and_then(Value::as_str) {
        Some("lossless") => Ok(Compression::Lossless),
        Some("lossy") => Ok(Compression::Lossy(quality)),
        _ => Err(CommandError::InvalidSettings),
    }
}

/// The files the user has added, in the order they were added. Each entry is
/// a path to a file, or to a folder that could not be read; a folder that
/// can be read is replaced by its images. An entry is never expanded later:
/// see [`FileList::rows`].
#[derive(Debug, Default)]
pub struct FileList {
    entries: Vec<PathBuf>,
    /// Canonical paths of the entries, so that a file is listed once.
    seen: HashSet<PathBuf>,
    /// A run over this list has finished: the next add starts a fresh list,
    /// unless [`FileList::readd`] was called first.
    finished: bool,
}

impl FileList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `paths`; see the type for folders. Returns the notice
    /// [`EMPTY_FOLDER_NOTICE`] when the add listed nothing new because a
    /// folder among `paths` held no image.
    pub fn add(&mut self, paths: &[PathBuf]) -> Option<String> {
        if self.finished {
            self.clear();
        }
        let before = self.entries.len();
        let mut empty_folder = false;
        for path in paths {
            if !path.is_dir() {
                self.push(path.clone());
                continue;
            }
            match images_in_folder(path) {
                Ok(images) => {
                    empty_folder |= images.is_empty();
                    for image in images {
                        self.push(image);
                    }
                }
                // Kept as it is: planned, it is one "Not a readable file" row.
                Err(_) => self.push(path.clone()),
            }
        }
        (empty_folder && self.entries.len() == before).then(|| EMPTY_FOLDER_NOTICE.to_owned())
    }

    /// Add `path` unless its canonical path is listed already. A path that
    /// cannot be resolved (it does not exist) counts as itself.
    fn push(&mut self, path: PathBuf) {
        let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if self.seen.insert(canonical) {
            self.entries.push(path);
        }
    }

    /// Empty the list.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.seen.clear();
        self.finished = false;
    }

    /// The list planned now, into `out` or next to the originals, so a file
    /// deleted since it was added is a skip. An entry that is a folder now
    /// (one that could not be read when it was added, or a path that did not
    /// exist then) is one "Not a readable file" skip: nothing inside it is
    /// planned, so a run never covers files the list did not show. An entry
    /// that now resolves to the same file as an earlier one is planned once,
    /// so the list has at most one row per entry.
    pub fn rows(&self, out: Option<&Path>) -> Vec<Job> {
        let is_dir: Vec<bool> = self.entries.iter().map(|path| path.is_dir()).collect();
        let files: Vec<PathBuf> = self
            .entries
            .iter()
            .zip(&is_dir)
            .filter(|(_, dir)| !**dir)
            .map(|(path, _)| path.clone())
            .collect();
        // A path that turns into a folder between the check above and the
        // planner's own look is planned as a folder; its files are dropped
        // here, so they never reach a run. That entry then has no row.
        let listed: HashSet<&PathBuf> = files.iter().collect();
        let mut planned = plan_files_to(&files, out)
            .into_iter()
            .filter(|job| listed.contains(job_src(job)))
            .peekable();
        let mut jobs = Vec::with_capacity(self.entries.len());
        for (path, dir) in self.entries.iter().zip(is_dir) {
            if dir {
                jobs.push(Job::Skip {
                    src: path.clone(),
                    reason: Skip::NotAFile,
                });
            } else if let Some(job) = planned.next_if(|job| job_src(job) == path) {
                jobs.push(job);
            }
        }
        jobs
    }

    /// Whether a run over the list would compress at least one file.
    pub fn has_work(&self, out: Option<&Path>) -> bool {
        has_optimize(&self.rows(out))
    }

    /// A run over the list has ended: the next add first empties the list,
    /// unless [`FileList::readd`] is called first.
    pub fn mark_finished(&mut self) {
        self.finished = true;
    }

    /// Bring the list back after a run: the entries and the canonical paths
    /// stay as they were, and the next add appends instead of starting a
    /// fresh list. What was added stays what is listed, so the outputs a run
    /// wrote beside the originals are not in it.
    pub fn readd(&mut self) {
        self.finished = false;
    }
}

/// The file `job` is about.
fn job_src(job: &Job) -> &PathBuf {
    match job {
        Job::Optimize { src, .. } | Job::Skip { src, .. } => src,
    }
}

/// Whether `jobs` compress at least one file.
fn has_optimize(jobs: &[Job]) -> bool {
    jobs.iter().any(|job| matches!(job, Job::Optimize { .. }))
}

/// The size of the file at `path`, if it can be read.
pub fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|meta| meta.len())
}

/// The `list-changed` rows for `jobs`, with each compressible row's size from
/// `size_of`. The files the run limit leaves out share one row at the end,
/// as in the run ([`over_limit_row`]). `keys_for` gets the image path of
/// each row (`None` for a skipped row and for the over-limit row) and
/// answers each row's thumbnail key ([`Thumbs::snapshot`]).
pub fn list_rows(
    jobs: &[Job],
    size_of: impl Fn(&Path) -> Option<u64>,
    keys_for: impl FnOnce(&[Option<PathBuf>]) -> Vec<Option<String>>,
) -> Vec<ListRow> {
    let (kept, left_out) = without_over_limit(jobs);
    let mut rows: Vec<ListRow> = kept
        .iter()
        .map(|job| match job {
            Job::Optimize { src, .. } => ListRow {
                name: file_name_of(src),
                size: size_of(src),
                skip: None,
                thumb: None,
            },
            Job::Skip { src, reason } => ListRow {
                name: file_name_of(src),
                size: None,
                skip: Some(reason.to_string()),
                thumb: None,
            },
        })
        .collect();
    if left_out > 0 {
        rows.push(ListRow {
            name: OVER_LIMIT_NAME.to_owned(),
            size: None,
            skip: Some(over_limit_reason(left_out)),
            thumb: None,
        });
    }
    let images: Vec<Option<PathBuf>> = kept
        .iter()
        .map(|job| match job {
            Job::Optimize { src, .. } => Some(src.clone()),
            Job::Skip { .. } => None,
        })
        .chain((left_out > 0).then_some(None))
        .collect();
    for (row, key) in rows.iter_mut().zip(keys_for(&images)) {
        row.thumb = key;
    }
    rows
}

/// Whether a file can be created in `dir` now: one is created there under a
/// fresh random name and removed again. Fails when `dir` is gone, is not a
/// folder or cannot be written to, or when the check file cannot be removed
/// again (a folder that keeps what is written but refuses deletes is not
/// one to run into).
fn can_write_in(dir: &Path) -> std::io::Result<()> {
    tempfile::Builder::new()
        .prefix(".lunaepix-check-")
        .tempfile_in(dir)?
        .close()
}

/// Run `work(value)` on a thread that `spawn` starts. If no thread can be
/// started, run it on this thread instead, so that `value` (a run, a drop
/// with its ticket, or a preview job) is never lost and the window never
/// waits for events that cannot come.
pub fn on_own_thread_or_here<T: Send + 'static>(
    value: T,
    work: impl FnOnce(T) + Clone + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) {
    on_own_thread_or_else(value, work.clone(), spawn, work);
}

/// Run `work(value)` on a thread that `spawn` starts. If no thread can be
/// started, `otherwise(value)` runs on this thread instead, so `value` (a
/// ticket, or a request's responder) is never lost.
pub fn on_own_thread_or_else<T: Send + 'static>(
    value: T,
    work: impl FnOnce(T) + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    otherwise: impl FnOnce(T),
) {
    // The value waits here until one side takes it: the new thread, or this
    // one when the thread never started.
    let slot = Arc::new(Mutex::new(Some(value)));
    let on_thread = {
        let slot = Arc::clone(&slot);
        move || {
            if let Some(value) = lock(&slot).take() {
                work(value);
            }
        }
    };
    if let Err(error) = spawn(Box::new(on_thread)) {
        eprintln!("lunaepix: no thread could be started ({error})");
        if let Some(value) = lock(&slot).take() {
            otherwise(value);
        }
    }
}

/// A run that passed every check of `start_run`, holding its ticket.
#[derive(Debug)]
pub struct PreparedRun {
    ticket: RunTicket,
    jobs: Vec<Job>,
    compression: Compression,
}

impl PreparedRun {
    /// The planned jobs, one per list row.
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    /// The options every worker of this run gets.
    pub fn options(&self) -> Options {
        Options {
            compression: self.compression,
            // The app never replaces an output, in a chosen folder or next
            // to the originals: a taken name gets a number (`-1`, `-2`, …).
            write: WriteMode::NoReplace,
        }
    }
}

/// Everything the backend keeps between commands, except the worker.
#[derive(Debug, Default)]
pub struct Session {
    guard: RunGuard,
    list: Mutex<FileList>,
    /// The chosen output folder; `None` for next to the originals.
    output: Mutex<Option<PathBuf>>,
    /// The folder of the last save, or the output folder of the last
    /// completed run (`None` if it wrote nothing), whichever came last. Both
    /// set it while holding the guard, so they never interleave.
    last_output: Mutex<Option<PathBuf>>,
    /// The thumbnail keys of the list sent last, and their thumbnails.
    thumbs: Thumbs,
    /// Single image mode: the view, the loaded image, the preview state (the
    /// version being made and the one kept), and the Switch to Batch offer.
    single: Single,
}

/// `mutex`'s value, also after a panic elsewhere poisoned it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// The thumbnails of the list, for the `thumb` scheme's handler.
    pub fn thumbs(&self) -> &Thumbs {
        &self.thumbs
    }

    /// `choose_files`: claim the guard and hand the ticket to `open_dialog`,
    /// whose answer goes to [`Session::add_paths`].
    pub fn choose_files(
        &self,
        emit: &mut impl FnMut(Event),
        open_dialog: impl FnOnce(RunTicket),
    ) -> Result<(), CommandError> {
        open_dialog(claim_run(&self.guard, emit)?);
        Ok(())
    }

    /// A file dialog's answer, a drop while Batch shows, or Switch to Batch:
    /// add the paths and send the list.
    /// `None` or no paths (a closed dialog) gives the ticket back and then
    /// sends `run-cancelled` instead. Otherwise the ticket is given back
    /// after `list-changed` has gone out. The first add after a finished run
    /// starts a new list, so its rows get keys of a new list number.
    pub fn add_paths(
        &self,
        ticket: RunTicket,
        picked: Option<Vec<PathBuf>>,
        emit: &mut impl FnMut(Event),
    ) {
        let Some(paths) = picked.filter(|paths| !paths.is_empty()) else {
            drop(ticket);
            emit(Event::RunCancelled);
            return;
        };
        let notice = {
            let mut list = lock(&self.list);
            if list.finished {
                self.thumbs.new_list();
            }
            list.add(&paths)
        };
        emit(self.list_changed(notice));
        drop(ticket);
    }

    /// The `list-changed` event for the list as it is now, planned against
    /// the output setting, with each row's thumbnail key; the keys' snapshot
    /// is taken as the event is made. The three locks (the output setting, the
    /// list and the thumbnails) are taken one after another, never together.
    fn list_changed(&self, notice: Option<String>) -> Event {
        let out = lock(&self.output).clone();
        let jobs = lock(&self.list).rows(out.as_deref());
        Event::ListChanged(ListChanged {
            files: list_rows(&jobs, file_size, |rows| self.thumbs.snapshot(rows)),
            notice,
        })
    }

    /// A drop on the window: nothing for no paths; `run-busy` while busy;
    /// otherwise `receive_later` gets the ticket and the paths, to call
    /// [`Session::receive_drop`] off the main thread.
    pub fn drop_paths(
        &self,
        paths: Vec<PathBuf>,
        emit: &mut impl FnMut(Event),
        receive_later: impl FnOnce(RunTicket, Vec<PathBuf>),
    ) {
        if paths.is_empty() {
            return;
        }
        if let Ok(ticket) = claim_run(&self.guard, emit) {
            receive_later(ticket, paths);
        }
    }

    /// `clear_list`: an empty list, which is a new list for the thumbnails.
    pub fn clear_list(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError> {
        let ticket = claim_run(&self.guard, emit)?;
        lock(&self.list).clear();
        self.thumbs.new_list();
        emit(self.list_changed(None));
        drop(ticket);
        Ok(())
    }

    /// `readd_list`: the list as it was added, after a run, so another run
    /// can start on the same files. The list is planned again now, so a file
    /// deleted since is a skipped row. Busy as `clear_list` is; the next add
    /// appends to the list instead of starting a fresh one. It is the same
    /// list for the thumbnails: the rows keep their keys and thumbnails.
    pub fn readd_list(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError> {
        let ticket = claim_run(&self.guard, emit)?;
        lock(&self.list).readd();
        emit(self.list_changed(None));
        drop(ticket);
        Ok(())
    }

    /// `choose_output_folder`: claim the guard and hand the ticket to
    /// `open_dialog`, whose answer goes to [`Session::set_output`].
    pub fn choose_output_folder(
        &self,
        emit: &mut impl FnMut(Event),
        open_dialog: impl FnOnce(RunTicket),
    ) -> Result<(), CommandError> {
        open_dialog(claim_run(&self.guard, emit)?);
        Ok(())
    }

    /// A folder dialog's answer: the new output folder, announced by its
    /// name, then, with an image loaded in Single image mode, the name Save
    /// as starts with there (`name-suggested`); or, for a closed dialog,
    /// `run-cancelled` with the setting left as it was.
    pub fn set_output(
        &self,
        ticket: RunTicket,
        picked: Option<PathBuf>,
        emit: &mut impl FnMut(Event),
    ) {
        let Some(folder) = picked else {
            drop(ticket);
            emit(Event::RunCancelled);
            return;
        };
        let changed = output_changed(Some(&folder));
        *lock(&self.output) = Some(folder);
        emit(Event::OutputChanged(changed));
        if let Some(suggested) = self.name_suggested() {
            emit(suggested);
        }
        drop(ticket);
    }

    /// `clear_output_folder`: back to next to the originals, then
    /// `name-suggested` as [`Session::set_output`] sends it.
    pub fn clear_output_folder(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError> {
        let ticket = claim_run(&self.guard, emit)?;
        *lock(&self.output) = None;
        emit(Event::OutputChanged(output_changed(None)));
        if let Some(suggested) = self.name_suggested() {
            emit(suggested);
        }
        drop(ticket);
        Ok(())
    }

    /// `start_run`: busy check, `settings`, the output folder, the list
    /// planned now, then at least one compressible row, in that order. On
    /// success a Single image preview being made is stopped and `start`
    /// gets the run, to hand to [`Session::run_with_worker`] on the run
    /// thread.
    pub fn start_run(
        &self,
        settings: Option<&Value>,
        emit: &mut impl FnMut(Event),
        start: impl FnOnce(PreparedRun),
    ) -> Result<(), CommandError> {
        // Every early return drops the ticket, which frees the guard.
        let ticket = claim_run(&self.guard, emit)?;
        // A call without settings is refused like any badly shaped one.
        let settings = settings.ok_or(CommandError::InvalidSettings)?;
        let compression = parse_settings(settings)?;
        let out = lock(&self.output).clone();
        if let Some(out) = &out {
            can_write_in(out).map_err(|_| CommandError::OutputMissing)?;
        }
        // Planned now, so that a file deleted since it was added is a skip.
        let jobs = lock(&self.list).rows(out.as_deref());
        if !has_optimize(&jobs) {
            return Err(CommandError::EmptyList);
        }
        // A run and a Single image worker never run at the same time.
        self.single.stop_preview();
        start(PreparedRun {
            ticket,
            jobs,
            compression,
        });
        Ok(())
    }

    /// Run `prepared` through `runner` (see [`run_session_with`]); the list
    /// is marked finished as `run-finished` goes out.
    pub fn run(
        &self,
        prepared: PreparedRun,
        emit: &mut impl FnMut(Event),
        runner: impl FnOnce(&[Job], OnStart<'_>, OnProgress<'_>),
    ) {
        let PreparedRun {
            ticket,
            jobs,
            compression,
        } = prepared;
        run_session_with(
            ticket,
            &jobs,
            compression,
            &self.last_output,
            &mut self.finishing(emit),
            runner,
        );
    }

    /// [`Session::run`] with the real runner: `worker` with the run's
    /// options, until `stop` is used.
    pub fn run_with_worker(
        &self,
        prepared: PreparedRun,
        worker: &Worker,
        stop: &StopHandle,
        emit: &mut impl FnMut(Event),
    ) {
        let worker = worker.with_options(&prepared.options());
        let PreparedRun {
            ticket,
            jobs,
            compression,
        } = prepared;
        run_session(
            ticket,
            &jobs,
            compression,
            &worker,
            stop,
            &self.last_output,
            &mut self.finishing(emit),
        );
    }

    /// `emit`, which also marks the list finished as `run-finished` goes
    /// out: the run still holds its ticket then, so no add can come between.
    fn finishing<'a>(&'a self, emit: &'a mut impl FnMut(Event)) -> impl FnMut(Event) + 'a {
        move |event| {
            if let Event::RunFinished(_) = event {
                lock(&self.list).mark_finished();
            }
            emit(event);
        }
    }

    /// Single image mode's state, for the `preview` scheme and the save.
    pub fn single(&self) -> &Single {
        &self.single
    }

    /// The view that shows.
    pub fn view(&self) -> View {
        self.single.view()
    }

    /// `set_view`: busy check, then `args` must be exactly
    /// `{ "view": "single" }` or `{ "view": "batch" }` (see
    /// [`crate::single::parse_view`]). The Switch to Batch offer goes; a
    /// switch to Batch stops the preview being made. The guard is claimed
    /// and freed at once.
    pub fn set_view(
        &self,
        args: Option<&Value>,
        emit: &mut impl FnMut(Event),
    ) -> Result<(), CommandError> {
        // Every early return drops the ticket, which frees the guard.
        let ticket = claim_run(&self.guard, emit)?;
        let view = parse_view(args)?;
        self.single.set_view(view);
        drop(ticket);
        Ok(())
    }

    /// `choose_image`: claim the guard and hand the ticket to `open_dialog`,
    /// a one-file dialog whose answer goes to [`Session::load_image`].
    pub fn choose_image(
        &self,
        emit: &mut impl FnMut(Event),
        open_dialog: impl FnOnce(RunTicket),
    ) -> Result<(), CommandError> {
        open_dialog(claim_run(&self.guard, emit)?);
        Ok(())
    }

    /// The one-file dialog's answer: `None` (closed) gives the ticket back
    /// and sends `run-cancelled`, changing nothing. A file is a load
    /// attempt: the Switch to Batch offer goes, the file is loaded through
    /// `prober` (see [`Single::load`]), and `image-changed` goes out before
    /// the ticket is given back.
    pub fn load_image(
        &self,
        ticket: RunTicket,
        picked: Option<PathBuf>,
        prober: impl Fn(&Path) -> ProbeAnswer,
        emit: &mut impl FnMut(Event),
    ) {
        let Some(path) = picked else {
            drop(ticket);
            emit(Event::RunCancelled);
            return;
        };
        self.single.set_offer(None);
        let notice = self.single.load(&path, prober).err();
        emit(self.image_changed(0, notice));
        drop(ticket);
    }

    /// The `image-changed` event for the image that shows now, with the
    /// name suggested for the output setting as it is now.
    fn image_changed(&self, left_out: usize, notice: Option<String>) -> Event {
        let out = lock(&self.output).clone();
        Event::ImageChanged(self.single.image_changed(left_out, notice, out.as_deref()))
    }

    /// `name-suggested` for the output setting as it is now, when an image
    /// is loaded.
    fn name_suggested(&self) -> Option<Event> {
        let out = lock(&self.output).clone();
        self.single
            .name_suggested(out.as_deref())
            .map(Event::NameSuggested)
    }

    /// A drop's paths, with the ticket [`Session::drop_paths`] claimed, off
    /// the main thread: to [`Session::load_drop`] while Single image shows,
    /// to [`Session::add_paths`] while Batch shows. The view cannot change
    /// in between: `set_view` needs the ticket this drop holds.
    pub fn receive_drop(
        &self,
        ticket: RunTicket,
        paths: Vec<PathBuf>,
        prober: impl Fn(&Path) -> ProbeAnswer,
        emit: &mut impl FnMut(Event),
    ) {
        match self.single.view() {
            View::Single => self.load_drop(ticket, paths, prober, emit),
            View::Batch => self.add_paths(ticket, Some(paths), emit),
        }
    }

    /// A drop in Single image mode: the first PNG, JPG, SVG or WebP file is
    /// loaded ([`crate::single::plan_drop`]); the folders and other images
    /// are left out. A drop that left items out is kept for Switch to
    /// Batch, any earlier one is forgotten. The notice says how many were
    /// left out; or, when nothing could be loaded, why (Batch's reason for
    /// the image tried, "Folders go in Batch." for only folders, else that
    /// no image was dropped). `image-changed` goes out before the ticket is
    /// given back.
    pub fn load_drop(
        &self,
        ticket: RunTicket,
        paths: Vec<PathBuf>,
        prober: impl Fn(&Path) -> ProbeAnswer,
        emit: &mut impl FnMut(Event),
    ) {
        let plan = plan_drop(&paths);
        self.single.set_offer(None);
        let notice = match &plan.load {
            Some(src) => match self.single.load(src, prober) {
                Ok(()) => (plan.left_out > 0).then(|| left_out_notice(plan.left_out)),
                Err(reason) => Some(reason),
            },
            None if plan.left_out > 0 => Some(FOLDERS_GO_IN_BATCH_NOTICE.to_owned()),
            None => Some(NOT_AN_IMAGE_NOTICE.to_owned()),
        };
        if plan.left_out > 0 {
            self.single.set_offer(Some(paths));
        }
        emit(self.image_changed(plan.left_out, notice));
        drop(ticket);
    }

    /// `move_drop_to_batch`: busy check, then the last drop that left items
    /// out (else [`CommandError::NothingToMove`]). The view goes to Batch
    /// (which stops a preview being made) and every item of that drop is
    /// added to the list, as [`Session::add_paths`] adds them. Not on the
    /// main thread: adding reads folders.
    pub fn move_drop_to_batch(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError> {
        let ticket = claim_run(&self.guard, emit)?;
        let paths = self
            .single
            .take_offer()
            .ok_or(CommandError::NothingToMove)?;
        self.single.set_view(View::Batch);
        self.add_paths(ticket, Some(paths), emit);
        Ok(())
    }

    /// `preview_image`: busy check, `settings` (as `start_run` checks them,
    /// [`parse_settings`]), then an image ([`CommandError::NoImage`]), in
    /// that order. Then the new version is the latest request and
    /// `preview-started` goes out, in one lock section
    /// ([`Single::begin_preview`]), the previous request's worker is
    /// stopped, and `start` gets the job, to hand to [`Session::run_preview`]
    /// on a thread of its own. Making a version is not busy: the guard is
    /// held through the checks and the new version only (so a run cannot
    /// start in between and miss the new worker), and is given back before
    /// `start`.
    pub fn preview_image(
        &self,
        settings: Option<&Value>,
        emit: &mut impl FnMut(Event),
        start: impl FnOnce(PreviewJob),
    ) -> Result<(), CommandError> {
        // Every early return drops the ticket, which frees the guard.
        let ticket = claim_run(&self.guard, emit)?;
        // A call without settings is refused like any badly shaped one.
        let settings = settings.ok_or(CommandError::InvalidSettings)?;
        let compression = parse_settings(settings)?;
        let job = self.single.begin_preview(compression, emit)?;
        drop(ticket);
        start(job);
        Ok(())
    }

    /// Make `job`'s version with `worker` (its options from the job: the
    /// request's compression, never replacing) until the job is stopped,
    /// and send what [`Single::settle_preview`] makes of the result: for
    /// the latest request `preview-ready` or `preview-failed`, otherwise
    /// nothing. The worker runs while this holds the worker gate
    /// ([`Single::worker_gate`]), so it never starts while a version folder
    /// is being cleaned up.
    pub fn run_preview(&self, job: PreviewJob, worker: &Worker, emit: &mut impl FnMut(Event)) {
        let worker = worker.with_options(&job.options());
        let result = {
            let _gate = self.single.worker_gate();
            run_until_stopped(&[job.job()], &worker, &job.stop, |_, _, _| {}).pop()
        };
        self.single.settle_preview(&job, result, emit);
    }

    /// `save_image`: write the version the window shows under the name
    /// typed in Save as, never replacing a file. The guard is held for the
    /// whole save. Checks, in this order: busy; `args` exactly
    /// `{ "name": <string>, "preview": <string> }`
    /// ([`crate::single::parse_save_args`], else
    /// [`CommandError::InvalidName`]); an image
    /// ([`CommandError::NoImage`]); `preview` the key of the kept version
    /// ([`Single::save_source`], else [`CommandError::NotReady`]); the name
    /// rules with the original's extension ([`check_save_name`], else
    /// `InvalidName`); the target folder (the chosen output folder, or the
    /// original's own) writable now ([`can_write_in`], else
    /// [`CommandError::OutputMissing`]). Then the version's bytes go to
    /// `<folder>/<name><ext>` with the original's permissions through
    /// [`save_new`]: a taken name (a file or a folder) is
    /// [`CommandError::NameTaken`], a folder gone by then
    /// [`CommandError::OutputMissing`], any other failure
    /// [`CommandError::SaveFailed`]; nothing is written then. On success
    /// the folder becomes the one `open_output_folder` opens, and
    /// `image-saved` goes out, both before the guard is given back.
    ///
    /// Closing the window does not wait for a save: [`Session::shutdown`]
    /// neither stops nor waits for it, so a save that has read its bytes
    /// completes while the process runs on. If the process exits during the
    /// write, the file may be missing and `save_new`'s hidden staging file
    /// (`.tmp` and six characters) may be left in the target folder. Waiting
    /// at close is not done: the close handler runs on the main thread,
    /// and the guard it would wait for may be held by an open dialog.
    pub fn save_image(
        &self,
        args: Option<&Value>,
        emit: &mut impl FnMut(Event),
    ) -> Result<(), CommandError> {
        self.save_image_with(args, emit, save_new, &mut |_| {})
    }

    /// [`Session::save_image`] with the writer given (`save_new` outside the
    /// tests) and `seam` called at each [`SaveSeam`].
    pub(crate) fn save_image_with(
        &self,
        args: Option<&Value>,
        emit: &mut impl FnMut(Event),
        write: impl FnOnce(&Path, &[u8], fs::Permissions) -> io::Result<()>,
        seam: &mut dyn FnMut(SaveSeam),
    ) -> Result<(), CommandError> {
        // Every early return drops the ticket, which frees the guard. While
        // it is held no load, view change, preview request, run or output
        // change can start; a preview already running can still finish.
        let ticket = claim_run(&self.guard, emit)?;
        let SaveArgs { name, preview } = parse_save_args(args)?;
        let source = self.single.save_source(&preview)?;
        check_save_name(&name, &source.image.extension).map_err(|_| CommandError::InvalidName)?;
        let out = lock(&self.output).clone();
        let folder = save_folder(out.as_deref(), &source.image.folder);
        can_write_in(&folder).map_err(|_| CommandError::OutputMissing)?;
        seam(SaveSeam::AfterCheck);
        let bytes = read_version(&source.version)?;
        seam(SaveSeam::BeforeWrite);
        let file_name = format!("{name}{}", source.image.extension);
        write(&folder.join(&file_name), &bytes, source.image.permissions)
            .map_err(|error| save_error(&error))?;
        // Stored before the event, as a run stores its folder before
        // `run-finished`, so "Open folder" works as soon as the window
        // hears it. A run stores it only while holding the guard too.
        *lock(&self.last_output) = Some(folder.clone());
        emit(Event::ImageSaved(ImageSaved {
            folder: output_changed(out.as_deref()).folder,
            suggested: free_stem(&folder, OsStr::new(&file_name), first_free_name),
            name: file_name,
        }));
        drop(ticket);
        Ok(())
    }

    /// The window closes: stop the preview and remove Single image mode's
    /// private folder ([`Single::shutdown`]). A save that has not read its
    /// version yet then answers [`CommandError::NotReady`]; one that has goes
    /// on, and nothing here waits for it (see [`Session::save_image`]).
    pub fn shutdown(&self) {
        self.single.shutdown();
    }

    /// `open_output_folder`: the folder of the last save, or the last
    /// completed run's output folder (the chosen output folder when the run
    /// had one), whichever came last.
    pub fn open_output_folder<E>(
        &self,
        open: impl FnOnce(&Path) -> Result<(), E>,
    ) -> Result<(), CommandError> {
        open_last_output(&self.last_output, open)
    }
}

/// Allows one guarded action at a time (the busy guard). A run holds a
/// [`RunTicket`] from the moment it is asked for (before a dialog opens)
/// until it is over; so do a load of a chosen or dropped image (from the
/// dialog or the drop until `image-changed` has gone out), a save (until
/// `image-saved` has gone out) and a dialog for files or a folder. Every
/// other command except `open_output_folder` holds one through its own
/// checks and changes, a preview request only until its job is registered.
#[derive(Debug, Clone, Default)]
pub struct RunGuard(Arc<AtomicBool>);

/// Proof that the holder may act (run, load, save, change the list or the
/// output). Dropping it frees the guard, also when the holder panics.
#[derive(Debug)]
pub struct RunTicket(Arc<AtomicBool>);

impl RunGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// A ticket, or `None` while another ticket is alive.
    pub fn try_start(&self) -> Option<RunTicket> {
        self.0
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| RunTicket(Arc::clone(&self.0)))
    }
}

impl Drop for RunTicket {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A ticket for the caller (a run, a dialog, a load, a save or another
/// command); or, while another ticket is alive, `run-busy` to the window and
/// [`CommandError::Busy`] to the caller.
pub fn claim_run(
    guard: &RunGuard,
    emit: &mut impl FnMut(Event),
) -> Result<RunTicket, CommandError> {
    guard.try_start().ok_or_else(|| {
        emit(Event::RunBusy);
        CommandError::Busy
    })
}

/// The prober Single image mode loads with: [`run_probe`] with `worker`'s
/// program, the same program the runs and the thumbnails use.
pub fn worker_prober(worker: &Worker) -> impl Fn(&Path) -> ProbeAnswer + '_ {
    move |src| run_probe(&worker.program, src)
}

/// Open the folder in `last_output` (the last save's, or the last completed
/// run's output folder) with `open` (the system file manager).
/// [`CommandError::NoOutput`] without calling `open` if there is none;
/// [`CommandError::OpenFailed`] if `open` fails. The lock is not held while
/// `open` runs.
pub fn open_last_output<E>(
    last_output: &Mutex<Option<PathBuf>>,
    open: impl FnOnce(&Path) -> Result<(), E>,
) -> Result<(), CommandError> {
    let folder = last_output
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or(CommandError::NoOutput)?;
    open(&folder).map_err(|_| CommandError::OpenFailed)
}

/// What a run has done so far, kept up to date file by file so that a run
/// that ends early still has a summary. The output folder it records is the
/// one source of both `has_output` and the folder "Open folder" opens after
/// the run (until a later save).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    optimised: usize,
    saved_bytes: u64,
    output: Option<PathBuf>,
}

impl Tally {
    /// The tally of a whole list of results, `results[i]` being the result
    /// of `jobs[i]`.
    pub fn of(jobs: &[Job], results: &[FileResult]) -> Self {
        let mut tally = Tally::default();
        for (index, result) in results.iter().enumerate() {
            tally.record(jobs.get(index), result);
        }
        tally
    }

    /// Count one finished file. A file that grew saves 0. The first written
    /// file sets the output folder: the folder of its destination, so
    /// `<dir>/optimized` for a folder run and the file's own folder for a
    /// file run. A destination with no folder sets none.
    pub fn record(&mut self, job: Option<&Job>, result: &FileResult) {
        let FileResult::Optimized(outcome) = result else {
            return;
        };
        self.optimised += 1;
        self.saved_bytes += outcome.before.saturating_sub(outcome.after);
        if self.output.is_none()
            && let Some(Job::Optimize { dest, .. }) = job
        {
            self.output = dest
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty())
                .map(Path::to_path_buf);
        }
    }

    /// The folder "Open folder" opens after this run, if anything was written
    /// to a known folder.
    pub fn output_folder(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    /// The `run-finished` payload. `has_output` is exactly
    /// `output_folder().is_some()`.
    pub fn finished(&self) -> RunFinished {
        RunFinished {
            optimised: self.optimised,
            saved_bytes: self.saved_bytes,
            has_output: self.output.is_some(),
        }
    }
}

/// Run `jobs` on this thread with the real runner and `worker`, until `stop`
/// is used (the window closes); see [`run_session_with`].
pub fn run_session(
    ticket: RunTicket,
    jobs: &[Job],
    compression: Compression,
    worker: &Worker,
    stop: &StopHandle,
    last_output: &Mutex<Option<PathBuf>>,
    emit: &mut impl FnMut(Event),
) {
    run_session_with(
        ticket,
        jobs,
        compression,
        last_output,
        emit,
        |jobs, on_start, on_progress| {
            lunaepix_core::run_observed(jobs, worker, stop, on_start, on_progress);
        },
    );
}

/// `jobs` without the files the run limit left out, and how many those were.
fn without_over_limit(jobs: &[Job]) -> (Vec<Job>, usize) {
    let is_over = |job: &&Job| {
        matches!(
            job,
            Job::Skip {
                reason: Skip::OverRunLimit,
                ..
            }
        )
    };
    let left_out = jobs.iter().filter(is_over).count();
    let kept = jobs.iter().filter(|job| !is_over(job)).cloned().collect();
    (kept, left_out)
}

/// What a runner calls right before a file's worker starts, with its index.
pub type OnStart<'a> = &'a mut dyn FnMut(usize);

/// What a runner calls once a file is finished: its index, its source and
/// the result.
pub type OnProgress<'a> = &'a mut dyn FnMut(usize, &Path, &FileResult);

/// Run `jobs` through `run` and tell the window: `run-started`, then per
/// file a `file-started` (only when `run` starts it) and a `file-done`, then
/// exactly one `run-finished`. `run` is `run_observed` or a double.
///
/// The files the run limit left out (a folder of 30,000 photos leaves out
/// 28,000) are not handed to `run`: after the other files, one `file-done`
/// row stands for all of them ([`over_limit_row`]), and `run-started`
/// counts it as one.
///
/// Before `run-finished` goes out, the output folder is stored in
/// `last_output`, so the window may open it as soon as it hears the event.
/// The ticket is given back only after `run-finished` has gone out, so the
/// next run's `run-started` always comes after it. If `run` or `emit`
/// panics during the run, `run-finished` still goes out once, with what was
/// counted so far, and then the panic carries on.
pub fn run_session_with(
    ticket: RunTicket,
    jobs: &[Job],
    compression: Compression,
    last_output: &Mutex<Option<PathBuf>>,
    emit: &mut impl FnMut(Event),
    run: impl FnOnce(&[Job], OnStart<'_>, OnProgress<'_>),
) {
    let (jobs, left_out) = without_over_limit(jobs);
    let total = jobs.len() + usize::from(left_out > 0);
    // Shared by the runner's two callbacks, which are never nested.
    let emit = RefCell::new(emit);
    let send = |event: Event| (*emit.borrow_mut())(event);
    send(Event::RunStarted(RunStarted::new(total, compression)));
    let mut tally = Tally::default();
    // Caught so that the window always hears the end of a run it saw start.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        run(
            &jobs,
            &mut |index| send(Event::FileStarted(FileStarted { index })),
            &mut |index, src, result| {
                tally.record(jobs.get(index), result);
                send(Event::FileDone(file_done(index, src, result)));
            },
        );
        if left_out > 0 {
            send(Event::FileDone(over_limit_row(jobs.len(), left_out)));
        }
    }));
    *last_output.lock().unwrap_or_else(PoisonError::into_inner) =
        tally.output_folder().map(Path::to_path_buf);
    send(Event::RunFinished(tally.finished()));
    // Given back only now (or by unwinding, if that emit panics), so a next
    // run's `run-started` can never overtake this `run-finished`.
    drop(ticket);
    if let Err(payload) = outcome {
        panic::resume_unwind(payload);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io;

    use lunaepix_core::{
        MAX_RUN_FILES, OptimizeError, Outcome, RunError, Skip, StopHandle, plan_files, plan_folder,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::events::{FileDone, Mode, Status};

    fn optimized(before: u64, after: u64) -> FileResult {
        FileResult::Optimized(Outcome {
            before,
            after,
            millis: 1,
            note: None,
        })
    }

    fn written() -> FileResult {
        optimized(10, 5)
    }

    fn failed(path: &Path) -> FileResult {
        FileResult::Failed(RunError::Optimize(OptimizeError::Invalid {
            path: path.to_path_buf(),
            message: "not a PNG".to_owned(),
        }))
    }

    fn optimize(dest: &str) -> Job {
        Job::Optimize {
            src: PathBuf::from("/srv/in.png"),
            dest: PathBuf::from(dest),
        }
    }

    fn skip(src: &str) -> Job {
        Job::Skip {
            src: PathBuf::from(src),
            reason: Skip::UnsupportedType,
        }
    }

    /// A folder holding `a.png` (its bytes do not matter to the planner).
    fn folder_with_a_png() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        (dir, png)
    }

    fn output_folder(jobs: &[Job], results: &[FileResult]) -> Option<PathBuf> {
        Tally::of(jobs, results)
            .output_folder()
            .map(Path::to_path_buf)
    }

    fn count_finished(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, Event::RunFinished(_)))
            .count()
    }

    // --- the busy guard -----------------------------------------------------

    #[test]
    fn refuses_a_second_start_while_a_run_is_active() {
        let guard = RunGuard::new();
        let first = guard.try_start();
        assert!(first.is_some());
        assert!(guard.try_start().is_none());
        // A clone of the guard is the same guard.
        assert!(guard.clone().try_start().is_none());
    }

    #[test]
    fn accepts_a_new_start_once_the_run_is_over() {
        let guard = RunGuard::new();
        drop(guard.try_start().unwrap());
        let second = guard.try_start();
        assert!(second.is_some());
    }

    #[test]
    fn claiming_a_run_while_one_is_active_is_busy_and_tells_the_window_once() {
        let guard = RunGuard::new();
        let _active = guard.try_start().unwrap();
        let mut events = Vec::new();
        let claimed = claim_run(&guard, &mut |event| events.push(event));
        assert_eq!(claimed.err(), Some(CommandError::Busy));
        assert_eq!(events, [Event::RunBusy]);
    }

    #[test]
    fn claiming_a_run_when_none_is_active_gives_a_ticket_and_no_busy_event() {
        let guard = RunGuard::new();
        let mut events = Vec::new();
        let ticket = claim_run(&guard, &mut |event| events.push(event));
        assert!(ticket.is_ok());
        assert_eq!(events, []);
        // The ticket is real: the guard is now taken.
        assert!(guard.try_start().is_none());
    }

    #[test]
    fn command_errors_reach_the_window_as_bare_strings() {
        let errors = [
            CommandError::Busy,
            CommandError::NoOutput,
            CommandError::OpenFailed,
            CommandError::InvalidView,
            CommandError::NoImage,
            CommandError::NothingToMove,
            CommandError::NotReady,
            CommandError::InvalidName,
            CommandError::NameTaken,
            CommandError::SaveFailed,
        ];
        let json: Vec<String> = errors
            .iter()
            .map(|error| serde_json::to_string(error).unwrap())
            .collect();
        assert_eq!(
            json,
            [
                r#""busy""#,
                r#""no-output""#,
                r#""open-failed""#,
                r#""invalid-view""#,
                r#""no-image""#,
                r#""nothing-to-move""#,
                r#""not-ready""#,
                r#""invalid-name""#,
                r#""name-taken""#,
                r#""save-failed""#
            ]
        );
    }

    // --- open the last output -----------------------------------------------

    #[test]
    fn opening_with_no_last_output_is_no_output_and_never_calls_the_opener() {
        let last_output = Mutex::new(None);
        let called = Cell::new(false);
        let opened = open_last_output(&last_output, |_| {
            called.set(true);
            Ok::<(), io::Error>(())
        });
        assert_eq!(opened, Err(CommandError::NoOutput));
        assert!(!called.get());
    }

    #[test]
    fn opening_a_last_output_the_system_cannot_open_is_open_failed() {
        let last_output = Mutex::new(Some(PathBuf::from("/srv/photos/optimized")));
        let opened = open_last_output(&last_output, |_| {
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(opened, Err(CommandError::OpenFailed));
    }

    #[test]
    fn opening_a_last_output_opens_exactly_that_folder() {
        let folder = PathBuf::from("/srv/photos/optimized");
        let last_output = Mutex::new(Some(folder.clone()));
        let given = RefCell::new(Vec::new());
        let opened = open_last_output(&last_output, |path| {
            given.borrow_mut().push(path.to_path_buf());
            // The lock is not held while the opener runs.
            assert!(last_output.try_lock().is_ok());
            Ok::<(), io::Error>(())
        });
        assert_eq!(opened, Ok(()));
        assert_eq!(*given.borrow(), [folder]);
    }

    // --- the tally: summary and output folder ---------------------------------

    #[test]
    fn a_folder_run_opens_the_optimized_folder_inside_it() {
        let (dir, _) = folder_with_a_png();
        let jobs = plan_folder(dir.path());
        assert_eq!(
            output_folder(&jobs, &[written()]),
            Some(dir.path().join("optimized"))
        );
    }

    #[test]
    fn a_file_run_opens_the_folder_the_file_is_in() {
        let (dir, png) = folder_with_a_png();
        let jobs = plan_files(&[png]);
        assert_eq!(
            output_folder(&jobs, &[written()]),
            Some(dir.path().to_path_buf())
        );
    }

    #[test]
    fn a_run_with_no_written_file_has_no_output_folder() {
        let (dir, png) = folder_with_a_png();
        let jobs = plan_files(std::slice::from_ref(&png));
        assert_eq!(output_folder(&jobs, &[failed(&png)]), None);
        let skipped = plan_files(&[dir.path().join("missing.png")]);
        assert_eq!(
            output_folder(&skipped, &[FileResult::Skipped(Skip::NotAFile)]),
            None
        );
        assert_eq!(output_folder(&[], &[]), None);
    }

    #[test]
    fn the_first_written_file_sets_the_output_folder() {
        let jobs = [
            optimize("/one/optimized/a.png"),
            optimize("/two/optimized/b.png"),
        ];
        let results = [failed(Path::new("/one/a.png")), written()];
        assert_eq!(
            output_folder(&jobs, &results),
            Some(PathBuf::from("/two/optimized"))
        );
    }

    #[test]
    fn the_summary_counts_the_optimised_files_and_their_saving() {
        let jobs = [
            optimize("/x/optimized-a.png"),
            optimize("/x/optimized-b.png"),
            skip("/x/big.png"),
            optimize("/x/optimized-c.png"),
        ];
        let results = [
            optimized(100, 60),
            optimized(50, 50),
            FileResult::Skipped(Skip::TooLarge),
            FileResult::Failed(RunError::TimedOut {
                path: PathBuf::from("/x/c.png"),
                limit: std::time::Duration::from_secs(600),
            }),
        ];
        assert_eq!(
            Tally::of(&jobs, &results).finished(),
            RunFinished {
                optimised: 2,
                saved_bytes: 40,
                has_output: true,
            }
        );
    }

    #[test]
    fn a_file_that_grew_saves_nothing_rather_than_wrapping() {
        let jobs = [
            optimize("/x/optimized-a.png"),
            optimize("/x/optimized-b.png"),
        ];
        let results = [optimized(10, 12), optimized(100, 90)];
        assert_eq!(Tally::of(&jobs, &results).finished().saved_bytes, 10);
    }

    #[test]
    fn an_all_skipped_run_has_nothing_optimised_and_no_output() {
        let jobs = [skip("/x/notes.txt"), skip("/x/missing.png")];
        let results = [
            FileResult::Skipped(Skip::UnsupportedType),
            FileResult::Skipped(Skip::NotAFile),
        ];
        assert_eq!(
            Tally::of(&jobs, &results).finished(),
            RunFinished {
                optimised: 0,
                saved_bytes: 0,
                has_output: false,
            }
        );
    }

    #[test]
    fn an_optimised_file_with_no_known_folder_is_counted_but_has_no_output() {
        // A destination with no folder part (never planned for a real pick or
        // drop, whose paths are absolute) and a result that does not match an
        // optimise job: both are optimised files with no folder to open.
        let jobs = [optimize("optimized-a.png"), skip("/x/b.png")];
        let tally = Tally::of(&jobs, &[optimized(10, 4), optimized(10, 4)]);
        assert_eq!(tally.output_folder(), None);
        assert_eq!(
            tally.finished(),
            RunFinished {
                optimised: 2,
                saved_bytes: 12,
                has_output: false,
            }
        );
    }

    #[test]
    fn has_output_agrees_with_the_output_folder_on_every_fixture() {
        let (dir, png) = folder_with_a_png();
        let cases: Vec<(Vec<Job>, Vec<FileResult>)> = vec![
            (plan_folder(dir.path()), vec![written()]),
            (plan_files(std::slice::from_ref(&png)), vec![written()]),
            (plan_files(std::slice::from_ref(&png)), vec![failed(&png)]),
            (
                vec![skip("/x/a.txt")],
                vec![FileResult::Skipped(Skip::UnsupportedType)],
            ),
            (vec![optimize("optimized-a.png")], vec![written()]),
            (Vec::new(), Vec::new()),
        ];
        let mut seen = Vec::new();
        for (jobs, results) in &cases {
            let tally = Tally::of(jobs, results);
            assert_eq!(tally.finished().has_output, tally.output_folder().is_some());
            seen.push(tally.finished().has_output);
        }
        // Both answers occur, so the agreement is not trivially all-false.
        assert!(seen.contains(&true) && seen.contains(&false));
    }

    // --- a whole session ----------------------------------------------------

    #[test]
    fn a_session_emits_start_one_file_done_per_job_then_finish() {
        let guard = RunGuard::new();
        let ticket = guard.try_start().unwrap();
        let jobs = vec![skip("/srv/notes.txt"), optimize("/srv/optimized-a.png")];
        // A worker that cannot be started fails its file without running.
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let last_output = Mutex::new(Some(PathBuf::from("/from/an/earlier/run")));
        let mut events = Vec::new();
        let mut held_at_finish = None;
        run_session(
            ticket,
            &jobs,
            Compression::Lossless,
            &worker,
            &StopHandle::new(),
            &last_output,
            &mut |event| {
                if let Event::RunFinished(_) = event {
                    held_at_finish = Some(guard.try_start().is_none());
                }
                events.push(event);
            },
        );

        let names: Vec<&str> = events.iter().map(Event::name).collect();
        // The skipped file gets no `file-started`; the other one does.
        assert_eq!(
            names,
            [
                "run-started",
                "file-done",
                "file-started",
                "file-done",
                "run-finished"
            ]
        );
        assert_eq!(count_finished(&events), 1);
        assert_eq!(
            events[0],
            Event::RunStarted(RunStarted {
                total: 2,
                mode: Mode::Lossless,
                quality: None,
            })
        );
        let statuses: Vec<(usize, Status)> = events
            .iter()
            .filter_map(|event| match event {
                Event::FileDone(done) => Some((done.index, done.status)),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, [(0, Status::Skipped), (1, Status::Failed)]);
        assert_eq!(
            events.last().unwrap(),
            &Event::RunFinished(RunFinished {
                optimised: 0,
                saved_bytes: 0,
                has_output: false,
            })
        );
        // The ticket is still held while `run-finished` goes out, so no next
        // run can start before it, and it is given back right after.
        assert_eq!(held_at_finish, Some(true));
        assert!(guard.try_start().is_some());
        // This run wrote nothing, so the earlier run's folder is forgotten.
        assert_eq!(*last_output.lock().unwrap(), None);
    }

    #[test]
    fn a_start_attempted_while_run_finished_is_sent_is_refused_as_busy() {
        let guard = RunGuard::new();
        let jobs = vec![optimize("/srv/photos/optimized/a.png")];
        let last_output = Mutex::new(None);
        let mut claimed_at_finish = None;
        let mut busy_events = Vec::new();
        run_session_with(
            guard.try_start().unwrap(),
            &jobs,
            Compression::Lossless,
            &last_output,
            &mut |event| {
                if let Event::RunFinished(_) = event {
                    let claimed = claim_run(&guard, &mut |busy| busy_events.push(busy));
                    claimed_at_finish = Some(claimed.err());
                }
            },
            |_, _, progress| progress(0, Path::new("/srv/photos/a.png"), &written()),
        );
        assert_eq!(claimed_at_finish, Some(Some(CommandError::Busy)));
        assert_eq!(busy_events, [Event::RunBusy]);
        // Once `run-finished` is out, the next run is accepted.
        assert!(claim_run(&guard, &mut |_| {}).is_ok());
    }

    #[test]
    fn a_session_keeps_the_guard_busy_until_it_finishes() {
        let guard = RunGuard::new();
        let ticket = guard.try_start().unwrap();
        let jobs = vec![skip("/srv/notes.txt")];
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let last_output = Mutex::new(None);
        let mut busy_during_file = None;
        run_session(
            ticket,
            &jobs,
            Compression::Lossless,
            &worker,
            &StopHandle::new(),
            &last_output,
            &mut |event| {
                if let Event::FileDone(_) = event {
                    busy_during_file = Some(guard.try_start().is_none());
                }
            },
        );
        assert_eq!(busy_during_file, Some(true));
    }

    #[test]
    fn a_session_stores_the_output_folder_its_finish_event_announces() {
        let guard = RunGuard::new();
        let jobs = vec![optimize("/srv/photos/optimized/a.png")];
        let last_output = Mutex::new(None);
        let mut events = Vec::new();
        let mut stored_at_finish = None;
        run_session_with(
            guard.try_start().unwrap(),
            &jobs,
            Compression::Lossless,
            &last_output,
            &mut |event| {
                if let Event::RunFinished(_) = event {
                    stored_at_finish = last_output.lock().unwrap().clone();
                }
                events.push(event);
            },
            |_, _, progress| progress(0, Path::new("/srv/photos/a.png"), &written()),
        );
        assert_eq!(
            stored_at_finish,
            Some(PathBuf::from("/srv/photos/optimized"))
        );
        assert_eq!(
            events.last(),
            Some(&Event::RunFinished(RunFinished {
                optimised: 1,
                saved_bytes: 5,
                has_output: true,
            }))
        );
    }

    #[test]
    fn a_session_whose_run_panics_still_finishes_once_and_frees_the_guard() {
        let guard = RunGuard::new();
        let jobs = vec![
            optimize("/srv/photos/optimized/a.png"),
            optimize("/srv/photos/optimized/b.png"),
        ];
        let last_output = Mutex::new(None);
        let mut events = Vec::new();
        let mut held_at_finish = None;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            run_session_with(
                guard.try_start().unwrap(),
                &jobs,
                Compression::Lossless,
                &last_output,
                &mut |event| {
                    if let Event::RunFinished(_) = event {
                        held_at_finish = Some(guard.try_start().is_none());
                    }
                    events.push(event);
                },
                |_, _, progress| {
                    progress(0, Path::new("/srv/photos/a.png"), &written());
                    panic!("the runner failed half-way");
                },
            );
        }));
        // On this path too, the ticket is held until `run-finished` is out.
        assert_eq!(held_at_finish, Some(true));
        // The panic is not swallowed.
        assert!(outcome.is_err());
        assert_eq!(count_finished(&events), 1);
        // The summary is what was counted before the panic.
        assert_eq!(
            events.last(),
            Some(&Event::RunFinished(RunFinished {
                optimised: 1,
                saved_bytes: 5,
                has_output: true,
            }))
        );
        assert_eq!(
            *last_output.lock().unwrap(),
            Some(PathBuf::from("/srv/photos/optimized"))
        );
        assert!(guard.try_start().is_some());
    }

    #[test]
    fn a_session_whose_window_sink_panics_on_a_file_still_finishes_once() {
        let guard = RunGuard::new();
        let jobs = vec![skip("/srv/notes.txt")];
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let last_output = Mutex::new(None);
        let mut events = Vec::new();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            run_session(
                guard.try_start().unwrap(),
                &jobs,
                Compression::Lossless,
                &worker,
                &StopHandle::new(),
                &last_output,
                &mut |event| {
                    let is_file = matches!(event, Event::FileDone(_));
                    events.push(event);
                    if is_file {
                        panic!("the window sink failed");
                    }
                },
            );
        }));
        assert!(outcome.is_err());
        assert_eq!(count_finished(&events), 1);
        assert_eq!(events.last().map(Event::name), Some("run-finished"));
        assert!(guard.try_start().is_some());
    }

    // --- the run limit ------------------------------------------------------

    /// A folder holding `pngs` PNG files named `img0000.png` and on, plus
    /// `extra` files; the planner does not read their bytes.
    fn folder_with_pngs(pngs: usize, extra: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for index in 0..pngs {
            fs::write(dir.path().join(format!("img{index:04}.png")), b"png").unwrap();
        }
        for name in extra {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        dir
    }

    /// The events of a session over `jobs`, and the jobs its runner was
    /// given. The runner is a double that reports every job as written.
    fn session_over(jobs: &[Job]) -> (Vec<Event>, Vec<Job>) {
        let guard = RunGuard::new();
        let last_output = Mutex::new(None);
        let mut events = Vec::new();
        let given = RefCell::new(Vec::new());
        run_session_with(
            guard.try_start().unwrap(),
            jobs,
            Compression::Lossless,
            &last_output,
            &mut |event| events.push(event),
            fake_runner(&given),
        );
        (events, given.into_inner())
    }

    /// A runner double, as `run_observed` behaves with a worker that writes
    /// every file: each optimise job is started (`on_start`) and then
    /// reported written; each skip is reported skipped, never started. It
    /// records the jobs it was given in `given`.
    fn fake_runner(
        given: &RefCell<Vec<Job>>,
    ) -> impl FnOnce(&[Job], OnStart<'_>, OnProgress<'_>) + '_ {
        move |jobs, on_start, progress| {
            given.borrow_mut().extend_from_slice(jobs);
            for (index, job) in jobs.iter().enumerate() {
                let (src, result) = match job {
                    Job::Optimize { src, .. } => {
                        on_start(index);
                        (src, written())
                    }
                    Job::Skip { src, reason } => (src, FileResult::Skipped(*reason)),
                };
                progress(index, src, &result);
            }
        }
    }

    fn rows(events: &[Event]) -> Vec<&FileDone> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::FileDone(done) => Some(done),
                _ => None,
            })
            .collect()
    }

    fn limit_rows<'a>(rows: &[&'a FileDone]) -> Vec<&'a FileDone> {
        rows.iter()
            .copied()
            .filter(|row| {
                row.reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("over the 2000-file limit"))
            })
            .collect()
    }

    #[test]
    fn files_left_out_by_the_run_limit_collapse_into_one_row_at_the_end() {
        let dir = folder_with_pngs(MAX_RUN_FILES + 3, &[]);
        let (events, given) = session_over(&plan_folder(dir.path()));

        assert_eq!(events[0], lossless_start(MAX_RUN_FILES + 1));
        let rows = rows(&events);
        assert_eq!(rows.len(), MAX_RUN_FILES + 1);
        let last = rows.last().unwrap();
        assert_eq!(last.index, MAX_RUN_FILES);
        assert_eq!(last.status, Status::Skipped);
        assert_eq!(last.name, "(over the limit)");
        let named = rows.iter().filter(|row| row.name == "(over the limit)");
        assert_eq!(
            named.count(),
            1,
            "only the limit row carries the placeholder"
        );
        assert_eq!(
            last.reason.as_deref(),
            Some("3 more files not optimised: over the 2000-file limit")
        );
        assert_eq!(limit_rows(&rows).len(), 1);
        // The runner never sees the files left out.
        assert_eq!(given.len(), MAX_RUN_FILES);
        assert!(given.iter().all(|job| matches!(job, Job::Optimize { .. })));
        assert_eq!(count_finished(&events), 1);
    }

    #[test]
    fn one_file_left_out_by_the_run_limit_is_counted_in_the_singular() {
        let dir = folder_with_pngs(MAX_RUN_FILES + 1, &[]);
        let (events, _) = session_over(&plan_folder(dir.path()));

        let rows = rows(&events);
        assert_eq!(
            rows.last().unwrap().reason.as_deref(),
            Some("1 more file not optimised: over the 2000-file limit")
        );
    }

    #[test]
    fn a_run_of_exactly_the_file_limit_has_no_row_for_files_left_out() {
        let dir = folder_with_pngs(MAX_RUN_FILES, &[]);
        let (events, given) = session_over(&plan_folder(dir.path()));

        assert_eq!(events[0], lossless_start(MAX_RUN_FILES));
        let rows = rows(&events);
        assert_eq!(rows.len(), MAX_RUN_FILES);
        assert!(limit_rows(&rows).is_empty());
        assert!(rows.iter().all(|row| row.status == Status::Done));
        assert_eq!(given.len(), MAX_RUN_FILES);
    }

    #[test]
    fn files_skipped_for_another_reason_keep_a_row_each_beside_the_limit_row() {
        let dir = folder_with_pngs(MAX_RUN_FILES + 2, &["a.gif", "notes.txt"]);
        let (events, _) = session_over(&plan_folder(dir.path()));

        let rows = rows(&events);
        // 2000 optimised, two unsupported files, one row for the two left out.
        assert_eq!(rows.len(), MAX_RUN_FILES + 3);
        let unsupported: Vec<&str> = rows
            .iter()
            .filter(|row| row.reason.as_deref() == Some("Not a PNG, JPG, SVG or WebP file"))
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(unsupported, ["a.gif", "notes.txt"]);
        let limit = limit_rows(&rows);
        assert_eq!(limit.len(), 1);
        assert_eq!(
            limit[0].reason.as_deref(),
            Some("2 more files not optimised: over the 2000-file limit")
        );
        // Every row has its own index, in order, and the total matches.
        let indexes: Vec<usize> = rows.iter().map(|row| row.index).collect();
        assert_eq!(indexes, (0..rows.len()).collect::<Vec<_>>());
        assert_eq!(events[0], lossless_start(rows.len()));
    }

    /// The `run-started` event of a lossless run of `total` rows.
    fn lossless_start(total: usize) -> Event {
        Event::RunStarted(RunStarted {
            total,
            mode: Mode::Lossless,
            quality: None,
        })
    }

    // --- whether a process is gone ----------------------------------------

    /// Whether process `pid` is gone from the process table: killed and reaped,
    /// not even a zombie left. `kill(pid, 0)` sends nothing and only asks whether
    /// the pid exists, which a zombie still does. It works on every Unix; macOS
    /// has no `/proc` to look in.
    #[cfg(unix)]
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
    fn sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap()
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

    // --- the window closes during a run -------------------------------------

    #[cfg(unix)]
    #[test]
    fn stopping_a_session_as_the_window_closes_kills_its_worker_and_finishes_once() {
        use std::time::{Duration, Instant};

        let dir = TempDir::new().unwrap();
        for name in ["a.png", "b.png"] {
            fs::write(dir.path().join(name), b"png").unwrap();
        }
        let jobs = plan_folder(dir.path());
        let pids = dir.path().join("pids");
        // A worker that hangs; every one started appends its PID first.
        let worker = Worker {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                format!("echo $$ >> '{}'; exec sleep 60", pids.display()).into(),
                "fake-worker".into(),
            ],
            timeout: Duration::from_secs(600),
        };
        let guard = RunGuard::new();
        let stop = StopHandle::new();
        let last_output = Mutex::new(None);
        let events = Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                run_session(
                    guard.try_start().unwrap(),
                    &jobs,
                    Compression::Lossless,
                    &worker,
                    &stop,
                    &last_output,
                    &mut |event| events.lock().unwrap().push(event),
                );
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !pids.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            // What the close handler does.
            stop.stop();
            running.join().unwrap();
        });

        let started = fs::read_to_string(&pids).unwrap();
        assert_eq!(started.lines().count(), 1, "b.png must not start");
        let pid = started.trim();
        assert!(
            process_is_gone(pid.parse().unwrap()),
            "worker {pid} runs on"
        );
        let events = events.into_inner().unwrap();
        assert_eq!(count_finished(&events), 1);
        let rows = rows(&events);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason.as_deref(), Some("the run was stopped"));
        assert!(guard.try_start().is_some());
    }

    // --- the session: list, settings, output folder ---------------------------

    use crate::events::{ListChanged, OutputChanged};
    use lunaepix_core::Quality;
    use serde_json::json;

    /// What the window hears.
    #[derive(Default)]
    struct Window(RefCell<Vec<Event>>);

    impl Window {
        fn emit(&self) -> impl FnMut(Event) + '_ {
            |event| self.0.borrow_mut().push(event)
        }

        fn take(&self) -> Vec<Event> {
            self.0.take()
        }
    }

    fn value(event: &Event) -> Value {
        serde_json::to_value(event).unwrap()
    }

    fn error_json(error: CommandError) -> Value {
        serde_json::to_value(error).unwrap()
    }

    fn lossy(quality: u8) -> Value {
        json!({ "mode": "lossy", "quality": quality })
    }

    fn lossless() -> Value {
        json!({ "mode": "lossless", "quality": 75 })
    }

    /// A new folder holding these files.
    fn files(contents: &[(&str, &[u8])]) -> (TempDir, Vec<PathBuf>) {
        let dir = TempDir::new().unwrap();
        let paths = contents
            .iter()
            .map(|(name, bytes)| {
                let path = dir.path().join(name);
                fs::write(&path, bytes).unwrap();
                path
            })
            .collect();
        (dir, paths)
    }

    /// `choose_files`, answered with `paths`.
    fn pick(session: &Session, window: &Window, paths: &[PathBuf]) {
        session
            .choose_files(&mut window.emit(), |ticket| {
                session.add_paths(ticket, Some(paths.to_vec()), &mut window.emit());
            })
            .unwrap();
    }

    /// A drop of `paths`, added at once instead of on another thread.
    fn drop_on(session: &Session, window: &Window, paths: &[PathBuf]) {
        session.drop_paths(paths.to_vec(), &mut window.emit(), |ticket, paths| {
            session.add_paths(ticket, Some(paths), &mut window.emit());
        });
    }

    /// `choose_output_folder`, answered with `folder`.
    fn choose_output(session: &Session, window: &Window, folder: &Path) {
        session
            .choose_output_folder(&mut window.emit(), |ticket| {
                session.set_output(ticket, Some(folder.to_path_buf()), &mut window.emit());
            })
            .unwrap();
    }

    /// `start_run` with `settings`: its answer, and the run handed to the
    /// start closure (only on success).
    fn start(
        session: &Session,
        window: &Window,
        settings: &Value,
    ) -> (Result<(), CommandError>, Option<PreparedRun>) {
        let mut prepared = None;
        let answer = session.start_run(Some(settings), &mut window.emit(), |run| {
            prepared = Some(run)
        });
        (answer, prepared)
    }

    /// `start_run` that must start: the prepared run.
    fn started(session: &Session, window: &Window, settings: &Value) -> PreparedRun {
        let (answer, prepared) = start(session, window, settings);
        assert_eq!(answer, Ok(()));
        prepared.expect("the start closure got the run")
    }

    /// `start_run` that must be refused with `error`, without starting.
    fn refused(session: &Session, window: &Window, settings: &Value, error: CommandError) {
        let (answer, prepared) = start(session, window, settings);
        assert_eq!(answer, Err(error));
        assert!(prepared.is_none(), "a refused run must not start");
        assert!(session.guard.try_start().is_some(), "the guard is free");
    }

    /// Run `prepared` with [`fake_runner`]: the jobs it was given.
    fn run_fake(session: &Session, window: &Window, prepared: PreparedRun) -> Vec<Job> {
        let given = RefCell::new(Vec::new());
        session.run(prepared, &mut window.emit(), fake_runner(&given));
        given.into_inner()
    }

    /// The last `list-changed` among `events`.
    fn last_list(events: &[Event]) -> &ListChanged {
        events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::ListChanged(list) => Some(list),
                _ => None,
            })
            .expect("a list-changed event")
    }

    fn row_names(list: &ListChanged) -> Vec<&str> {
        list.files.iter().map(|row| row.name.as_str()).collect()
    }

    fn sources(jobs: &[Job]) -> Vec<PathBuf> {
        jobs.iter()
            .map(|job| match job {
                Job::Optimize { src, .. } | Job::Skip { src, .. } => src.clone(),
            })
            .collect()
    }

    /// `(name, index)` of every `file-started` and `file-done`, in order.
    fn file_events(events: &[Event]) -> Vec<(&'static str, usize)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::FileStarted(started) => Some(("file-started", started.index)),
                Event::FileDone(done) => Some(("file-done", done.index)),
                _ => None,
            })
            .collect()
    }

    // Adding.

    #[test]
    fn adding_three_files_sends_three_rows() {
        let (_dir, paths) = files(&[
            ("holiday.jpg", b"jpgjpg"),
            ("logo.png", b"png"),
            ("icon.svg", b"<svg/>"),
        ]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let events = window.take();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].name(), "list-changed");
        assert_eq!(
            value(&events[0]),
            json!({ "files": [
                { "name": "holiday.jpg", "size": 6, "skip": null, "thumb": "1-0" },
                { "name": "logo.png", "size": 3, "skip": null, "thumb": "1-1" },
                { "name": "icon.svg", "size": 6, "skip": null, "thumb": "1-2" }
            ], "notice": null })
        );
        // The dialog's ticket is given back once the list is sent.
        assert!(session.guard.try_start().is_some());
    }

    #[test]
    fn the_same_file_twice_is_listed_once() {
        let (dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[paths[0].clone(), paths[0].clone()]);
        // Again, through another spelling of the same file, beside a new one.
        let alias = dir.path().join(".").join("a.png");
        pick(&session, &window, &[alias, paths[1].clone()]);
        pick(&session, &window, &[paths[0].clone()]);
        assert_eq!(row_names(last_list(&window.take())), ["a.png", "b.png"]);
        let prepared = started(&session, &window, &lossless());
        assert_eq!(sources(prepared.jobs()).len(), 2);
    }

    #[test]
    fn a_dropped_folder_adds_its_images_including_optimized_ones_but_not_subfolders() {
        let (dir, _) = files(&[
            ("b.png", b"png"),
            ("a.jpg", b"jpg"),
            ("optimized-a.jpg", b"jpg"),
            ("d.webp", b"webp"),
        ]);
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("c.png"), b"png").unwrap();
        fs::create_dir(dir.path().join("optimized")).unwrap();
        fs::write(dir.path().join("optimized").join("e.png"), b"png").unwrap();
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        let events = window.take();
        let list = last_list(&events);
        // Sorted by name, as the folder's images come.
        assert_eq!(
            row_names(list),
            ["a.jpg", "b.png", "d.webp", "optimized-a.jpg"]
        );
        assert!(list.files.iter().all(|row| row.skip.is_none()));
        assert_eq!(list.notice, None);
    }

    #[test]
    fn notes_txt_in_the_folder_is_not_listed() {
        let (dir, _) = files(&[("a.png", b"png"), ("notes.txt", b"x"), ("README", b"x")]);
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        assert_eq!(row_names(last_list(&window.take())), ["a.png"]);
    }

    #[test]
    fn an_empty_folder_sends_the_notice_and_adds_nothing() {
        let (empty, _) = files(&[("notes.txt", b"x")]);
        fs::create_dir(empty.path().join("sub")).unwrap();
        fs::write(empty.path().join("sub").join("c.png"), b"png").unwrap();
        let session = Session::new();
        let window = Window::default();
        // Onto an empty list: the spec's example.
        drop_on(&session, &window, &[empty.path().to_path_buf()]);
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({
                "files": [],
                "notice": "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)"
            })]
        );
        // Onto a list that has a file: the file stays, with the notice.
        let (_dir, paths) = files(&[("a.png", b"png")]);
        pick(&session, &window, &paths);
        window.take();
        drop_on(&session, &window, &[empty.path().to_path_buf()]);
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(row_names(list), ["a.png"]);
        assert_eq!(list.notice.as_deref(), Some(EMPTY_FOLDER_NOTICE));
    }

    #[test]
    fn an_empty_folder_dropped_with_an_image_sends_no_notice() {
        let empty = TempDir::new().unwrap();
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        drop_on(
            &session,
            &window,
            &[empty.path().to_path_buf(), paths[0].clone()],
        );
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(row_names(list), ["a.png"]);
        assert_eq!(list.notice, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_is_listed_as_not_a_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("a.png"), b"png").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            // Running as root: every folder can be read, so there is no
            // unreadable folder to drop.
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: the folder stays readable (running as root)");
            return;
        }
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, std::slice::from_ref(&locked));
        let events = window.take();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            value(&events[0]),
            json!({ "files": [
                { "name": "locked", "size": null, "skip": "Not a readable file", "thumb": null }
            ], "notice": null })
        );
    }

    #[test]
    fn a_loose_non_image_file_is_listed_as_a_skipped_row() {
        let (dir, paths) = files(&[("notes.txt", b"hello")]);
        let missing = dir.path().join("missing.png");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[paths[0].clone(), missing]);
        assert_eq!(
            value(&window.take()[0]),
            json!({ "files": [
                { "name": "notes.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file", "thumb": null },
                { "name": "missing.png", "size": null, "skip": "Not a readable file", "thumb": null }
            ], "notice": null })
        );
    }

    #[test]
    fn a_row_whose_size_cannot_be_read_has_a_null_size() {
        let jobs = [Job::Optimize {
            src: PathBuf::from("/srv/a.png"),
            dest: PathBuf::from("/srv/optimized-a.png"),
        }];
        let asked = RefCell::new(Vec::new());
        let rows = list_rows(
            &jobs,
            |path| {
                asked.borrow_mut().push(path.to_path_buf());
                None
            },
            |_| vec![None],
        );
        assert_eq!(
            rows,
            [ListRow {
                name: "a.png".to_owned(),
                size: None,
                skip: None,
                thumb: None,
            }]
        );
        assert_eq!(*asked.borrow(), [PathBuf::from("/srv/a.png")]);
        assert_eq!(
            list_rows(&jobs, |_| Some(7), |_| vec![None])[0].size,
            Some(7)
        );
    }

    #[test]
    fn list_rows_asks_for_keys_with_each_image_rows_path_and_puts_them_on_the_rows() {
        let jobs = [
            Job::Optimize {
                src: PathBuf::from("/srv/a.png"),
                dest: PathBuf::from("/srv/optimized-a.png"),
            },
            Job::Skip {
                src: PathBuf::from("/srv/notes.txt"),
                reason: Skip::UnsupportedType,
            },
            Job::Optimize {
                src: PathBuf::from("/srv/c.svg"),
                dest: PathBuf::from("/srv/optimized-c.svg"),
            },
        ];
        let asked = RefCell::new(Vec::new());
        let rows = list_rows(
            &jobs,
            |_| Some(1),
            |images| {
                asked.borrow_mut().push(images.to_vec());
                vec![Some("7-0".to_owned()), None, Some("7-2".to_owned())]
            },
        );
        assert_eq!(
            *asked.borrow(),
            [vec![
                Some(PathBuf::from("/srv/a.png")),
                None,
                Some(PathBuf::from("/srv/c.svg"))
            ]]
        );
        let thumbs: Vec<Option<&str>> = rows.iter().map(|row| row.thumb.as_deref()).collect();
        assert_eq!(thumbs, [Some("7-0"), None, Some("7-2")]);
    }

    // Settings.

    #[test]
    fn parse_settings_accepts_lossy_0_and_100_and_lossless_75() {
        let quality = |q| Compression::Lossy(Quality::new(q).unwrap());
        assert_eq!(parse_settings(&lossy(0)), Ok(quality(0)));
        assert_eq!(parse_settings(&lossy(100)), Ok(quality(100)));
        assert_eq!(parse_settings(&lossy(75)), Ok(quality(75)));
        assert_eq!(parse_settings(&lossless()), Ok(Compression::Lossless));
        for q in [0, 100] {
            let settings = json!({ "mode": "lossless", "quality": q });
            assert_eq!(parse_settings(&settings), Ok(Compression::Lossless));
        }
        // As the window sends it, keys in either order.
        let text: Value = serde_json::from_str(r#"{ "quality": 40, "mode": "lossy" }"#).unwrap();
        assert_eq!(parse_settings(&text), Ok(quality(40)));
    }

    #[test]
    fn parse_settings_rejects_minus_1_101_75_5_string_75_null_missing_key_extra_key_and_mode_fast()
    {
        let from_text = |text: &str| -> Value { serde_json::from_str(text).unwrap() };
        let bad = [
            json!({ "mode": "lossy", "quality": -1 }),
            json!({ "mode": "lossy", "quality": 101 }),
            json!({ "mode": "lossy", "quality": 256 }),
            json!({ "mode": "lossy", "quality": u64::MAX }),
            json!({ "mode": "lossy", "quality": 75.5 }),
            from_text(r#"{ "mode": "lossy", "quality": 75.0 }"#),
            from_text(r#"{ "mode": "lossy", "quality": 1e2 }"#),
            json!({ "mode": "lossy", "quality": "75" }),
            json!({ "mode": "lossy", "quality": null }),
            json!({ "mode": "lossy", "quality": true }),
            json!({ "mode": "lossy", "quality": [75] }),
            json!({ "mode": "lossy" }),
            json!({ "quality": 75 }),
            json!({ "mode": "lossy", "quality": 75, "out": "/tmp" }),
            json!({ "mode": "lossy", "quality": 75, "other": null }),
            json!({ "quality": 75, "other": "lossy" }),
            json!({ "mode": "fast", "quality": 75 }),
            json!({ "mode": "Lossy", "quality": 75 }),
            json!({ "mode": "LOSSLESS", "quality": 75 }),
            json!({ "mode": " lossy", "quality": 75 }),
            json!({ "mode": 1, "quality": 75 }),
            json!({ "mode": null, "quality": 75 }),
            json!({ "mode": "lossless", "quality": 101 }),
            json!({ "mode": "lossless", "quality": 75.5 }),
            json!({ "mode": "lossless", "quality": null }),
            json!({ "mode": "lossless" }),
            json!({}),
            json!(null),
            json!(75),
            json!("lossy"),
            json!(["lossy", 75]),
        ];
        for settings in &bad {
            assert_eq!(
                parse_settings(settings),
                Err(CommandError::InvalidSettings),
                "{settings}"
            );
        }
    }

    // start_run.

    #[test]
    fn a_list_with_one_compressible_file_starts() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        window.take();
        for settings in [lossy(0), lossy(100), lossless()] {
            let prepared = started(&session, &window, &settings);
            assert_eq!(
                prepared.jobs(),
                [Job::Optimize {
                    src: paths[0].clone(),
                    dest: dir.path().join("optimized-a.png"),
                }]
            );
            // The run holds the guard from `start_run` until it has run.
            assert!(session.guard.try_start().is_none());
            drop(prepared);
            assert!(session.guard.try_start().is_some());
        }
        // Starting sends nothing by itself; the run sends `run-started`.
        assert_eq!(window.take(), []);
    }

    #[test]
    fn an_empty_list_or_only_skipped_rows_is_empty_list() {
        let session = Session::new();
        let window = Window::default();
        refused(&session, &window, &lossless(), CommandError::EmptyList);
        let (dir, paths) = files(&[("notes.txt", b"x")]);
        pick(
            &session,
            &window,
            &[paths[0].clone(), dir.path().join("gone.png")],
        );
        refused(&session, &window, &lossy(75), CommandError::EmptyList);
        assert_eq!(error_json(CommandError::EmptyList), json!("empty-list"));
    }

    #[test]
    fn start_run_with_invalid_settings_is_refused_and_starts_nothing() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        for settings in [
            json!({ "mode": "lossy", "quality": 75.5 }),
            json!({ "mode": "lossy", "quality": "75" }),
            json!({ "mode": "lossy", "quality": 75, "extra": 1 }),
            json!(null),
        ] {
            refused(&session, &window, &settings, CommandError::InvalidSettings);
        }
        assert_eq!(
            error_json(CommandError::InvalidSettings),
            json!("invalid-settings")
        );
    }

    #[test]
    fn the_order_of_checks_is_busy_settings_output_then_empty_list() {
        let session = Session::new();
        let window = Window::default();
        let dir = TempDir::new().unwrap();
        let gone = dir.path().join("gone");
        fs::create_dir(&gone).unwrap();
        choose_output(&session, &window, &gone);
        fs::remove_dir(&gone).unwrap();
        // Busy beats everything else (bad settings, missing output, empty list).
        let mut held = None;
        session
            .choose_files(&mut window.emit(), |ticket| held = Some(ticket))
            .unwrap();
        let (answer, prepared) = start(&session, &window, &json!(null));
        assert_eq!(answer, Err(CommandError::Busy));
        assert!(prepared.is_none());
        drop(held);
        // Then the settings, before the output folder and the list.
        refused(
            &session,
            &window,
            &json!(null),
            CommandError::InvalidSettings,
        );
        // Then the output folder, before the list.
        refused(&session, &window, &lossless(), CommandError::OutputMissing);
        // Then the list.
        fs::create_dir(&gone).unwrap();
        refused(&session, &window, &lossless(), CommandError::EmptyList);
    }

    #[test]
    fn an_existing_writable_output_folder_starts() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        let prepared = started(&session, &window, &lossy(75));
        assert_eq!(
            prepared.jobs(),
            [Job::Optimize {
                src: paths[0].clone(),
                dest: out.join("a.png"),
            }]
        );
        // The write check leaves nothing behind in the folder.
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    }

    #[test]
    fn a_deleted_output_folder_is_output_missing() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        fs::remove_dir(&out).unwrap();
        refused(&session, &window, &lossy(75), CommandError::OutputMissing);
        // The folder is not made again behind the user's back.
        assert!(!out.exists());
        assert_eq!(
            error_json(CommandError::OutputMissing),
            json!("output-missing")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_output_folder_is_output_missing() {
        // Unix permissions only; Windows has no read-only folders in this
        // sense, so this case is not checked there.
        use std::os::unix::fs::PermissionsExt;
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        fs::set_permissions(&out, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(out.join("probe"), b"").is_ok() {
            // Running as root: the folder stays writable.
            fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: the folder stays writable (running as root)");
            return;
        }
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        refused(&session, &window, &lossy(75), CommandError::OutputMissing);
        fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    }

    #[test]
    fn an_output_folder_replaced_by_a_file_is_output_missing() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        fs::remove_dir(&out).unwrap();
        fs::write(&out, b"a file now").unwrap();
        refused(&session, &window, &lossy(75), CommandError::OutputMissing);
        assert_eq!(fs::read(&out).unwrap(), b"a file now");
    }

    // Busy.

    #[test]
    fn after_run_finished_every_command_works_again() {
        let (dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        let prepared = started(&session, &window, &lossless());
        run_fake(&session, &window, prepared);
        window.take();

        drop(started(&session, &window, &lossy(50)));
        let opened = Cell::new(false);
        session
            .choose_files(&mut window.emit(), |ticket| {
                opened.set(true);
                drop(ticket);
            })
            .unwrap();
        assert!(opened.take());
        choose_output(&session, &window, &out);
        assert_eq!(session.clear_output_folder(&mut window.emit()), Ok(()));
        drop_on(&session, &window, &paths[1..]);
        assert_eq!(row_names(last_list(&window.take())), ["b.png"]);
        assert_eq!(session.clear_list(&mut window.emit()), Ok(()));
        let shown = RefCell::new(Vec::new());
        let answer = session.open_output_folder(|folder| {
            shown.borrow_mut().push(folder.to_path_buf());
            Ok::<(), io::Error>(())
        });
        assert_eq!(answer, Ok(()));
        assert_eq!(*shown.borrow(), [dir.path().to_path_buf()]);
        assert!(!window.take().contains(&Event::RunBusy));
    }

    #[test]
    fn every_command_but_open_output_folder_is_busy_during_a_run() {
        let (dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        // A first run, so that "Open folder" has a folder.
        let prepared = started(&session, &window, &lossless());
        run_fake(&session, &window, prepared);

        let busy = Window::default();
        let answers = RefCell::new(Vec::new());
        let called = Cell::new(false);
        let shown = RefCell::new(Vec::new());
        let prepared = started(&session, &window, &lossless());
        session.run(prepared, &mut window.emit(), |_, _, _| {
            let mut answers = answers.borrow_mut();
            answers.push(session.choose_files(&mut busy.emit(), |_| called.set(true)));
            answers.push(session.clear_list(&mut busy.emit()));
            answers.push(session.choose_output_folder(&mut busy.emit(), |_| called.set(true)));
            answers.push(session.clear_output_folder(&mut busy.emit()));
            answers
                .push(session.start_run(Some(&lossless()), &mut busy.emit(), |_| called.set(true)));
            session.drop_paths(paths[1..].to_vec(), &mut busy.emit(), |_, _| {
                called.set(true);
            });
            let opened = session.open_output_folder(|folder| {
                shown.borrow_mut().push(folder.to_path_buf());
                Ok::<(), io::Error>(())
            });
            assert_eq!(opened, Ok(()));
        });
        assert_eq!(*answers.borrow(), [Err(CommandError::Busy); 5]);
        assert!(!called.get(), "no dialog, run or add went ahead");
        // Each refusal, the drop's too, told the window.
        assert_eq!(busy.take(), vec![Event::RunBusy; 6]);
        assert_eq!(*shown.borrow(), [dir.path().to_path_buf()]);
        // The refused clear changed nothing.
        let prepared = started(&session, &window, &lossless());
        assert_eq!(sources(prepared.jobs()), [paths[0].clone()]);
        assert_eq!(error_json(CommandError::Busy), json!("busy"));
    }

    #[test]
    fn an_empty_drop_does_nothing() {
        let session = Session::new();
        let window = Window::default();
        let called = Cell::new(false);
        session.drop_paths(Vec::new(), &mut window.emit(), |_, _| called.set(true));
        assert!(!called.get());
        assert_eq!(window.take(), []);
        assert!(session.guard.try_start().is_some());
    }

    // Dialogs and the output setting.

    #[test]
    fn a_cancelled_file_dialog_sends_run_cancelled_frees_the_guard_and_keeps_the_list() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        window.take();
        for answer in [None, Some(Vec::new())] {
            let free_at_cancel = Cell::new(None);
            session
                .choose_files(&mut window.emit(), |ticket| {
                    session.add_paths(ticket, answer, &mut |event| {
                        free_at_cancel.set(Some(session.guard.try_start().is_some()));
                        window.emit()(event);
                    });
                })
                .unwrap();
            assert_eq!(window.take(), [Event::RunCancelled]);
            // The ticket was given back before `run-cancelled` went out.
            assert_eq!(free_at_cancel.get(), Some(true));
        }
        let prepared = started(&session, &window, &lossless());
        assert_eq!(sources(prepared.jobs()), paths);
    }

    #[test]
    fn a_cancelled_output_dialog_keeps_the_output_folder_and_sends_run_cancelled() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        window.take();
        session
            .choose_output_folder(&mut window.emit(), |ticket| {
                session.set_output(ticket, None, &mut window.emit());
            })
            .unwrap();
        assert_eq!(window.take(), [Event::RunCancelled]);
        assert!(session.guard.try_start().is_some());
        let prepared = started(&session, &window, &lossless());
        assert_eq!(
            prepared.jobs(),
            [Job::Optimize {
                src: paths[0].clone(),
                dest: out.join("a.png"),
            }]
        );
    }

    #[test]
    fn output_changed_sends_the_folder_name_only() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("my exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        choose_output(&session, &window, &out);
        assert!(session.guard.try_start().is_some());
        assert_eq!(session.clear_output_folder(&mut window.emit()), Ok(()));
        let events = window.take();
        assert_eq!(
            events,
            [
                Event::OutputChanged(OutputChanged {
                    folder: Some("my exports".to_owned())
                }),
                Event::OutputChanged(OutputChanged { folder: None }),
            ]
        );
        assert_eq!(value(&events[0]), json!({ "folder": "my exports" }));
        assert_eq!(value(&events[1]), json!({ "folder": null }));
        assert!(session.guard.try_start().is_some());
    }

    #[test]
    fn a_root_output_folder_is_named_by_an_empty_string_never_a_path() {
        let session = Session::new();
        let window = Window::default();
        choose_output(&session, &window, Path::new(std::path::MAIN_SEPARATOR_STR));
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({ "folder": "" })]
        );
    }

    /// Every string in `value`, at any depth.
    fn strings_in(value: &Value) -> Vec<&str> {
        match value {
            Value::String(text) => vec![text.as_str()],
            Value::Array(items) => items.iter().flat_map(strings_in).collect(),
            Value::Object(map) => map.values().flat_map(strings_in).collect(),
            _ => Vec::new(),
        }
    }

    /// The events whose payload holds `needle` in one of its strings,
    /// compared as parsed JSON (so a Windows path's escaped backslashes are
    /// found too).
    fn payloads_holding(events: &[Event], needle: &str) -> Vec<String> {
        events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .filter(|text| {
                let parsed: Value = serde_json::from_str(text).unwrap();
                strings_in(&parsed).iter().any(|s| s.contains(needle))
            })
            .collect()
    }

    #[test]
    fn no_event_payload_contains_the_temp_dir_path() {
        let (dir, paths) = files(&[("a.png", b"png"), ("notes.txt", b"x")]);
        let folder = dir.path().join("dropped");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("b.jpg"), b"jpg").unwrap();
        let empty = dir.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        // A worker that cannot start fails each file with its reason.
        let worker = Worker::new(dir.path().join("no-such-worker"));
        let session = Session::new();
        let window = Window::default();

        pick(
            &session,
            &window,
            &[
                paths[0].clone(),
                paths[1].clone(),
                dir.path().join("gone.png"),
            ],
        );
        drop_on(&session, &window, std::slice::from_ref(&folder));
        drop_on(&session, &window, std::slice::from_ref(&empty));
        choose_output(&session, &window, &out);
        let prepared = started(&session, &window, &lossy(60));
        session.run_with_worker(prepared, &worker, &StopHandle::new(), &mut window.emit());
        assert_eq!(session.clear_output_folder(&mut window.emit()), Ok(()));
        let prepared = started(&session, &window, &lossless());
        session.run_with_worker(prepared, &worker, &StopHandle::new(), &mut window.emit());
        pick(&session, &window, &[paths[0].clone()]);
        session
            .choose_files(&mut window.emit(), |ticket| {
                session.add_paths(ticket, None, &mut window.emit());
            })
            .unwrap();
        assert_eq!(session.clear_list(&mut window.emit()), Ok(()));
        let events = window.take();

        // Every kind of event the window can get in a session is here.
        let mut kinds: Vec<&str> = events.iter().map(Event::name).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(
            kinds,
            [
                "file-done",
                "file-started",
                "list-changed",
                "output-changed",
                "run-cancelled",
                "run-finished",
                "run-started"
            ]
        );
        let tmp = dir.path().to_string_lossy().into_owned();
        let tmp_name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(payloads_holding(&events, &tmp), Vec::<String>::new());
        assert_eq!(payloads_holding(&events, &tmp_name), Vec::<String>::new());

        // The same search finds a path when a payload holds one.
        let fixture = [
            Event::OutputChanged(OutputChanged {
                folder: Some(out.display().to_string()),
            }),
            Event::FileDone(FileDone {
                index: 0,
                name: "a.png".to_owned(),
                status: Status::Failed,
                before: None,
                after: None,
                millis: None,
                reason: Some(format!("could not write {}", out.join("a.png").display())),
                note: None,
            }),
        ];
        assert_eq!(payloads_holding(&fixture, &tmp).len(), 2);
        assert_eq!(payloads_holding(&fixture, &tmp_name).len(), 2);
    }

    // The run.

    #[test]
    fn file_started_precedes_file_done_per_file() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        window.take();
        let prepared = started(&session, &window, &lossless());
        session.run_with_worker(prepared, &worker, &StopHandle::new(), &mut window.emit());
        assert_eq!(
            file_events(&window.take()),
            [
                ("file-started", 0),
                ("file-done", 0),
                ("file-started", 1),
                ("file-done", 1)
            ]
        );
    }

    #[test]
    fn no_file_started_for_a_skipped_row() {
        let (dir, paths) = files(&[("notes.txt", b"x"), ("a.png", b"png")]);
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let session = Session::new();
        let window = Window::default();
        pick(
            &session,
            &window,
            &[
                paths[0].clone(),
                dir.path().join("gone.png"),
                paths[1].clone(),
            ],
        );
        window.take();
        let prepared = started(&session, &window, &lossless());
        session.run_with_worker(prepared, &worker, &StopHandle::new(), &mut window.emit());
        let events = window.take();
        assert_eq!(
            file_events(&events),
            [
                ("file-done", 0),
                ("file-done", 1),
                ("file-started", 2),
                ("file-done", 2)
            ]
        );
        assert_eq!(events.first().map(Event::name), Some("run-started"));
        assert_eq!(events.last().map(Event::name), Some("run-finished"));
    }

    #[test]
    fn the_first_add_after_a_finished_run_lists_only_the_new_files() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        let prepared = started(&session, &window, &lossless());
        run_fake(&session, &window, prepared);
        window.take();
        pick(&session, &window, &paths[1..2]);
        assert_eq!(row_names(last_list(&window.take())), ["b.png"]);
        // Only the first add starts afresh; the next one adds to it.
        pick(&session, &window, &paths[2..]);
        assert_eq!(row_names(last_list(&window.take())), ["b.png", "c.png"]);
    }

    #[test]
    fn the_old_rows_are_not_in_it() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        let prepared = started(&session, &window, &lossless());
        run_fake(&session, &window, prepared);
        pick(&session, &window, &paths[1..]);
        let prepared = started(&session, &window, &lossless());
        assert_eq!(sources(prepared.jobs()), [paths[1].clone()]);
        let given = run_fake(&session, &window, prepared);
        assert_eq!(sources(&given), [paths[1].clone()]);
    }

    #[test]
    fn a_deleted_file_shows_as_skipped_and_the_rest_runs() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        fs::remove_file(&paths[0]).unwrap();
        window.take();
        let prepared = started(&session, &window, &lossless());
        let given = run_fake(&session, &window, prepared);
        assert_eq!(
            given,
            [
                Job::Skip {
                    src: paths[0].clone(),
                    reason: Skip::NotAFile,
                },
                Job::Optimize {
                    src: paths[1].clone(),
                    dest: paths[1].with_file_name("optimized-b.png"),
                },
            ]
        );
        let events = window.take();
        let done: Vec<Value> = events
            .iter()
            .filter(|event| event.name() == "file-done")
            .map(value)
            .collect();
        assert_eq!(
            done,
            [
                json!({ "index": 0, "name": "a.png", "status": "skipped", "reason": "Not a readable file" }),
                json!({ "index": 1, "name": "b.png", "status": "done", "before": 10, "after": 5, "millis": 1 }),
            ]
        );
        assert_eq!(
            file_events(&events),
            [("file-done", 0), ("file-started", 1), ("file-done", 1)]
        );
    }

    #[test]
    fn a_list_of_2001_files_gives_2000_rows_and_one_over_limit_row() {
        let dir = folder_with_pngs(MAX_RUN_FILES + 1, &[]);
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(list.files.len(), MAX_RUN_FILES + 1);
        assert!(
            list.files[..MAX_RUN_FILES]
                .iter()
                .all(|row| row.skip.is_none() && row.size == Some(3))
        );
        assert_eq!(
            list.files.last(),
            Some(&ListRow {
                name: "(over the limit)".to_owned(),
                size: None,
                skip: Some("1 more file not optimised: over the 2000-file limit".to_owned()),
                thumb: None,
            })
        );
        let placeholders = list
            .files
            .iter()
            .filter(|row| row.name == "(over the limit)")
            .count();
        assert_eq!(placeholders, 1);
    }

    #[test]
    fn a_list_of_2000_files_has_no_over_limit_row() {
        let dir = folder_with_pngs(MAX_RUN_FILES, &[]);
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(list.files.len(), MAX_RUN_FILES);
        assert!(list.files.iter().all(|row| row.skip.is_none()));
        assert!(list.files.iter().all(|row| row.name != "(over the limit)"));
    }

    #[test]
    fn run_started_from_start_run_carries_the_chosen_settings() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        for (settings, expected) in [
            (
                lossy(40),
                json!({ "total": 1, "mode": "lossy", "quality": 40 }),
            ),
            (
                lossless(),
                json!({ "total": 1, "mode": "lossless", "quality": null }),
            ),
            (
                lossy(0),
                json!({ "total": 1, "mode": "lossy", "quality": 0 }),
            ),
        ] {
            window.take();
            let prepared = started(&session, &window, &settings);
            run_fake(&session, &window, prepared);
            let events = window.take();
            assert_eq!(events[0].name(), "run-started");
            assert_eq!(value(&events[0]), expected);
        }
    }

    /// A worker that writes the arguments it got, one per line, to `log`,
    /// then fails.
    #[cfg(unix)]
    fn recording_worker(log: &Path) -> Worker {
        Worker {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                format!("printf '%s\\n' \"$@\" > '{}'; exit 3", log.display()).into(),
                "fake-worker".into(),
            ],
            timeout: std::time::Duration::from_secs(60),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_worker_gets_lossy_and_no_replace_with_an_output_folder() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let logs = TempDir::new().unwrap();
        let log = logs.path().join("args");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        let prepared = started(&session, &window, &lossy(40));
        session.run_with_worker(
            prepared,
            &recording_worker(&log),
            &StopHandle::new(),
            &mut window.emit(),
        );
        let args = fs::read_to_string(&log).unwrap();
        let expected = format!(
            "--lossy\n40\n--no-replace\n{}\n{}\n",
            paths[0].display(),
            out.join("a.png").display()
        );
        assert_eq!(args, expected);
    }

    #[cfg(unix)]
    #[test]
    fn the_worker_gets_no_replace_for_lossless_next_to_the_originals() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let logs = TempDir::new().unwrap();
        let log = logs.path().join("args");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let prepared = started(&session, &window, &lossless());
        session.run_with_worker(
            prepared,
            &recording_worker(&log),
            &StopHandle::new(),
            &mut window.emit(),
        );
        let args = fs::read_to_string(&log).unwrap();
        let expected = format!(
            "--no-replace\n{}\n{}\n",
            paths[0].display(),
            dir.path().join("optimized-a.png").display()
        );
        assert_eq!(args, expected);
    }

    #[test]
    fn open_folder_opens_the_chosen_output_folder_or_else_the_first_files_folder() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let opened = |session: &Session| {
            let shown = RefCell::new(Vec::new());
            let answer = session.open_output_folder(|folder| {
                shown.borrow_mut().push(folder.to_path_buf());
                Ok::<(), io::Error>(())
            });
            assert_eq!(answer, Ok(()));
            shown.into_inner()
        };
        let window = Window::default();

        let into_out = Session::new();
        pick(&into_out, &window, &paths);
        choose_output(&into_out, &window, &out);
        let prepared = started(&into_out, &window, &lossy(75));
        run_fake(&into_out, &window, prepared);
        assert_eq!(opened(&into_out), std::slice::from_ref(&out));

        let next_to = Session::new();
        pick(&next_to, &window, &paths);
        let prepared = started(&next_to, &window, &lossy(75));
        run_fake(&next_to, &window, prepared);
        assert_eq!(opened(&next_to), [dir.path().to_path_buf()]);
    }

    #[test]
    fn clear_list_sends_an_empty_list_and_the_next_start_is_empty_list() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        window.take();
        assert_eq!(session.clear_list(&mut window.emit()), Ok(()));
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({ "files": [], "notice": null })]
        );
        assert!(session.guard.try_start().is_some());
        refused(&session, &window, &lossless(), CommandError::EmptyList);
    }

    // --- Re-add: `readd_list` -------------------------------------------------

    /// The file names of the list as it is planned now, next to the originals.
    fn listed_names(list: &FileList) -> Vec<String> {
        sources(&list.rows(None))
            .iter()
            .map(|path| file_name_of(path))
            .collect()
    }

    /// `readd_list` that must be accepted.
    fn readd(session: &Session, window: &Window) {
        assert_eq!(session.readd_list(&mut window.emit()), Ok(()));
    }

    /// A lossless run over the whole list with the fake runner: the jobs it got.
    fn run_the_list(session: &Session, window: &Window) -> Vec<Job> {
        let prepared = started(session, window, &lossless());
        run_fake(session, window, prepared)
    }

    #[test]
    fn readd_clears_the_finished_mark_so_the_next_add_appends() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let mut list = FileList::new();
        list.add(&paths[..1]);
        list.mark_finished();
        list.readd();
        list.add(&paths[1..]);
        assert_eq!(listed_names(&list), ["a.png", "b.png"]);
    }

    #[test]
    fn without_readd_a_finished_list_starts_afresh_on_the_next_add() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let mut list = FileList::new();
        list.add(&paths[..1]);
        list.mark_finished();
        list.add(&paths[1..]);
        assert_eq!(listed_names(&list), ["b.png"]);
    }

    #[test]
    fn a_readd_keeps_the_seen_set_so_a_file_is_listed_once() {
        let (dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
        let mut list = FileList::new();
        list.add(&paths[..2]);
        list.mark_finished();
        list.readd();
        // `a.png` again, through another spelling, beside a new file.
        let alias = dir.path().join(".").join("a.png");
        list.add(&[alias, paths[2].clone()]);
        assert_eq!(listed_names(&list), ["a.png", "b.png", "c.png"]);
    }

    #[test]
    fn a_cleared_list_forgets_the_seen_set() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let mut list = FileList::new();
        list.add(&paths);
        list.clear();
        assert_eq!(listed_names(&list), Vec::<String>::new());
        list.add(&paths);
        assert_eq!(listed_names(&list), ["a.png"]);
    }

    #[test]
    fn readd_after_a_run_brings_back_the_same_rows_in_order() {
        let (_dir, paths) = files(&[("b.png", b"png"), ("a.png", b"png"), ("c.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        run_the_list(&session, &window);
        window.take();
        readd(&session, &window);
        let events = window.take();
        // The list in the order it was added, and nothing else is sent.
        assert_eq!(events.len(), 1);
        assert_eq!(row_names(last_list(&events)), ["b.png", "a.png", "c.png"]);
        assert_eq!(last_list(&events).notice, None);
    }

    #[test]
    fn without_readd_the_first_add_after_a_run_starts_a_new_list() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..2]);
        run_the_list(&session, &window);
        window.take();
        pick(&session, &window, &paths[2..]);
        assert_eq!(row_names(last_list(&window.take())), ["c.png"]);
    }

    #[test]
    fn adds_after_a_readd_append() {
        let (_dir, paths) = files(&[
            ("a.png", b"png"),
            ("b.png", b"png"),
            ("c.png", b"png"),
            ("d.png", b"png"),
        ]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..2]);
        run_the_list(&session, &window);
        readd(&session, &window);
        window.take();
        pick(&session, &window, &paths[2..3]);
        assert_eq!(
            row_names(last_list(&window.take())),
            ["a.png", "b.png", "c.png"]
        );
        // And the next one appends as well.
        pick(&session, &window, &paths[3..]);
        assert_eq!(
            row_names(last_list(&window.take())),
            ["a.png", "b.png", "c.png", "d.png"]
        );
    }

    /// The file a fake run "wrote" for the first job, an optimise job.
    fn write_the_output(jobs: &[Job]) -> PathBuf {
        let Some(Job::Optimize { dest, .. }) = jobs.first() else {
            panic!("the run has an optimise job first");
        };
        fs::write(dest, b"optimised").unwrap();
        dest.clone()
    }

    #[test]
    fn readd_lists_the_added_paths_not_the_outputs() {
        let (dir, _paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        let jobs = run_the_list(&session, &window);
        // The run wrote its output next to the original.
        let output = write_the_output(&jobs);
        assert_eq!(output, dir.path().join("optimized-a.png"));
        window.take();
        readd(&session, &window);
        assert_eq!(row_names(last_list(&window.take())), ["a.png"]);
    }

    #[test]
    fn dropping_the_folder_again_lists_the_output_too() {
        let (dir, _paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        let jobs = run_the_list(&session, &window);
        write_the_output(&jobs);
        window.take();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        assert_eq!(
            row_names(last_list(&window.take())),
            ["a.png", "optimized-a.png"]
        );
    }

    #[test]
    fn readd_plans_the_rows_again() {
        let (_dir, paths) = files(&[("a.png", b"aaaa"), ("b.png", b"bb"), ("c.png", b"ccccccc")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        run_the_list(&session, &window);
        fs::remove_file(&paths[1]).unwrap();
        window.take();
        readd(&session, &window);
        let events = window.take();
        assert_eq!(
            events.iter().map(value).collect::<Vec<_>>(),
            [json!({ "files": [
                { "name": "a.png", "size": 4, "skip": null, "thumb": "1-0" },
                { "name": "b.png", "size": null, "skip": "Not a readable file", "thumb": null },
                { "name": "c.png", "size": 7, "skip": null, "thumb": "1-2" }
            ], "notice": null })]
        );
    }

    #[test]
    fn readd_while_busy_is_refused_and_tells_the_window() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        run_the_list(&session, &window);
        window.take();

        let ticket = session.guard.try_start().expect("the guard is free");
        assert_eq!(
            session.readd_list(&mut window.emit()),
            Err(CommandError::Busy)
        );
        assert_eq!(window.take(), vec![Event::RunBusy]);
        drop(ticket);

        // The refusal changed nothing: the list is still finished, so an add
        // starts a fresh list.
        pick(&session, &window, &paths[1..]);
        assert_eq!(row_names(last_list(&window.take())), ["b.png"]);
    }

    #[test]
    fn readd_when_idle_resolves_and_sends_the_list() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        run_the_list(&session, &window);
        window.take();
        assert_eq!(session.readd_list(&mut window.emit()), Ok(()));
        let events = window.take();
        assert!(!events.contains(&Event::RunBusy));
        assert_eq!(
            events.iter().map(Event::name).collect::<Vec<_>>(),
            ["list-changed"]
        );
        assert!(session.guard.try_start().is_some(), "the guard is free");
    }

    #[test]
    fn readd_of_an_empty_list_sends_an_empty_list() {
        let session = Session::new();
        let window = Window::default();
        readd(&session, &window);
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({ "files": [], "notice": null })]
        );
    }

    #[test]
    fn readd_of_an_all_skipped_list_brings_it_back() {
        let (_dir, paths) = files(&[("a.txt", b"x"), ("b.txt", b"x")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        // Nothing to compress, so no run can start and finish on this list.
        refused(&session, &window, &lossless(), CommandError::EmptyList);
        window.take();
        readd(&session, &window);
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({ "files": [
                { "name": "a.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file", "thumb": null },
                { "name": "b.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file", "thumb": null }
            ], "notice": null })]
        );
    }

    #[test]
    fn readd_after_every_file_was_deleted_brings_back_skipped_rows() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        run_the_list(&session, &window);
        for path in &paths {
            fs::remove_file(path).unwrap();
        }
        window.take();
        readd(&session, &window);
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({ "files": [
                { "name": "a.png", "size": null, "skip": "Not a readable file", "thumb": null },
                { "name": "b.png", "size": null, "skip": "Not a readable file", "thumb": null }
            ], "notice": null })]
        );
    }

    #[test]
    fn readd_lists_an_entry_that_became_a_folder_as_one_skipped_row() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let later = dir.path().join("later");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[later.clone(), paths[0].clone()]);
        // A path that does not exist is one "Not a readable file" row.
        let events = window.take();
        assert_eq!(row_names(last_list(&events)), ["later", "a.png"]);
        assert_eq!(
            last_list(&events).files[0].skip.as_deref(),
            Some("Not a readable file")
        );
        run_the_list(&session, &window);
        fs::create_dir(&later).unwrap();
        fs::write(later.join("inside.png"), b"png").unwrap();
        window.take();
        readd(&session, &window);
        let events = window.take();
        let list = last_list(&events);
        // Still one row: nothing inside the folder is listed.
        assert_eq!(row_names(list), ["later", "a.png"]);
        assert_eq!(list.files[0].skip.as_deref(), Some("Not a readable file"));
        assert_eq!(list.files[1].skip, None);
    }

    #[cfg(unix)]
    #[test]
    fn readd_lists_an_entry_that_became_the_same_file_once() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let link = dir.path().join("link.png");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[paths[0].clone(), link.clone()]);
        // Before the link exists, two rows.
        assert_eq!(row_names(last_list(&window.take())), ["a.png", "link.png"]);
        run_the_list(&session, &window);
        std::os::unix::fs::symlink(&paths[0], &link).unwrap();
        window.take();
        readd(&session, &window);
        assert_eq!(row_names(last_list(&window.take())), ["a.png"]);
    }

    #[cfg(unix)]
    #[test]
    fn readd_lists_a_file_whose_output_name_links_back_to_it_as_skipped() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        run_the_list(&session, &window);
        // Without the link the re-added row is compressible.
        readd(&session, &window);
        assert_eq!(last_list(&window.take()).files[0].skip, None);
        std::os::unix::fs::symlink(&paths[0], dir.path().join("optimized-a.png")).unwrap();
        readd(&session, &window);
        assert_eq!(
            last_list(&window.take()).files[0].skip.as_deref(),
            Some("Its output would overwrite the file itself")
        );
    }

    #[cfg(unix)]
    #[test]
    fn readd_with_an_output_folder_plans_into_it_so_a_link_beside_the_original_does_not_skip_it() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        choose_output(&session, &window, &out);
        pick(&session, &window, &paths);
        run_the_list(&session, &window);
        // Next to the original, the output name `optimized-a.png` would be the
        // file itself (a link back to it). Into the output folder it is not.
        std::os::unix::fs::symlink(&paths[0], dir.path().join("optimized-a.png")).unwrap();
        window.take();
        readd(&session, &window);
        let events = window.take();
        assert_eq!(last_list(&events).files[0].skip, None);
        assert_eq!(last_list(&events).files[0].size, Some(3));

        // The same files and the same link with no output folder: skipped.
        assert_eq!(session.clear_output_folder(&mut window.emit()), Ok(()));
        window.take();
        readd(&session, &window);
        assert_eq!(
            last_list(&window.take()).files[0].skip.as_deref(),
            Some("Its output would overwrite the file itself")
        );
    }

    #[test]
    fn readd_lists_a_too_large_file_as_skipped() {
        let (dir, paths) = files(&[("small.png", b"png")]);
        let big = dir.path().join("big.png");
        fs::File::create(&big)
            .unwrap()
            .set_len(lunaepix_core::MAX_FILE_BYTES + 1)
            .unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[big, paths[0].clone()]);
        run_the_list(&session, &window);
        window.take();
        readd(&session, &window);
        assert_eq!(
            window.take().iter().map(value).collect::<Vec<_>>(),
            [json!({ "files": [
                { "name": "big.png", "size": null, "skip": "File is too large", "thumb": null },
                { "name": "small.png", "size": 3, "skip": null, "thumb": "1-1" }
            ], "notice": null })]
        );
    }

    #[test]
    fn readd_after_a_run_over_the_limit_keeps_the_limit_row() {
        let dir = folder_with_pngs(MAX_RUN_FILES + 1, &[]);
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        run_the_list(&session, &window);
        window.take();
        readd(&session, &window);
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(list.files.len(), MAX_RUN_FILES + 1);
        assert_eq!(
            list.files.last(),
            Some(&ListRow {
                name: "(over the limit)".to_owned(),
                size: None,
                skip: Some("1 more file not optimised: over the 2000-file limit".to_owned()),
                thumb: None,
            })
        );

        // A list of exactly the limit has no such row.
        let exact = folder_with_pngs(MAX_RUN_FILES, &[]);
        let session = Session::new();
        drop_on(&session, &window, &[exact.path().to_path_buf()]);
        run_the_list(&session, &window);
        window.take();
        readd(&session, &window);
        let events = window.take();
        assert_eq!(last_list(&events).files.len(), MAX_RUN_FILES);
        assert!(
            last_list(&events)
                .files
                .iter()
                .all(|row| row.skip.is_none())
        );
    }

    // --- fix round 1 ----------------------------------------------------------

    fn skip_row(src: &Path) -> Job {
        Job::Skip {
            src: src.to_path_buf(),
            reason: Skip::NotAFile,
        }
    }

    #[test]
    fn a_missing_entry_that_becomes_a_folder_stays_one_skipped_row() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let later = dir.path().join("later");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[later.clone(), paths[0].clone()]);
        fs::create_dir(&later).unwrap();
        fs::write(later.join("b.png"), b"png").unwrap();
        fs::write(later.join("notes.txt"), b"x").unwrap();
        let prepared = started(&session, &window, &lossless());
        assert_eq!(
            prepared.jobs(),
            [
                skip_row(&later),
                Job::Optimize {
                    src: paths[0].clone(),
                    dest: dir.path().join("optimized-a.png"),
                },
            ]
        );
        drop(prepared);
        // The list shows the same row on its next change.
        let (_more, more) = files(&[("c.png", b"png")]);
        window.take();
        pick(&session, &window, &more);
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(row_names(list), ["later", "a.png", "c.png"]);
        assert_eq!(list.files[0].skip.as_deref(), Some("Not a readable file"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_that_becomes_readable_stays_one_skipped_row() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, paths) = files(&[("b.png", b"png")]);
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("a.png"), b"png").unwrap();
        fs::write(locked.join("notes.txt"), b"x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: the folder stays readable (running as root)");
            return;
        }
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[locked.clone(), paths[0].clone()]);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let prepared = started(&session, &window, &lossless());
        assert_eq!(
            prepared.jobs(),
            [
                skip_row(&locked),
                Job::Optimize {
                    src: paths[0].clone(),
                    dest: dir.path().join("optimized-b.png"),
                },
            ]
        );
    }

    #[test]
    fn a_folder_entry_plans_nothing_inside_it() {
        let (dir, paths) = files(&[("x.png", b"png")]);
        let later = dir.path().join("later");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[later.clone(), paths[0].clone()]);
        fs::create_dir(&later).unwrap();
        fs::write(later.join("a.png"), b"png").unwrap();
        fs::write(later.join("notes.txt"), b"x").unwrap();
        for settings in [lossless(), lossy(75)] {
            let prepared = started(&session, &window, &settings);
            let jobs = prepared.jobs().to_vec();
            assert_eq!(jobs.len(), 2, "{jobs:?}");
            for job in &jobs {
                match job {
                    Job::Optimize { src, dest } => {
                        assert!(!src.starts_with(&later), "{job:?}");
                        assert!(!dest.starts_with(later.join("optimized")), "{job:?}");
                    }
                    Job::Skip { src, .. } => assert_eq!(src, &later),
                }
            }
            run_fake(&session, &window, prepared);
        }
        let events = window.take();
        let names: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::FileDone(done) => Some(done.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(!names.contains(&"notes.txt"), "{names:?}");
        assert!(!names.contains(&"a.png"), "{names:?}");
        assert!(!later.join("optimized").exists());
    }

    #[test]
    fn start_run_without_settings_is_invalid_settings() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let called = Cell::new(false);
        let answer = session.start_run(None, &mut window.emit(), |_| called.set(true));
        assert_eq!(answer, Err(CommandError::InvalidSettings));
        assert_eq!(
            serde_json::to_value(answer.unwrap_err()).unwrap(),
            json!("invalid-settings")
        );
        assert!(!called.get());
        assert!(session.guard.try_start().is_some());
        // A well-formed value still starts.
        drop(started(&session, &window, &lossless()));
    }

    /// What `work` recorded: the value it got, and whether it ran on the
    /// calling thread.
    type Ran = Arc<Mutex<Vec<(u32, bool)>>>;

    fn recording_work(ran: &Ran) -> impl FnOnce(u32) + Clone + Send + 'static {
        let ran = Arc::clone(ran);
        let caller = std::thread::current().id();
        move |value| {
            let here = std::thread::current().id() == caller;
            ran.lock().unwrap().push((value, here));
        }
    }

    #[test]
    fn work_runs_once_on_the_started_thread() {
        let ran: Ran = Arc::default();
        on_own_thread_or_here(7, recording_work(&ran), |job| {
            std::thread::spawn(job).join().unwrap();
            Ok(())
        });
        assert_eq!(*ran.lock().unwrap(), [(7, false)]);
    }

    #[test]
    fn work_runs_here_when_no_thread_can_start() {
        let ran: Ran = Arc::default();
        on_own_thread_or_here(7, recording_work(&ran), |job| {
            drop(job);
            Err(std::io::Error::other("no threads left"))
        });
        assert_eq!(*ran.lock().unwrap(), [(7, true)]);
        // A run's ticket is not lost either: it is freed once the work is done.
        let guard = RunGuard::new();
        let freed = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&freed);
        let check = guard.clone();
        on_own_thread_or_here(
            guard.try_start().unwrap(),
            move |ticket: RunTicket| {
                *seen.lock().unwrap() = Some(check.try_start().is_none());
                drop(ticket);
            },
            |_| Err(std::io::Error::other("no threads left")),
        );
        assert_eq!(*freed.lock().unwrap(), Some(true));
        assert!(guard.try_start().is_some());
    }

    #[test]
    fn has_work_is_true_with_one_compressible_file() {
        let (dir, paths) = files(&[("notes.txt", b"x"), ("a.png", b"png")]);
        let mut list = FileList::new();
        list.add(&paths);
        assert!(list.has_work(None));
        assert!(list.has_work(Some(&dir.path().join("exports"))));
    }

    #[test]
    fn has_work_is_false_for_an_empty_or_all_skipped_list() {
        let (dir, paths) = files(&[("notes.txt", b"x")]);
        let mut list = FileList::new();
        assert!(!list.has_work(None));
        list.add(&[paths[0].clone(), dir.path().join("gone.png")]);
        assert!(!list.has_work(None));
        assert!(!list.has_work(Some(dir.path())));
    }

    #[test]
    fn the_app_never_replaces_in_an_output_folder() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        choose_output(&session, &window, &out);
        for (settings, compression) in [
            (lossy(30), Compression::Lossy(Quality::new(30).unwrap())),
            (lossless(), Compression::Lossless),
        ] {
            let prepared = started(&session, &window, &settings);
            assert_eq!(
                prepared.options(),
                Options {
                    compression,
                    write: WriteMode::NoReplace,
                }
            );
        }
    }

    #[test]
    fn the_app_never_replaces_next_to_the_originals() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let no_replace = |compression| Options {
            compression,
            write: WriteMode::NoReplace,
        };
        let prepared = started(&session, &window, &lossless());
        assert_eq!(prepared.options(), no_replace(Compression::Lossless));
        drop(prepared);
        let prepared = started(&session, &window, &lossy(30));
        assert_eq!(
            prepared.options(),
            no_replace(Compression::Lossy(Quality::new(30).unwrap()))
        );
        drop(prepared);
        // Chosen and cleared again: still no replacing.
        choose_output(&session, &window, &out);
        assert_eq!(session.clear_output_folder(&mut window.emit()), Ok(()));
        let prepared = started(&session, &window, &lossy(30));
        assert_eq!(
            prepared.options(),
            no_replace(Compression::Lossy(Quality::new(30).unwrap()))
        );
    }

    #[test]
    fn a_missing_path_added_twice_is_listed_once() {
        let dir = TempDir::new().unwrap();
        let gone = dir.path().join("gone.png");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[gone.clone(), gone.clone()]);
        pick(&session, &window, std::slice::from_ref(&gone));
        let events = window.take();
        let list = last_list(&events);
        assert_eq!(row_names(list), ["gone.png"]);
        assert_eq!(list.files[0].skip.as_deref(), Some("Not a readable file"));
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_that_became_the_same_file_as_another_is_planned_once() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let link = dir.path().join("link.png");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[paths[0].clone(), link.clone()]);
        std::os::unix::fs::symlink(&paths[0], &link).unwrap();
        let prepared = started(&session, &window, &lossless());
        assert_eq!(sources(prepared.jobs()), [paths[0].clone()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_in_the_list_is_not_a_readable_file() {
        let dir = TempDir::new().unwrap();
        let socket = dir.path().join("s.png");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, std::slice::from_ref(&socket));
        assert_eq!(
            value(&window.take()[0]),
            json!({ "files": [
                { "name": "s.png", "size": null, "skip": "Not a readable file", "thumb": null }
            ], "notice": null })
        );
    }

    #[test]
    fn a_file_over_the_size_limit_is_a_skipped_row() {
        let dir = TempDir::new().unwrap();
        let big = dir.path().join("big.png");
        fs::File::create(&big)
            .unwrap()
            .set_len(lunaepix_core::MAX_FILE_BYTES + 1)
            .unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, std::slice::from_ref(&big));
        assert_eq!(
            value(&window.take()[0]),
            json!({ "files": [
                { "name": "big.png", "size": null, "skip": "File is too large", "thumb": null }
            ], "notice": null })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_whose_output_name_links_back_to_it_is_a_skipped_row() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        std::os::unix::fs::symlink(&paths[0], dir.path().join("optimized-a.png")).unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        assert_eq!(
            value(&window.take()[0]),
            json!({ "files": [
                { "name": "a.png", "size": null, "skip": "Its output would overwrite the file itself", "thumb": null }
            ], "notice": null })
        );
    }

    #[test]
    fn a_taken_name_in_the_output_folder_is_planned_with_a_number() {
        let (dir, first) = files(&[("a.png", b"png")]);
        let (_other, second) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("a.png"), b"keep").unwrap();
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &[first[0].clone(), second[0].clone()]);
        choose_output(&session, &window, &out);
        let prepared = started(&session, &window, &lossy(75));
        assert_eq!(
            prepared.jobs(),
            [
                Job::Optimize {
                    src: first[0].clone(),
                    dest: out.join("a-1.png"),
                },
                Job::Optimize {
                    src: second[0].clone(),
                    dest: out.join("a-2.png"),
                },
            ]
        );
    }

    #[test]
    fn a_list_planned_against_a_deleted_output_folder_still_lists_its_files() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let out = dir.path().join("exports");
        fs::create_dir(&out).unwrap();
        let session = Session::new();
        let window = Window::default();
        choose_output(&session, &window, &out);
        fs::remove_dir(&out).unwrap();
        window.take();
        pick(&session, &window, &paths);
        assert_eq!(
            value(&window.take()[0]),
            json!({ "files": [
                { "name": "a.png", "size": 3, "skip": null, "thumb": "1-0" }
            ], "notice": null })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_a_dropped_folder_is_not_listed() {
        let (dir, _) = files(&[("a.png", b"png")]);
        let (_elsewhere, outside) = files(&[("b.png", b"png")]);
        std::os::unix::fs::symlink(&outside[0], dir.path().join("link.png")).unwrap();
        let session = Session::new();
        let window = Window::default();
        drop_on(&session, &window, &[dir.path().to_path_buf()]);
        assert_eq!(row_names(last_list(&window.take())), ["a.png"]);
    }

    // --- thumbnail keys -------------------------------------------------------

    use lunaepix_core::ThumbAnswer;

    /// The `thumb` of each row of `list`.
    fn thumbs_of(list: &ListChanged) -> Vec<Option<String>> {
        list.files.iter().map(|row| row.thumb.clone()).collect()
    }

    /// The `thumb`s of the last `list-changed` the window heard.
    fn last_thumbs(window: &Window) -> Vec<Option<String>> {
        thumbs_of(last_list(&window.take()))
    }

    /// The list number in `key`, read by the test itself.
    fn list_number(key: &Option<String>) -> u64 {
        let key = key.as_deref().expect("a key");
        key.split('-').next().unwrap().parse().unwrap()
    }

    fn key(list: u64, index: usize) -> Option<String> {
        Some(format!("{list}-{index}"))
    }

    /// Ask the session's thumbnails for `key` with a `make` double that
    /// records the paths it is given: the status answered.
    fn ask(session: &Session, key: &Option<String>, calls: &RefCell<Vec<PathBuf>>) -> u16 {
        let uri = format!("/{}", key.as_deref().expect("a key"));
        let make = |path: &Path| {
            calls.borrow_mut().push(path.to_path_buf());
            ThumbAnswer::Png(b"\x89PNG\r\n\x1a\n".to_vec())
        };
        session.thumbs().answer(&uri, make).status
    }

    #[test]
    fn three_images_get_keys_n_0_n_1_n_2() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.jpg", b"jpg"), ("c.svg", b"<svg/>")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let thumbs = last_thumbs(&window);
        let n = list_number(&thumbs[0]);
        assert_eq!(thumbs, [key(n, 0), key(n, 1), key(n, 2)]);
        // Each key names its own row's file.
        let calls = RefCell::new(Vec::new());
        for key in &thumbs {
            assert_eq!(ask(&session, key, &calls), 200);
        }
        assert_eq!(*calls.borrow(), paths);
    }

    #[test]
    fn a_skipped_row_and_the_over_limit_row_have_no_thumb() {
        let dir = folder_with_pngs(MAX_RUN_FILES + 1, &[]);
        let (_loose, notes) = files(&[("notes.txt", b"x")]);
        let session = Session::new();
        let window = Window::default();
        drop_on(
            &session,
            &window,
            &[notes[0].clone(), dir.path().to_path_buf()],
        );
        let events = window.take();
        let list = last_list(&events);
        // `notes.txt`, the 2000 images, then the row for the one left out.
        assert_eq!(list.files.len(), MAX_RUN_FILES + 2);
        let thumbs = thumbs_of(list);
        assert_eq!(list.files[0].name, "notes.txt");
        assert_eq!(thumbs[0], None);
        let n = list_number(&thumbs[1]);
        for (index, thumb) in thumbs.iter().enumerate().take(MAX_RUN_FILES + 1).skip(1) {
            assert_eq!(*thumb, key(n, index));
        }
        assert_eq!(list.files[MAX_RUN_FILES + 1].name, "(over the limit)");
        assert_eq!(thumbs[MAX_RUN_FILES + 1], None);
    }

    #[test]
    fn an_add_keeps_the_earlier_keys() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        let first = last_thumbs(&window);
        let n = list_number(&first[0]);
        assert_eq!(first, [key(n, 0)]);
        pick(&session, &window, &paths[1..]);
        assert_eq!(last_thumbs(&window), [key(n, 0), key(n, 1)]);
    }

    #[test]
    fn clear_raises_the_list_number_and_drops_the_cache() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let first = last_thumbs(&window);
        let n = list_number(&first[0]);
        let calls = RefCell::new(Vec::new());
        assert_eq!(ask(&session, &first[0], &calls), 200);
        assert_eq!(session.clear_list(&mut window.emit()), Ok(()));
        // The same file again: only the Clear explains a new number.
        pick(&session, &window, &paths);
        let second = last_thumbs(&window);
        assert_eq!(second, [key(n + 1, 0)]);
        assert_eq!(ask(&session, &second[0], &calls), 200);
        assert_eq!(ask(&session, &first[0], &calls), 404);
        assert_eq!(*calls.borrow(), [paths[0].clone(), paths[0].clone()]);
    }

    #[test]
    fn a_busy_clear_keeps_the_list_number() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        let n = list_number(&last_thumbs(&window)[0]);
        let ticket = session.guard.try_start().unwrap();
        assert_eq!(
            session.clear_list(&mut window.emit()),
            Err(CommandError::Busy)
        );
        drop(ticket);
        window.take();
        pick(&session, &window, &paths[1..]);
        assert_eq!(last_thumbs(&window), [key(n, 0), key(n, 1)]);
    }

    #[test]
    fn the_first_add_after_a_finished_run_raises_the_list_number_and_the_next_add_keeps_it() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..1]);
        let n = list_number(&last_thumbs(&window)[0]);
        run_the_list(&session, &window);
        window.take();
        // The same file again: only the new list explains a new number.
        pick(&session, &window, &paths[..1]);
        assert_eq!(last_thumbs(&window), [key(n + 1, 0)]);
        pick(&session, &window, &paths[1..]);
        assert_eq!(last_thumbs(&window), [key(n + 1, 0), key(n + 1, 1)]);
    }

    #[test]
    fn a_cancelled_dialog_after_a_run_starts_no_list() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let n = list_number(&last_thumbs(&window)[0]);
        run_the_list(&session, &window);
        for cancelled in [None, Some(Vec::new())] {
            session
                .choose_files(&mut window.emit(), |ticket| {
                    session.add_paths(ticket, cancelled, &mut window.emit());
                })
                .unwrap();
        }
        window.take();
        pick(&session, &window, &paths);
        // One new list, not one per cancelled dialog.
        assert_eq!(last_thumbs(&window), [key(n + 1, 0)]);
    }

    #[test]
    fn readd_keeps_the_list_number_and_the_cache() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let first = last_thumbs(&window);
        let calls = RefCell::new(Vec::new());
        assert_eq!(ask(&session, &first[0], &calls), 200);
        run_the_list(&session, &window);
        window.take();
        readd(&session, &window);
        assert_eq!(last_thumbs(&window), first);
        assert_eq!(ask(&session, &first[0], &calls), 200);
        assert_eq!(*calls.borrow(), [paths[0].clone()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_row_dropped_from_the_middle_raises_the_list_number() {
        let (dir, paths) = files(&[
            ("a.png", b"png"),
            ("link.png", b"png"),
            ("b.png", b"png"),
            ("d.png", b"png"),
        ]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..3]);
        let first = last_thumbs(&window);
        let n = list_number(&first[0]);
        assert_eq!(first, [key(n, 0), key(n, 1), key(n, 2)]);
        // `link.png` becomes the same file as `a.png`: planned once, so
        // `b.png` moves up to index 1.
        let link = dir.path().join("link.png");
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&paths[0], &link).unwrap();
        pick(&session, &window, &paths[3..]);
        let events = window.take();
        assert_eq!(row_names(last_list(&events)), ["a.png", "b.png", "d.png"]);
        let second = thumbs_of(last_list(&events));
        assert_eq!(second, [key(n + 1, 0), key(n + 1, 1), key(n + 1, 2)]);
        // No key changed its file: the old index 1 names nothing, the new
        // one names `b.png`.
        let calls = RefCell::new(Vec::new());
        assert_eq!(ask(&session, &first[1], &calls), 404);
        assert_eq!(ask(&session, &second[1], &calls), 200);
        assert_eq!(*calls.borrow(), [paths[2].clone()]);
    }

    #[test]
    fn an_entry_that_became_a_folder_keeps_the_list_number_and_has_no_thumb() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths[..2]);
        let n = list_number(&last_thumbs(&window)[0]);
        fs::remove_file(&paths[0]).unwrap();
        fs::create_dir(&paths[0]).unwrap();
        pick(&session, &window, &paths[2..]);
        let events = window.take();
        assert_eq!(row_names(last_list(&events)), ["a.png", "b.png", "c.png"]);
        assert_eq!(thumbs_of(last_list(&events)), [None, key(n, 1), key(n, 2)]);
    }

    // --- off the calling thread, or else --------------------------------------

    #[test]
    fn other_work_runs_on_the_started_thread_and_not_the_fallback() {
        let ran: Ran = Arc::default();
        let fell_back = Cell::new(None);
        on_own_thread_or_else(
            7,
            recording_work(&ran),
            |job| {
                std::thread::spawn(job).join().unwrap();
                Ok(())
            },
            |value| fell_back.set(Some(value)),
        );
        assert_eq!(*ran.lock().unwrap(), [(7, false)]);
        assert_eq!(fell_back.get(), None);
    }

    #[test]
    fn the_fallback_gets_the_value_when_no_thread_starts() {
        let ran: Ran = Arc::default();
        let fell_back = Cell::new(None);
        on_own_thread_or_else(
            7,
            recording_work(&ran),
            |job| {
                drop(job);
                Err(io::Error::other("no threads left"))
            },
            |value| fell_back.set(Some(value)),
        );
        assert!(ran.lock().unwrap().is_empty());
        assert_eq!(fell_back.get(), Some(7));
    }

    // --- Single image mode ----------------------------------------------------

    use crate::events::ImageChanged;
    use crate::single::LoadedImage;
    use lunaepix_core::{MAX_FILE_BYTES, Probe};

    /// A prober double: answers `answer` and records every path it is given.
    struct Prober {
        answer: ProbeAnswer,
        asked: RefCell<Vec<PathBuf>>,
    }

    impl Prober {
        fn new(answer: ProbeAnswer) -> Self {
            Prober {
                answer,
                asked: RefCell::default(),
            }
        }

        /// A 640 × 480 picture.
        fn sized() -> Self {
            Self::new(ProbeAnswer::Size(Probe {
                width: Some(640),
                height: Some(480),
            }))
        }

        fn probe(&self) -> impl Fn(&Path) -> ProbeAnswer + '_ {
            |path| {
                self.asked.borrow_mut().push(path.to_path_buf());
                self.answer.clone()
            }
        }

        fn asked(&self) -> Vec<PathBuf> {
            self.asked.borrow().clone()
        }
    }

    /// `choose_image`, answered with `picked`.
    fn choose_image(session: &Session, window: &Window, picked: Option<&Path>, prober: &Prober) {
        session
            .choose_image(&mut window.emit(), |ticket| {
                session.load_image(
                    ticket,
                    picked.map(Path::to_path_buf),
                    prober.probe(),
                    &mut window.emit(),
                );
            })
            .unwrap();
    }

    /// A drop of `paths` as `lib.rs` makes it, received at once instead of
    /// on another thread.
    fn drop_routed(session: &Session, window: &Window, paths: &[PathBuf], prober: &Prober) {
        session.drop_paths(paths.to_vec(), &mut window.emit(), |ticket, paths| {
            session.receive_drop(ticket, paths, prober.probe(), &mut window.emit());
        });
    }

    fn set_view(session: &Session, window: &Window, view: &str) {
        session
            .set_view(Some(&json!({ "view": view })), &mut window.emit())
            .unwrap();
    }

    /// Every `image-changed` among `events`.
    fn images(events: &[Event]) -> Vec<&ImageChanged> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::ImageChanged(changed) => Some(changed),
                _ => None,
            })
            .collect()
    }

    /// The one event the window heard: an `image-changed`, as JSON.
    fn only_image_changed(window: &Window) -> Value {
        let events = window.take();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].name(), "image-changed");
        value(&events[0])
    }

    fn loaded(session: &Session) -> LoadedImage {
        session.single().current().expect("an image is loaded")
    }

    /// The folder of the image with `key`, beside the current image's.
    fn image_folder(session: &Session, key: u64) -> PathBuf {
        let private = loaded(session)
            .copy
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        private.join(format!("image-{key}"))
    }

    fn guard_is_free(session: &Session) -> bool {
        session.guard.try_start().is_some()
    }

    // The view.

    #[test]
    fn single_is_the_view_at_start() {
        let session = Session::new();
        assert_eq!(session.view(), View::Single);
        assert_ne!(session.view(), View::Batch);
    }

    #[test]
    fn set_view_refuses_anything_but_single_or_batch() {
        let session = Session::new();
        let window = Window::default();
        set_view(&session, &window, "batch");
        assert_eq!(session.view(), View::Batch);
        set_view(&session, &window, "single");
        assert_eq!(session.view(), View::Single);
        assert!(window.take().is_empty(), "no event for a switch");

        let bad = [
            Some(json!({ "view": "Single" })),
            Some(json!({ "view": "BATCH" })),
            Some(json!({ "view": 1 })),
            Some(json!({ "view": null })),
            Some(json!({})),
            Some(json!({ "view": "batch", "extra": 1 })),
            Some(json!("batch")),
            Some(json!(["batch"])),
            Some(Value::Null),
            None,
        ];
        for args in &bad {
            assert_eq!(
                session.set_view(args.as_ref(), &mut window.emit()),
                Err(CommandError::InvalidView),
                "{args:?}"
            );
            assert_eq!(session.view(), View::Single, "{args:?}");
            assert!(guard_is_free(&session));
        }
        assert!(window.take().is_empty());
        assert_eq!(error_json(CommandError::InvalidView), json!("invalid-view"));
    }

    #[test]
    fn set_view_is_busy_during_a_run() {
        let session = Session::new();
        let window = Window::default();
        let run = session.guard.try_start().unwrap();
        assert_eq!(
            session.set_view(Some(&json!({ "view": "batch" })), &mut window.emit()),
            Err(CommandError::Busy)
        );
        assert_eq!(window.take(), [Event::RunBusy]);
        assert_eq!(session.view(), View::Single);
        drop(run);
        set_view(&session, &window, "batch");
        assert_eq!(session.view(), View::Batch);
    }

    #[test]
    fn a_drop_in_single_view_loads_and_does_not_touch_the_list() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        drop_routed(&session, &window, &paths, &prober);
        let events = window.take();
        assert_eq!(images(&events).len(), 1, "{events:?}");
        assert!(
            !events.iter().any(|event| event.name() == "list-changed"),
            "{events:?}"
        );
        assert_eq!(prober.asked().len(), 1);
        assert!(lock(&session.list).rows(None).is_empty());
        assert!(guard_is_free(&session));
    }

    #[test]
    fn a_drop_in_batch_view_adds_to_the_list_and_does_not_load() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        set_view(&session, &window, "batch");
        let prober = Prober::sized();
        drop_routed(&session, &window, &paths, &prober);
        let events = window.take();
        assert_eq!(row_names(last_list(&events)), ["a.png"]);
        assert!(images(&events).is_empty(), "{events:?}");
        assert!(prober.asked().is_empty());
        assert_eq!(session.single().current(), None);
        assert!(guard_is_free(&session));
    }

    // Loading.

    #[test]
    fn loading_copies_the_file_and_probes_the_copy() {
        let (_dir, paths) = files(&[("holiday.png", b"the picture")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        choose_image(&session, &window, Some(&paths[0]), &prober);
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": { "name": "holiday.png", "size": 11, "width": 640, "height": 480,
                               "key": "1", "suggested": "optimized-holiday", "extension": ".png" },
                    "left_out": 0, "notice": null })
        );
        let image = loaded(&session);
        assert_eq!(image.copy.file_name().unwrap(), "original.png");
        assert_eq!(image.copy.parent().unwrap().file_name().unwrap(), "image-1");
        assert_eq!(fs::read(&image.copy).unwrap(), b"the picture");
        // The prober got the copy, never the original.
        assert_eq!(prober.asked(), std::slice::from_ref(&image.copy));
        assert_ne!(prober.asked(), std::slice::from_ref(&paths[0]));
        assert_eq!(image.original(), paths[0]);
        assert_eq!(image.extension, ".png");
        assert_eq!(image.size, 11);
        assert!(guard_is_free(&session));
    }

    #[test]
    fn a_new_load_gives_a_new_key_and_removes_the_old_images_folder() {
        let (_dir, paths) = files(&[("a.png", b"one")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        choose_image(&session, &window, Some(&paths[0]), &prober);
        let first = loaded(&session);
        assert_eq!(first.key, 1);
        // The file as it is at the next load is the one taken.
        fs::write(&paths[0], b"two!").unwrap();
        choose_image(&session, &window, Some(&paths[0]), &prober);
        let second = loaded(&session);
        assert_eq!(second.key, 2);
        assert_eq!(fs::read(&second.copy).unwrap(), b"two!");
        assert!(!image_folder(&session, 1).exists());
        assert!(image_folder(&session, 2).is_dir());
        let keys: Vec<Value> = images(&window.take())
            .iter()
            .map(|changed| value(&Event::ImageChanged((*changed).clone()))["image"]["key"].clone())
            .collect();
        assert_eq!(keys, [json!("1"), json!("2")]);
    }

    #[test]
    fn a_refused_probe_keeps_the_current_image_and_sends_its_reason() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"broken")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        window.take();
        let refusing = Prober::new(ProbeAnswer::Refused("Batch's reason".to_owned()));
        choose_image(&session, &window, Some(&paths[1]), &refusing);
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["key"], json!("1"));
        assert_eq!(changed["image"]["name"], json!("a.png"));
        assert_eq!(changed["notice"], json!("Batch's reason"));
        assert_eq!(changed["left_out"], json!(0));
        assert_eq!(loaded(&session).key, 1);
        assert!(image_folder(&session, 1).is_dir());
        // The refused attempt's copy is gone.
        assert_eq!(refusing.asked().len(), 1);
        assert!(!refusing.asked()[0].exists());
        assert!(!image_folder(&session, 2).exists());
    }

    #[test]
    fn a_failed_probe_keeps_the_current_image_with_a_reason() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        window.take();
        choose_image(
            &session,
            &window,
            Some(&paths[1]),
            &Prober::new(ProbeAnswer::Failed),
        );
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["key"], json!("1"));
        assert_eq!(
            changed["notice"],
            json!("the optimiser crashed on this file (it gave no valid result)")
        );
        assert_eq!(loaded(&session).key, 1);
        assert!(!image_folder(&session, 2).exists());
    }

    #[test]
    fn a_non_image_choice_keeps_the_current_image_with_the_not_an_image_notice() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("notes.txt", b"text")]);
        let session = Session::new();
        let window = Window::default();
        // Without an image: `image` is null.
        let prober = Prober::sized();
        choose_image(&session, &window, Some(&paths[1]), &prober);
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": null, "left_out": 0, "notice": "Not a PNG, JPG, SVG or WebP file." })
        );
        assert!(prober.asked().is_empty());
        // With one: it stays.
        choose_image(&session, &window, Some(&paths[0]), &prober);
        window.take();
        choose_image(&session, &window, Some(&paths[1]), &prober);
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["key"], json!("1"));
        assert_eq!(
            changed["notice"],
            json!("Not a PNG, JPG, SVG or WebP file.")
        );
        assert_eq!(prober.asked().len(), 1, "only a.png was probed");
    }

    #[test]
    fn a_closed_dialog_sends_run_cancelled_and_changes_nothing() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        // An image, and a drop that left one out.
        drop_routed(&session, &window, &paths, &prober);
        window.take();
        let before = loaded(&session);

        choose_image(&session, &window, None, &prober);
        assert_eq!(window.take(), [Event::RunCancelled]);
        assert_eq!(loaded(&session), before);
        assert_eq!(prober.asked().len(), 1);
        assert!(guard_is_free(&session));
        // The offer is still there.
        assert_eq!(session.move_drop_to_batch(&mut window.emit()), Ok(()));
    }

    #[test]
    fn choose_image_is_busy_during_a_run() {
        let session = Session::new();
        let window = Window::default();
        let run = session.guard.try_start().unwrap();
        let opened = Cell::new(false);
        assert_eq!(
            session.choose_image(&mut window.emit(), |_| opened.set(true)),
            Err(CommandError::Busy)
        );
        assert!(!opened.get());
        assert_eq!(window.take(), [Event::RunBusy]);
        drop(run);
        assert_eq!(
            session.choose_image(&mut window.emit(), |_| opened.set(true)),
            Ok(())
        );
        assert!(opened.get());
    }

    /// Every file named `original*` under `root`, two levels down.
    fn copies_under(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for private in fs::read_dir(root).unwrap().flatten() {
            let Ok(images) = fs::read_dir(private.path()) else {
                continue;
            };
            for image in images.flatten() {
                for file in fs::read_dir(image.path()).into_iter().flatten().flatten() {
                    found.push(file.path());
                }
            }
        }
        found
    }

    #[test]
    fn a_file_over_max_file_bytes_is_refused_before_it_is_copied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("private");
        fs::create_dir(&root).unwrap();
        let session = Session {
            single: Single::with_root(root.clone()),
            ..Session::default()
        };
        let window = Window::default();
        let prober = Prober::sized();
        let big = dir.path().join("big.png");
        // Sparse: no 100 MiB is written.
        fs::File::create(&big)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        choose_image(&session, &window, Some(&big), &prober);
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": null, "left_out": 0, "notice": "File is too large" })
        );
        assert!(prober.asked().is_empty());
        assert!(copies_under(&root).is_empty());

        // A small file in the same session is copied there.
        let small = dir.path().join("small.png");
        fs::write(&small, b"png").unwrap();
        choose_image(&session, &window, Some(&small), &prober);
        assert_eq!(copies_under(&root).len(), 1);
    }

    #[test]
    fn changing_the_original_after_load_does_not_change_the_copy() {
        let (_dir, paths) = files(&[("a.png", b"as loaded")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        fs::write(&paths[0], b"changed on disk").unwrap();
        let image = loaded(&session);
        assert_eq!(fs::read(&image.copy).unwrap(), b"as loaded");
        assert_eq!(image.size, 9);
    }

    #[test]
    fn deleting_the_original_after_load_keeps_the_copy() {
        let (_dir, paths) = files(&[("a.png", b"as loaded")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        fs::remove_file(&paths[0]).unwrap();
        let image = loaded(&session);
        assert_eq!(fs::read(&image.copy).unwrap(), b"as loaded");
        assert_eq!(image.key, 1);
    }

    // Drops in Single image mode.

    #[test]
    fn one_image_loads_with_left_out_0_and_no_notice() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        drop_routed(&session, &window, &paths, &Prober::sized());
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": { "name": "a.png", "size": 3, "width": 640, "height": 480,
                               "key": "1", "suggested": "optimized-a", "extension": ".png" },
                    "left_out": 0, "notice": null })
        );
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
    }

    #[test]
    fn three_images_load_the_first_with_left_out_2_and_the_plural_notice() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.jpg", b"jpg"), ("c.webp", b"webp")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        drop_routed(&session, &window, &paths, &prober);
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["name"], json!("a.png"));
        assert_eq!(changed["left_out"], json!(2));
        assert_eq!(
            changed["notice"],
            json!("Single image mode takes one image; 2 others were left out.")
        );
        assert_eq!(prober.asked().len(), 1);
    }

    #[test]
    fn two_images_give_the_singular_notice() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.svg", b"<svg/>")]);
        let session = Session::new();
        let window = Window::default();
        drop_routed(&session, &window, &paths, &Prober::sized());
        let changed = only_image_changed(&window);
        assert_eq!(changed["left_out"], json!(1));
        assert_eq!(
            changed["notice"],
            json!("Single image mode takes one image; 1 other was left out.")
        );
    }

    #[test]
    fn a_non_image_then_an_image_loads_the_image() {
        let (_dir, paths) = files(&[("notes.txt", b"text"), ("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        drop_routed(&session, &window, &paths, &prober);
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["name"], json!("a.png"));
        assert_eq!(changed["left_out"], json!(0));
        assert_eq!(changed["notice"], Value::Null);
        assert_eq!(prober.asked().len(), 1);
    }

    #[test]
    fn an_image_and_a_text_file_have_left_out_0_and_no_notice() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("notes.txt", b"text")]);
        let session = Session::new();
        let window = Window::default();
        drop_routed(&session, &window, &paths, &Prober::sized());
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["name"], json!("a.png"));
        assert_eq!(changed["left_out"], json!(0));
        assert_eq!(changed["notice"], Value::Null);
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
    }

    #[test]
    fn a_folder_loads_nothing_keeps_the_image_and_says_folders_go_in_batch() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let folder = dir.path().join("photos");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("c.png"), b"png").unwrap();
        // Without an image: the spec's example, exactly.
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        drop_routed(&session, &window, std::slice::from_ref(&folder), &prober);
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": null, "left_out": 1, "notice": "Folders go in Batch." })
        );
        assert!(prober.asked().is_empty());
        // With one: it stays.
        drop_routed(&session, &window, &paths, &prober);
        window.take();
        drop_routed(&session, &window, &[folder], &prober);
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["key"], json!("1"));
        assert_eq!(changed["left_out"], json!(1));
        assert_eq!(changed["notice"], json!("Folders go in Batch."));
        assert_eq!(prober.asked().len(), 1);
        assert!(lock(&session.list).rows(None).is_empty(), "nothing listed");
    }

    #[test]
    fn only_text_files_load_nothing_with_left_out_0() {
        let (_dir, paths) = files(&[("a.txt", b"text"), ("b.gif", b"gif")]);
        let session = Session::new();
        let window = Window::default();
        let prober = Prober::sized();
        drop_routed(&session, &window, &paths, &prober);
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": null, "left_out": 0, "notice": "Not a PNG, JPG, SVG or WebP file." })
        );
        assert!(prober.asked().is_empty());
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
    }

    #[test]
    fn a_refused_first_image_of_three_keeps_the_offer_with_the_reason() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        let refusing = Prober::new(ProbeAnswer::Refused("Batch's reason".to_owned()));
        drop_routed(&session, &window, &paths, &refusing);
        assert_eq!(
            only_image_changed(&window),
            json!({ "image": null, "left_out": 2, "notice": "Batch's reason" })
        );
        assert_eq!(refusing.asked().len(), 1, "only the first is tried");
        assert_eq!(session.move_drop_to_batch(&mut window.emit()), Ok(()));
        assert_eq!(
            row_names(last_list(&window.take())),
            ["a.png", "b.png", "c.png"]
        );
    }

    // Switch to Batch.

    #[test]
    fn move_drop_to_batch_adds_every_item_of_that_drop_and_switches_to_batch() {
        let (dir, mut paths) = files(&[("a.png", b"png"), ("b.jpg", b"jpg")]);
        let folder = dir.path().join("photos");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("c.png"), b"png").unwrap();
        fs::write(folder.join("d.webp"), b"webp").unwrap();
        fs::write(folder.join("notes.txt"), b"text").unwrap();
        paths.push(folder);
        let session = Session::new();
        let window = Window::default();
        drop_routed(&session, &window, &paths, &Prober::sized());
        assert_eq!(only_image_changed(&window)["left_out"], json!(2));

        assert_eq!(session.move_drop_to_batch(&mut window.emit()), Ok(()));
        assert_eq!(session.view(), View::Batch);
        let events = window.take();
        assert_eq!(events.len(), 1, "{events:?}");
        // The loaded image included; the folder adds its images.
        assert_eq!(
            row_names(last_list(&events)),
            ["a.png", "b.jpg", "c.png", "d.webp"]
        );
        assert!(guard_is_free(&session));
        // The image stays loaded for when Single image shows again.
        assert_eq!(loaded(&session).key, 1);
        // Once: the offer is used up.
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
    }

    #[test]
    fn move_drop_to_batch_after_a_finished_run_starts_a_fresh_list() {
        let (_dir, paths) = files(&[("old.png", b"png"), ("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        set_view(&session, &window, "batch");
        pick(&session, &window, &paths[..1]);
        let prepared = started(&session, &window, &lossless());
        run_fake(&session, &window, prepared);
        set_view(&session, &window, "single");
        drop_routed(&session, &window, &paths[1..], &Prober::sized());
        window.take();

        assert_eq!(session.move_drop_to_batch(&mut window.emit()), Ok(()));
        let list = last_list(&window.take()).clone();
        assert_eq!(row_names(&list), ["a.png", "b.png"]);
        assert_eq!(thumbs_of(&list), [key(2, 0), key(2, 1)]);
    }

    #[test]
    fn move_drop_to_batch_without_a_left_out_drop_is_nothing_to_move() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
        assert!(window.take().is_empty());
        assert!(guard_is_free(&session));
        drop_routed(&session, &window, &paths, &Prober::sized());
        window.take();
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
        assert_eq!(session.view(), View::Single);
        assert!(lock(&session.list).rows(None).is_empty());
        assert_eq!(
            error_json(CommandError::NothingToMove),
            json!("nothing-to-move")
        );
    }

    #[test]
    fn move_drop_to_batch_is_busy_during_a_run() {
        let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        drop_routed(&session, &window, &paths, &Prober::sized());
        window.take();
        let run = session.guard.try_start().unwrap();
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::Busy)
        );
        assert_eq!(window.take(), [Event::RunBusy]);
        assert_eq!(session.view(), View::Single);
        drop(run);
        // The refusal kept the offer.
        assert_eq!(session.move_drop_to_batch(&mut window.emit()), Ok(()));
    }

    /// A session that has just had a drop of two images, so Switch to Batch
    /// is offered.
    fn with_an_offer() -> (TempDir, Vec<PathBuf>, Session, Window) {
        let (dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        drop_routed(&session, &window, &paths[..2], &Prober::sized());
        assert_eq!(only_image_changed(&window)["left_out"], json!(1));
        (dir, paths, session, window)
    }

    #[test]
    fn the_offer_is_gone_after_the_next_drop() {
        let (_dir, paths, session, window) = with_an_offer();
        drop_routed(&session, &window, &paths[2..], &Prober::sized());
        window.take();
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
        assert!(lock(&session.list).rows(None).is_empty());
    }

    #[test]
    fn the_offer_is_gone_after_a_choice() {
        let (_dir, paths, session, window) = with_an_offer();
        choose_image(&session, &window, Some(&paths[2]), &Prober::sized());
        window.take();
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
    }

    #[test]
    fn the_offer_is_gone_after_set_view_by_hand() {
        let (_dir, _paths, session, window) = with_an_offer();
        set_view(&session, &window, "batch");
        assert_eq!(
            session.move_drop_to_batch(&mut window.emit()),
            Err(CommandError::NothingToMove)
        );
        assert!(lock(&session.list).rows(None).is_empty());
    }

    // The suggested name.

    #[test]
    fn suggested_is_optimized_name_next_to_the_original() {
        let (_dir, paths) = files(&[("holiday.jpg", b"jpg")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["suggested"], json!("optimized-holiday"));
        assert_eq!(changed["image"]["extension"], json!(".jpg"));
    }

    #[test]
    fn suggested_is_the_plain_name_in_a_chosen_folder() {
        let (dir, paths) = files(&[("holiday.jpg", b"jpg")]);
        let session = Session::new();
        let window = Window::default();
        // Not made yet: a missing folder has no names taken.
        choose_output(&session, &window, &dir.path().join("exports"));
        window.take();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["suggested"], json!("holiday"));
    }

    #[test]
    fn suggested_is_numbered_when_taken() {
        let (dir, paths) = files(&[("holiday.jpg", b"jpg"), ("optimized-holiday.jpg", b"jpg")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        assert_eq!(
            only_image_changed(&window)["image"]["suggested"],
            json!("optimized-holiday-1")
        );
        // The original's own folder chosen as the output: its own name is
        // taken by the original.
        let other = Session::new();
        choose_output(&other, &window, dir.path());
        window.take();
        choose_image(&other, &window, Some(&paths[0]), &Prober::sized());
        assert_eq!(
            only_image_changed(&window)["image"]["suggested"],
            json!("holiday-1")
        );
    }

    #[test]
    fn name_suggested_follows_output_changed_with_an_image_loaded() {
        let (dir, paths) = files(&[("holiday.jpg", b"jpg")]);
        let exports = dir.path().join("exports");
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        window.take();

        choose_output(&session, &window, &exports);
        let events = window.take();
        let names: Vec<&str> = events.iter().map(Event::name).collect();
        assert_eq!(names, ["output-changed", "name-suggested"]);
        assert_eq!(
            value(&events[1]),
            json!({ "image": "1", "suggested": "holiday" })
        );

        session.clear_output_folder(&mut window.emit()).unwrap();
        let events = window.take();
        let names: Vec<&str> = events.iter().map(Event::name).collect();
        assert_eq!(names, ["output-changed", "name-suggested"]);
        assert_eq!(
            value(&events[1]),
            json!({ "image": "1", "suggested": "optimized-holiday" })
        );
    }

    #[test]
    fn no_name_suggested_without_an_image() {
        let dir = TempDir::new().unwrap();
        let session = Session::new();
        let window = Window::default();
        choose_output(&session, &window, dir.path());
        session.clear_output_folder(&mut window.emit()).unwrap();
        let names: Vec<&str> = window.take().iter().map(Event::name).collect();
        assert_eq!(names, ["output-changed", "output-changed"]);
    }

    #[test]
    fn name_suggested_keeps_the_key() {
        let (dir, paths) = files(&[("holiday.jpg", b"jpg")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        let before = loaded(&session);
        choose_output(&session, &window, dir.path());
        assert_eq!(loaded(&session), before);
        assert!(before.copy.is_file());
        // A cancelled folder dialog sends none.
        window.take();
        session
            .choose_output_folder(&mut window.emit(), |ticket| {
                session.set_output(ticket, None, &mut window.emit());
            })
            .unwrap();
        assert_eq!(window.take(), [Event::RunCancelled]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_utf8_original_name_gives_a_lossy_suggested_name() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(OsStr::from_bytes(b"caf\xe9.png"));
        fs::write(&path, b"png").unwrap();
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&path), &Prober::sized());
        let changed = only_image_changed(&window);
        assert_eq!(changed["image"]["name"], json!("caf\u{FFFD}.png"));
        assert_eq!(
            changed["image"]["suggested"],
            json!("optimized-caf\u{FFFD}")
        );
        assert_eq!(changed["notice"], Value::Null);
        // The original's own bytes are kept for the save.
        assert_eq!(
            loaded(&session).file_name,
            OsStr::from_bytes(b"caf\xe9.png")
        );
    }

    #[test]
    fn the_session_shutdown_removes_the_private_folder() {
        let (_dir, paths) = files(&[("a.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        let private = image_folder(&session, 1).parent().unwrap().to_path_buf();
        assert!(private.is_dir());
        session.shutdown();
        assert!(!private.exists());
        assert_eq!(session.single().current(), None);
    }

    // --- preview_image ----------------------------------------------------------

    /// `preview_image` with `settings`: its answer and the job handed to
    /// `start` (only when it started).
    fn preview(
        session: &Session,
        window: &Window,
        settings: Option<&Value>,
    ) -> (Result<(), CommandError>, Option<PreviewJob>) {
        let mut job = None;
        let answer =
            session.preview_image(settings, &mut window.emit(), |started| job = Some(started));
        (answer, job)
    }

    /// A session with `a.png` loaded as image 1, the window's events taken.
    fn with_an_image() -> (TempDir, Vec<PathBuf>, Session, Window) {
        let (dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png")]);
        let session = Session::new();
        let window = Window::default();
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        window.take();
        (dir, paths, session, window)
    }

    /// `preview_image` refused with `error`: nothing started, nothing sent
    /// but `run-busy` for busy, the guard free afterwards.
    fn preview_refused(
        session: &Session,
        window: &Window,
        settings: Option<&Value>,
        error: CommandError,
    ) {
        let (answer, job) = preview(session, window, settings);
        assert_eq!(answer, Err(error), "{settings:?}");
        assert!(job.is_none(), "a refused preview must not start");
        let expected: Vec<&str> = if error == CommandError::Busy {
            vec!["run-busy"]
        } else {
            vec![]
        };
        let heard: Vec<&str> = window.take().iter().map(Event::name).collect();
        assert_eq!(heard, expected);
    }

    #[test]
    fn a_preview_without_an_image_is_no_image() {
        let session = Session::new();
        let window = Window::default();
        preview_refused(&session, &window, Some(&lossy(40)), CommandError::NoImage);
        assert!(guard_is_free(&session));
        // A refused request takes no version: the first one is still 1.
        let (_dir, paths) = files(&[("a.png", b"png")]);
        choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
        window.take();
        let (_, job) = preview(&session, &window, Some(&lossy(40)));
        assert_eq!(job.unwrap().version, 1);
    }

    #[test]
    fn a_preview_after_shutdown_is_no_image() {
        let (_dir, _paths, session, window) = with_an_image();
        session.shutdown();
        preview_refused(&session, &window, Some(&lossy(40)), CommandError::NoImage);
    }

    #[test]
    fn a_preview_with_an_image_starts_and_sends_preview_started() {
        let (_dir, _paths, session, window) = with_an_image();
        let (answer, job) = preview(&session, &window, Some(&lossy(40)));
        assert_eq!(answer, Ok(()));
        let job = job.expect("start got the job");
        assert_eq!((job.key, job.version), (1, 1));
        let events = window.take();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].name(), "preview-started");
        assert_eq!(value(&events[0]), json!({ "image": "1", "version": 1 }));
        // The guard is not held while the version is made.
        assert!(guard_is_free(&session));
    }

    #[test]
    fn lossy_0_lossy_100_and_lossless_settings_start_a_preview() {
        let (_dir, _paths, session, window) = with_an_image();
        for (settings, compression) in [
            (lossy(0), Compression::Lossy(Quality::new(0).unwrap())),
            (lossy(100), Compression::Lossy(Quality::new(100).unwrap())),
            (lossless(), Compression::Lossless),
        ] {
            let (answer, job) = preview(&session, &window, Some(&settings));
            assert_eq!(answer, Ok(()), "{settings}");
            assert_eq!(job.unwrap().compression, compression);
        }
    }

    /// Every shape `parse_settings` refuses, as `start_run` refuses them.
    fn bad_settings() -> Vec<Value> {
        let from_text = |text: &str| -> Value { serde_json::from_str(text).unwrap() };
        vec![
            json!({ "mode": "lossy", "quality": -1 }),
            json!({ "mode": "lossy", "quality": 101 }),
            json!({ "mode": "lossy", "quality": 75.5 }),
            from_text(r#"{ "mode": "lossy", "quality": 75.0 }"#),
            json!({ "mode": "lossy", "quality": "75" }),
            json!({ "mode": "lossy", "quality": null }),
            json!({ "mode": "lossy" }),
            json!({ "quality": 75 }),
            json!({ "mode": "lossy", "quality": 75, "extra": 1 }),
            json!({ "mode": "fast", "quality": 75 }),
            json!({ "mode": "Lossy", "quality": 75 }),
            json!({ "mode": "lossless", "quality": 101 }),
            json!({}),
            json!(null),
            json!(75),
            json!(["lossy", 75]),
        ]
    }

    #[test]
    fn bad_settings_are_invalid_settings() {
        let (_dir, _paths, session, window) = with_an_image();
        for settings in bad_settings() {
            preview_refused(
                &session,
                &window,
                Some(&settings),
                CommandError::InvalidSettings,
            );
        }
        assert!(guard_is_free(&session));
    }

    #[test]
    fn a_preview_without_settings_is_invalid_settings() {
        let (_dir, _paths, session, window) = with_an_image();
        preview_refused(&session, &window, None, CommandError::InvalidSettings);
    }

    #[test]
    fn bad_settings_without_an_image_are_invalid_settings() {
        let session = Session::new();
        let window = Window::default();
        preview_refused(
            &session,
            &window,
            Some(&json!({ "mode": "lossy", "quality": 75.5 })),
            CommandError::InvalidSettings,
        );
    }

    #[test]
    fn a_preview_during_a_save_is_busy() {
        // A save holds the guard as every command does (`claim_run`).
        let session = Session::new();
        let window = Window::default();
        let saving = session.guard.try_start().expect("a free guard");
        // Busy comes first: bad settings and no image are not looked at.
        preview_refused(
            &session,
            &window,
            Some(&json!({ "mode": "lossy" })),
            CommandError::Busy,
        );
        drop(saving);
        preview_refused(
            &session,
            &window,
            Some(&json!({ "mode": "lossy" })),
            CommandError::InvalidSettings,
        );
    }

    #[test]
    fn a_preview_during_a_run_is_busy() {
        let (_dir, paths, session, window) = with_an_image();
        pick(&session, &window, &paths);
        window.take();
        let running = started(&session, &window, &lossless());
        preview_refused(&session, &window, Some(&lossy(40)), CommandError::Busy);
        drop(running);
        let (answer, _) = preview(&session, &window, Some(&lossy(40)));
        assert_eq!(answer, Ok(()));
    }

    #[test]
    fn a_preview_while_a_dialog_is_open_is_busy() {
        let (_dir, _paths, session, window) = with_an_image();
        let mut open = None;
        session
            .choose_image(&mut window.emit(), |ticket| open = Some(ticket))
            .unwrap();
        preview_refused(&session, &window, Some(&lossy(40)), CommandError::Busy);
        drop(open);
        let (answer, _) = preview(&session, &window, Some(&lossy(40)));
        assert_eq!(answer, Ok(()));
    }

    #[test]
    fn a_preview_running_does_not_make_choose_image_set_view_drop_or_choose_output_folder_busy() {
        let (dir, paths, session, window) = with_an_image();
        let (answer, job) = preview(&session, &window, Some(&lossy(40)));
        assert_eq!(answer, Ok(()));
        // Begun and not settled: the version is being made.
        let _running = job.unwrap();
        window.take();

        assert_eq!(
            session.set_view(Some(&json!({ "view": "single" })), &mut window.emit()),
            Ok(())
        );
        let mut dialogs = 0;
        session
            .choose_image(&mut window.emit(), |_ticket| dialogs += 1)
            .unwrap();
        session
            .choose_output_folder(&mut window.emit(), |_ticket| dialogs += 1)
            .unwrap();
        assert_eq!(dialogs, 2);
        let mut dropped = Vec::new();
        session.drop_paths(paths.clone(), &mut window.emit(), |_ticket, paths| {
            dropped = paths
        });
        assert_eq!(dropped, paths);
        assert_eq!(window.take(), []);
        drop(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_lossy_preview_worker_gets_lossy_no_replace_the_copy_and_the_version_file() {
        let (_dir, _paths, session, window) = with_an_image();
        let logs = TempDir::new().unwrap();
        let log = logs.path().join("args");
        let (_, job) = preview(&session, &window, Some(&lossy(40)));
        session.run_preview(job.unwrap(), &recording_worker(&log), &mut window.emit());
        let copy = loaded(&session).copy;
        let expected = format!(
            "--lossy\n40\n--no-replace\n{}\n{}\n",
            copy.display(),
            copy.with_file_name("v1.png").display()
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn a_lossless_preview_worker_gets_no_replace_and_no_lossy() {
        let (_dir, _paths, session, window) = with_an_image();
        let logs = TempDir::new().unwrap();
        let log = logs.path().join("args");
        let (_, job) = preview(&session, &window, Some(&lossless()));
        session.run_preview(job.unwrap(), &recording_worker(&log), &mut window.emit());
        let copy = loaded(&session).copy;
        let expected = format!(
            "--no-replace\n{}\n{}\n",
            copy.display(),
            copy.with_file_name("v1.png").display()
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), expected);
    }

    /// Fix round 1: a call that competes with a preview's outcome, the
    /// offer after a failed attempt, the suggested name on clearing the
    /// output folder.
    mod fix1 {
        use std::cell::Cell;

        use lunaepix_core::{Crash, Outcome, RunError};

        use super::*;
        use crate::single::SettleSeam;

        /// A call that can land while a preview's outcome is settled.
        #[derive(Debug, Clone, Copy)]
        enum Call {
            Choice,
            Drop,
            BatchView,
            MoveDrop,
            StartRun,
            Shutdown,
            NewerRequest,
        }

        fn ready() -> FileResult {
            FileResult::Optimized(Outcome {
                before: 3,
                after: 2,
                millis: 5,
                note: None,
            })
        }

        fn failure() -> FileResult {
            FileResult::Failed(RunError::Crashed {
                path: PathBuf::new(),
                cause: Crash::NoValidResult,
                stderr: String::new(),
            })
        }

        fn is_outcome(event: &Event) -> bool {
            matches!(event, Event::PreviewReady(_) | Event::PreviewFailed(_))
        }

        /// The event the call itself sends, if it sends one.
        fn own_event(call: Call) -> Option<&'static str> {
            match call {
                Call::Choice | Call::Drop => Some("image-changed"),
                Call::MoveDrop => Some("list-changed"),
                Call::StartRun => Some("run-started"),
                Call::NewerRequest => Some("preview-started"),
                Call::BatchView | Call::Shutdown => None,
            }
        }

        fn compete(session: &Session, window: &Window, paths: &[PathBuf], call: Call) {
            match call {
                Call::Choice => choose_image(session, window, Some(&paths[1]), &Prober::sized()),
                Call::Drop => drop_routed(session, window, &paths[1..2], &Prober::sized()),
                Call::BatchView => set_view(session, window, "batch"),
                Call::MoveDrop => session.move_drop_to_batch(&mut window.emit()).unwrap(),
                Call::StartRun => {
                    let prepared = started(session, window, &lossy(40));
                    run_fake(session, window, prepared);
                }
                Call::Shutdown => session.shutdown(),
                Call::NewerRequest => {
                    let (answer, _newer) = preview(session, window, Some(&lossy(60)));
                    answer.unwrap();
                }
            }
        }

        /// `a.png` loaded by a drop that left `b.png` out, `c.png` in the
        /// list, and version 1 asked for; it settles with `result` while
        /// `call` lands at `at`. The outcome never follows the call's own
        /// events: before the decision it is not sent at all, right after
        /// the send it came first, once.
        fn race(call: Call, at: SettleSeam, result: FileResult) {
            let (_dir, paths) = files(&[("a.png", b"png"), ("b.png", b"png"), ("c.png", b"png")]);
            let session = Session::new();
            let window = Window::default();
            drop_routed(&session, &window, &paths[..2], &Prober::sized());
            pick(&session, &window, &paths[2..]);
            let (answer, job) = preview(&session, &window, Some(&lossy(40)));
            answer.unwrap();
            let job = job.unwrap();
            fs::write(&job.dest, b"v").unwrap();
            window.take();

            let from = Cell::new(None);
            session.single().settle_preview_with(
                &job,
                Some(result),
                &mut window.emit(),
                &mut |seam| {
                    if seam == at {
                        from.set(Some(window.0.borrow().len()));
                        compete(&session, &window, &paths, call);
                    }
                },
            );
            let from = from.get().expect("the seam was reached");
            let events = window.take();
            let names: Vec<&str> = events.iter().map(Event::name).collect();
            if let Some(own) = own_event(call) {
                assert!(names[from..].contains(&own), "{names:?}");
            }
            assert!(!events[from..].iter().any(is_outcome), "{names:?}");
            let sent = events[..from]
                .iter()
                .filter(|event| is_outcome(event))
                .count();
            let expected = match at {
                SettleSeam::BeforeDecision => 0,
                SettleSeam::AfterSend => 1,
            };
            assert_eq!(sent, expected, "{names:?}");
        }

        macro_rules! races {
            ($($name:ident: $call:ident, $seam:ident, $result:ident;)*) => {
                $(
                    #[test]
                    fn $name() {
                        race(Call::$call, SettleSeam::$seam, $result());
                    }
                )*
            };
        }

        races! {
            race_choice_before_ready: Choice, BeforeDecision, ready;
            race_choice_before_failed: Choice, BeforeDecision, failure;
            race_choice_after_ready: Choice, AfterSend, ready;
            race_choice_after_failed: Choice, AfterSend, failure;
            race_drop_before_ready: Drop, BeforeDecision, ready;
            race_drop_before_failed: Drop, BeforeDecision, failure;
            race_drop_after_ready: Drop, AfterSend, ready;
            race_drop_after_failed: Drop, AfterSend, failure;
            race_batch_view_before_ready: BatchView, BeforeDecision, ready;
            race_batch_view_before_failed: BatchView, BeforeDecision, failure;
            race_batch_view_after_ready: BatchView, AfterSend, ready;
            race_batch_view_after_failed: BatchView, AfterSend, failure;
            race_move_drop_before_ready: MoveDrop, BeforeDecision, ready;
            race_move_drop_before_failed: MoveDrop, BeforeDecision, failure;
            race_move_drop_after_ready: MoveDrop, AfterSend, ready;
            race_move_drop_after_failed: MoveDrop, AfterSend, failure;
            race_start_run_before_ready: StartRun, BeforeDecision, ready;
            race_start_run_before_failed: StartRun, BeforeDecision, failure;
            race_start_run_after_ready: StartRun, AfterSend, ready;
            race_start_run_after_failed: StartRun, AfterSend, failure;
            race_shutdown_before_ready: Shutdown, BeforeDecision, ready;
            race_shutdown_before_failed: Shutdown, BeforeDecision, failure;
            race_shutdown_after_ready: Shutdown, AfterSend, ready;
            race_shutdown_after_failed: Shutdown, AfterSend, failure;
            race_newer_request_before_ready: NewerRequest, BeforeDecision, ready;
            race_newer_request_before_failed: NewerRequest, BeforeDecision, failure;
            race_newer_request_after_ready: NewerRequest, AfterSend, ready;
            race_newer_request_after_failed: NewerRequest, AfterSend, failure;
        }

        // Every load attempt ends the offer, also one that fails.

        fn nothing_to_move(session: &Session, window: &Window) {
            window.take();
            assert_eq!(
                session.move_drop_to_batch(&mut window.emit()),
                Err(CommandError::NothingToMove)
            );
            assert!(lock(&session.list).rows(None).is_empty());
        }

        #[test]
        fn the_offer_is_gone_after_a_refused_choice() {
            let (_dir, paths, session, window) = with_an_offer();
            let refusing = Prober::new(ProbeAnswer::Refused("damaged".to_owned()));
            choose_image(&session, &window, Some(&paths[2]), &refusing);
            assert_eq!(refusing.asked().len(), 1, "the probe refused it");
            nothing_to_move(&session, &window);
        }

        #[test]
        fn the_offer_is_gone_after_a_non_image_choice() {
            let (dir, _paths, session, window) = with_an_offer();
            let text = dir.path().join("notes.txt");
            fs::write(&text, b"text").unwrap();
            choose_image(&session, &window, Some(&text), &Prober::sized());
            nothing_to_move(&session, &window);
        }

        #[test]
        fn the_offer_is_gone_after_a_text_only_drop() {
            let (dir, _paths, session, window) = with_an_offer();
            let text = dir.path().join("notes.txt");
            fs::write(&text, b"text").unwrap();
            drop_routed(&session, &window, &[text], &Prober::sized());
            nothing_to_move(&session, &window);
        }

        #[test]
        fn the_offer_is_gone_after_a_folder_only_drop() {
            let (dir, _paths, session, window) = with_an_offer();
            let folder = dir.path().join("more");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("d.png"), b"png").unwrap();
            drop_routed(
                &session,
                &window,
                std::slice::from_ref(&folder),
                &Prober::sized(),
            );
            window.take();
            // A folder-only drop leaves its folder out, so it is the offer
            // now (INTERFACE.md: a drop with `left_out` above 0 is kept); the
            // earlier drop's images are not.
            assert_eq!(session.move_drop_to_batch(&mut window.emit()), Ok(()));
            let rows = lock(&session.list).rows(None);
            assert_eq!(sources(&rows), [folder.join("d.png")]);
        }

        // Clearing the output folder lists the original's folder.

        #[test]
        fn clear_output_folder_suggests_the_first_free_name_next_to_the_original() {
            let (dir, paths) = files(&[("holiday.jpg", b"jpg"), ("optimized-holiday.jpg", b"jpg")]);
            let session = Session::new();
            let window = Window::default();
            choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
            choose_output(&session, &window, &dir.path().join("exports"));
            window.take();
            session.clear_output_folder(&mut window.emit()).unwrap();
            let events = window.take();
            let names: Vec<&str> = events.iter().map(Event::name).collect();
            assert_eq!(names, ["output-changed", "name-suggested"]);
            assert_eq!(
                value(&events[1]),
                json!({ "image": "1", "suggested": "optimized-holiday-1" })
            );
        }
    }

    // --- save_image -------------------------------------------------------------

    mod save {
        use std::ffi::OsString;

        use lunaepix_core::{Crash, Outcome, RunError, save_new};

        use super::*;
        use crate::names::save_names;

        /// The bytes every ready version in these tests has: not the copy's.
        const VERSION: &[u8] = b"the compressed version";

        /// The bytes of every original in these tests.
        const ORIGINAL: &[u8] = b"the original";

        fn ready_result() -> FileResult {
            FileResult::Optimized(Outcome {
                before: ORIGINAL.len() as u64,
                after: VERSION.len() as u64,
                millis: 5,
                note: None,
            })
        }

        fn failed_result() -> FileResult {
            FileResult::Failed(RunError::Crashed {
                path: PathBuf::new(),
                cause: Crash::NoValidResult,
                stderr: String::new(),
            })
        }

        /// A preview request on the current image, which must start: its job.
        fn request(session: &Session) -> PreviewJob {
            let (answer, job) = preview(session, &Window::default(), Some(&lossy(40)));
            answer.unwrap();
            job.expect("the preview started")
        }

        /// `job`'s version file holding `bytes`, settled ready: its key.
        fn settle_ready(session: &Session, job: &PreviewJob, bytes: &[u8]) -> String {
            fs::write(&job.dest, bytes).unwrap();
            session
                .single()
                .settle_preview(job, Some(ready_result()), &mut |_| {});
            format!("{}-{}", job.key, job.version)
        }

        /// A new version of the current image holding `bytes`, ready: its key.
        fn make_ready(session: &Session, bytes: &[u8]) -> String {
            let job = request(session);
            settle_ready(session, &job, bytes)
        }

        /// An image loaded with a ready version.
        struct Saving {
            dir: TempDir,
            original: PathBuf,
            session: Session,
            window: Window,
            /// The ready version's key, `"1-1"`.
            key: String,
        }

        /// `name` (holding [`ORIGINAL`]) in a new folder, loaded as image 1,
        /// with version 1 ready (holding [`VERSION`]); the window's events
        /// taken.
        fn ready_image(name: &str) -> Saving {
            let (dir, paths) = files(&[(name, ORIGINAL)]);
            let session = Session::new();
            let window = Window::default();
            choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
            let key = make_ready(&session, VERSION);
            assert_eq!(key, "1-1");
            window.take();
            Saving {
                dir,
                original: paths[0].clone(),
                session,
                window,
                key,
            }
        }

        /// [`ready_image`] of `holiday.jpg` with the output folder
        /// `<dir>/exports` chosen (and made): the folder.
        fn ready_with_exports() -> (Saving, PathBuf) {
            let s = ready_image("holiday.jpg");
            let out = s.dir.path().join("exports");
            fs::create_dir(&out).unwrap();
            choose_output(&s.session, &s.window, &out);
            s.window.take();
            (s, out)
        }

        fn args(name: &str, preview: &str) -> Value {
            json!({ "name": name, "preview": preview })
        }

        /// Save the ready version as `name`.
        fn save(s: &Saving, name: &str) -> Result<(), CommandError> {
            s.session
                .save_image(Some(&args(name, &s.key)), &mut s.window.emit())
        }

        /// [`save`] through the real `save_new`, with `seam`.
        fn save_with(
            s: &Saving,
            name: &str,
            seam: &mut dyn FnMut(SaveSeam),
        ) -> Result<(), CommandError> {
            s.session.save_image_with(
                Some(&args(name, &s.key)),
                &mut s.window.emit(),
                save_new,
                seam,
            )
        }

        /// The names in `folder`, sorted.
        fn entries(folder: &Path) -> Vec<OsString> {
            let mut names: Vec<OsString> = fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        }

        /// Every path under `folder`, and each file's bytes.
        fn snapshot(folder: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
            let mut all = Vec::new();
            let mut pending = vec![folder.to_path_buf()];
            while let Some(next) = pending.pop() {
                for entry in fs::read_dir(&next).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        pending.push(path.clone());
                        all.push((path, None));
                    } else {
                        let bytes = fs::read(&path).ok();
                        all.push((path, bytes));
                    }
                }
            }
            all.sort();
            all
        }

        /// The one event the window heard: an `image-saved`, as JSON.
        fn only_saved(window: &Window) -> Value {
            let events = window.take();
            assert_eq!(events.len(), 1, "{events:?}");
            assert_eq!(events[0].name(), "image-saved");
            value(&events[0])
        }

        fn last_output(session: &Session) -> Option<PathBuf> {
            lock(&session.last_output).clone()
        }

        /// A save with `args` refused with `error`: nothing heard but
        /// `run-busy` for busy, nothing changed in the folder, `last_output`
        /// as it was, the guard free afterwards (unless it is held by the
        /// test).
        fn refused_save(s: &Saving, args: Option<&Value>, error: CommandError) {
            let before = snapshot(s.dir.path());
            let output = last_output(&s.session);
            let answer = s.session.save_image(args, &mut s.window.emit());
            assert_eq!(answer, Err(error), "{args:?}");
            let heard: Vec<&str> = s.window.take().iter().map(Event::name).collect();
            let expected: Vec<&str> = if error == CommandError::Busy {
                vec!["run-busy"]
            } else {
                vec![]
            };
            assert_eq!(heard, expected, "{args:?}");
            assert_eq!(snapshot(s.dir.path()), before, "{args:?}");
            assert_eq!(last_output(&s.session), output);
            if error != CommandError::Busy {
                assert!(guard_is_free(&s.session));
            }
        }

        /// What `open_output_folder` opens.
        fn opened(session: &Session) -> Result<PathBuf, CommandError> {
            let mut got = None;
            session
                .open_output_folder(|folder| {
                    got = Some(folder.to_path_buf());
                    Ok::<(), io::Error>(())
                })
                .map(|()| got.expect("opened"))
        }

        /// Make `folder` read-only; `false` when running as root, where it
        /// stays writable and the test has nothing to check.
        #[cfg(unix)]
        fn read_only(folder: &Path) -> bool {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
            let probe = folder.join("probe");
            if fs::write(&probe, b"").is_ok() {
                fs::remove_file(probe).unwrap();
                writable(folder);
                eprintln!("skipped: the folder stays writable (running as root)");
                return false;
            }
            true
        }

        #[cfg(unix)]
        fn writable(folder: &Path) {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Writing.

        #[test]
        fn save_writes_exactly_the_shown_versions_bytes_under_name_plus_extension() {
            let s = ready_image("holiday.jpg");
            assert_eq!(save(&s, "beach"), Ok(()));
            let saved = fs::read(s.dir.path().join("beach.jpg")).unwrap();
            assert_eq!(saved, VERSION);
            assert_ne!(saved, ORIGINAL);
            assert_eq!(entries(s.dir.path()), ["beach.jpg", "holiday.jpg"]);
            assert_eq!(
                only_saved(&s.window),
                json!({ "name": "beach.jpg", "folder": null, "suggested": "beach-1" })
            );
            assert!(guard_is_free(&s.session));
        }

        #[test]
        fn the_extension_is_the_originals_as_spelled() {
            let s = ready_image("HOLIDAY.JPG");
            assert_eq!(save(&s, "beach"), Ok(()));
            assert_eq!(entries(s.dir.path()), ["HOLIDAY.JPG", "beach.JPG"]);
            assert_eq!(only_saved(&s.window)["name"], json!("beach.JPG"));
        }

        #[test]
        fn save_next_to_the_original_writes_in_the_originals_folder() {
            let s = ready_image("holiday.jpg");
            assert_eq!(save(&s, "optimized-holiday"), Ok(()));
            assert_eq!(
                entries(s.dir.path()),
                ["holiday.jpg", "optimized-holiday.jpg"]
            );
            assert_eq!(only_saved(&s.window)["folder"], Value::Null);
            assert_eq!(last_output(&s.session), Some(s.dir.path().to_path_buf()));
        }

        #[test]
        fn save_in_a_chosen_folder_writes_there_and_not_beside_the_original() {
            let (s, out) = ready_with_exports();
            assert_eq!(save(&s, "holiday"), Ok(()));
            assert_eq!(entries(&out), ["holiday.jpg"]);
            assert_eq!(fs::read(out.join("holiday.jpg")).unwrap(), VERSION);
            assert_eq!(entries(s.dir.path()), ["exports", "holiday.jpg"]);
            assert_eq!(fs::read(&s.original).unwrap(), ORIGINAL);
            assert_eq!(
                only_saved(&s.window),
                json!({ "name": "holiday.jpg", "folder": "exports", "suggested": "holiday-1" })
            );
            assert_eq!(last_output(&s.session), Some(out));
        }

        #[cfg(unix)]
        #[test]
        fn the_saved_file_gets_the_originals_permissions() {
            use std::os::unix::fs::PermissionsExt;
            for mode in [0o640, 0o600] {
                let (dir, paths) = files(&[("holiday.jpg", ORIGINAL)]);
                fs::set_permissions(&paths[0], fs::Permissions::from_mode(mode)).unwrap();
                let session = Session::new();
                choose_image(
                    &session,
                    &Window::default(),
                    Some(&paths[0]),
                    &Prober::sized(),
                );
                let key = make_ready(&session, VERSION);
                session
                    .save_image(Some(&args("beach", &key)), &mut |_| {})
                    .unwrap();
                let saved = fs::metadata(dir.path().join("beach.jpg")).unwrap();
                assert_eq!(saved.permissions().mode() & 0o777, mode);
            }
        }

        #[test]
        fn the_writer_gets_the_target_path_the_version_bytes_and_the_originals_permissions() {
            let (dir, paths) = files(&[("holiday.jpg", ORIGINAL)]);
            // A mode the version file does not have, so the two cannot be
            // mistaken for each other.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&paths[0], fs::Permissions::from_mode(0o640)).unwrap();
            }
            let session = Session::new();
            let window = Window::default();
            choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
            let job = request(&session);
            let key = settle_ready(&session, &job, VERSION);
            window.take();
            let calls = RefCell::new(Vec::new());
            let answer = session.save_image_with(
                Some(&args("beach", &key)),
                &mut window.emit(),
                |dest, bytes, permissions| {
                    calls
                        .borrow_mut()
                        .push((dest.to_path_buf(), bytes.to_vec(), permissions));
                    Ok(())
                },
                &mut |_| {},
            );
            assert_eq!(answer, Ok(()));
            let calls = calls.into_inner();
            assert_eq!(calls.len(), 1);
            let (dest, bytes, permissions) = &calls[0];
            assert_eq!(dest, &dir.path().join("beach.jpg"));
            assert_eq!(bytes, VERSION);
            assert_eq!(permissions, &fs::metadata(&paths[0]).unwrap().permissions());
            #[cfg(unix)]
            assert_ne!(permissions, &fs::metadata(&job.dest).unwrap().permissions());
        }

        // Never replacing.

        #[test]
        fn a_taken_name_is_name_taken_and_the_existing_file_is_unchanged() {
            let s = ready_image("holiday.jpg");
            let theirs = s.dir.path().join("beach.jpg");
            fs::write(&theirs, b"theirs").unwrap();
            refused_save(&s, Some(&args("beach", &s.key)), CommandError::NameTaken);
            assert_eq!(fs::read(&theirs).unwrap(), b"theirs");
            assert_eq!(last_output(&s.session), None);
        }

        #[test]
        fn a_name_taken_by_a_folder_is_name_taken() {
            let s = ready_image("holiday.jpg");
            let folder = s.dir.path().join("beach.jpg");
            fs::create_dir(&folder).unwrap();
            refused_save(&s, Some(&args("beach", &s.key)), CommandError::NameTaken);
            assert!(folder.is_dir());
            assert!(entries(&folder).is_empty());
        }

        #[test]
        fn a_name_taken_in_a_chosen_folder_is_name_taken() {
            let (s, out) = ready_with_exports();
            fs::write(out.join("holiday.jpg"), b"theirs").unwrap();
            refused_save(&s, Some(&args("holiday", &s.key)), CommandError::NameTaken);
            assert_eq!(fs::read(out.join("holiday.jpg")).unwrap(), b"theirs");
        }

        #[test]
        fn the_originals_own_name_is_name_taken_and_the_original_is_unchanged() {
            let s = ready_image("holiday.jpg");
            refused_save(&s, Some(&args("holiday", &s.key)), CommandError::NameTaken);
            assert_eq!(fs::read(&s.original).unwrap(), ORIGINAL);
        }

        #[test]
        fn a_file_appearing_just_before_the_write_is_name_taken_and_kept() {
            let s = ready_image("holiday.jpg");
            let late = s.dir.path().join("beach.jpg");
            let answer = save_with(&s, "beach", &mut |seam| {
                if seam == SaveSeam::BeforeWrite {
                    fs::write(&late, b"late").unwrap();
                }
            });
            assert_eq!(answer, Err(CommandError::NameTaken));
            assert_eq!(fs::read(&late).unwrap(), b"late");
            // No staging file is left behind.
            assert_eq!(entries(s.dir.path()), ["beach.jpg", "holiday.jpg"]);
            assert!(s.window.take().is_empty());
            assert_eq!(last_output(&s.session), None);
        }

        // Refusals.

        #[test]
        fn every_refused_fixture_name_is_invalid_name_and_writes_nothing() {
            let names = save_names::load();
            assert_eq!(names.extension, ".jpg");
            let s = ready_image("holiday.jpg");
            let kinds: Vec<&str> = names
                .refused
                .iter()
                .map(|(kind, _)| kind.as_str())
                .collect();
            assert_eq!(
                kinds,
                [
                    "Empty",
                    "ForbiddenCharacter",
                    "Reserved",
                    "TooLong",
                    "TrailingSpaceOrDot"
                ]
            );
            for (_, stems) in &names.refused {
                assert!(!stems.is_empty());
                for stem in stems {
                    refused_save(&s, Some(&args(stem, &s.key)), CommandError::InvalidName);
                }
            }
        }

        #[test]
        fn every_accepted_fixture_name_saves() {
            let names = save_names::load();
            assert_eq!(names.extension, ".jpg");
            let s = ready_image("holiday.jpg");
            assert!(!names.accepted.is_empty());
            for stem in &names.accepted {
                assert_eq!(save(&s, stem), Ok(()), "{stem:?}");
                let file = format!("{stem}.jpg");
                assert_eq!(fs::read(s.dir.path().join(&file)).unwrap(), VERSION);
                assert_eq!(only_saved(&s.window)["name"], json!(file));
            }
        }

        #[test]
        fn a_badly_shaped_argument_is_invalid_name() {
            let s = ready_image("holiday.jpg");
            let key = s.key.clone();
            refused_save(&s, None, CommandError::InvalidName);
            for shape in [
                Value::Null,
                json!("beach"),
                json!(["beach", key]),
                json!({}),
                json!({ "preview": key }),
                json!({ "name": "beach" }),
                json!({ "name": "beach", "preview": key, "folder": "exports" }),
                json!({ "Name": "beach", "preview": key }),
                json!({ "name": 7, "preview": key }),
                json!({ "name": null, "preview": key }),
                json!({ "name": ["beach"], "preview": key }),
                json!({ "name": "beach", "preview": 1 }),
                json!({ "name": "beach", "preview": null }),
            ] {
                refused_save(&s, Some(&shape), CommandError::InvalidName);
            }
            assert_eq!(save(&s, "beach"), Ok(()));
        }

        #[test]
        fn a_stale_or_foreign_preview_key_is_not_ready_older_version() {
            let s = ready_image("holiday.jpg");
            let newer = make_ready(&s.session, b"newer");
            assert_eq!(newer, "1-2");
            refused_save(&s, Some(&args("beach", "1-1")), CommandError::NotReady);
            assert_eq!(
                s.session
                    .save_image(Some(&args("beach", &newer)), &mut s.window.emit()),
                Ok(())
            );
            assert_eq!(fs::read(s.dir.path().join("beach.jpg")).unwrap(), b"newer");
        }

        #[test]
        fn a_stale_or_foreign_preview_key_is_not_ready_other_image() {
            let s = ready_image("holiday.jpg");
            refused_save(&s, Some(&args("beach", "2-1")), CommandError::NotReady);
            // A second image, with nothing ready: the first image's key is
            // foreign now.
            let other = s.dir.path().join("other.jpg");
            fs::write(&other, ORIGINAL).unwrap();
            choose_image(&s.session, &s.window, Some(&other), &Prober::sized());
            s.window.take();
            refused_save(&s, Some(&args("beach", "1-1")), CommandError::NotReady);
        }

        #[test]
        fn a_stale_or_foreign_preview_key_is_not_ready_not_the_key_text() {
            let s = ready_image("holiday.jpg");
            for text in [
                "1", "01-1", "1-01", "x", "", "1-1 ", " 1-1", "1-1-1", "1-", "-1",
            ] {
                refused_save(&s, Some(&args("beach", text)), CommandError::NotReady);
            }
            assert_eq!(save(&s, "beach"), Ok(()));
        }

        /// `holiday.jpg` loaded as image 1, nothing ready yet.
        fn loaded_only() -> Saving {
            let (dir, paths) = files(&[("holiday.jpg", ORIGINAL)]);
            let session = Session::new();
            let window = Window::default();
            choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
            window.take();
            Saving {
                dir,
                original: paths[0].clone(),
                session,
                window,
                key: "1-1".to_owned(),
            }
        }

        #[test]
        fn a_save_before_any_ready_version_is_not_ready_none_asked() {
            let s = loaded_only();
            refused_save(&s, Some(&args("beach", "1-1")), CommandError::NotReady);
        }

        #[test]
        fn a_save_before_any_ready_version_is_not_ready_one_running() {
            let s = loaded_only();
            let job = request(&s.session);
            // Its worker has even written the file; it is not settled.
            fs::write(&job.dest, VERSION).unwrap();
            refused_save(&s, Some(&args("beach", "1-1")), CommandError::NotReady);
        }

        #[test]
        fn a_save_before_any_ready_version_is_not_ready_one_failed() {
            let s = loaded_only();
            let job = request(&s.session);
            s.session
                .single()
                .settle_preview(&job, Some(failed_result()), &mut |_| {});
            refused_save(&s, Some(&args("beach", "1-1")), CommandError::NotReady);
        }

        #[test]
        fn a_failed_newer_version_keeps_the_older_ready_one_saveable() {
            let s = ready_image("holiday.jpg");
            let job = request(&s.session);
            s.session
                .single()
                .settle_preview(&job, Some(failed_result()), &mut |_| {});
            refused_save(&s, Some(&args("beach", "1-2")), CommandError::NotReady);
            assert_eq!(save(&s, "beach"), Ok(()));
            assert_eq!(fs::read(s.dir.path().join("beach.jpg")).unwrap(), VERSION);
        }

        #[test]
        fn a_save_without_an_image_is_no_image() {
            let session = Session::new();
            let window = Window::default();
            assert_eq!(
                session.save_image(Some(&args("beach", "1-1")), &mut window.emit()),
                Err(CommandError::NoImage)
            );
            assert!(window.take().is_empty());
            assert!(guard_is_free(&session));
        }

        #[test]
        fn a_save_after_shutdown_is_no_image() {
            let s = ready_image("holiday.jpg");
            s.session.shutdown();
            refused_save(&s, Some(&args("beach", &s.key)), CommandError::NoImage);
        }

        #[test]
        fn a_deleted_chosen_folder_is_output_missing() {
            let (s, out) = ready_with_exports();
            fs::remove_dir(&out).unwrap();
            refused_save(
                &s,
                Some(&args("holiday", &s.key)),
                CommandError::OutputMissing,
            );
            assert!(!out.exists(), "the folder is not made again");
        }

        #[test]
        fn a_deleted_original_folder_is_output_missing() {
            let dir = TempDir::new().unwrap();
            let photos = dir.path().join("photos");
            fs::create_dir(&photos).unwrap();
            let original = photos.join("holiday.jpg");
            fs::write(&original, ORIGINAL).unwrap();
            let session = Session::new();
            let window = Window::default();
            choose_image(&session, &window, Some(&original), &Prober::sized());
            let key = make_ready(&session, VERSION);
            window.take();
            fs::remove_dir_all(&photos).unwrap();
            let s = Saving {
                dir,
                original,
                session,
                window,
                key,
            };
            refused_save(
                &s,
                Some(&args("beach", &s.key)),
                CommandError::OutputMissing,
            );
            assert!(!photos.exists(), "the folder is not made again");
        }

        #[test]
        fn a_chosen_folder_replaced_by_a_file_is_output_missing() {
            let (s, out) = ready_with_exports();
            fs::remove_dir(&out).unwrap();
            fs::write(&out, b"a file").unwrap();
            refused_save(
                &s,
                Some(&args("holiday", &s.key)),
                CommandError::OutputMissing,
            );
            assert_eq!(fs::read(&out).unwrap(), b"a file");
        }

        #[cfg(unix)]
        #[test]
        fn a_read_only_chosen_folder_is_output_missing() {
            let (s, out) = ready_with_exports();
            if !read_only(&out) {
                return;
            }
            refused_save(
                &s,
                Some(&args("holiday", &s.key)),
                CommandError::OutputMissing,
            );
            writable(&out);
            assert!(entries(&out).is_empty());
        }

        #[test]
        fn the_order_of_checks_is_busy_shape_image_preview_name_then_folder() {
            let (dir, paths) = files(&[("holiday.jpg", ORIGINAL)]);
            let out = dir.path().join("exports");
            fs::create_dir(&out).unwrap();
            let session = Session::new();
            let window = Window::default();
            choose_output(&session, &window, &out);
            fs::remove_dir(&out).unwrap();
            window.take();
            let ask = |args: &Value| session.save_image(Some(args), &mut |_| {});
            let bad_shape = json!({ "name": 7, "preview": "1-1" });
            let bad_name = args("a/b", "1-1");
            // Busy before a bad shape.
            let held = session.guard.try_start().expect("a free guard");
            assert_eq!(ask(&bad_shape), Err(CommandError::Busy));
            drop(held);
            // A bad shape before no image.
            assert_eq!(ask(&bad_shape), Err(CommandError::InvalidName));
            // No image before no ready version and a bad name.
            assert_eq!(ask(&bad_name), Err(CommandError::NoImage));
            choose_image(&session, &window, Some(&paths[0]), &Prober::sized());
            // No ready version before a bad name.
            assert_eq!(ask(&bad_name), Err(CommandError::NotReady));
            let key = make_ready(&session, VERSION);
            assert_eq!(key, "1-1");
            // A bad name before a missing folder.
            assert_eq!(ask(&bad_name), Err(CommandError::InvalidName));
            // The missing folder.
            assert_eq!(ask(&args("beach", "1-1")), Err(CommandError::OutputMissing));
            fs::create_dir(&out).unwrap();
            assert_eq!(ask(&args("beach", "1-1")), Ok(()));
            assert_eq!(fs::read(out.join("beach.jpg")).unwrap(), VERSION);
        }

        // Failures.

        /// The writer here fails without touching the disk, so this test
        /// claims only the answer and that nothing is sent or stored. That
        /// `save_new` removes its staging file on a failure is core's
        /// `save_new_leaves_no_temporary_file_after_a_refusal` and
        /// `save_new_returns_a_rename_error_that_is_not_a_taken_name_and_leaves_no_temp_file`;
        /// the real staging case is
        /// `a_folder_made_read_only_before_the_write_is_save_failed_and_leaves_no_file`.
        #[test]
        fn a_failing_write_is_save_failed_and_sends_nothing() {
            let s = ready_image("holiday.jpg");
            let before = snapshot(s.dir.path());
            let answer = s.session.save_image_with(
                Some(&args("beach", &s.key)),
                &mut s.window.emit(),
                |_, _, _| Err(io::Error::other("no space left")),
                &mut |_| {},
            );
            assert_eq!(answer, Err(CommandError::SaveFailed));
            assert!(s.window.take().is_empty());
            assert_eq!(snapshot(s.dir.path()), before);
            assert_eq!(last_output(&s.session), None);
            assert!(guard_is_free(&s.session));
        }

        #[cfg(unix)]
        #[test]
        fn a_folder_made_read_only_before_the_write_is_save_failed_and_leaves_no_file() {
            let (s, out) = ready_with_exports();
            let mut locked = true;
            let answer = save_with(&s, "holiday", &mut |seam| {
                if seam == SaveSeam::BeforeWrite {
                    locked = read_only(&out);
                }
            });
            if !locked {
                return;
            }
            writable(&out);
            assert_eq!(answer, Err(CommandError::SaveFailed));
            assert!(entries(&out).is_empty());
            assert!(s.window.take().is_empty());
            assert_eq!(last_output(&s.session), None);
        }

        #[test]
        fn a_folder_removed_before_the_write_is_output_missing() {
            let (s, out) = ready_with_exports();
            let answer = save_with(&s, "holiday", &mut |seam| {
                if seam == SaveSeam::BeforeWrite {
                    fs::remove_dir(&out).unwrap();
                }
            });
            assert_eq!(answer, Err(CommandError::OutputMissing));
            assert!(!out.exists(), "the folder is not made again");
            assert!(s.window.take().is_empty());
            assert_eq!(last_output(&s.session), None);
        }

        #[test]
        fn an_unreadable_version_is_save_failed() {
            let s = ready_image("holiday.jpg");
            let version = s.session.single().served_file(1, Some(1)).unwrap();
            let answer = save_with(&s, "beach", &mut |seam| {
                if seam == SaveSeam::AfterCheck {
                    fs::remove_file(&version).unwrap();
                    fs::create_dir(&version).unwrap();
                }
            });
            assert_eq!(answer, Err(CommandError::SaveFailed));
            assert_eq!(entries(s.dir.path()), ["holiday.jpg"]);
            assert!(s.window.take().is_empty());
        }

        #[test]
        fn a_save_during_a_run_is_busy() {
            let s = ready_image("holiday.jpg");
            pick(&s.session, &s.window, std::slice::from_ref(&s.original));
            s.window.take();
            let running = started(&s.session, &s.window, &lossless());
            s.window.take();
            refused_save(&s, Some(&args("beach", &s.key)), CommandError::Busy);
            drop(running);
            assert_eq!(save(&s, "beach"), Ok(()));
        }

        #[test]
        fn a_save_while_a_dialog_is_open_is_busy() {
            let s = ready_image("holiday.jpg");
            let mut open = None;
            s.session
                .choose_image(&mut s.window.emit(), |ticket| open = Some(ticket))
                .unwrap();
            refused_save(&s, Some(&args("beach", &s.key)), CommandError::Busy);
            drop(open);
            assert_eq!(save(&s, "beach"), Ok(()));
        }

        #[test]
        fn a_save_while_an_image_is_loading_is_busy() {
            let s = ready_image("holiday.jpg");
            let other = s.dir.path().join("other.jpg");
            fs::write(&other, ORIGINAL).unwrap();
            let loading = Window::default();
            let asked = Cell::new(false);
            s.session
                .choose_image(&mut loading.emit(), |ticket| {
                    s.session.load_image(
                        ticket,
                        Some(other.clone()),
                        |_| {
                            // The probe runs while the load holds the guard.
                            asked.set(true);
                            refused_save(&s, Some(&args("beach", &s.key)), CommandError::Busy);
                            ProbeAnswer::Size(Probe {
                                width: Some(1),
                                height: Some(1),
                            })
                        },
                        &mut loading.emit(),
                    );
                })
                .unwrap();
            assert!(asked.get(), "the probe ran");
            // Once the load is over the guard is free: the new image has no
            // ready version yet, so the save is refused for that, not busy.
            refused_save(&s, Some(&args("beach", &s.key)), CommandError::NotReady);
        }

        // The ticket is held for the whole save.

        #[derive(Debug, Clone, Copy)]
        enum Call {
            Choice,
            Drop,
            SetView,
            Preview,
            StartRun,
            ClearOutput,
            ChooseOutput,
        }

        /// `call` while the save holds the ticket: it is refused as busy,
        /// tells the window once and starts nothing.
        fn compete(s: &Saving, call: Call) {
            let other = Window::default();
            let went_ahead = Cell::new(false);
            let answer = match call {
                Call::Choice => s
                    .session
                    .choose_image(&mut other.emit(), |_| went_ahead.set(true)),
                Call::Drop => {
                    s.session
                        .drop_paths(vec![s.original.clone()], &mut other.emit(), |_, _| {
                            went_ahead.set(true)
                        });
                    Err(CommandError::Busy)
                }
                Call::SetView => s
                    .session
                    .set_view(Some(&json!({ "view": "batch" })), &mut other.emit()),
                Call::Preview => {
                    s.session
                        .preview_image(Some(&lossy(60)), &mut other.emit(), |_| {
                            went_ahead.set(true)
                        })
                }
                Call::StartRun => s
                    .session
                    .start_run(Some(&lossless()), &mut other.emit(), |_| {
                        went_ahead.set(true)
                    }),
                Call::ClearOutput => s.session.clear_output_folder(&mut other.emit()),
                Call::ChooseOutput => s
                    .session
                    .choose_output_folder(&mut other.emit(), |_| went_ahead.set(true)),
            };
            assert_eq!(answer, Err(CommandError::Busy), "{call:?}");
            assert_eq!(other.take(), [Event::RunBusy], "{call:?}");
            assert!(!went_ahead.get(), "{call:?}");
        }

        /// `holiday.jpg` with `exports` chosen, saved as `holiday` while
        /// `call` lands at `at`: the call is busy, the save goes on.
        fn ticket_race(call: Call, at: SaveSeam) {
            let (s, out) = ready_with_exports();
            pick(&s.session, &s.window, std::slice::from_ref(&s.original));
            s.window.take();
            let reached = Cell::new(false);
            let answer = save_with(&s, "holiday", &mut |seam| {
                if seam == at {
                    reached.set(true);
                    compete(&s, call);
                }
            });
            assert!(reached.get(), "the seam was reached");
            assert_eq!(answer, Ok(()));
            assert_eq!(fs::read(out.join("holiday.jpg")).unwrap(), VERSION);
            assert_eq!(only_saved(&s.window)["folder"], json!("exports"));
            // Nothing the competing call would have changed did change.
            assert_eq!(s.session.view(), View::Single);
            assert_eq!(lock(&s.session.output).as_deref(), Some(out.as_path()));
            assert_eq!(loaded(&s.session).key, 1);
            assert_eq!(s.session.single().latest_ready(), Some((1, 1)));
        }

        macro_rules! ticket_races {
            ($($name:ident: $call:ident, $seam:ident;)*) => {
                $(
                    #[test]
                    fn $name() {
                        ticket_race(Call::$call, SaveSeam::$seam);
                    }
                )*
            };
        }

        ticket_races! {
            race_choice_after_check_is_busy: Choice, AfterCheck;
            race_choice_before_write_is_busy: Choice, BeforeWrite;
            race_drop_after_check_is_busy: Drop, AfterCheck;
            race_drop_before_write_is_busy: Drop, BeforeWrite;
            race_set_view_after_check_is_busy: SetView, AfterCheck;
            race_set_view_before_write_is_busy: SetView, BeforeWrite;
            race_preview_after_check_is_busy: Preview, AfterCheck;
            race_preview_before_write_is_busy: Preview, BeforeWrite;
            race_start_run_after_check_is_busy: StartRun, AfterCheck;
            race_start_run_before_write_is_busy: StartRun, BeforeWrite;
            race_clear_output_folder_after_check_is_busy: ClearOutput, AfterCheck;
            race_clear_output_folder_before_write_is_busy: ClearOutput, BeforeWrite;
            race_choose_output_folder_after_check_is_busy: ChooseOutput, AfterCheck;
            race_choose_output_folder_before_write_is_busy: ChooseOutput, BeforeWrite;
        }

        // A newer version or shutdown between the decision and the write.

        /// Version 1 ready, version 2 asked for (its worker running) before
        /// the save of `"1-1"` as `beach` starts; at `at` version 2 settles
        /// ready, which deletes version 1's file. The save's answer.
        fn newer_ready_at(s: &Saving, at: SaveSeam) -> Result<(), CommandError> {
            let newer = request(&s.session);
            let v1 = s.session.single().served_file(1, Some(1)).unwrap();
            let answer = save_with(s, "beach", &mut |seam| {
                if seam == at {
                    assert_eq!(settle_ready(&s.session, &newer, b"newer"), "1-2");
                    assert!(!v1.exists(), "the newer version removed version 1");
                }
            });
            assert_eq!(s.session.single().latest_ready(), Some((1, 2)));
            answer
        }

        #[test]
        fn race_newer_ready_after_check_is_not_ready_and_writes_nothing() {
            let s = ready_image("holiday.jpg");
            assert_eq!(
                newer_ready_at(&s, SaveSeam::AfterCheck),
                Err(CommandError::NotReady)
            );
            assert_eq!(entries(s.dir.path()), ["holiday.jpg"]);
            assert!(s.window.take().is_empty());
            assert_eq!(last_output(&s.session), None);
        }

        #[test]
        fn race_newer_ready_before_write_saves_the_named_versions_bytes() {
            let s = ready_image("holiday.jpg");
            assert_eq!(newer_ready_at(&s, SaveSeam::BeforeWrite), Ok(()));
            let saved = fs::read(s.dir.path().join("beach.jpg")).unwrap();
            assert_eq!(saved, VERSION);
            assert_ne!(saved, b"newer");
            assert_eq!(only_saved(&s.window)["name"], json!("beach.jpg"));
        }

        #[test]
        fn race_shutdown_after_check_is_not_ready_and_writes_nothing() {
            let s = ready_image("holiday.jpg");
            let answer = save_with(&s, "beach", &mut |seam| {
                if seam == SaveSeam::AfterCheck {
                    s.session.shutdown();
                }
            });
            assert_eq!(answer, Err(CommandError::NotReady));
            assert_eq!(entries(s.dir.path()), ["holiday.jpg"]);
            assert!(s.window.take().is_empty());
        }

        /// `Session::shutdown` (what the window closing calls) neither waits
        /// for nor stops a save that has read its bytes: while the process
        /// runs on, the write completes. This does not cover the process
        /// exiting during the write; then the file may be missing and a
        /// staging file may be left in the folder (see `save_image`).
        #[test]
        fn race_session_shutdown_before_write_does_not_stop_the_write() {
            let s = ready_image("holiday.jpg");
            let private = loaded(&s.session).copy.parent().unwrap().to_path_buf();
            let answer = save_with(&s, "beach", &mut |seam| {
                if seam == SaveSeam::BeforeWrite {
                    s.session.shutdown();
                }
            });
            assert!(!private.exists(), "shutdown removed the private folder");
            assert_eq!(answer, Ok(()));
            assert_eq!(fs::read(s.dir.path().join("beach.jpg")).unwrap(), VERSION);
        }

        #[test]
        fn image_saved_names_a_chosen_folder_without_a_name_by_an_empty_string() {
            // A folder path ending in `..` has no file name, as a drive root
            // such as `D:\` has none; it is still a writable folder.
            let s = ready_image("holiday.jpg");
            let exports = s.dir.path().join("exports");
            fs::create_dir(&exports).unwrap();
            choose_output(&s.session, &s.window, &exports.join(".."));
            s.window.take();
            assert_eq!(save(&s, "beach"), Ok(()));
            assert_eq!(
                only_saved(&s.window),
                json!({ "name": "beach.jpg", "folder": "", "suggested": "beach-1" })
            );
            assert_eq!(fs::read(s.dir.path().join("beach.jpg")).unwrap(), VERSION);
        }

        // After saving.

        #[test]
        fn image_saved_gives_the_next_free_name_after_the_suggested_one() {
            let s = ready_image("holiday.jpg");
            let suggested = loaded(&s.session).info(None).suggested;
            assert_eq!(suggested, "optimized-holiday");
            assert_eq!(save(&s, &suggested), Ok(()));
            let saved = only_saved(&s.window);
            assert_eq!(saved["suggested"], json!("optimized-holiday-1"));
            assert_ne!(saved["suggested"], json!(suggested));
        }

        #[test]
        fn image_saved_gives_the_next_free_name_after_a_typed_one() {
            let s = ready_image("holiday.jpg");
            assert_eq!(save(&s, "beach"), Ok(()));
            let saved = only_saved(&s.window);
            assert_eq!(saved["suggested"], json!("beach-1"));
            assert_ne!(saved["suggested"], json!("beach"));
        }

        #[test]
        fn image_saved_skips_a_taken_number() {
            let s = ready_image("holiday.jpg");
            fs::write(s.dir.path().join("beach-1.jpg"), b"theirs").unwrap();
            assert_eq!(save(&s, "beach"), Ok(()));
            assert_eq!(only_saved(&s.window)["suggested"], json!("beach-2"));
            // Pressing Save again with it works; the next one numbers that
            // name, as Batch numbers a taken output name.
            assert_eq!(save(&s, "beach-2"), Ok(()));
            assert_eq!(only_saved(&s.window)["suggested"], json!("beach-2-1"));
        }

        #[test]
        fn open_output_folder_opens_the_folder_of_the_last_save() {
            let s = ready_image("holiday.jpg");
            assert_eq!(save(&s, "beach"), Ok(()));
            assert_eq!(opened(&s.session), Ok(s.dir.path().to_path_buf()));
            let out = s.dir.path().join("exports");
            fs::create_dir(&out).unwrap();
            choose_output(&s.session, &s.window, &out);
            assert_eq!(save(&s, "beach"), Ok(()));
            assert_eq!(opened(&s.session), Ok(out));
        }

        #[test]
        fn a_refused_save_leaves_open_output_folder_as_it_was() {
            let s = ready_image("holiday.jpg");
            assert_eq!(opened(&s.session), Err(CommandError::NoOutput));
            refused_save(&s, Some(&args("holiday", &s.key)), CommandError::NameTaken);
            assert_eq!(opened(&s.session), Err(CommandError::NoOutput));
            assert_eq!(save(&s, "beach"), Ok(()));
            let out = s.dir.path().join("exports");
            fs::create_dir(&out).unwrap();
            choose_output(&s.session, &s.window, &out);
            s.window.take();
            refused_save(&s, Some(&args("a/b", &s.key)), CommandError::InvalidName);
            assert_eq!(opened(&s.session), Ok(s.dir.path().to_path_buf()));
        }

        /// The list holds the original; a run into `runs` (chosen first).
        fn run_into(s: &Saving, runs: &Path) {
            fs::create_dir_all(runs).unwrap();
            choose_output(&s.session, &s.window, runs);
            pick(&s.session, &s.window, std::slice::from_ref(&s.original));
            let prepared = started(&s.session, &s.window, &lossless());
            run_fake(&s.session, &s.window, prepared);
            s.window.take();
        }

        #[test]
        fn a_run_after_a_save_wins() {
            let (s, out) = ready_with_exports();
            assert_eq!(save(&s, "holiday"), Ok(()));
            assert_eq!(opened(&s.session), Ok(out));
            let runs = s.dir.path().join("runs");
            run_into(&s, &runs);
            assert_eq!(opened(&s.session), Ok(runs));
        }

        #[test]
        fn a_save_after_a_run_wins() {
            let s = ready_image("holiday.jpg");
            let runs = s.dir.path().join("runs");
            run_into(&s, &runs);
            assert_eq!(opened(&s.session), Ok(runs));
            let out = s.dir.path().join("exports");
            fs::create_dir(&out).unwrap();
            choose_output(&s.session, &s.window, &out);
            assert_eq!(save(&s, "holiday"), Ok(()));
            assert_eq!(opened(&s.session), Ok(out));
        }

        // Names are saved exactly as typed.

        #[test]
        fn a_name_with_e_acute_saves_under_exactly_that_name() {
            let s = ready_image("holiday.jpg");
            assert_eq!(save(&s, "vakantie-caf\u{e9}"), Ok(()));
            assert_eq!(
                entries(s.dir.path()),
                [
                    OsString::from("holiday.jpg"),
                    OsString::from("vakantie-caf\u{e9}.jpg")
                ]
            );
            assert!(!entries(s.dir.path()).contains(&OsString::from("vakantie-cafe\u{301}.jpg")));
            assert_eq!(
                only_saved(&s.window)["name"],
                json!("vakantie-caf\u{e9}.jpg")
            );
        }

        #[test]
        fn a_decomposed_e_acute_saves_under_exactly_those_bytes() {
            let s = ready_image("holiday.jpg");
            assert_eq!(save(&s, "vakantie-cafe\u{301}"), Ok(()));
            assert_eq!(
                entries(s.dir.path()),
                [
                    OsString::from("holiday.jpg"),
                    OsString::from("vakantie-cafe\u{301}.jpg")
                ]
            );
            assert!(!entries(s.dir.path()).contains(&OsString::from("vakantie-caf\u{e9}.jpg")));
        }
    }
}
