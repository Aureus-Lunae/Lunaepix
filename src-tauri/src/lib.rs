//! Thin Tauri shell over `lunaepix-core`: three commands, the window's
//! drag and drop, and events back to the window. `INTERFACE.md` is the
//! contract for the window.
//!
//! The window never hands the backend a path. Paths come only from a native
//! dialog opened here, or from a drop that Tauri delivers here as a window
//! event; the window hears file names and numbers, never paths.

pub mod events;
pub mod session;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use lunaepix_core::{Job, StopHandle, Worker, plan_files, plan_folder};
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, Window, WindowEvent, Wry};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};

use events::{DragState, Event};
use session::{
    CommandError, RunGuard, RunTicket, answer_dialog, claim_run, open_last_output, plan_pick,
    run_session,
};

/// Extensions the file dialog shows. GTK matches them case-sensitively, so
/// both spellings are listed; "All files" covers any other spelling, and the
/// planner skips what it does not handle.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "svg", "PNG", "JPG", "JPEG", "SVG"];

/// Everything the backend keeps between calls.
struct AppState {
    guard: RunGuard,
    worker: Worker,
    /// Stops the active run when the window closes.
    stop: StopHandle,
    /// Output folder of the last completed run, if it wrote anything.
    last_output: Mutex<Option<PathBuf>>,
    /// Last `drag-state` sent, so a stream of drag-over events sends one.
    drag_over: AtomicBool,
}

fn emit(app: &AppHandle, event: Event) {
    if let Err(error) = app.emit(event.name(), &event) {
        eprintln!(
            "lunaepix: could not send {} to the window: {error}",
            event.name()
        );
    }
}

/// A ticket for a new run, or `run-busy` and [`CommandError::Busy`].
fn ticket(app: &AppHandle) -> Result<RunTicket, CommandError> {
    claim_run(&app.state::<AppState>().guard, &mut |event| {
        emit(app, event)
    })
}

/// Run planned jobs on the calling thread (never the main one).
fn run_jobs(app: &AppHandle, ticket: RunTicket, jobs: &[Job]) {
    let state = app.state::<AppState>();
    run_session(
        ticket,
        jobs,
        &state.worker,
        &state.stop,
        &state.last_output,
        &mut |event| emit(app, event),
    );
}

/// Dialog paths as plain paths. On desktop every entry is a path.
fn into_paths(picked: Vec<FilePath>) -> Vec<PathBuf> {
    picked
        .into_iter()
        .filter_map(|path| path.into_path().ok())
        .collect()
}

/// A file dialog that is modal to the main window.
fn file_dialog(app: &AppHandle) -> FileDialogBuilder<Wry> {
    let dialog = app.dialog().file();
    match app.get_webview_window("main") {
        Some(window) => dialog.set_parent(&window),
        None => dialog,
    }
}

