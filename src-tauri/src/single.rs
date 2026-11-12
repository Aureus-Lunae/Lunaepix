//! Single image mode's state: which view shows, the loaded image (a private
//! copy taken at load), the last drop that left items out, and the slot for
//! an in-flight preview's stop handle. No Tauri here: the probe comes in as a
//! closure, so all of it is tested without a window or a worker.
//!
//! The private folder is made once per launch, the first time an image is
//! loaded, with `tempfile` (mode 0700 on Unix, in the user's own temp folder
//! on Windows), and removed by [`Single::shutdown`] when the window closes. Each
//! image gets a subfolder `image-<key>` holding its copy, `original<.ext>`.
//! Keys start at 1 and only go up during a launch.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lunaepix_core::{
    Crash, MAX_FILE_BYTES, ProbeAnswer, RunError, Skip, StopHandle, batch_output_name,
    first_free_name, has_supported_extension,
};
use serde_json::Value;
use tempfile::TempDir;

use crate::events::{ImageChanged, ImageInfo, NameSuggested};
use crate::session::{CommandError, lock};

/// Which mode the window shows, so a drop goes there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum View {
    /// Every launch starts here.
    #[default]
    Single,
    Batch,
}

/// The view from `set_view`'s whole argument object: exactly
/// `{ "view": "single" }` or `{ "view": "batch" }`. Anything else (another
/// spelling, another type, `null`, a missing or an extra key, no JSON at all)
/// is [`CommandError::InvalidView`].
pub fn parse_view(args: Option<&Value>) -> Result<View, CommandError> {
    let args = args
        .and_then(Value::as_object)
        .filter(|args| args.len() == 1)
        .ok_or(CommandError::InvalidView)?;
    match args.get("view").and_then(Value::as_str) {
        Some("single") => Ok(View::Single),
        Some("batch") => Ok(View::Batch),
        _ => Err(CommandError::InvalidView),
    }
}

/// The notice of a choice or a drop with no PNG, JPG, SVG or WebP file in it.
pub const NOT_AN_IMAGE_NOTICE: &str = "Not a PNG, JPG, SVG or WebP file.";

/// The notice of a drop of only folders (and files that are not images).
pub const FOLDERS_GO_IN_BATCH_NOTICE: &str = "Folders go in Batch.";

/// The notice of a drop that loaded one image and left `count` (1 or more)
/// items out that Batch can take.
pub fn left_out_notice(count: usize) -> String {
    let others = if count == 1 {
        "1 other was".to_owned()
    } else {
        format!("{count} others were")
    };
    format!("Single image mode takes one image; {others} left out.")
}

/// Batch's reason for a worker that gave no valid answer, used when the probe
/// worker failed (crashed, timed out, could not start): the reason Batch
/// gives a file whose worker ended without a valid result.
pub fn probe_failed_reason() -> String {
    RunError::Crashed {
        path: PathBuf::new(),
        cause: Crash::NoValidResult,
        stderr: String::new(),
    }
    .reason()
}

/// The image Single image mode has loaded. Everything after the load works
/// from `copy`, so a file that changes on disk cannot mix versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedImage {
    /// 1 for the first image of a launch, then one more for each load.
    pub key: u64,
    /// The original's file name, its bytes as they are.
    pub file_name: OsString,
    /// The folder the original was in: where Save writes next to it.
    pub folder: PathBuf,
    /// The original's extension with its dot (`".jpg"`), as it is spelled.
    pub extension: String,
    /// The private copy, `<private folder>/image-<key>/original<.ext>`.
    pub copy: PathBuf,
    /// The original's permissions, for the saved file.
    pub permissions: fs::Permissions,
    /// The size of the copy in bytes.
    pub size: u64,
    /// The size it is shown at; `None` for an SVG.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl LoadedImage {
    /// The original's path: its folder and its file name.
    pub fn original(&self) -> PathBuf {
        self.folder.join(&self.file_name)
    }

    /// What the window hears about this image, with the name Save as starts
    /// with for the output folder `out` (`None`: next to the original).
    pub fn info(&self, out: Option<&Path>) -> ImageInfo {
        ImageInfo {
            name: self.file_name.to_string_lossy().into_owned(),
            size: self.size,
            width: self.width,
            height: self.height,
            key: self.key.to_string(),
            suggested: suggested_name(&self.original(), out),
            extension: self.extension.clone(),
        }
    }
}

