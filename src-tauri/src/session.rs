//! One run at a time: the busy guard, what a pick or a drop turns into, the
//! run itself with its events, the output folder it leaves behind, and what
//! each command gives back. No Tauri here: the side effects (the window's
//! events, the system opener, the runner) come in as closures, so all of it
//! is tested without a window, failures included.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lunaepix_core::{FileResult, Job, Skip, StopHandle, Worker};
use serde::Serialize;

use crate::events::{Event, RunFinished, RunStarted, file_done, over_limit_row};

/// Why a command did nothing. Serialised as a bare string: `"busy"`,
/// `"no-output"` or `"open-failed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandError {
    /// A run (or a dialog for one) is already active.
    Busy,
    /// No completed run has written output yet.
    NoOutput,
    /// The system could not open the folder (for example, it was removed).
    OpenFailed,
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

/// The jobs for what a dialog or a drop gave: `None` (a cancelled dialog) or
/// an empty list plans nothing and runs nothing; otherwise `plan` is called
/// once with exactly those paths.
pub fn plan_pick(
    picked: Option<Vec<PathBuf>>,
    plan: impl FnOnce(&[PathBuf]) -> Vec<Job>,
) -> Option<Vec<Job>> {
    let picked = picked.filter(|paths| !paths.is_empty())?;
    Some(plan(&picked))
}

