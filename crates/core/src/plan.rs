//! Planning: decide, for each input, whether it is optimised, where the output
//! goes, or why it is left alone. Nothing here writes to the filesystem.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Job, Skip};

/// Largest file that is planned, in bytes (100 MiB). One byte more is skipped.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Most files a single run optimises. Further optimisable files are skipped.
pub const MAX_RUN_FILES: usize = 2000;

/// Most pixels (width times height) an image may declare: 100 megapixels. A
/// small file can declare huge dimensions, and decoding it would allocate for
/// all of them, so a larger image is refused before it is decoded.
pub const MAX_PIXELS: u64 = 100_000_000;

/// Deepest element nesting an SVG may have: the root `<svg>` is level 1. oxvg
/// walks the tree recursively (parse, every job, the writer), so a deeper
/// file could overflow the stack, which aborts the process. Measured with
/// nested groups: about 7.3 KiB of stack per level in a debug build and
/// 2 KiB in release, so 128 levels need about 1 MiB and 0.25 MiB, within the
/// 2 MiB a spawned thread gets by default. oxvg's own limit, 1024 levels,
/// would need 7.4 MiB in debug and 2 MiB in release: too much for such a
/// thread. Real drawings rarely nest more than a few dozen levels.
pub const MAX_SVG_DEPTH: usize = 128;

/// Most nodes (elements, text, comments) an SVG may parse into: roxmltree's
/// own `nodes_limit`. Memory and time in oxvg grow with the node count
/// (measured: about 3 KiB and 75 µs per element in release), so this caps
/// one file at about 3 GB and a minute and a quarter. It also caps the
/// markup that entity references can add.
pub const MAX_SVG_NODES: u32 = 1_000_000;

/// Most bytes the entity references of an SVG may expand to (16 MiB),
/// estimated from above before the file is parsed. roxmltree refuses a
/// "billion laughs" (more than 255 references inside one reference), but not
/// many references to one large entity, which grow quadratically: an 11 KB
/// file expanded to 200 MB in a measurement. See `svg::check_entities`.
pub const MAX_SVG_ENTITY_BYTES: u64 = 16 * 1024 * 1024;

/// Longest the worker process may spend on one file before the runner stops
/// it: 10 minutes.
///
/// The slowest file within the limits is a PNG of [`MAX_PIXELS`] at oxipng
/// `-o 4`. Measured in release on a 10-core machine: a 25-megapixel
/// photo-like RGB PNG took 15.6 s wall (19.7 s CPU, so mostly one core) and
/// 0.5 GB; time grows with the pixel count, so 100 megapixels is about 65 s.
/// A laptop with slow cores may be three times slower: about 3 to 4
/// minutes. An SVG at [`MAX_SVG_NODES`] takes about 75 s. Ten minutes leaves
/// room above both, and still ends a hung worker within one coffee break.
pub const WORKER_TIMEOUT: Duration = Duration::from_secs(600);

/// Most bytes the runner reads from a worker's stdout, and separately from
/// its stderr: 64 KiB. A result record is at most a few KiB (the worker cuts
/// a message to [`MAX_WORKER_MESSAGE_BYTES`]), and a crash report on stderr
/// is a few lines. A worker that writes more is stopped and its file counts
/// as crashed, so a runaway child cannot fill the parent's memory.
pub const MAX_WORKER_OUTPUT_BYTES: usize = 64 * 1024;

/// Longest error message a worker puts in its result record, in bytes
/// (4 KiB), so that the record always fits in [`MAX_WORKER_OUTPUT_BYTES`]. A
/// longer message is cut at a character boundary and ends in `…`.
pub const MAX_WORKER_MESSAGE_BYTES: usize = 4 * 1024;

/// Name of the output folder used when a whole folder is planned.
pub(crate) const OUTPUT_DIR: &str = "optimized";

/// Prefix of the output name used when a single file is planned.
const OUTPUT_PREFIX: &str = "optimized-";

/// Plan a run over an explicit list of files. A directory among the inputs is
/// planned exactly like [`plan_folder`]. Never writes to the filesystem.
pub fn plan_files(inputs: &[PathBuf]) -> Vec<Job> {
    let mut plan = Plan::default();
    for input in inputs {
        match fs::metadata(input) {
            Ok(meta) if meta.is_dir() => plan.add_folder(input),
            Ok(meta) if meta.is_file() => {
                let dest = input.with_file_name(prefixed_name(input));
                plan.add_file(input, dest, meta.len());
            }
            _ => plan.jobs.push(Job::Skip {
                src: input.clone(),
                reason: Skip::NotAFile,
            }),
        }
    }
    plan.jobs
}

