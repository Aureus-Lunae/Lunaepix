fn main() {
    // Declaring the app's commands makes each one need a permission
    // (`allow-<command>`), so the capability lists exactly what the window
    // may call. Without this, every app command would be open to the window.
    let manifest = tauri_build::AppManifest::new().commands(&[
        "choose_files",
        "clear_list",
        "readd_list",
        "choose_output_folder",
        "clear_output_folder",
        "start_run",
        "open_output_folder",
        "set_view",
        "choose_image",
        "preview_image",
        "save_image",
        "move_drop_to_batch",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to run tauri-build");
}
