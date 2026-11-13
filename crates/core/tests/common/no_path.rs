//! The check that a failure reason carries no path, shared by the tests of the
//! core (`atomic.rs`, `worker.rs`) and of the app (`session_run.rs`). Each
//! includes this file with `#[path]`; the helper's own tests are in
//! `worker.rs`, so they run once.
//!
//! A path made by a test starts with the system's temp folder, and that part
//! is not the test's to hide: on macOS it is `/var/folders/<2 characters>/
//! <hash>/T`, and a 2-character folder name such as `er` is a part of
//! "Permission denied (os error 13)". Only the names below the temp folder
//! are checked.
#![allow(dead_code)]

use std::path::{Component, Path};

/// The names in `path` that the test made: those below `temp_root`, or all of
/// them when `path` is not under it.
pub fn names_below(path: &Path, temp_root: &Path) -> Vec<String> {
    path.strip_prefix(temp_root)
        .unwrap_or(path)
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The first name of `path` below `temp_root` that `message` holds, or "at
/// path" or ".tmp" (what `tempfile` writes into a message and into the name of
/// a staging file), or `None` when it holds none of them.
pub fn leaked_name(message: &str, path: &Path, temp_root: &Path) -> Option<String> {
    ["at path", ".tmp"]
        .into_iter()
        .map(str::to_owned)
        .chain(names_below(path, temp_root))
        .find(|name| message.contains(name.as_str()))
}

/// `message` names no part of `path` below the system's temp folder and no
/// temporary file: the reason a file failed reaches the window, which never
/// receives a path.
pub fn assert_no_path(message: &str, path: &Path) {
    if let Some(name) = leaked_name(message, path, &std::env::temp_dir()) {
        panic!("{name:?} in {message:?}");
    }
}