/// Plan a run over the files directly inside `dir` (not recursive). Never
/// writes to the filesystem.
pub fn plan_folder(dir: &Path) -> Vec<Job> {
    let mut plan = Plan::default();
    plan.add_folder(dir);
    plan.jobs
}

/// Accumulates jobs for one call, so the run limit and the duplicate check
/// span every input of that call.
#[derive(Default)]
struct Plan {
    jobs: Vec<Job>,
    /// Canonical paths of the sources already seen.
    seen: HashSet<PathBuf>,
    /// Number of `Job::Optimize` entries so far.
    optimizing: usize,
}

impl Plan {
    /// Add the regular files directly inside `dir`, sorted by file name.
    /// Subfolders (including `optimized/`) and symlinks are ignored.
    fn add_folder(&mut self, dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            self.jobs.push(Job::Skip {
                src: dir.to_path_buf(),
                reason: Skip::NotAFile,
            });
            return;
        };
        let mut files: Vec<(OsString, u64)> = entries
            .flatten()
            .filter_map(|entry| {
                // `DirEntry::file_type` does not follow symlinks.
                let is_file = entry.file_type().ok()?.is_file();
                let len = entry.metadata().ok()?.len();
                is_file.then(|| (entry.file_name(), len))
            })
            .collect();
        files.sort();

        let out_dir = dir.join(OUTPUT_DIR);
        for (name, len) in files {
            self.add_file(&dir.join(&name), out_dir.join(&name), len);
        }
    }

    /// Decide one existing regular file: skip it for a reason, or plan it.
    fn add_file(&mut self, src: &Path, dest: PathBuf, len: u64) {
        // Checked first so that a file given twice is dropped, not re-skipped.
        let canonical_src = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
        if !self.seen.insert(canonical_src.clone()) {
            return;
        }
        let src = src.to_path_buf();
        let reason = if !has_supported_extension(&src) {
            Some(Skip::UnsupportedType)
        } else if len > MAX_FILE_BYTES {
            Some(Skip::TooLarge)
        } else if resolve(&dest) == canonical_src {
            Some(Skip::IsOwnDestination)
        } else if self.optimizing >= MAX_RUN_FILES {
            Some(Skip::OverRunLimit)
        } else {
            None
        };
        match reason {
            Some(reason) => self.jobs.push(Job::Skip { src, reason }),
            None => {
                self.optimizing += 1;
                self.jobs.push(Job::Optimize { src, dest });
            }
        }
    }
}

/// `optimized-<file name>`, built from the `OsStr` so that nothing is lost on
/// names that are not valid UTF-8.
fn prefixed_name(path: &Path) -> OsString {
    let mut name = OsString::from(OUTPUT_PREFIX);
    name.push(path.file_name().unwrap_or_default());
    name
}

/// The file formats Lunaepix handles, told apart by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Png,
    Jpg,
    Svg,
    Webp,
}

/// Case-insensitive match of the extension on `png`, `jpg`/`jpeg`, `svg` and
/// `webp`.
/// The planner and `optimize_file` both decide the format here.
pub(crate) fn format_of(path: &Path) -> Option<Format> {
    let ext = path.extension()?;
    let is = |name: &str| ext.eq_ignore_ascii_case(OsStr::new(name));
    if is("png") {
        Some(Format::Png)
    } else if is("jpg") || is("jpeg") {
        Some(Format::Jpg)
    } else if is("svg") {
        Some(Format::Svg)
    } else if is("webp") {
        Some(Format::Webp)
    } else {
        None
    }
}

fn has_supported_extension(path: &Path) -> bool {
    format_of(path).is_some()
}

/// Canonical form of a path that may not exist yet. If the path exists, it is
/// canonicalised whole, which resolves a symlink at the destination. Otherwise
/// the nearest existing parent is canonicalised and the remaining names are
/// joined back on.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    resolve(parent).join(name)
}