/// The name Save as starts with, without the extension: the name Batch would
/// give the output of `original` ([`batch_output_name`]), or its first free
/// numbered form ([`first_free_name`]) in the folder it would be written to,
/// `out` or the original's own folder. When no number is free, the unnumbered
/// name. A name that is not valid UTF-8 is decoded lossily.
pub fn suggested_name(original: &Path, out: Option<&Path>) -> String {
    suggested_name_with(original, out, first_free_name)
}

/// [`suggested_name`] with the free-name finder given; the tests pass one
/// that finds none.
fn suggested_name_with(
    original: &Path,
    out: Option<&Path>,
    free: impl FnOnce(&Path, &OsStr) -> Option<OsString>,
) -> String {
    let folder = out.unwrap_or_else(|| original.parent().unwrap_or(Path::new("")));
    let name = batch_output_name(original, out.is_some());
    let chosen = free(folder, &name).unwrap_or(name);
    Path::new(&chosen)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// What a drop in Single image mode does with its items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropPlan {
    /// The first item that is not a folder and has a PNG, JPG, SVG or WebP
    /// extension: the one to load.
    pub load: Option<PathBuf>,
    /// The other items Batch could take: folders, and files with one of
    /// those extensions. Other files do not count; switching cannot help
    /// them.
    pub left_out: usize,
}

/// Which item of a drop to load, and how many are left out.
pub fn plan_drop(paths: &[PathBuf]) -> DropPlan {
    let mut plan = DropPlan {
        load: None,
        left_out: 0,
    };
    for path in paths {
        let is_folder = path.is_dir();
        let is_image = !is_folder && has_supported_extension(path);
        if is_image && plan.load.is_none() {
            plan.load = Some(path.clone());
        } else if is_folder || is_image {
            plan.left_out += 1;
        }
    }
    plan
}

/// A reason for the window from an I/O error, never with a path in it: the
/// operating system's own text for an OS error, else only the kind's text
/// (a wrapped error such as `tempfile`'s carries the path in its message).
pub(crate) fn io_reason(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(_) => error.to_string(),
        None => error.kind().to_string(),
    }
}

/// Copy `src` to a new file `dest`, at most `limit` bytes: the copy's size
/// and `src`'s permissions. A file that grew past `limit` since its size was
/// checked is refused with Batch's size reason, having read one byte past
/// it at most; on any failure no copy is left behind. Every reason is free
/// of paths.
pub(crate) fn copy_capped(
    src: &Path,
    dest: &Path,
    limit: u64,
) -> Result<(u64, fs::Permissions), String> {
    let file = fs::File::open(src).map_err(|error| io_reason(&error))?;
    let permissions = file
        .metadata()
        .map_err(|error| io_reason(&error))?
        .permissions();
    let mut copy = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|error| io_reason(&error))?;
    let copied = io::copy(&mut file.take(limit.saturating_add(1)), &mut copy);
    drop(copy);
    let failure = match copied {
        Ok(size) if size <= limit => return Ok((size, permissions)),
        Ok(_) => Skip::TooLarge.to_string(),
        Err(error) => io_reason(&error),
    };
    // The copy was made by this call, in a folder only this user can use.
    if let Err(error) = fs::remove_file(dest) {
        eprintln!("lunaepix: could not remove a partial copy: {error}");
    }
    Err(failure)
}

/// Remove the folder at `path` and everything in it; a failure is logged and
/// otherwise ignored. A folder that is not there counts as removed. Whether
/// it is gone.
fn remove_folder(path: &Path) -> bool {
    match fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            eprintln!("lunaepix: could not remove a private image folder: {error}");
            false
        }
    }
}

