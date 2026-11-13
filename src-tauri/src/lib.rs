//! Thin Tauri shell over `lunaepix-core`: the commands, the window's drag
//! and drop, events back to the window, the `thumb` scheme that serves the
//! list's thumbnails and the `preview` scheme that serves Single image
//! mode's image and its compressed version. `INTERFACE.md` is the contract
//! for the window; `session` holds what the commands do, `single` Single
//! image mode's state, `thumbs` and `previews` what the schemes answer.
//!
//! The window never hands the backend a path. Paths come only from a native
//! dialog opened here, or from a drop that Tauri delivers here as a window
//! event; the window hears file names, folder names, numbers, thumbnail
//! keys and Single image mode's image and version keys, never paths.

pub mod events;
pub mod names;
pub mod previews;
pub mod session;
pub mod single;
pub mod thumbs;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use lunaepix_core::{StopHandle, Worker};
use tauri::ipc::InvokeBody;
use tauri::{
    AppHandle, DragDropEvent, Emitter, Manager, UriSchemeContext, UriSchemeResponder, Window,
    WindowEvent, Wry,
};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};

use events::{DragState, Event};
use previews::PreviewResponse;
use session::{CommandError, Session};
use thumbs::ThumbResponse;

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
    /// The list, the output setting, the busy guard, the last output, the
    /// list's thumbnails and Single image mode's state.
    session: Session,
    /// The worker [`choose_worker`] chose: its program optimises each file
    /// of a run, makes each thumbnail and probes each image Single image
    /// mode loads.
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
/// thread when this fails, so a run, a drop or a preview job is never lost.
fn spawn_work_thread(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("lunaepix-run".to_owned())
        .spawn(job)
        .map(drop)
}

/// Start `job` on a thread of its own named `thumbnail`: it starts a
/// thumbnail worker and waits for it there.
fn spawn_thumbnail_thread(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("thumbnail".to_owned())
        .spawn(job)
        .map(drop)
}

/// The `thumb` scheme: a request is answered by [`thumbs::serve`] on a
/// thread of its own, which starts the thumbnail worker and waits for it,
/// so the webview's thread never waits. The request goes along unchanged:
/// [`thumbs::serve`] decides what of it counts (no query, the path as
/// received). If no thread can be started, the request gets `404`.
fn thumb_protocol(
    context: UriSchemeContext<'_, Wry>,
    request: tauri::http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = context.app_handle().clone();
    session::on_own_thread_or_else(
        (app, request, responder),
        |(app, request, responder)| {
            let state = app.state::<AppState>();
            let answer = thumbs::serve(state.session.thumbs(), &state.worker.program, &request);
            responder.respond(answer.into_http());
        },
        spawn_thumbnail_thread,
        |(_, _, responder)| responder.respond(ThumbResponse::not_found().into_http()),
    );
}

/// Start `job` on a thread of its own named `preview`: it reads a file the
/// `preview` scheme serves, or makes a compressed version in a worker and
/// waits for it.
fn spawn_preview_thread(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("preview".to_owned())
        .spawn(job)
        .map(drop)
}

/// The `preview` scheme: a request is answered by [`previews::serve`] on a
/// thread of its own, which reads the file, so the webview's thread never
/// waits. The request's path and query go along unchanged: `previews::serve`
/// decides what of them counts. If no thread can be started, the request
/// gets `404`.
fn preview_protocol(
    context: UriSchemeContext<'_, Wry>,
    request: tauri::http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = context.app_handle().clone();
    session::on_own_thread_or_else(
        (app, request, responder),
        |(app, request, responder)| {
            let state = app.state::<AppState>();
            let uri = request.uri();
            let answer = previews::serve(state.session.single(), uri.path(), uri.query());
            responder.respond(answer.into_http());
        },
        spawn_preview_thread,
        |(_, _, responder)| responder.respond(PreviewResponse::not_found().into_http()),
    );
}

