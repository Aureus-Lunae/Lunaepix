//! The worker mode on its own. The bundle installs it beside the app as
//! `lunaepix-worker[.exe]` (Tauri's `externalBin`), and a release app starts
//! it for each file, for each thumbnail and for each probe: it links a
//! handful of libraries instead of the window's hundred and sixty, so it
//! starts many times faster. The integration tests reach it through
//! `env!("CARGO_BIN_EXE_lunaepix-worker")`. The CLI, and an app without the
//! sidecar, answer all three modes from their own binaries.

use std::process::ExitCode;

fn main() -> ExitCode {
    lunaepix_core::worker::serve_if_requested().unwrap_or_else(|| {
        eprintln!(
            "usage: lunaepix-worker {} [--lossy <0-100>] [--no-replace] <src> <dest>\n       lunaepix-worker {} <src>\n       lunaepix-worker {} <src>",
            lunaepix_core::WORKER_FLAG,
            lunaepix_core::THUMB_FLAG,
            lunaepix_core::PROBE_FLAG
        );
        ExitCode::from(2)
    })
}