/// The reason a load is refused once the window has closed. The window is
/// gone by then, so nobody reads it.
const CLOSED: &str = "the window has closed";

/// Single image mode's state, behind one lock that is never held while a
/// file is copied or probed.
#[derive(Debug, Default)]
pub struct Single {
    state: Mutex<State>,
    /// Where the private folder is made; `None` for the system's temp folder.
    /// The tests give one they can break.
    root: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct State {
    view: View,
    /// The private folder, once made.
    folder: Option<TempDir>,
    /// The window has closed: no private folder is made again.
    closed: bool,
    image: Option<LoadedImage>,
    /// The key of the last load attempt; 0 before the first.
    last_key: u64,
    /// Every item of the last drop that left something out, for Switch to
    /// Batch; gone at the next load attempt and at a switch by hand.
    offer: Option<Vec<PathBuf>>,
    /// Stops the preview being made. Filled by the preview (Task 5); taken
    /// and used when the view goes to Batch, a run starts, a new image is
    /// loaded and the window closes.
    preview: Option<StopHandle>,
}

impl Single {
    pub fn new() -> Self {
        Self::default()
    }

    /// A state whose private folder is made inside `root`.
    #[cfg(test)]
    pub(crate) fn with_root(root: PathBuf) -> Self {
        Single {
            root: Some(root),
            ..Single::default()
        }
    }

    /// The view that shows.
    pub fn view(&self) -> View {
        lock(&self.state).view
    }

    /// Show `view`. The Switch to Batch offer goes; a switch to Batch stops
    /// the preview being made.
    pub(crate) fn set_view(&self, view: View) {
        let preview = {
            let mut state = lock(&self.state);
            state.view = view;
            state.offer = None;
            match view {
                View::Batch => state.preview.take(),
                View::Single => None,
            }
        };
        if let Some(preview) = preview {
            preview.stop();
        }
    }

    /// The loaded image, if any.
    pub fn current(&self) -> Option<LoadedImage> {
        lock(&self.state).image.clone()
    }