/// What a dialog's answer leads to. A cancelled or empty answer frees the
/// ticket and then sends `run-cancelled`, running nothing. Otherwise `run`
/// gets the ticket and the planned jobs, and no `run-cancelled` goes out.
pub fn answer_dialog(
    ticket: RunTicket,
    picked: Option<Vec<PathBuf>>,
    plan: impl FnOnce(&[PathBuf]) -> Vec<Job>,
    emit: &mut impl FnMut(Event),
    run: impl FnOnce(RunTicket, Vec<Job>),
) {
    match plan_pick(picked, plan) {
        Some(jobs) => run(ticket, jobs),
        None => {
            drop(ticket);
            emit(Event::RunCancelled);
        }
    }
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
    worker: &Worker,
    stop: &StopHandle,
    last_output: &Mutex<Option<PathBuf>>,
    emit: &mut impl FnMut(Event),
) {
    run_session_with(ticket, jobs, last_output, emit, |jobs, progress| {
        lunaepix_core::run_until_stopped(jobs, worker, stop, progress);
    });
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

/// Run `jobs` through `run` and tell the window: `run-started`, one
/// `file-done` per finished file, then exactly one `run-finished`.
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
    last_output: &Mutex<Option<PathBuf>>,
    emit: &mut impl FnMut(Event),
    run: impl FnOnce(&[Job], &mut dyn FnMut(usize, &Path, &FileResult)),
) {
    let (jobs, left_out) = without_over_limit(jobs);
    let total = jobs.len() + usize::from(left_out > 0);
    emit(Event::RunStarted(RunStarted { total }));
    let mut tally = Tally::default();
    // Caught so that the window always hears the end of a run it saw start.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        run(&jobs, &mut |index, src, result| {
            tally.record(jobs.get(index), result);
            emit(Event::FileDone(file_done(index, src, result)));
        });
        if left_out > 0 {
            emit(Event::FileDone(over_limit_row(jobs.len(), left_out)));
        }
    }));
    *last_output.lock().unwrap_or_else(PoisonError::into_inner) =
        tally.output_folder().map(Path::to_path_buf);
    emit(Event::RunFinished(tally.finished()));
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
    use crate::events::{FileDone, Status};

    fn optimized(before: u64, after: u64) -> FileResult {
        FileResult::Optimized(Outcome {
            before,
            after,
            millis: 1,
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

    // --- picks and drops ------------------------------------------------------

    #[test]
    fn a_cancelled_dialog_frees_the_run_and_tells_the_window_without_running() {
        let guard = RunGuard::new();
        let ran = Cell::new(false);
        let planned = Cell::new(false);
        let mut events = Vec::new();
        let mut free_at_cancel = None;
        answer_dialog(
            guard.try_start().unwrap(),
            None,
            |_| {
                planned.set(true);
                Vec::new()
            },
            &mut |event| {
                free_at_cancel = Some(guard.try_start().is_some());
                events.push(event);
            },
            |_, _| ran.set(true),
        );
        assert_eq!(events, [Event::RunCancelled]);
        assert!(!planned.get() && !ran.get());
        // The ticket was given back before `run-cancelled` went out.
        assert_eq!(free_at_cancel, Some(true));
    }

    #[test]
    fn an_answered_dialog_runs_the_planned_jobs_with_the_ticket_and_sends_no_cancel() {
        let guard = RunGuard::new();
        let picked = vec![PathBuf::from("/srv/a.png")];
        let job = optimize("/srv/optimized-a.png");
        let mut events = Vec::new();
        let ran_with = RefCell::new(Vec::new());
        answer_dialog(
            guard.try_start().unwrap(),
            Some(picked.clone()),
            |paths| {
                assert_eq!(paths, picked.as_slice());
                vec![job.clone()]
            },
            &mut |event| events.push(event),
            |_ticket, jobs| {
                // The run holds the ticket: the guard is taken while it runs.
                assert!(guard.try_start().is_none());
                ran_with.borrow_mut().push(jobs);
            },
        );
        assert_eq!(events, []);
        assert_eq!(*ran_with.borrow(), [vec![job]]);
        assert!(guard.try_start().is_some());
    }

    #[test]
    fn a_cancelled_dialog_plans_nothing_and_runs_nothing() {
        let called = Cell::new(false);
        let plan = |_: &[PathBuf]| {
            called.set(true);
            Vec::new()
        };
        assert_eq!(plan_pick(None, plan), None);
        assert!(!called.get());
    }

    #[test]
    fn an_empty_pick_or_drop_plans_nothing_and_runs_nothing() {
        let called = Cell::new(false);
        let plan = |_: &[PathBuf]| {
            called.set(true);
            Vec::new()
        };
        assert_eq!(plan_pick(Some(Vec::new()), plan), None);
        assert!(!called.get());
    }

    #[test]
    fn a_picked_set_is_planned_once_with_exactly_those_paths() {
        let picked = vec![PathBuf::from("/a/x.png"), PathBuf::from("/b/y.svg")];
        let job = Job::Skip {
            src: PathBuf::from("/a/x.png"),
            reason: Skip::NotAFile,
        };
        let calls = Cell::new(0);
        let jobs = plan_pick(Some(picked.clone()), |paths| {
            calls.set(calls.get() + 1);
            assert_eq!(paths, picked.as_slice());
            vec![job.clone()]
        });
        assert_eq!(jobs, Some(vec![job]));
        assert_eq!(calls.get(), 1);
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
        assert_eq!(
            names,
            ["run-started", "file-done", "file-done", "run-finished"]
        );
        assert_eq!(count_finished(&events), 1);
        assert_eq!(events[0], Event::RunStarted(RunStarted { total: 2 }));
        let statuses: Vec<(usize, Status)> = events
            .iter()
            .filter_map(|event| match event {
                Event::FileDone(done) => Some((done.index, done.status)),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, [(0, Status::Skipped), (1, Status::Failed)]);
        assert_eq!(
            events[3],
            Event::RunFinished(RunFinished {
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
            &last_output,
            &mut |event| {
                if let Event::RunFinished(_) = event {
                    let claimed = claim_run(&guard, &mut |busy| busy_events.push(busy));
                    claimed_at_finish = Some(claimed.err());
                }
            },
            |_, progress| progress(0, Path::new("/srv/photos/a.png"), &written()),
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
            &last_output,
            &mut |event| {
                if let Event::RunFinished(_) = event {
                    stored_at_finish = last_output.lock().unwrap().clone();
                }
                events.push(event);
            },
            |_, progress| progress(0, Path::new("/srv/photos/a.png"), &written()),
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
                &last_output,
                &mut |event| {
                    if let Event::RunFinished(_) = event {
                        held_at_finish = Some(guard.try_start().is_none());
                    }
                    events.push(event);
                },
                |_, progress| {
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
        let mut given = Vec::new();
        run_session_with(
            guard.try_start().unwrap(),
            jobs,
            &last_output,
            &mut |event| events.push(event),
            |jobs, progress| {
                given = jobs.to_vec();
                for (index, job) in jobs.iter().enumerate() {
                    let (src, result) = match job {
                        Job::Optimize { src, .. } => (src, written()),
                        Job::Skip { src, reason } => (src, FileResult::Skipped(*reason)),
                    };
                    progress(index, src, &result);
                }
            },
        );
        (events, given)
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

        assert_eq!(
            events[0],
            Event::RunStarted(RunStarted {
                total: MAX_RUN_FILES + 1
            })
        );
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

        assert_eq!(
            events[0],
            Event::RunStarted(RunStarted {
                total: MAX_RUN_FILES
            })
        );
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
            .filter(|row| row.reason.as_deref() == Some("Not a PNG, JPG or SVG file"))
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
        assert_eq!(
            events[0],
            Event::RunStarted(RunStarted { total: rows.len() })
        );
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
}
