//! Thin Tauri shell over `lunaepix-core`: six commands, the window's drag
//! and drop, and events back to the window. `INTERFACE.md` is the contract
//! for the window; `session` holds what the commands do.
//!
//! The window never hands the backend a path. Paths come only from a native
//! dialog opened here, or from a drop that Tauri delivers here as a window
//! event; the window hears file names, folder names and numbers, never paths.

pub mod events;
pub mod session;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use lunaepix_core::{StopHandle, Worker};
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, Window, WindowEvent, Wry};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};

use events::{DragState, Event};
use session::{CommandError, Session};

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
    /// The list, the output setting, the busy guard and the last output.
    session: Session,
    worker: Worker,
    /// Stops the active run when the window closes.
    stop: StopHandle,
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

/// The session in the app's state.
fn session(app: &AppHandle) -> &Session {
    &app.state::<AppState>().inner().session
}

/// Start `job` on a thread of its own, so that reading folders and running
/// files never block the main thread. Used through
/// [`session::on_own_thread_or_here`], which does the work on the calling
/// thread when this fails, so a run or a drop is never lost.
fn spawn_work_thread(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("lunaepix-run".to_owned())
        .spawn(job)
        .map(drop)
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

/// Open a native multi-file dialog; the files picked are added to the list.
/// Returns at once; the outcome arrives as events.
#[tauri::command]
fn choose_files(app: AppHandle) -> Result<(), CommandError> {
    session(&app).choose_files(&mut |event| emit(&app, event), |ticket| {
        let answer = app.clone();
        file_dialog(&app)
            .set_title("Choose images")
            .add_filter(IMAGE_FILTER_NAME, IMAGE_EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_files(move |picked| {
                // The plugin calls this on a thread of its own.
                session(&answer).add_paths(ticket, picked.map(into_paths), &mut |event| {
                    emit(&answer, event)
                });
            });
    })
}

/// Empty the list.
#[tauri::command]
fn clear_list(app: AppHandle) -> Result<(), CommandError> {
    session(&app).clear_list(&mut |event| emit(&app, event))
}

/// Open a native folder dialog; the folder picked becomes the output folder.
/// Returns at once; the outcome arrives as events.
#[tauri::command]
fn choose_output_folder(app: AppHandle) -> Result<(), CommandError> {
    session(&app).choose_output_folder(&mut |event| emit(&app, event), |ticket| {
        let answer = app.clone();
        file_dialog(&app)
            .set_title("Choose the output folder")
            .pick_folder(move |picked| {
                let folder = picked.and_then(|folder| folder.into_path().ok());
                session(&answer).set_output(ticket, folder, &mut |event| emit(&answer, event));
            });
    })
}

/// Back to writing next to the originals.
#[tauri::command]
fn clear_output_folder(app: AppHandle) -> Result<(), CommandError> {
    session(&app).clear_output_folder(&mut |event| emit(&app, event))
}

/// Run the list with the window's `settings` (checked by
/// [`session::parse_settings`]). Returns once the run has started; its
/// progress arrives as events. Not on the main thread (`async`): planning
/// the list reads every file's metadata.
#[tauri::command(async)]
fn start_run(app: AppHandle, settings: Option<serde_json::Value>) -> Result<(), CommandError> {
    session(&app).start_run(
        settings.as_ref(),
        &mut |event| emit(&app, event),
        |prepared| {
            let app = app.clone();
            let run = move |prepared| {
                let state = app.state::<AppState>();
                state
                    .session
                    .run_with_worker(prepared, &state.worker, &state.stop, &mut |event| {
                        emit(&app, event)
                    });
            };
            session::on_own_thread_or_here(prepared, run, spawn_work_thread);
        },
    )
}

/// Open the output folder of the last completed run in the file manager.
#[tauri::command]
fn open_output_folder(app: AppHandle) -> Result<(), CommandError> {
    session(&app).open_output_folder(|folder| tauri_plugin_opener::open_path(folder, None::<&str>))
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
/// window hears `drag-state` and `list-changed` only, and a drop adds to the
/// list.
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
    let Some(paths) = dropped else {
        return;
    };
    state
        .session
        .drop_paths(paths, &mut |event| emit(app, event), |ticket, paths| {
            // Adding reads folders, so it runs off the main thread.
            let app = app.clone();
            let add = move |(ticket, paths)| {
                session(&app).add_paths(ticket, Some(paths), &mut |event| emit(&app, event));
            };
            session::on_own_thread_or_here((ticket, paths), add, spawn_work_thread);
        });
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
    // Every launch starts on Lossless (the window's choice), with an empty
    // list and the output next to the originals.
    let state = AppState {
        session: Session::new(),
        worker,
        stop: StopHandle::new(),
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
            clear_list,
            choose_output_folder,
            clear_output_folder,
            start_run,
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