    /// A new key and the private folder to put its image folder in, making
    /// the private folder the first time. Refused after [`Single::shutdown`].
    fn next_key(&self) -> Result<(u64, PathBuf), String> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(CLOSED.to_owned());
        }
        let folder = match &state.folder {
            Some(folder) => folder.path().to_path_buf(),
            None => {
                let mut builder = tempfile::Builder::new();
                builder.prefix("lunaepix-");
                // tempfile makes a folder with the process's default mode
                // (0755 with the usual umask); only this user may read it.
                #[cfg(unix)]
                builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
                let made = match &self.root {
                    Some(root) => builder.tempdir_in(root),
                    None => builder.tempdir(),
                }
                .map_err(|error| io_reason(&error))?;
                let path = made.path().to_path_buf();
                state.folder = Some(made);
                path
            }
        };
        state.last_key += 1;
        Ok((state.last_key, folder))
    }

    /// Load `src`: check its extension and size, copy it into the private
    /// folder and ask `prober` about the copy. On success it is the current
    /// image, the old image's folder is removed and its preview stopped. On
    /// failure the current image stays, the attempt's folder is removed, and
    /// the reason (Batch's) is returned.
    pub(crate) fn load(
        &self,
        src: &Path,
        prober: impl Fn(&Path) -> ProbeAnswer,
    ) -> Result<(), String> {
        if !has_supported_extension(src) {
            return Err(NOT_AN_IMAGE_NOTICE.to_owned());
        }
        // Checked before anything is copied, as the planner checks: a path
        // that is not a regular file is not opened (a FIFO would block).
        let metadata = fs::metadata(src).map_err(|_| Skip::NotAFile.to_string())?;
        if !metadata.is_file() {
            return Err(Skip::NotAFile.to_string());
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Skip::TooLarge.to_string());
        }
        let (key, private) = self.next_key()?;
        let folder = private.join(format!("image-{key}"));
        // A supported extension is ASCII, so nothing is lost here.
        let extension = format!(".{}", src.extension().unwrap_or_default().to_string_lossy());
        let copy = folder.join(format!("original{extension}"));
        let loaded = fs::create_dir(&folder)
            .map_err(|error| io_reason(&error))
            .and_then(|()| copy_capped(src, &copy, MAX_FILE_BYTES))
            .and_then(|(size, permissions)| {
                let probe = match prober(&copy) {
                    ProbeAnswer::Size(probe) => probe,
                    ProbeAnswer::Refused(reason) => return Err(reason),
                    ProbeAnswer::Failed => return Err(probe_failed_reason()),
                };
                Ok(LoadedImage {
                    key,
                    file_name: src.file_name().unwrap_or_default().to_owned(),
                    folder: src.parent().unwrap_or(Path::new("")).to_path_buf(),
                    extension,
                    copy: copy.clone(),
                    permissions,
                    size,
                    width: probe.width,
                    height: probe.height,
                })
            });
        let image = match loaded {
            Ok(image) => image,
            Err(reason) => {
                remove_folder(&folder);
                return Err(reason);
            }
        };
        let (old, preview) = {
            let mut state = lock(&self.state);
            (state.image.replace(image), state.preview.take())
        };
        if let Some(preview) = preview {
            preview.stop();
        }
        if let Some(old) = old
            && let Some(old_folder) = old.copy.parent()
        {
            remove_folder(old_folder);
        }
        Ok(())
    }

    /// The `image-changed` payload: the current image (if any) with the name
    /// suggested for `out`, `left_out` and `notice`.
    pub(crate) fn image_changed(
        &self,
        left_out: usize,
        notice: Option<String>,
        out: Option<&Path>,
    ) -> ImageChanged {
        // `current` clones the image, so the folder listing behind the
        // suggested name is read without holding the lock.
        ImageChanged {
            image: self.current().map(|image| image.info(out)),
            left_out,
            notice,
        }
    }

    /// The `name-suggested` payload for `out`, when an image is loaded.
    pub(crate) fn name_suggested(&self, out: Option<&Path>) -> Option<NameSuggested> {
        self.current().map(|image| NameSuggested {
            image: image.key.to_string(),
            suggested: suggested_name(&image.original(), out),
        })
    }

    /// Keep `offer` (the items of a drop that left something out), or none.
    pub(crate) fn set_offer(&self, offer: Option<Vec<PathBuf>>) {
        lock(&self.state).offer = offer;
    }

    /// The offer, taken: a second call gives `None`.
    pub(crate) fn take_offer(&self) -> Option<Vec<PathBuf>> {
        lock(&self.state).offer.take()
    }

    /// Stop the preview being made, if one is.
    pub(crate) fn stop_preview(&self) {
        let preview = lock(&self.state).preview.take();
        if let Some(preview) = preview {
            preview.stop();
        }
    }

    /// The window closes: stop the preview, forget the image and the offer,
    /// and remove the private folder. No private folder is made after this.
    pub fn shutdown(&self) {
        let (folder, preview) = {
            let mut state = lock(&self.state);
            state.closed = true;
            state.image = None;
            state.offer = None;
            (state.folder.take(), state.preview.take())
        };
        if let Some(preview) = preview {
            preview.stop();
        }
        if let Some(folder) = folder
            && let Err(error) = folder.close()
        {
            eprintln!("lunaepix: could not remove the private folder: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use lunaepix_core::Probe;
    use tempfile::TempDir;

    use super::*;

    fn one_pixel(_: &Path) -> ProbeAnswer {
        ProbeAnswer::Size(Probe {
            width: Some(1),
            height: Some(1),
        })
    }

    /// The private folder of `single`, found from its current image's copy.
    fn private_folder(single: &Single) -> PathBuf {
        let image = single.current().expect("an image is loaded");
        image.copy.parent().unwrap().parent().unwrap().to_path_buf()
    }

    // --- picking the image out of a drop ----------------------------------------

    #[test]
    fn plan_drop_counts_folders_and_images_only() {
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("photos");
        fs::create_dir(&folder).unwrap();
        // A folder whose name looks like an image is a folder.
        let png_folder = dir.path().join("looks.png");
        fs::create_dir(&png_folder).unwrap();
        let text = dir.path().join("notes.txt");
        let first = dir.path().join("FIRST.PNG");
        let second = dir.path().join("second.webp");
        for file in [&text, &first, &second] {
            fs::write(file, b"x").unwrap();
        }

        let plan = plan_drop(&[
            text.clone(),
            png_folder.clone(),
            first.clone(),
            folder.clone(),
            second.clone(),
        ]);
        // The first image file, in any case of the extension; the folders and
        // the other image count, the text file does not.
        assert_eq!(
            plan,
            DropPlan {
                load: Some(first),
                left_out: 3
            }
        );

        // No image file: nothing to load, the folders still count.
        assert_eq!(
            plan_drop(&[text.clone(), folder]),
            DropPlan {
                load: None,
                left_out: 1
            }
        );
        assert_eq!(
            plan_drop(&[text]),
            DropPlan {
                load: None,
                left_out: 0
            }
        );
    }

    // --- the copy -----------------------------------------------------------------

    #[test]
    fn copy_capped_copies_a_file_of_exactly_the_limit() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"12345").unwrap();
        let dest = dir.path().join("copy.png");
        let (size, permissions) = copy_capped(&src, &dest, 5).unwrap();
        assert_eq!(size, 5);
        assert_eq!(fs::read(&dest).unwrap(), b"12345");
        assert_eq!(permissions, fs::metadata(&src).unwrap().permissions());
    }

    #[test]
    fn copy_capped_refuses_a_file_over_the_limit_and_leaves_no_copy() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"123456").unwrap();
        let dest = dir.path().join("copy.png");
        assert_eq!(
            copy_capped(&src, &dest, 5),
            Err("File is too large".to_owned())
        );
        assert!(!dest.exists());
    }

    #[test]
    fn copy_capped_reports_a_write_error_for_a_missing_folder() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"png").unwrap();
        let dest = dir.path().join("gone").join("copy.png");
        let reason = copy_capped(&src, &dest, 5).unwrap_err();
        assert!(!reason.is_empty());
        assert!(!reason.contains(dir.path().to_str().unwrap()), "{reason}");
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_capped_reports_a_read_error_without_a_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        fs::write(&src, b"png").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&src).is_ok() {
            // Run as root, which reads it anyway: nothing to check here.
            return;
        }
        let dest = dir.path().join("copy.png");
        let reason = copy_capped(&src, &dest, 5).unwrap_err();
        assert_eq!(
            reason,
            io::Error::from_raw_os_error(13).to_string(),
            "the OS's own text"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn io_reason_never_carries_a_path() {
        // An OS error: its own text, which has no path.
        let os = io::Error::from_raw_os_error(2);
        assert_eq!(io_reason(&os), os.to_string());
        // A wrapped error with the path in its message: only the kind's text.
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing");
        let wrapped = tempfile::Builder::new()
            .tempdir_in(&missing)
            .expect_err("no such folder");
        assert!(
            wrapped.to_string().contains(missing.to_str().unwrap()),
            "the fixture carries the path: {wrapped}"
        );
        let reason = io_reason(&wrapped);
        assert!(!reason.contains(missing.to_str().unwrap()), "{reason}");
        assert_eq!(reason, wrapped.kind().to_string());
    }

    #[test]
    fn a_folder_that_cannot_be_removed_is_logged_and_skipped() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a file, not a folder");
        fs::write(&file, b"x").unwrap();
        assert!(!remove_folder(&file));
        assert!(file.exists());
        // A folder that does not exist (any more) is not a failure.
        assert!(remove_folder(&dir.path().join("gone")));
        let folder = dir.path().join("image-1");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("original.png"), b"x").unwrap();
        assert!(remove_folder(&folder));
        assert!(!folder.exists());
    }

    // --- the suggested name ---------------------------------------------------------

    #[test]
    fn suggested_falls_back_to_the_plain_stem_when_no_number_is_free() {
        let asked = Cell::new(false);
        let original = Path::new("/srv/in/holiday.jpg");
        let suggested = suggested_name_with(original, None, |folder, name| {
            assert_eq!(folder, Path::new("/srv/in"));
            assert_eq!(name, OsStr::new("optimized-holiday.jpg"));
            asked.set(true);
            None
        });
        assert!(asked.get());
        assert_eq!(suggested, "optimized-holiday");
        // When a free name is found, that one.
        let found = suggested_name_with(original, None, |_, _| {
            Some(OsString::from("optimized-holiday-7.jpg"))
        });
        assert_eq!(found, "optimized-holiday-7");
    }

    // --- loading ------------------------------------------------------------------

    #[test]
    fn a_missing_file_or_a_folder_is_not_a_readable_file() {
        let dir = TempDir::new().unwrap();
        let single = Single::with_root(dir.path().to_path_buf());
        let called = Cell::new(0);
        let prober = |path: &Path| {
            called.set(called.get() + 1);
            one_pixel(path)
        };
        let folder = dir.path().join("folder.png");
        fs::create_dir(&folder).unwrap();
        for src in [dir.path().join("missing.png"), folder] {
            assert_eq!(
                single.load(&src, prober),
                Err("Not a readable file".to_owned()),
                "{src:?}"
            );
        }
        assert_eq!(called.get(), 0);
        assert_eq!(single.current(), None);
        // A real file is loaded.
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        assert_eq!(single.load(&png, prober), Ok(()));
        assert_eq!(called.get(), 1);
    }

    #[test]
    fn a_private_folder_that_cannot_be_made_refuses_the_load_without_a_path() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("not a folder");
        fs::write(&root, b"x").unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root.clone());
        let reason = single.load(&png, one_pixel).unwrap_err();
        assert!(!reason.is_empty());
        assert!(!reason.contains(dir.path().to_str().unwrap()), "{reason}");
        assert_eq!(single.current(), None);

        // With a root that is a folder, the same file loads.
        let single = Single::with_root(dir.path().to_path_buf());
        assert_eq!(single.load(&png, one_pixel), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn the_private_folder_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::new();
        single.load(&png, one_pixel).unwrap();
        let mode = fs::metadata(private_folder(&single))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        single.shutdown();
    }

    // --- clean-up -----------------------------------------------------------------

    #[test]
    fn shutdown_removes_the_private_folder() {
        let dir = TempDir::new().unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let single = Single::with_root(root);
        single.load(&png, one_pixel).unwrap();
        let private = private_folder(&single);
        assert!(private.is_dir());
        single.set_offer(Some(vec![png.clone()]));

        single.shutdown();
        assert!(!private.exists());
        assert_eq!(single.current(), None);
        assert_eq!(single.take_offer(), None);
        // The original is never touched.
        assert_eq!(fs::read(&png).unwrap(), b"png");
    }

    #[test]
    fn a_load_after_shutdown_makes_no_new_folder() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root.clone());
        single.shutdown();
        assert!(single.load(&png, one_pixel).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(single.current(), None);
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_logs_a_folder_it_cannot_remove_and_forgets_the_image() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let png = dir.path().join("a.png");
        fs::write(&png, b"png").unwrap();
        let single = Single::with_root(root);
        single.load(&png, one_pixel).unwrap();
        let image_folder = single
            .current()
            .unwrap()
            .copy
            .parent()
            .unwrap()
            .to_path_buf();
        fs::set_permissions(&image_folder, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(image_folder.join("probe"), b"x").is_ok() {
            // Run as root, which removes it anyway: nothing to check here.
            single.shutdown();
            return;
        }

        single.shutdown();
        assert_eq!(single.current(), None);
        assert!(image_folder.exists(), "the folder could not be removed");
        // No second private folder is made either.
        assert!(single.load(&png, one_pixel).is_err());

        fs::set_permissions(&image_folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
