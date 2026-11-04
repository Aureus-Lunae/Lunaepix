//! The worker mode on its own, for the integration tests: they reach it
//! through `env!("CARGO_BIN_EXE_lunaepix-worker")`. Not part of any bundle;
//! the CLI and the app answer the worker mode from their own binaries.

use std::process::ExitCode;

fn main() -> ExitCode {
    lunaepix_core::worker::serve_if_requested().unwrap_or_else(|| {
        eprintln!(
            "usage: lunaepix-worker {} <src> <dest>",
            lunaepix_core::WORKER_FLAG
        );
        ExitCode::from(2)
    })
}
