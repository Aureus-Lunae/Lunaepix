//! Thin Tauri shell: window config, commands and capabilities. All logic lives
//! in `lunaepix-core`.

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the Lunaepix application");
}
