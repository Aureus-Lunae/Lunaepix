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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use lunaepix_core::{Job, Worker, plan_files, plan_folder};
use serde::Serialize;
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, Window, WindowEvent, Wry};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};

use events::{DragState, Event};
use session::{RunGuard, RunTicket, plan_pick, run_session};

/// Extensions the file dialog shows. GTK matches them case-sensitively, so
/// both spellings are listed; "All files" covers any other spelling, and the
/// planner skips what it does not handle.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "svg", "PNG", "JPG", "JPEG", "SVG"];

/// Everything the backend keeps between calls.
struct AppState {
    guard: RunGuard,
    worker: Worker,
    /// Output folder of the last completed run, if it wrote anything.
    last_output: Mutex<Option<PathBuf>>,
    /// Last `drag-state` sent, so a stream of drag-over events sends one.
    drag_over: AtomicBool,
}

/// Why a command did nothing. Serialised as a bare string: `"busy"`,
/// `"no-output"` or `"open-failed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CommandError {
    /// A run (or a dialog for one) is already active.
    Busy,
    /// No completed run has written output yet.
    NoOutput,
    /// The system could not open the folder (for example, it was removed).
    OpenFailed,
}

fn emit(app: &AppHandle, event: Event) {
    if let Err(error) = app.emit(event.name(), &event) {
        eprintln!(
            "lunaepix: could not send {} to the window: {error}",
            event.name()
        );
    }
}

/// A ticket for a new run, or `run-busy` and `None`.
fn ticket(app: &AppHandle) -> Option<RunTicket> {
    let ticket = app.state::<AppState>().guard.try_start();
    if ticket.is_none() {
        emit(app, Event::RunBusy);
    }
    ticket
}

/// Plan and run what a dialog or a drop gave, on the calling thread (never
/// the main one). Returns `false`, having run nothing, if there was nothing.
fn run_picked(
    app: &AppHandle,
    ticket: RunTicket,
    picked: Option<Vec<PathBuf>>,
    plan: impl FnOnce(&[PathBuf]) -> Vec<Job>,
) -> bool {
    let Some(jobs) = plan_pick(picked, plan) else {
        return false;
    };
    let state = app.state::<AppState>();
    run_session(
        ticket,
        &jobs,
        &state.worker,
        &state.last_output,
        &mut |event| emit(app, event),
    );
    true
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
    let ticket = ticket(&app).ok_or(CommandError::Busy)?;
    file_dialog(&app)
        .set_title("Choose images")
        .add_filter("PNG, JPG and SVG images", IMAGE_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_files(move |picked| {
            // The plugin calls this on a thread of its own.
            if !run_picked(&app, ticket, picked.map(into_paths), plan_files) {
                emit(&app, Event::RunCancelled);
            }
        });
    Ok(())
}

/// Open a native folder dialog, then optimise the folder into
/// `<folder>/optimized/`. Returns at once; the outcome arrives as events.
#[tauri::command]
fn choose_folder(app: AppHandle) -> Result<(), CommandError> {
    let ticket = ticket(&app).ok_or(CommandError::Busy)?;
    file_dialog(&app)
        .set_title("Choose a folder")
        .pick_folder(move |picked| {
            let picked = picked.map(|folder| into_paths(vec![folder]));
            let plan =
                |folders: &[PathBuf]| folders.iter().flat_map(|dir| plan_folder(dir)).collect();
            if !run_picked(&app, ticket, picked, plan) {
                emit(&app, Event::RunCancelled);
            }
        });
    Ok(())
}

/// Open the output folder of the last completed run in the file manager.
#[tauri::command]
fn open_output_folder(app: AppHandle) -> Result<(), CommandError> {
    let state = app.state::<AppState>();
    let folder = state
        .last_output
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or(CommandError::NoOutput)?;
    tauri_plugin_opener::open_path(&folder, None::<&str>).map_err(|_| CommandError::OpenFailed)
}

/// Drag and drop, handled here so that no path reaches the window: the
/// window hears `drag-state` only, and a drop starts a run.
fn on_window_event(window: &Window, event: &WindowEvent) {
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
    let Some(ticket) = ticket(app) else {
        return;
    };
    // Planning reads the folder, so it runs off the main thread too.
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("lunaepix-run".to_owned())
        .spawn(move || run_picked(&app, ticket, Some(paths), plan_files));
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
    use super::*;

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
}
