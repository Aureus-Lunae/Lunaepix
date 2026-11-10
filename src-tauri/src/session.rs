//! The backend's state and what each command does with it: the file list,
//! the output setting, the busy guard, the run itself with its events, and
//! the output folder a run leaves behind. No Tauri here: the side effects
//! (the window's events, the dialogs, the run thread, the system opener, the
//! runner) come in as closures, so all of it is tested without a window,
//! failures included.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lunaepix_core::{
    Compression, FileResult, Job, Options, Quality, Skip, StopHandle, Worker, WriteMode,
    images_in_folder, plan_files_to,
};
use serde::Serialize;
use serde_json::Value;

use crate::events::{
    Event, FileStarted, ListChanged, ListRow, OVER_LIMIT_NAME, RunFinished, RunStarted, file_done,
    file_name_of, output_changed, over_limit_reason, over_limit_row,
};

/// Why a command did nothing. Serialised as a bare string: `"busy"`,
/// `"no-output"`, `"open-failed"`, `"invalid-settings"`, `"empty-list"` or
/// `"output-missing"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandError {
    /// A run (or a dialog for one) is already active.
    Busy,
    /// No completed run has written output yet.
    NoOutput,
    /// The system could not open the folder (for example, it was removed).
    OpenFailed,
    /// `start_run` was given settings that are not exactly
    /// `{ "mode": "lossless" | "lossy", "quality": <integer 0 to 100> }`.
    InvalidSettings,
    /// No file in the list can be compressed.
    EmptyList,
    /// The chosen output folder is gone or cannot be written to.
    OutputMissing,
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
/// can be read is replaced by its images.
#[derive(Debug, Default)]
pub struct FileList {
    entries: Vec<PathBuf>,
    /// Canonical paths of the entries, so that a file is listed once.
    seen: HashSet<PathBuf>,
    /// A run over this list has finished: the next add starts a fresh list.
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

    /// The list planned now, into `out` or next to the originals: one job
    /// per row, so a file deleted since it was added is a skip.
    pub fn rows(&self, out: Option<&Path>) -> Vec<Job> {
        plan_files_to(&self.entries, out)
    }

    /// Whether a run over the list would compress at least one file.
    pub fn has_work(&self, out: Option<&Path>) -> bool {
        has_optimize(&self.rows(out))
    }

