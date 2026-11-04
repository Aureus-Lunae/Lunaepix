//! Lunaepix core: lossless PNG, JPG and SVG optimisation.
//!
//! All logic and all tests live here. The CLI and the Tauri app are thin
//! front ends over this crate.
//!
//! The `plan_*` functions never write to the filesystem; destination folders
//! are created only when a job runs.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

mod plan;

pub use plan::{MAX_FILE_BYTES, MAX_RUN_FILES, plan_files, plan_folder};

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
    /// The run already holds the maximum number of files to optimise.
    OverRunLimit,
    /// The file is itself in the destination folder, so optimising it would
    /// overwrite its own output.
    IsOwnDestination,
    /// The input does not exist or is not a regular file (for example a
    /// socket), or it is a folder that could not be read.
    NotAFile,
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Skip::UnsupportedType => "Not a PNG, JPG or SVG file",
            Skip::TooLarge => "File is too large",
            Skip::OverRunLimit => "Run size limit reached",
            Skip::IsOwnDestination => "Already in the output folder",
            Skip::NotAFile => "Not a readable file",
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
    todo!("implemented in Step 4")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reasons_are_human_readable() {
        assert_eq!(Skip::UnsupportedType.to_string(), "Not a PNG, JPG or SVG file");
        assert_eq!(Skip::NotAFile.to_string(), "Not a readable file");
        assert_ne!(Skip::TooLarge.to_string(), Skip::OverRunLimit.to_string());
    }
}