/// Open a native multi-file dialog, then optimise the chosen files. Returns
/// at once; the dialog's outcome arrives as events.
#[tauri::command]
fn choose_files(app: AppHandle) -> Result<(), CommandError> {
    let ticket = ticket(&app)?;
    file_dialog(&app)
        .set_title("Choose images")
        .add_filter("PNG, JPG and SVG images", IMAGE_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_files(move |picked| {
            // The plugin calls this on a thread of its own.
            answer_dialog(
                ticket,
                picked.map(into_paths),
                plan_files,
                &mut |event| emit(&app, event),
                |ticket, jobs| run_jobs(&app, ticket, &jobs),
            );
        });
    Ok(())
}

/// Open a native folder dialog, then optimise the folder into
/// `<folder>/optimized/`. Returns at once; the outcome arrives as events.
#[tauri::command]
fn choose_folder(app: AppHandle) -> Result<(), CommandError> {
    let ticket = ticket(&app)?;
    file_dialog(&app)
        .set_title("Choose a folder")
        .pick_folder(move |picked| {
            let picked = picked.map(|folder| into_paths(vec![folder]));
            let plan =
                |folders: &[PathBuf]| folders.iter().flat_map(|dir| plan_folder(dir)).collect();
            answer_dialog(
                ticket,
                picked,
                plan,
                &mut |event| emit(&app, event),
                |ticket, jobs| run_jobs(&app, ticket, &jobs),
            );
        });
    Ok(())
}

/// Open the output folder of the last completed run in the file manager.
#[tauri::command]
fn open_output_folder(app: AppHandle) -> Result<(), CommandError> {
    open_last_output(&app.state::<AppState>().last_output, |folder| {
        tauri_plugin_opener::open_path(folder, None::<&str>)
    })
}

/// Call `stop` when `event` is the window closing. The app ends with its one
/// window, so the run ends too: `stop` kills the running worker and starts no
/// other, before the process exits. Asking to close and the window going
/// away both count (stopping twice is harmless), so that a close that skips
/// the request is covered as well.
fn stop_on_close(event: &WindowEvent, stop: impl FnOnce()) {
    if matches!(
        event,
        WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
    ) {
        stop();
    }
}

/// Drag and drop, handled here so that no path reaches the window: the
/// window hears `drag-state` only, and a drop starts a run.
fn on_window_event(window: &Window, event: &WindowEvent) {
    stop_on_close(event, || {
        window.app_handle().state::<AppState>().stop.stop()
    });
    let WindowEvent::DragDrop(drag) = event else {
        return;
    };
    let app = window.app_handle();
    let (over, dropped) = match drag {
        DragDropEvent::Enter { .. } | DragDropEvent::Over { .. } => (true, None),
        DragDropEvent::Leave => (false, None),
        DragDropEvent::Drop { paths, .. } => (false, Some(paths.clone())),
        _ => return,
    };
    let state = app.state::<AppState>();
    if state.drag_over.swap(over, Ordering::SeqCst) != over {
        emit(app, Event::DragState(DragState { over }));
    }
    let Some(paths) = dropped.filter(|paths| !paths.is_empty()) else {
        return;
    };
    let Ok(ticket) = ticket(app) else {
        return;
    };
    // Planning reads the folder, so it runs off the main thread too.
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("lunaepix-run".to_owned())
        .spawn(move || {
            if let Some(jobs) = plan_pick(Some(paths), plan_files) {
                run_jobs(&app, ticket, &jobs);
            }
        });
    if let Err(error) = spawned {
        // The closure, and with it the ticket, is gone: the guard is free.
        eprintln!("lunaepix: could not start the run: {error}");
    }
}

/// Start the window. `main` calls this after the worker-mode check.
pub fn run() -> ExitCode {
    let worker = match Worker::current_exe() {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("lunaepix: cannot find its own program file: {error}");
            return ExitCode::FAILURE;
        }
    };
    let state = AppState {
        guard: RunGuard::new(),
        worker,
        stop: StopHandle::new(),
        last_output: Mutex::new(None),
        drag_over: AtomicBool::new(false),
    };
    // The opener plugin is not registered: its Rust function is all that is
    // needed, and registering it would add commands and a link-click script
    // to the window. The dialog plugin must be registered for its Rust API;
    // its commands stay unreachable, as the window has no dialog permission.
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            choose_files,
            choose_folder,
            open_output_folder
        ])
        .on_window_event(on_window_event)
        .run(tauri::generate_context!());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lunaepix: the window could not start: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tauri::PhysicalSize;

    use super::*;

    #[test]
    fn a_closing_window_stops_the_run_once() {
        let stops = Cell::new(0);
        stop_on_close(&WindowEvent::Destroyed, || stops.set(stops.get() + 1));
        assert_eq!(stops.get(), 1);
    }

    #[test]
    fn other_window_events_leave_the_run_alone() {
        let stops = Cell::new(0);
        for event in [
            WindowEvent::Focused(true),
            WindowEvent::Focused(false),
            WindowEvent::Resized(PhysicalSize::new(800, 600)),
        ] {
            stop_on_close(&event, || stops.set(stops.get() + 1));
        }
        assert_eq!(stops.get(), 0);
    }
}