/// The JSON a command was invoked with: its whole argument object, for a
/// command that checks the shape itself (an extra key included). `None` for
/// a raw body, which no command here takes.
fn json_body(body: &InvokeBody) -> Option<&serde_json::Value> {
    match body {
        InvokeBody::Json(value) => Some(value),
        InvokeBody::Raw(_) => None,
    }
}

/// A one-file dialog's answer as a path: `None` for a closed dialog, and for
/// an entry that is not a file path (desktop dialogs give paths only).
fn one_path(picked: Option<FilePath>) -> Option<PathBuf> {
    picked.and_then(|path| path.into_path().ok())
}

/// What the window closing does: stop the run, then stop the preview and
/// remove Single image mode's private folder. Stopping kills the worker that
/// is running (the run's file, or the preview) and waits for it to end
/// ([`StopHandle::stop`]); it does not wait for the run's thread to finish
/// its loop, nor for a save (see [`Session::save_image`]).
fn close_down(stop: &StopHandle, session: &Session) {
    stop.stop();
    session.shutdown();
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

/// [`file_dialog`] titled `title`, with the image filter first and "All
/// files" second: the filters of both image dialogs.
fn image_dialog(app: &AppHandle, title: &str) -> FileDialogBuilder<Wry> {
    file_dialog(app)
        .set_title(title)
        .add_filter(IMAGE_FILTER_NAME, IMAGE_EXTENSIONS)
        .add_filter("All files", &["*"])
}

/// Open a native multi-file dialog; the files picked are added to the list.
/// Returns at once; the outcome arrives as events, from the dialog
/// plugin's thread.
/// Runs on the main thread: it sends nothing but `run-busy`, whose place
/// among other events does not matter (see `MAIN_THREAD_COMMANDS` in
/// `tests/config.rs`).
#[tauri::command]
fn choose_files(app: AppHandle) -> Result<(), CommandError> {
    session(&app).choose_files(&mut |event| emit(&app, event), |ticket| {
        let answer = app.clone();
        image_dialog(&app, "Choose images").pick_files(move |picked| {
            // The plugin calls this on a thread of its own.
            session(&answer).add_paths(ticket, picked.map(into_paths), &mut |event| {
                emit(&answer, event)
            });
        });
    })
}

/// Tell the backend which mode shows, so a drop goes there. Takes the whole
/// argument object, `{ "view": "single" }` or `{ "view": "batch" }`, so an
/// extra key is refused too.
/// Runs on the main thread: it sends nothing but `run-busy`, whose place
/// among other events does not matter (see `MAIN_THREAD_COMMANDS` in
/// `tests/config.rs`).
#[tauri::command]
fn set_view(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<(), CommandError> {
    session(&app).set_view(json_body(request.body()), &mut |event| emit(&app, event))
}

/// Open a native one-file dialog; the file picked is loaded in Single image
/// mode. Returns at once; the outcome arrives as `image-changed`, from the
/// dialog plugin's thread.
/// Runs on the main thread: it sends nothing but `run-busy`, whose place
/// among other events does not matter (see `MAIN_THREAD_COMMANDS` in
/// `tests/config.rs`).
#[tauri::command]
fn choose_image(app: AppHandle) -> Result<(), CommandError> {
    session(&app).choose_image(&mut |event| emit(&app, event), |ticket| {
        let answer = app.clone();
        image_dialog(&app, "Choose an image").pick_file(move |picked| {
            // The plugin calls this on a thread of its own, so the copy and
            // the probe do not block the main thread.
            let state = answer.state::<AppState>();
            state.session.load_image(
                ticket,
                one_path(picked),
                session::worker_prober(&state.worker),
                &mut |event| emit(&answer, event),
            );
        });
    })
}

/// Make the compressed version of the loaded image with the window's
/// `settings` (checked by [`session::parse_settings`]), replacing any
/// request still going. Returns once it has started; the outcome arrives as
/// `preview-ready` or `preview-failed`. The worker runs on a thread of its
/// own. Not on the main thread (`async`): an event sent from the main
/// thread is shown at once, while the worker's outcome waits in the event
/// loop's queue, so a `preview-started` sent from here could overtake the
/// older request's outcome. Off the main thread, every preview event goes
/// through that one queue in the order it was sent.
#[tauri::command(async)]
fn preview_image(app: AppHandle, settings: Option<serde_json::Value>) -> Result<(), CommandError> {
    session(&app).preview_image(settings.as_ref(), &mut |event| emit(&app, event), |job| {
        let app = app.clone();
        let make = move |job| {
            let state = app.state::<AppState>();
            state
                .session
                .run_preview(job, &state.worker, &mut |event| emit(&app, event));
        };
        session::on_own_thread_or_here(job, make, spawn_preview_thread);
    })
}

/// Save the compressed version the window shows under the name typed in
/// Save as, never replacing a file. Takes the whole argument object,
/// `{ "name": "beach", "preview": "7-3" }`, so an extra key is refused too
/// (see [`Session::save_image`]). Resolves once the file is written and
/// `image-saved` has gone out. Not on the main thread (`async`): it reads
/// and writes files, and its `image-saved` goes through the one event
/// queue behind the preview's events.
#[tauri::command(async)]
fn save_image(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<(), CommandError> {
    session(&app).save_image(json_body(request.body()), &mut |event| emit(&app, event))
}

/// Switch to Batch and add every item of the last Single image drop that
/// left items out. Not on the main thread (`async`): adding reads folders.
#[tauri::command(async)]
fn move_drop_to_batch(app: AppHandle) -> Result<(), CommandError> {
    session(&app).move_drop_to_batch(&mut |event| emit(&app, event))
}

/// Empty the list. Not on the main thread (`async`), so its `list-changed`
/// queues behind a run's `run-finished` instead of overtaking it (see
/// `preview_image`).
#[tauri::command(async)]
fn clear_list(app: AppHandle) -> Result<(), CommandError> {
    session(&app).clear_list(&mut |event| emit(&app, event))
}

/// Bring the list of the last run back, so it can run again with other
/// settings. Sends the list as `list-changed`. Not on the main thread
/// (`async`), for the same reason as `clear_list`.
#[tauri::command(async)]
fn readd_list(app: AppHandle) -> Result<(), CommandError> {
    session(&app).readd_list(&mut |event| emit(&app, event))
}

/// Open a native folder dialog; the folder picked becomes the output folder.
/// Returns at once; the outcome arrives as events, from the dialog
/// plugin's thread.
/// Runs on the main thread: it sends nothing but `run-busy`, whose place
/// among other events does not matter (see `MAIN_THREAD_COMMANDS` in
/// `tests/config.rs`).
#[tauri::command]
fn choose_output_folder(app: AppHandle) -> Result<(), CommandError> {
    session(&app).choose_output_folder(&mut |event| emit(&app, event), |ticket| {
        let answer = app.clone();
        file_dialog(&app)
            .set_title("Choose the output folder")
            .pick_folder(move |picked| {
                session(&answer)
                    .set_output(ticket, one_path(picked), &mut |event| emit(&answer, event));
            });
    })
}

/// Back to writing next to the originals. Not on the main thread (`async`):
/// the name Save as starts with is found by listing the original's folder.
/// (`choose_output_folder`'s answer already runs on the dialog plugin's
/// own thread.)
#[tauri::command(async)]
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

/// Open the folder of the last save, or the output folder of the last
/// completed run, whichever came last, in the file manager.
/// Runs on the main thread: it sends no event.
#[tauri::command]
fn open_output_folder(app: AppHandle) -> Result<(), CommandError> {
    session(&app).open_output_folder(|folder| tauri_plugin_opener::open_path(folder, None::<&str>))
}

/// Call `stop` when `event` is the window closing. The app ends with its one
/// window, so the run ends too: `stop` ([`close_down`]) kills the running
/// worker and starts no other, and removes Single image mode's private
/// folder, before the process exits. Asking to close and the window going
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
/// window hears `drag-state`, then `list-changed` (Batch) or `image-changed`
/// (Single image), and a drop goes to the mode that shows. This runs on the
/// main thread, so `drag-state` and a refused drop's `run-busy` are shown
/// at once, ahead of events still queued; neither depends on its place
/// among them. The drop's own events come from the thread it is handed to.
fn on_window_event(window: &Window, event: &WindowEvent) {
    stop_on_close(event, || {
        let state = window.app_handle().state::<AppState>();
        close_down(&state.stop, &state.session);
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
            // Adding reads folders, and loading copies and probes a file, so
            // either runs off the main thread.
            let app = app.clone();
            let receive = move |(ticket, paths)| {
                let state = app.state::<AppState>();
                state.session.receive_drop(
                    ticket,
                    paths,
                    session::worker_prober(&state.worker),
                    &mut |event| emit(&app, event),
                );
            };
            session::on_own_thread_or_here((ticket, paths), receive, spawn_work_thread);
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
    // Every launch starts in Single image with no image, on Lossless (the
    // window's choice), with an empty list and the output next to the
    // originals.
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
        .register_asynchronous_uri_scheme_protocol("thumb", thumb_protocol)
        .register_asynchronous_uri_scheme_protocol("preview", preview_protocol)
        .invoke_handler(tauri::generate_handler![
            set_view,
            choose_image,
            preview_image,
            save_image,
            move_drop_to_batch,
            choose_files,
            clear_list,
            readd_list,
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

    // --- the window closing: Single image mode's clean-up ---------------------

    /// A session with an image loaded through a probe double: the session
    /// and the image's private folder.
    fn session_with_an_image() -> (tempfile::TempDir, Session, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        std::fs::write(&png, b"png").unwrap();
        let session = Session::new();
        let prober = |_: &Path| {
            lunaepix_core::ProbeAnswer::Size(lunaepix_core::Probe {
                width: Some(1),
                height: Some(1),
            })
        };
        session
            .choose_image(&mut |_| {}, |ticket| {
                session.load_image(ticket, Some(png.clone()), prober, &mut |_| {});
            })
            .unwrap();
        let copy = session.single().current().expect("loaded").copy;
        let private = copy.parent().unwrap().parent().unwrap().to_path_buf();
        (dir, session, private)
    }

    #[test]
    fn the_close_handler_calls_shutdown() {
        let (_dir, session, private) = session_with_an_image();
        assert!(private.is_dir());
        let stop = StopHandle::new();
        stop_on_close(&WindowEvent::Destroyed, || close_down(&stop, &session));
        assert!(!private.exists(), "the private folder is removed");
        assert_eq!(session.single().current(), None);
        assert!(stop.is_stopped(), "the run is stopped too");
    }

    #[test]
    fn other_window_events_do_not_shut_down() {
        let (_dir, session, private) = session_with_an_image();
        let stop = StopHandle::new();
        for event in [
            WindowEvent::Focused(false),
            WindowEvent::Resized(PhysicalSize::new(800, 600)),
        ] {
            stop_on_close(&event, || close_down(&stop, &session));
        }
        assert!(private.is_dir());
        assert!(session.single().current().is_some());
        assert!(!stop.is_stopped());
    }

    // --- what the commands take from Tauri --------------------------------------

    #[test]
    fn json_body_gives_the_json_of_a_json_body() {
        let args = serde_json::json!({ "view": "single", "extra": 1 });
        let body = InvokeBody::Json(args.clone());
        // The whole object, extra key and all: the session checks the shape.
        assert_eq!(json_body(&body), Some(&args));
    }

    #[test]
    fn json_body_is_none_for_a_raw_body() {
        assert_eq!(
            json_body(&InvokeBody::Raw(b"{\"view\":\"single\"}".to_vec())),
            None
        );
    }

    #[test]
    fn one_picked_path_is_that_path() {
        let path = PathBuf::from("/srv/in/holiday.jpg");
        assert_eq!(one_path(Some(FilePath::Path(path.clone()))), Some(path));
    }

    #[test]
    fn no_pick_or_a_non_file_url_is_none() {
        assert_eq!(one_path(None), None);
        let web = tauri::Url::parse("https://example.com/holiday.jpg").unwrap();
        assert_eq!(one_path(Some(FilePath::Url(web))), None);
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
