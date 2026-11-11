// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    // The runner starts this same binary as the worker for each file, and as
    // the thumbnail worker for each thumbnail. This must come before anything
    // touches Tauri, or every file would open a window of its own.
    if let Some(code) = lunaepix_core::worker::serve_if_requested() {
        return code;
    }
    lunaepix_app_lib::run()
}
