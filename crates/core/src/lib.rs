//! Lunaepix core: lossless PNG, JPG and SVG optimisation.
//!
//! All logic and all tests live here. The CLI and the Tauri app are thin
//! front ends over this crate.
//!
//! The `plan_*` functions never write to the filesystem; destination folders
//! are created only when a job runs.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod atomic;
mod plan;
mod png;

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
            Skip::OverRunLimit => {
                return write!(f, "Too many files in one run (max {MAX_RUN_FILES})");
            }
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
    /// Reading or writing `path` failed. The cause is part of the message and
    /// deliberately not a `source()`, so an error chain does not print it twice.
    #[error("{path}: {cause}", path = path.display())]
    Io { path: PathBuf, cause: io::Error },
    /// The file is not a PNG, JPG or SVG.
    #[error("{path}: unsupported file type", path = path.display())]
    UnsupportedType { path: PathBuf },
    /// A supported format whose optimiser is not built yet (JPG: Step 5, SVG:
    /// Step 6). Nothing is written.
    #[error("{path}: {format} files cannot be optimised yet", path = path.display())]
    NotYetSupported { path: PathBuf, format: &'static str },
    /// The file has a supported extension but its content could not be
    /// parsed, or the optimised result did not pass the lossless check.
    #[error("{path}: {message}", path = path.display())]
    Invalid { path: PathBuf, message: String },
}

/// Optimise `src` losslessly and write the result to `dest`.
///
/// The format is chosen by the extension, case-insensitively, exactly as the
/// planner does. `src` is only ever read. `dest` is written atomically (the
/// `write_atomically` helper) and its folder is created if missing, only
/// when there is a result to write. If anything fails, `dest` is not touched
/// and no temporary file is left behind. The output gets the source's
/// permissions. A `dest` that is the same file as `src` (after resolving
/// symlinks and `.`/`..`) is refused, and so is a `dest` whose folder is not
/// the source's folder or its `optimized/` subfolder (checked before anything
/// is created). `Outcome::millis` is the wall time of
/// the whole call: read, optimise and write.
pub fn optimize_file(src: &Path, dest: &Path) -> Result<Outcome, OptimizeError> {
    let started = Instant::now();
    let format = plan::format_of(src).ok_or_else(|| OptimizeError::UnsupportedType {
        path: src.to_path_buf(),
    })?;
    let optimize: fn(&[u8]) -> Result<Vec<u8>, String> = match format {
        plan::Format::Png => png::optimize,
        plan::Format::Jpg => {
            return Err(OptimizeError::NotYetSupported {
                path: src.to_path_buf(),
                format: "JPG",
            });
        }
        plan::Format::Svg => {
            return Err(OptimizeError::NotYetSupported {
                path: src.to_path_buf(),
                format: "SVG",
            });
        }
    };

    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |cause| OptimizeError::Io { path, cause }
    };
    // Writing the output over the source would destroy the original.
    if plan::resolve(src) == plan::resolve(dest) {
        return Err(OptimizeError::Invalid {
            path: src.to_path_buf(),
            message: "the source and the destination are the same file".to_owned(),
        });
    }
    // Opened read-only; the same handle gives the size, the permissions and
    // the bytes, so they all describe one file.
    let mut file = fs::File::open(src).map_err(io_error(src))?;
    let permissions = file.metadata().map_err(io_error(src))?.permissions();
    let mut original = Vec::new();
    file.read_to_end(&mut original).map_err(io_error(src))?;
    let optimized = optimize(&original).map_err(|message| OptimizeError::Invalid {
        path: src.to_path_buf(),
        message,
    })?;
    ensure_destination_folder(src, dest)?;
    atomic::write_atomically(dest, &optimized, permissions).map_err(io_error(dest))?;

    Ok(Outcome {
        before: original.len() as u64,
        after: optimized.len() as u64,
        millis: started.elapsed().as_millis() as u64,
    })
}

/// Output may only go to the source's own folder (`optimized-<name>` beside
/// it) or to its `optimized/` subfolder, which are the two places the planner
/// chooses. Both folders are compared after resolving symlinks, so the user's
/// own folder may itself be reached through a link, but an `optimized` that is
/// a link to anywhere else is refused. Nothing is created or written here: a
/// folder that does not exist yet is resolved through its nearest existing
/// parent.
///
/// The check and the write are two steps, so a link swapped in between them is
/// not caught; the folders are the user's own.
fn ensure_destination_folder(src: &Path, dest: &Path) -> Result<(), OptimizeError> {
    let source_folder = plan::resolve(atomic::folder_of(src));
    let dest_folder = plan::resolve(atomic::folder_of(dest));
    if dest_folder == source_folder || dest_folder == source_folder.join(plan::OUTPUT_DIR) {
        return Ok(());
    }
    Err(OptimizeError::Invalid {
        path: dest.to_path_buf(),
        message: format!(
            "the destination must be the source's folder or its '{}' folder",
            plan::OUTPUT_DIR
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reasons_are_human_readable() {
        assert_eq!(
            Skip::UnsupportedType.to_string(),
            "Not a PNG, JPG or SVG file"
        );
        assert_eq!(Skip::NotAFile.to_string(), "Not a readable file");
        assert_ne!(Skip::TooLarge.to_string(), Skip::OverRunLimit.to_string());
    }

    #[test]
    fn the_run_limit_skip_names_the_file_count_limit() {
        assert_eq!(
            Skip::OverRunLimit.to_string(),
            "Too many files in one run (max 2000)"
        );
        // The old text spoke of a size limit; the limit is a file count.
        assert!(!Skip::OverRunLimit.to_string().contains("size"));
    }
}
