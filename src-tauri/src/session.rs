//! One run at a time: the busy guard, what a pick or a drop turns into, the
//! run itself with its events, and the output folder it leaves behind. No
//! Tauri here, so all of it is tested without a window.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lunaepix_core::{FileResult, Job, Worker};

use crate::events::{Event, RunStarted, file_done, run_finished};

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

/// The folder "Open folder" opens after this run: the folder of the first
/// file that was written. For a folder run that is `<dir>/optimized`; for a
/// file run, the folder of that file. `None` if nothing was written.
pub fn output_folder(jobs: &[Job], results: &[FileResult]) -> Option<PathBuf> {
    jobs.iter()
        .zip(results)
        .find_map(|(job, result)| match (job, result) {
            (Job::Optimize { dest, .. }, FileResult::Optimized(_)) => dest.parent(),
            _ => None,
        })
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// Run `jobs` on this thread and tell the window: `run-started`, one
/// `file-done` per job, then `run-finished`. Before `run-finished` the output
/// folder is stored in `last_output` and the ticket is given back, so the
/// window may open the folder or start the next run as soon as it hears it.
pub fn run_session(
    ticket: RunTicket,
    jobs: &[Job],
    worker: &Worker,
    last_output: &Mutex<Option<PathBuf>>,
    emit: &mut impl FnMut(Event),
) {
    emit(Event::RunStarted(RunStarted { total: jobs.len() }));
    let results = lunaepix_core::run(jobs, worker, |index, src, result| {
        emit(Event::FileDone(file_done(index, src, result)));
    });
    *last_output.lock().unwrap_or_else(PoisonError::into_inner) = output_folder(jobs, &results);
    drop(ticket);
    emit(Event::RunFinished(run_finished(&results)));
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;

    use lunaepix_core::{OptimizeError, Outcome, RunError, Skip, plan_files, plan_folder};
    use tempfile::TempDir;

    use super::*;
    use crate::events::{RunFinished, Status};

    fn written() -> FileResult {
        FileResult::Optimized(Outcome {
            before: 10,
            after: 5,
            millis: 1,
        })
    }

    fn failed(path: &Path) -> FileResult {
        FileResult::Failed(RunError::Optimize(OptimizeError::Invalid {
            path: path.to_path_buf(),
            message: "not a PNG".to_owned(),
        }))
    }

    /// A folder holding `a.png` (its bytes do not matter to the planner).
    fn folder_with_a_png() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        (dir, png)
    }

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
    fn a_session_emits_start_one_file_done_per_job_then_finish() {
        let guard = RunGuard::new();
        let ticket = guard.try_start().unwrap();
        let jobs = vec![
            Job::Skip {
                src: PathBuf::from("/srv/notes.txt"),
                reason: Skip::UnsupportedType,
            },
            Job::Optimize {
                src: PathBuf::from("/srv/a.png"),
                dest: PathBuf::from("/srv/optimized-a.png"),
            },
        ];
        // A worker that cannot be started fails its file without running.
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let last_output = Mutex::new(Some(PathBuf::from("/from/an/earlier/run")));
        let mut events = Vec::new();
        let mut free_at_finish = None;
        run_session(ticket, &jobs, &worker, &last_output, &mut |event| {
            if let Event::RunFinished(_) = event {
                free_at_finish = Some(guard.try_start().is_some());
            }
            events.push(event);
        });

        let names: Vec<&str> = events.iter().map(Event::name).collect();
        assert_eq!(
            names,
            ["run-started", "file-done", "file-done", "run-finished"]
        );
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
        // The ticket was given back before `run-finished` went out.
        assert_eq!(free_at_finish, Some(true));
        // This run wrote nothing, so the earlier run's folder is forgotten.
        assert_eq!(*last_output.lock().unwrap(), None);
    }

    #[test]
    fn a_session_keeps_the_guard_busy_until_it_finishes() {
        let guard = RunGuard::new();
        let ticket = guard.try_start().unwrap();
        let jobs = vec![Job::Skip {
            src: PathBuf::from("/srv/notes.txt"),
            reason: Skip::UnsupportedType,
        }];
        let worker = Worker::new("/nonexistent/lunaepix-worker-for-tests");
        let last_output = Mutex::new(None);
        let mut busy_during_file = None;
        run_session(ticket, &jobs, &worker, &last_output, &mut |event| {
            if let Event::FileDone(_) = event {
                busy_during_file = Some(guard.try_start().is_none());
            }
        });
        assert_eq!(busy_during_file, Some(true));
    }
}
