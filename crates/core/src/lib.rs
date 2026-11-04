//! Lunaepix core: lossless PNG, JPG and SVG optimisation.
//!
//! All logic and all tests live here. The CLI and the Tauri app are thin
//! front ends over this crate.
//!
//! Scaffold: the three functions below compile but are not implemented yet.
//! The `plan_*` functions will never write to the filesystem; destination
//! folders are created only when a job runs.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Result of optimising one file. Sizes are in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub before: u64,
    pub after: u64,
    pub millis: u64,
}

/// Why a file is left alone. The `Display` text is shown to the user in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Not a PNG, JPG or SVG file.
    UnsupportedType,
    /// Larger than the per-file size limit.
    TooLarge,
    /// Would take the run over the total size limit.
    OverRunLimit,
    /// The file is itself in the destination folder, so optimising it would
    /// overwrite its own output.
    IsOwnDestination,
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Skip::UnsupportedType => "Not a PNG, JPG or SVG file",
            Skip::TooLarge => "File is too large",
            Skip::OverRunLimit => "Run size limit reached",
            Skip::IsOwnDestination => "Already in the output folder",
        })
    }
}

/// One planned unit of work, or the reason a file will not be touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Optimise `src` and write the result to `dest`.
    Optimize { src: PathBuf, dest: PathBuf },
    /// Leave `src` alone, for the given reason.
    Skip { src: PathBuf, reason: Skip },
}

/// A file that failed while being optimised.
#[derive(Debug, thiserror::Error)]
pub enum OptimizeError {
    /// Reading or writing `path` failed.
    #[error("{path}: {source}", path = path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file is not a PNG, JPG or SVG.
    #[error("{path}: unsupported file type", path = path.display())]
    UnsupportedType { path: PathBuf },
    /// The file has a supported extension but its content could not be
    /// parsed, or the optimised result did not pass the lossless check.
    #[error("{path}: {message}", path = path.display())]
    Invalid { path: PathBuf, message: String },
}

/// Optimise `src` losslessly and write the result to `dest`.
pub fn optimize_file(src: &Path, dest: &Path) -> Result<Outcome, OptimizeError> {
    let _ = (src, dest);
    todo!("implemented in Step 3")
}

/// Plan a run over an explicit list of files. Never writes to the filesystem.
pub fn plan_files(inputs: &[PathBuf]) -> Vec<Job> {
    let _ = inputs;
    todo!("implemented in Step 3")
}

/// Plan a run over the files directly inside `dir` (not recursive). Never
/// writes to the filesystem.
pub fn plan_folder(dir: &Path) -> Vec<Job> {
    let _ = dir;
    todo!("implemented in Step 3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reasons_are_human_readable() {
        assert_eq!(Skip::UnsupportedType.to_string(), "Not a PNG, JPG or SVG file");
        assert_ne!(Skip::TooLarge.to_string(), Skip::OverRunLimit.to_string());
    }

    #[test]
    fn outcome_holds_sizes() {
        let o = Outcome { before: 10, after: 4, millis: 1 };
        assert!(o.after < o.before);
    }
}