/// Refuse an image whose declared dimensions exceed [`MAX_PIXELS`]. The error
/// is a message for the user.
pub(crate) fn check_pixels(width: u32, height: u32) -> Result<(), String> {
    // Both factors fit in 32 bits, so the product cannot overflow 64.
    if u64::from(width) * u64::from(height) <= MAX_PIXELS {
        return Ok(());
    }
    Err(format!(
        "image too large: {width}×{height} exceeds {} megapixels",
        MAX_PIXELS / 1_000_000
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Skip;

    #[test]
    fn accepts_an_image_of_exactly_the_pixel_limit() {
        assert_eq!(check_pixels(10_000, 10_000), Ok(()));
        assert_eq!(check_pixels(1, 1), Ok(()));
        assert_eq!(check_pixels(100_000_000, 1), Ok(()));
    }

    #[test]
    fn refuses_an_image_one_row_over_the_pixel_limit() {
        let err = check_pixels(10_000, 10_001).unwrap_err();
        assert_eq!(err, "image too large: 10000×10001 exceeds 100 megapixels");
        assert!(check_pixels(100_000_001, 1).is_err());
    }

    #[test]
    fn refuses_the_largest_declarable_dimensions_without_overflowing() {
        assert!(check_pixels(65_535, 65_535).is_err());
        assert!(check_pixels(u32::MAX, u32::MAX).is_err());
    }
    use std::ffi::OsString;
    use std::fs::{self, File};
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        File::create(&path).unwrap();
        path
    }

    fn sized(dir: &Path, name: &str, len: u64) -> PathBuf {
        let path = dir.join(name);
        File::create(&path).unwrap().set_len(len).unwrap();
        path
    }

    fn optimize(src: PathBuf, dest: PathBuf) -> Job {
        Job::Optimize { src, dest }
    }

    fn skip(src: PathBuf, reason: Skip) -> Job {
        Job::Skip { src, reason }
    }

    fn count_optimize(jobs: &[Job]) -> usize {
        jobs.iter()
            .filter(|j| matches!(j, Job::Optimize { .. }))
            .count()
    }

    fn count_skip(jobs: &[Job], wanted: Skip) -> usize {
        jobs.iter()
            .filter(|j| matches!(j, Job::Skip { reason, .. } if *reason == wanted))
            .count()
    }

    fn listing(dir: &Path) -> Vec<OsString> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    // --- naming -----------------------------------------------------------

    #[test]
    fn plans_an_uppercase_extension_to_optimized_prefix_in_the_same_folder() {
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "a.PNG");
        let jobs = plan_files(std::slice::from_ref(&src));
        assert_eq!(
            jobs,
            vec![optimize(src, tmp.path().join("optimized-a.PNG"))]
        );
    }

    #[test]
    fn accepts_png_jpg_jpeg_and_svg_in_any_letter_case() {
        let tmp = TempDir::new().unwrap();
        let names = ["a.png", "b.Jpg", "c.JPEG", "d.sVg", "e.jpeg"];
        let inputs: Vec<PathBuf> = names.iter().map(|n| touch(tmp.path(), n)).collect();
        let jobs = plan_files(&inputs);
        assert_eq!(count_optimize(&jobs), names.len());
    }

    #[test]
    fn skips_gif_and_lookalike_extensions_as_unsupported() {
        let tmp = TempDir::new().unwrap();
        let names = ["a.gif", "a.webpx", "a.web"];
        let inputs: Vec<PathBuf> = names.iter().map(|n| touch(tmp.path(), n)).collect();
        let jobs = plan_files(&inputs);
        assert_eq!(
            jobs,
            inputs
                .into_iter()
                .map(|src| skip(src, Skip::UnsupportedType))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn plans_webp_in_either_letter_case() {
        let tmp = TempDir::new().unwrap();
        let lower = touch(tmp.path(), "a.webp");
        let upper = touch(tmp.path(), "a.WEBP");
        let jobs = plan_files(&[lower.clone(), upper.clone()]);
        assert_eq!(
            jobs,
            vec![
                optimize(lower, tmp.path().join("optimized-a.webp")),
                optimize(upper, tmp.path().join("optimized-a.WEBP")),
            ]
        );
    }

    #[test]
    fn skips_files_without_an_extension_and_names_that_only_end_in_a_type() {
        let tmp = TempDir::new().unwrap();
        let bare = touch(tmp.path(), "png");
        let hidden = touch(tmp.path(), ".png");
        let tricky = touch(tmp.path(), "a.png.txt");
        let jobs = plan_files(&[bare, hidden, tricky]);
        assert_eq!(count_skip(&jobs, Skip::UnsupportedType), 3);
        assert_eq!(count_optimize(&jobs), 0);
    }

    #[test]
    fn plans_a_name_with_spaces_and_non_ascii_characters() {
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "Mobilis Logo – zwart é.png");
        let jobs = plan_files(std::slice::from_ref(&src));
        assert_eq!(
            jobs,
            vec![optimize(
                src,
                tmp.path().join("optimized-Mobilis Logo – zwart é.png")
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn plans_a_name_that_is_not_valid_utf8_without_altering_it() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let tmp = TempDir::new().unwrap();
        let mut bytes = b"caf".to_vec();
        bytes.push(0xE9); // Latin-1 e-acute, invalid UTF-8
        bytes.extend_from_slice(b".png");
        let src = tmp.path().join(OsString::from_vec(bytes.clone()));
        File::create(&src).unwrap();

        let jobs = plan_files(&[src]);
        let mut expected = b"optimized-".to_vec();
        expected.extend_from_slice(&bytes);
        let Some(Job::Optimize { dest, .. }) = jobs.first() else {
            panic!("expected an Optimize job, got {jobs:?}");
        };
        assert_eq!(dest.file_name().unwrap().as_bytes(), expected.as_slice());
    }

    // --- folders ----------------------------------------------------------

    #[test]
    fn plans_three_images_in_a_folder_into_its_optimized_subfolder() {
        let tmp = TempDir::new().unwrap();
        let a = touch(tmp.path(), "a.png");
        let b = touch(tmp.path(), "b.jpg");
        let c = touch(tmp.path(), "c.svg");
        let out = tmp.path().join("optimized");
        assert_eq!(
            plan_folder(tmp.path()),
            vec![
                optimize(a, out.join("a.png")),
                optimize(b, out.join("b.jpg")),
                optimize(c, out.join("c.svg")),
            ]
        );
    }

    #[test]
    fn plans_webp_files_in_a_folder_in_either_letter_case_and_skips_lookalikes() {
        let tmp = TempDir::new().unwrap();
        let lower = touch(tmp.path(), "a.webp");
        let upper = touch(tmp.path(), "b.WEBP");
        let lookalike = touch(tmp.path(), "c.webpx");
        let out = tmp.path().join("optimized");
        assert_eq!(
            plan_folder(tmp.path()),
            vec![
                optimize(lower, out.join("a.webp")),
                optimize(upper, out.join("b.WEBP")),
                skip(lookalike, Skip::UnsupportedType),
            ]
        );
    }

    #[test]
    fn orders_a_folders_jobs_by_file_name() {
        let tmp = TempDir::new().unwrap();
        for name in ["m.png", "z.png", "b.png", "a.gif"] {
            touch(tmp.path(), name);
        }
        let names: Vec<_> = plan_folder(tmp.path())
            .iter()
            .map(|j| match j {
                Job::Optimize { src, .. } | Job::Skip { src, .. } => {
                    src.file_name().unwrap().to_owned()
                }
            })
            .collect();
        assert_eq!(names, ["a.gif", "b.png", "m.png", "z.png"]);
    }

    #[test]
    fn does_not_plan_images_inside_subfolders() {
        let tmp = TempDir::new().unwrap();
        let top = touch(tmp.path(), "top.png");
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        touch(&sub, "deep.png");
        assert_eq!(
            plan_folder(tmp.path()),
            vec![optimize(top, tmp.path().join("optimized").join("top.png"))]
        );
    }

    #[test]
    fn does_not_plan_a_file_already_inside_the_optimized_folder() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("optimized");
        fs::create_dir(&out).unwrap();
        touch(&out, "done.png");
        assert_eq!(plan_folder(tmp.path()), vec![]);
    }

    #[test]
    fn plans_nothing_for_an_empty_folder() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(plan_folder(tmp.path()), vec![]);
    }

    #[test]
    fn skips_a_path_that_is_not_a_folder_when_planning_a_folder() {
        let tmp = TempDir::new().unwrap();
        let file = touch(tmp.path(), "a.png");
        assert_eq!(plan_folder(&file), vec![skip(file, Skip::NotAFile)]);
        let missing = tmp.path().join("nope");
        assert_eq!(plan_folder(&missing), vec![skip(missing, Skip::NotAFile)]);
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinks_inside_a_folder_without_following_them() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let real = touch(tmp.path(), "a.png");
        symlink(&real, tmp.path().join("link.png")).unwrap();
        let elsewhere = TempDir::new().unwrap();
        touch(elsewhere.path(), "far.png");
        symlink(elsewhere.path(), tmp.path().join("linked-dir")).unwrap();

        assert_eq!(
            plan_folder(tmp.path()),
            vec![optimize(real, tmp.path().join("optimized").join("a.png"))]
        );
    }

    #[test]
    fn plans_a_directory_given_to_plan_files_exactly_like_plan_folder() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.png");
        touch(tmp.path(), "b.gif");
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        touch(&sub, "deep.png");
        assert_eq!(
            plan_files(&[tmp.path().to_path_buf()]),
            plan_folder(tmp.path())
        );
    }

    #[test]
    fn plans_loose_files_next_to_themselves_not_into_an_optimized_folder() {
        // The negative of the folder rule: a file picked directly is not
        // routed through `optimized/`.
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "a.png");
        let jobs = plan_files(&[src]);
        let Some(Job::Optimize { dest, .. }) = jobs.first() else {
            panic!("expected an Optimize job, got {jobs:?}");
        };
        assert_eq!(dest.parent().unwrap(), tmp.path());
    }

    // --- planning is pure -------------------------------------------------

    #[test]
    fn writes_nothing_while_planning() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.png");
        touch(tmp.path(), "b.jpg");
        let before = listing(tmp.path());

        let folder_jobs = plan_folder(tmp.path());
        let file_jobs = plan_files(&[tmp.path().join("a.png")]);

        // Jobs were planned, so the unchanged folder is not just "no work".
        assert_eq!(count_optimize(&folder_jobs), 2);
        assert_eq!(count_optimize(&file_jobs), 1);
        assert!(!tmp.path().join("optimized").exists());
        assert_eq!(listing(tmp.path()), before);
    }

    // --- empty and missing inputs ----------------------------------------

    #[test]
    fn returns_an_empty_plan_for_a_cancelled_dialog() {
        assert_eq!(plan_files(&[]), vec![]);
    }

    #[test]
    fn skips_a_missing_path_as_not_a_file() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("gone.png");
        assert_eq!(
            plan_files(std::slice::from_ref(&missing)),
            vec![skip(missing, Skip::NotAFile)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn skips_a_socket_as_not_a_file() {
        use std::os::unix::net::UnixListener;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("s.png");
        let _listener = UnixListener::bind(&path).unwrap();
        assert_eq!(
            plan_files(std::slice::from_ref(&path)),
            vec![skip(path, Skip::NotAFile)]
        );
    }

    // --- size limit -------------------------------------------------------

    #[test]
    fn plans_a_file_of_exactly_the_size_limit() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(MAX_FILE_BYTES, 104_857_600);
        let src = sized(tmp.path(), "edge.png", 104_857_600);
        assert_eq!(
            plan_files(std::slice::from_ref(&src)),
            vec![optimize(src, tmp.path().join("optimized-edge.png"))]
        );
    }

    #[test]
    fn skips_a_file_one_byte_over_the_size_limit_as_too_large() {
        let tmp = TempDir::new().unwrap();
        let src = sized(tmp.path(), "big.png", 104_857_601);
        assert_eq!(
            plan_files(std::slice::from_ref(&src)),
            vec![skip(src, Skip::TooLarge)]
        );
    }

    #[test]
    fn skips_an_oversized_file_in_a_folder_but_plans_its_neighbours() {
        let tmp = TempDir::new().unwrap();
        let big = sized(tmp.path(), "a-big.png", MAX_FILE_BYTES + 1);
        let ok = touch(tmp.path(), "b-ok.png");
        assert_eq!(
            plan_folder(tmp.path()),
            vec![
                skip(big, Skip::TooLarge),
                optimize(ok, tmp.path().join("optimized").join("b-ok.png")),
            ]
        );
    }

    // --- run limit --------------------------------------------------------

    fn many(dir: &Path, prefix: &str, n: usize) {
        for i in 0..n {
            File::create(dir.join(format!("{prefix}{i:04}.png"))).unwrap();
        }
    }

    #[test]
    fn plans_exactly_the_run_limit_of_files() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(MAX_RUN_FILES, 2000);
        many(tmp.path(), "img", 2000);
        let jobs = plan_folder(tmp.path());
        assert_eq!(jobs.len(), 2000);
        assert_eq!(count_optimize(&jobs), 2000);
    }

    #[test]
    fn skips_the_file_after_the_run_limit_as_over_the_run_limit() {
        let tmp = TempDir::new().unwrap();
        many(tmp.path(), "img", 2001);
        let jobs = plan_folder(tmp.path());
        assert_eq!(count_optimize(&jobs), 2000);
        assert_eq!(
            jobs[2000],
            skip(tmp.path().join("img2000.png"), Skip::OverRunLimit)
        );
    }

    #[test]
    fn does_not_count_skipped_inputs_against_the_run_limit() {
        let tmp = TempDir::new().unwrap();
        many(tmp.path(), "img", 2000);
        for i in 0..50 {
            touch(tmp.path(), &format!("a-note{i:02}.gif"));
        }
        sized(tmp.path(), "a-huge.png", MAX_FILE_BYTES + 1);
        let jobs = plan_folder(tmp.path());
        assert_eq!(count_optimize(&jobs), 2000);
        assert_eq!(count_skip(&jobs, Skip::OverRunLimit), 0);
    }

    #[test]
    fn counts_the_run_limit_across_files_and_folders_together() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        many(first.path(), "x", 1500);
        many(second.path(), "y", 600);
        let loose_dir = TempDir::new().unwrap();
        let loose = touch(loose_dir.path(), "z-loose.png");
        let jobs = plan_files(&[
            first.path().to_path_buf(),
            second.path().to_path_buf(),
            loose,
        ]);
        assert_eq!(count_optimize(&jobs), 2000);
        // 100 left over from the second folder, plus the loose file.
        assert_eq!(count_skip(&jobs, Skip::OverRunLimit), 101);
    }

    // --- destination never equals the source ------------------------------

    #[test]
    fn plans_a_destination_that_differs_from_its_source() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.png");
        touch(tmp.path(), "b.png");
        let mut jobs = plan_folder(tmp.path());
        jobs.extend(plan_files(&[tmp.path().join("a.png")]));
        assert_eq!(count_optimize(&jobs), 3);
        for job in &jobs {
            let Job::Optimize { src, dest } = job else {
                panic!("unexpected {job:?}")
            };
            assert_ne!(src, dest);
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_source_whose_destination_is_a_symlink_back_to_it() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "a.png");
        symlink(&src, tmp.path().join("optimized-a.png")).unwrap();
        assert_eq!(
            plan_files(std::slice::from_ref(&src)),
            vec![skip(src, Skip::IsOwnDestination)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_files_in_a_folder_whose_optimized_folder_points_back_at_the_folder() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "a.png");
        symlink(tmp.path(), tmp.path().join("optimized")).unwrap();
        assert_eq!(
            plan_folder(tmp.path()),
            vec![skip(src, Skip::IsOwnDestination)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn plans_a_source_whose_destination_symlink_points_at_another_file() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "a.png");
        let other = touch(tmp.path(), "other.png");
        symlink(&other, tmp.path().join("optimized-a.png")).unwrap();
        let jobs = plan_files(std::slice::from_ref(&src));
        assert_eq!(
            jobs,
            vec![optimize(src, tmp.path().join("optimized-a.png"))]
        );
    }

    // --- duplicates -------------------------------------------------------

    #[test]
    fn plans_the_same_file_given_twice_only_once() {
        let tmp = TempDir::new().unwrap();
        let src = touch(tmp.path(), "a.png");
        let respelled = tmp.path().join(".").join("a.png");
        let jobs = plan_files(&[src.clone(), src.clone(), respelled]);
        assert_eq!(
            jobs,
            vec![optimize(src, tmp.path().join("optimized-a.png"))]
        );
    }

    #[test]
    fn plans_two_different_files_with_the_same_name_in_different_folders_both() {
        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        let a = touch(one.path(), "a.png");
        let b = touch(two.path(), "a.png");
        assert_eq!(count_optimize(&plan_files(&[a, b])), 2);
    }

    #[test]
    fn plans_a_file_once_when_it_is_given_directly_and_through_its_folder() {
        let tmp = TempDir::new().unwrap();
        let a = touch(tmp.path(), "a.png");
        touch(tmp.path(), "b.png");
        let jobs = plan_files(&[a, tmp.path().to_path_buf()]);
        assert_eq!(count_optimize(&jobs), 2);
    }

    #[cfg(unix)]
    #[test]
    fn plans_a_file_once_when_it_is_also_given_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let a = touch(tmp.path(), "a.png");
        let link = tmp.path().join("alias.png");
        symlink(&a, &link).unwrap();
        let jobs = plan_files(&[a, link]);
        assert_eq!(count_optimize(&jobs), 1);
    }
}