    /// A run over the list has ended: the next add first empties the list.
    pub fn mark_finished(&mut self) {
        self.finished = true;
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
/// as in the run ([`over_limit_row`]).
pub fn list_rows(jobs: &[Job], size_of: impl Fn(&Path) -> Option<u64>) -> Vec<ListRow> {
    let (kept, left_out) = without_over_limit(jobs);
    let mut rows: Vec<ListRow> = kept
        .iter()
        .map(|job| match job {
            Job::Optimize { src, .. } => ListRow {
                name: file_name_of(src),
                size: size_of(src),
                skip: None,
            },
            Job::Skip { src, reason } => ListRow {
                name: file_name_of(src),
                size: None,
                skip: Some(reason.to_string()),
            },
        })
        .collect();
    if left_out > 0 {
        rows.push(ListRow {
            name: OVER_LIMIT_NAME.to_owned(),
            size: None,
            skip: Some(over_limit_reason(left_out)),
        });
    }
    rows
}

/// Whether a file can be created in `dir` now: one is created there under a
/// fresh random name and removed again. Fails when `dir` is gone, is not a
/// folder or cannot be written to.
fn can_write_in(dir: &Path) -> std::io::Result<()> {
    tempfile::Builder::new()
        .prefix(".lunaepix-check-")
        .tempfile_in(dir)
        .map(drop)
}

/// A run that passed every check of `start_run`, holding its ticket.
#[derive(Debug)]
pub struct PreparedRun {
    ticket: RunTicket,
    jobs: Vec<Job>,
    compression: Compression,
    out: Option<PathBuf>,
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
            // A chosen output folder is never replaced into: a taken name
            // gets a number. Next to the originals, an earlier
            // `optimized-<name>` is replaced, as always.
            write: if self.out.is_some() {
                WriteMode::NoReplace
            } else {
                WriteMode::Replace
            },
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
    /// Output folder of the last completed run, if it wrote anything.
    last_output: Mutex<Option<PathBuf>>,
}

/// `mutex`'s value, also after a panic elsewhere poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
    pub fn new() -> Self {
        Self::default()
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

    /// A file dialog's answer, or a drop: add the paths and send the list.
    /// `None` or no paths (a closed dialog) gives the ticket back and then
    /// sends `run-cancelled` instead. Otherwise the ticket is given back
    /// after `list-changed` has gone out.
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
        let notice = lock(&self.list).add(&paths);
        emit(self.list_changed(notice));
        drop(ticket);
    }

    /// The `list-changed` event for the list as it is now, planned against
    /// the output setting. The two locks are taken one after the other,
    /// never together.
    fn list_changed(&self, notice: Option<String>) -> Event {
        let out = lock(&self.output).clone();
        let jobs = lock(&self.list).rows(out.as_deref());
        Event::ListChanged(ListChanged {
            files: list_rows(&jobs, file_size),
            notice,
        })
    }

    /// A drop on the window: nothing for no paths; `run-busy` while busy;
    /// otherwise `add_later` gets the ticket and the paths, to call
    /// [`Session::add_paths`] off the main thread.
    pub fn drop_paths(
        &self,
        paths: Vec<PathBuf>,
        emit: &mut impl FnMut(Event),
        add_later: impl FnOnce(RunTicket, Vec<PathBuf>),
    ) {
        if paths.is_empty() {
            return;
        }
        if let Ok(ticket) = claim_run(&self.guard, emit) {
            add_later(ticket, paths);
        }
    }

    /// `clear_list`.
    pub fn clear_list(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError> {
        let ticket = claim_run(&self.guard, emit)?;
        lock(&self.list).clear();
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
    /// name; or, for a closed dialog, `run-cancelled` with the setting left
    /// as it was.
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
        drop(ticket);
    }

    /// `clear_output_folder`: back to next to the originals.
    pub fn clear_output_folder(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError> {
        let ticket = claim_run(&self.guard, emit)?;
        *lock(&self.output) = None;
        emit(Event::OutputChanged(output_changed(None)));
        drop(ticket);
        Ok(())
    }

    /// `start_run`: busy check, `settings`, the output folder, the list
    /// planned now, then at least one compressible row, in that order. On
    /// success `start` gets the run, to hand to [`Session::run_with_worker`]
    /// on the run thread.
    pub fn start_run(
        &self,
        settings: &Value,
        emit: &mut impl FnMut(Event),
        start: impl FnOnce(PreparedRun),
    ) -> Result<(), CommandError> {
        // Every early return drops the ticket, which frees the guard.
        let ticket = claim_run(&self.guard, emit)?;
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
        start(PreparedRun {
            ticket,
            jobs,
            compression,
            out,
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
            ..
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
            ..
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

    /// `open_output_folder`: the last completed run's output folder, which
    /// is the chosen output folder when the run had one.
    pub fn open_output_folder<E>(
        &self,
        open: impl FnOnce(&Path) -> Result<(), E>,
    ) -> Result<(), CommandError> {
        open_last_output(&self.last_output, open)
    }
}

/// Allows one run at a time. A run holds a [`RunTicket`] from the moment it
/// is asked for (before a dialog opens) until it is over.
#[derive(Debug, Clone, Default)]
pub struct RunGuard(Arc<AtomicBool>);

/// Proof that the holder may run. Dropping it frees the guard, also when the
/// run panics.
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

/// A ticket for a new run; or, while a run is active, `run-busy` to the
/// window and [`CommandError::Busy`] to the caller.
pub fn claim_run(
    guard: &RunGuard,
    emit: &mut impl FnMut(Event),
) -> Result<RunTicket, CommandError> {
    guard.try_start().ok_or_else(|| {
        emit(Event::RunBusy);
        CommandError::Busy
    })
}

/// Open the output folder of the last completed run with `open` (the system
/// file manager). [`CommandError::NoOutput`] without calling `open` if there
/// is none; [`CommandError::OpenFailed`] if `open` fails. The lock is not
/// held while `open` runs.
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
/// one source of both `has_output` and the folder "Open folder" opens.
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
        ];
        let json: Vec<String> = errors
            .iter()
            .map(|error| serde_json::to_string(error).unwrap())
            .collect();
        assert_eq!(json, [r#""busy""#, r#""no-output""#, r#""open-failed""#]);
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

    // --- the window closes during a run -------------------------------------

    #[cfg(target_os = "linux")]
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
            !Path::new("/proc").join(pid).exists(),
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
        let answer = session.start_run(settings, &mut window.emit(), |run| prepared = Some(run));
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
                { "name": "holiday.jpg", "size": 6, "skip": null },
                { "name": "logo.png", "size": 3, "skip": null },
                { "name": "icon.svg", "size": 6, "skip": null }
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
                { "name": "locked", "size": null, "skip": "Not a readable file" }
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
                { "name": "notes.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file" },
                { "name": "missing.png", "size": null, "skip": "Not a readable file" }
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
        let rows = list_rows(&jobs, |path| {
            asked.borrow_mut().push(path.to_path_buf());
            None
        });
        assert_eq!(
            rows,
            [ListRow {
                name: "a.png".to_owned(),
                size: None,
                skip: None,
            }]
        );
        assert_eq!(*asked.borrow(), [PathBuf::from("/srv/a.png")]);
        assert_eq!(list_rows(&jobs, |_| Some(7))[0].size, Some(7));
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
            answers.push(session.start_run(&lossless(), &mut busy.emit(), |_| called.set(true)));
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
        assert_eq!(
            prepared.options(),
            Options {
                compression: Compression::Lossy(Quality::new(40).unwrap()),
                write: WriteMode::NoReplace,
            }
        );
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
    fn the_worker_gets_neither_flag_for_lossless_next_to_the_originals() {
        let (dir, paths) = files(&[("a.png", b"png")]);
        let logs = TempDir::new().unwrap();
        let log = logs.path().join("args");
        let session = Session::new();
        let window = Window::default();
        pick(&session, &window, &paths);
        let prepared = started(&session, &window, &lossless());
        assert_eq!(
            prepared.options(),
            Options {
                compression: Compression::Lossless,
                write: WriteMode::Replace,
            }
        );
        session.run_with_worker(
            prepared,
            &recording_worker(&log),
            &StopHandle::new(),
            &mut window.emit(),
        );
        let args = fs::read_to_string(&log).unwrap();
        let expected = format!(
            "{}\n{}\n",
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
}
