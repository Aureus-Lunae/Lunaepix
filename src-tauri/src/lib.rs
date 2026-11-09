//! Thin Tauri shell over `lunaepix-core`: three commands, the window's
//! drag and drop, and events back to the window. `INTERFACE.md` is the
//! contract for the window.
//!
//! The window never hands the backend a path. Paths come only from a native
//! dialog opened here, or from a drop that Tauri delivers here as a window
//! event; the window hears file names and numbers, never paths.

pub mod events;
pub mod session;

use std::path::{Path, PathBuf};
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
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "svg", "webp", "PNG", "JPG", "JPEG", "SVG", "WEBP",
];

/// What the file dialog calls the filter over [`IMAGE_EXTENSIONS`].
const IMAGE_FILTER_NAME: &str = "PNG, JPG, SVG and WebP images";

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
        .add_filter(IMAGE_FILTER_NAME, IMAGE_EXTENSIONS)
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

/// The worker binary the bundle installs beside the app (Tauri's
/// `externalBin`, see `tauri.bundle.conf.json`).
fn sidecar_name() -> String {
    format!("lunaepix-worker{}", std::env::consts::EXE_SUFFIX)
}

/// The program that optimises each file, and a warning to print, if any.
///
/// A release build uses the worker beside it: that binary links five
/// libraries instead of the window's 160, so it starts in about 6 ms
/// instead of 58. When it is missing (an app built without the bundle
/// config), the app's own worker mode does the work, with one warning.
/// A debug build always uses its own worker mode, so `cargo run` needs no
/// sidecar. `is_file` is the file-system check, injected for the tests.
fn choose_worker(
    app_exe: &Path,
    release: bool,
    is_file: impl Fn(&Path) -> bool,
) -> (Worker, Option<String>) {
    let itself = || Worker::new(app_exe);
    if !release {
        return (itself(), None);
    }
    let sidecar = app_exe.with_file_name(sidecar_name());
    if is_file(&sidecar) {
        (Worker::new(sidecar), None)
    } else {
        let warning = format!(
            "lunaepix: no worker at {}, so the app optimises in its own, slower worker mode",
            sidecar.display()
        );
        (itself(), Some(warning))
    }
}

/// Start the window. `main` calls this after the worker-mode check.
pub fn run() -> ExitCode {
    let app_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("lunaepix: cannot find its own program file: {error}");
            return ExitCode::FAILURE;
        }
    };
    let release = !cfg!(debug_assertions);
    let (worker, warning) = choose_worker(&app_exe, release, Path::is_file);
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
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
    use std::cell::{Cell, RefCell};

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

    // --- the file dialog's filter ---------------------------------------------

    #[test]
    fn the_dialog_filter_lists_webp_in_both_cases() {
        assert!(IMAGE_EXTENSIONS.contains(&"webp"));
        assert!(IMAGE_EXTENSIONS.contains(&"WEBP"));
        assert_eq!(IMAGE_FILTER_NAME, "PNG, JPG, SVG and WebP images");
    }

    #[test]
    fn the_dialog_filter_lists_exactly_the_five_extensions_each_in_both_cases() {
        let mut listed = IMAGE_EXTENSIONS.to_vec();
        listed.sort_unstable();
        // Upper case sorts before lower case. Nothing else is in the list, so
        // a lookalike such as "webpx" or "gif" would be caught here.
        assert_eq!(
            listed,
            [
                "JPEG", "JPG", "PNG", "SVG", "WEBP", "jpeg", "jpg", "png", "svg", "webp"
            ]
        );
    }

    // --- which worker ---------------------------------------------------------

    fn app_exe() -> PathBuf {
        Path::new("/opt/lunaepix/bin").join(format!("lunaepix-app{}", std::env::consts::EXE_SUFFIX))
    }

    fn sidecar() -> PathBuf {
        Path::new("/opt/lunaepix/bin")
            .join(format!("lunaepix-worker{}", std::env::consts::EXE_SUFFIX))
    }

    #[test]
    fn a_release_app_uses_the_sidecar_beside_it() {
        let asked = RefCell::new(Vec::new());
        let (worker, warning) = choose_worker(&app_exe(), true, |path| {
            asked.borrow_mut().push(path.to_path_buf());
            path == sidecar()
        });
        assert_eq!(worker, Worker::new(sidecar()));
        assert_eq!(warning, None);
        assert_eq!(*asked.borrow(), [sidecar()]);
    }

    #[test]
    fn a_release_app_without_the_sidecar_falls_back_to_itself_and_warns_once() {
        let (worker, warning) = choose_worker(&app_exe(), true, |_| false);
        assert_eq!(worker, Worker::new(app_exe()));
        let warning = warning.expect("one warning");
        assert!(
            warning.contains(&sidecar().display().to_string()),
            "{warning}"
        );
        assert_eq!(warning.lines().count(), 1, "{warning}");
    }

    #[test]
    fn a_debug_app_always_uses_itself_even_with_a_sidecar_beside_it() {
        let asked = Cell::new(0);
        let (worker, warning) = choose_worker(&app_exe(), false, |_| {
            asked.set(asked.get() + 1);
            true
        });
        assert_eq!(worker, Worker::new(app_exe()));
        assert_eq!(warning, None);
        assert_eq!(
            asked.get(),
            0,
            "a debug build does not look for the sidecar"
        );
    }

    #[test]
    fn the_sidecar_name_is_lunaepix_worker_with_exe_on_windows_only() {
        #[cfg(windows)]
        assert_eq!(sidecar_name(), "lunaepix-worker.exe");
        #[cfg(not(windows))]
        assert_eq!(sidecar_name(), "lunaepix-worker");
    }
}
