//! Atomic file output, shared by the PNG, JPG, SVG and WebP optimisers.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

/// The folder a file sits in. A bare file name (`a.png`) sits in `.`.
pub(crate) fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Write `bytes` to `dest` so that `dest` is either the complete new file or
/// untouched. The bytes go to a temporary file in `dest`'s own folder (same
/// filesystem, so the rename is atomic), which is then renamed onto `dest`.
///
/// - The parent folder is created if it is missing, and only now, once there
///   is something to write. A folder created here is left in place if the
///   write then fails: an empty `optimized/` folder is harmless.
/// - The parent folder may be reached through a symlink: this helper does not
///   decide where output may go. `optimize_file` checks that the folder is the
///   source's own folder or its `optimized/` before calling this.
/// - A symlink at `dest` itself is replaced, never followed: the rename swaps
///   the directory entry, so the link's target is not written to.
/// - The file gets exactly `permissions` (the caller passes the source's, so
///   the output is never more readable than the original), except that on
///   Windows it is never read-only ([`output_permissions`]).
/// - The data is synced to disk before the rename, so a crash cannot leave
///   `dest` pointing at an empty or partial file.
/// - On any failure the temporary file is removed and `dest` is not changed.
pub(crate) fn write_atomically(
    dest: &Path,
    bytes: &[u8],
    permissions: fs::Permissions,
) -> io::Result<()> {
    let parent = folder_of(dest);
    fs::create_dir_all(parent)?;

    // Dropping a `NamedTempFile` deletes it, so every early return below
    // (including a failed `persist`) leaves no temporary file behind.
    let mut temp = new_temp_file(parent)?;
    temp.as_file_mut().write_all(bytes)?;
    temp.as_file_mut().flush()?;
    temp.as_file()
        .set_permissions(output_permissions(permissions))?;
    temp.as_file().sync_all()?;
    temp.persist(dest).map_err(|failed| failed.error)?;
    Ok(())
}

/// A new, empty temporary file in `folder` (`.tmp` and six random
/// characters), deleted again when it is dropped.
///
/// The reason a file failed reaches the window, which never receives a path,
/// so no error from here or from the writes may carry one. tempfile adds the
/// temporary file's path to the errors of `NamedTempFile::new_in` and of the
/// `NamedTempFile` `Write` impl, so the file is opened here through
/// `Builder::make_in`, which returns this `open`'s error as it is (the
/// system's, with its OS code), and the callers write through
/// `as_file_mut()`, a plain `File`. The open is tempfile's own on unix: a
/// new file, readable and writable by the owner only, until the caller sets
/// the permissions. `persist` and `persist_noclobber` return the system's
/// rename error as it is; the file they hand back is not part of the error.
fn new_temp_file(folder: &Path) -> io::Result<NamedTempFile> {
    tempfile::Builder::new()
        .make_in(folder, |path| {
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            options.open(path)
        })
        .map_err(without_path)
}

/// `error` without a path: an error from the system (it has an OS code, and
/// its text is the system's) stays as it is; any other, such as tempfile's
/// "too many temporary files exist at path …", keeps only its kind.
fn without_path(error: io::Error) -> io::Error {
    if error.raw_os_error().is_some() {
        error
    } else {
        io::Error::new(error.kind(), "could not create a temporary file")
    }
}

/// Highest number an output name may get when its name is taken:
/// `logo-1.png` … `logo-9999.png` are tried (never `logo-0.png`), then the
/// file fails with "no free name in the output folder". An explicit bound on
/// the loop.
pub(crate) const MAX_NAME_NUMBER: u32 = 9999;

/// The error when every name up to [`MAX_NAME_NUMBER`] is taken.
const NO_FREE_NAME: &str = "no free name in the output folder";

/// Write `bytes` to a new file at `dest`, or at the first free numbered name
/// beside it (`numbered(name, 1)` … `numbered(name, MAX_NAME_NUMBER)`), and
/// return the path written. Nothing that exists is ever replaced: the final
/// step is a rename that refuses an existing name (`persist_noclobber`), so a
/// file or folder that appears at the name at any moment before it is kept,
/// and the next number is tried.
///
/// As [`write_atomically`]: the parent folder is created if missing, the data
/// is written to a temporary file in that folder and synced first, and the
/// file gets `permissions`. On any failure the temporary file is removed.
pub(crate) fn write_new(
    dest: &Path,
    bytes: &[u8],
    permissions: fs::Permissions,
) -> io::Result<PathBuf> {
    write_new_with(dest, bytes, permissions, MAX_NAME_NUMBER, &mut |_| {})
}

/// [`write_new`] with the highest number given, and `before_persist` called
/// with each name right before the rename onto it is tried: the tests use it
/// to make a file appear at that moment.
fn write_new_with(
    dest: &Path,
    bytes: &[u8],
    permissions: fs::Permissions,
    last: u32,
    before_persist: &mut dyn FnMut(&Path),
) -> io::Result<PathBuf> {
    let name = dest.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the destination has no file name",
        )
    })?;
    let parent = folder_of(dest);
    fs::create_dir_all(parent)?;

    // Dropping a `NamedTempFile` deletes it, so every early return below
    // (including a failed `persist_noclobber`) leaves no temporary file.
    // Created and written as in `write_atomically`, so no error carries a
    // path (see `new_temp_file`).
    let mut temp = new_temp_file(parent)?;
    temp.as_file_mut().write_all(bytes)?;
    temp.as_file_mut().flush()?;
    temp.as_file()
        .set_permissions(output_permissions(permissions))?;
    temp.as_file().sync_all()?;

    let mut candidate = dest.to_path_buf();
    // 0 is the name itself; the first clash gets 1.
    let mut number = 0;
    loop {
        before_persist(&candidate);
        match temp.persist_noclobber(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(failed) if failed.error.kind() == io::ErrorKind::AlreadyExists => {
                temp = failed.file;
            }
            Err(failed) => return Err(failed.error),
        }
        number += 1;
        if number > last {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, NO_FREE_NAME));
        }
        candidate = parent.join(numbered(name, number));
    }
}

/// `name` with `-<n>` before its last extension (`logo.png` → `logo-1.png`,
/// `a.tar.gz` → `a.tar-1.gz`), or at the end of a name without one
/// (`README` → `README-1`, `.png` → `.png-1`). Callers start at 1. Built from
/// the `OsStr` pieces, so a name that is not valid UTF-8 keeps its bytes.
pub(crate) fn numbered(name: &OsStr, n: u32) -> OsString {
    let path = Path::new(name);
    let mut out = OsString::from(path.file_stem().unwrap_or(name));
    out.push(format!("-{n}"));
    if let Some(extension) = path.extension() {
        out.push(".");
        out.push(extension);
    }
    out
}

/// The permissions the output gets for a source with `permissions`.
///
/// On Windows the only permission is the read-only attribute, and renaming
/// onto a read-only file fails there ("Access is denied"). A read-only source
/// would give a read-only output that the next run could not replace, so the
/// output is never read-only. Who may read it is not changed by that.
#[cfg(windows)]
// On Windows `set_readonly(false)` clears one attribute; the lint is about
// unix, where it would add write bits for everyone.
#[allow(clippy::permissions_set_readonly_false)]
fn output_permissions(mut permissions: fs::Permissions) -> fs::Permissions {
    permissions.set_readonly(false);
    permissions
}

/// The permissions the output gets for a source with `permissions`: exactly
/// those. On unix a read-only output does not block the next run, because a
/// rename needs write access to the folder, not to the file.
#[cfg(not(windows))]
fn output_permissions(permissions: fs::Permissions) -> fs::Permissions {
    permissions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::TempDir;

    /// Some permissions to hand in; the tests that care build their own.
    fn perms(dir: &Path) -> fs::Permissions {
        let file = dir.join(".perms-donor");
        fs::write(&file, b"").unwrap();
        let perms = fs::metadata(&file).unwrap().permissions();
        fs::remove_file(&file).unwrap();
        perms
    }

    fn listing(dir: &Path) -> Vec<OsString> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_the_bytes_to_a_new_file_and_leaves_only_that_file() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");
        write_atomically(&dest, b"hello", perms(tmp.path())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert_eq!(listing(tmp.path()), [OsString::from("out.bin")]);
    }

    #[test]
    fn replaces_the_whole_content_of_an_existing_file() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");
        fs::write(&dest, b"a much longer previous content").unwrap();
        write_atomically(&dest, b"new", perms(tmp.path())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
    }

    #[test]
    fn creates_missing_parent_folders() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("a").join("b").join("out.bin");
        write_atomically(&dest, b"x", perms(tmp.path())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"x");
    }

    #[test]
    fn leaves_no_temp_file_and_keeps_the_target_when_the_rename_fails() {
        // `dest` is a non-empty folder: the rename onto it must fail.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("inside"), b"keep").unwrap();

        assert!(write_atomically(&dest, b"x", perms(tmp.path())).is_err());

        assert_eq!(listing(tmp.path()), [OsString::from("out.bin")]);
        assert_eq!(fs::read(dest.join("inside")).unwrap(), b"keep");
    }

    #[test]
    fn fails_without_creating_anything_when_a_parent_is_a_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("blocker"), b"file").unwrap();
        let dest = tmp.path().join("blocker").join("out.bin");
        assert!(write_atomically(&dest, b"x", perms(tmp.path())).is_err());
        assert_eq!(listing(tmp.path()), [OsString::from("blocker")]);
    }

    #[test]
    fn writes_into_an_existing_real_folder() {
        let tmp = TempDir::new().unwrap();
        let folder = tmp.path().join("optimized");
        fs::create_dir(&folder).unwrap();
        write_atomically(&folder.join("out.bin"), b"x", perms(tmp.path())).unwrap();
        assert_eq!(listing(&folder), [OsString::from("out.bin")]);
    }

    #[cfg(unix)]
    #[test]
    fn writes_into_a_folder_reached_through_a_symlink() {
        // Where output may go is `optimize_file`'s decision, not this helper's.
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomically(&link.join("a.png"), b"x", perms(tmp.path())).unwrap();

        assert_eq!(listing(&real), [OsString::from("a.png")]);
    }

    #[test]
    fn names_the_current_folder_for_a_bare_file_name() {
        assert_eq!(folder_of(Path::new("a.png")), Path::new("."));
        assert_eq!(folder_of(Path::new("dir/a.png")), Path::new("dir"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_destination_folder_that_is_a_dangling_symlink() {
        let tmp = TempDir::new().unwrap();
        let link = tmp.path().join("optimized");
        std::os::unix::fs::symlink(tmp.path().join("missing"), &link).unwrap();

        assert!(write_atomically(&link.join("a.png"), b"x", perms(tmp.path())).is_err());

        assert!(!tmp.path().join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn gives_the_output_exactly_the_permissions_it_is_handed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        for mode in [0o600, 0o644, 0o640] {
            let dest = tmp.path().join(format!("out-{mode:o}.bin"));
            write_atomically(&dest, b"x", fs::Permissions::from_mode(mode)).unwrap();
            let actual = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
            assert_eq!(actual, mode);
        }
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_symlink_without_writing_to_its_target() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target.bin");
        fs::write(&target, b"target").unwrap();
        let dest = tmp.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &dest).unwrap();

        write_atomically(&dest, b"new", perms(tmp.path())).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(fs::symlink_metadata(&dest).unwrap().file_type().is_file());
        assert_eq!(fs::read(&dest).unwrap(), b"new");
    }

    // --- writing without replacing ---------------------------------------------

    /// The names in `dir` that look like `NamedTempFile`'s (`.tmp…`).
    fn temp_files(dir: &Path) -> Vec<OsString> {
        listing(dir)
            .into_iter()
            .filter(|name| name.to_string_lossy().starts_with(".tmp"))
            .collect()
    }

    #[test]
    fn numbered_puts_the_number_before_the_last_extension() {
        let cases = [
            ("logo.png", 2, "logo-2.png"),
            ("logo.png", 9999, "logo-9999.png"),
            (
                "Mobilis Logo – zwart é.png",
                2,
                "Mobilis Logo – zwart é-2.png",
            ),
            ("archive.tar.gz", 3, "archive.tar-3.gz"),
            ("README", 2, "README-2"),
            (".png", 2, ".png-2"),
        ];
        for (name, n, expected) in cases {
            assert_eq!(
                numbered(OsStr::new(name), n),
                OsString::from(expected),
                "{name}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn numbered_keeps_non_utf8_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let name = OsString::from_vec(b"caf\xE9.png".to_vec());
        assert_eq!(numbered(&name, 2).as_bytes(), b"caf\xE9-2.png");
        let bare = OsString::from_vec(b"caf\xE9".to_vec());
        assert_eq!(numbered(&bare, 12).as_bytes(), b"caf\xE9-12");
    }

    #[test]
    fn a_first_clash_gets_minus_one() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("logo.png"), b"taken").unwrap();

        let written = write_new(&tmp.path().join("logo.png"), b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, tmp.path().join("logo-1.png"));
        assert_eq!(fs::read(&written).unwrap(), b"new");
    }

    #[test]
    fn a_second_clash_gets_minus_two() {
        let tmp = TempDir::new().unwrap();
        for name in ["logo.png", "logo-1.png"] {
            fs::write(tmp.path().join(name), b"taken").unwrap();
        }

        let written = write_new(&tmp.path().join("logo.png"), b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, tmp.path().join("logo-2.png"));
        assert_eq!(
            listing(tmp.path()),
            ["logo-1.png", "logo-2.png", "logo.png"]
        );
    }

    #[test]
    fn minus_two_is_not_used_while_minus_one_is_free() {
        let tmp = TempDir::new().unwrap();
        for name in ["logo.png", "logo-2.png"] {
            fs::write(tmp.path().join(name), b"taken").unwrap();
        }

        let written = write_new(&tmp.path().join("logo.png"), b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, tmp.path().join("logo-1.png"));
        assert_eq!(fs::read(tmp.path().join("logo-2.png")).unwrap(), b"taken");
    }

    #[test]
    fn no_name_ends_in_minus_zero() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("logo.png"), b"taken").unwrap();
        for _ in 0..3 {
            write_new(&tmp.path().join("logo.png"), b"new", perms(tmp.path())).unwrap();
        }

        assert_eq!(
            listing(tmp.path()),
            ["logo-1.png", "logo-2.png", "logo-3.png", "logo.png"]
        );
        assert!(!tmp.path().join("logo-0.png").exists());
    }

    #[test]
    fn readme_clashes_to_readme_1() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("README");
        fs::write(&dest, b"existing").unwrap();

        let written = write_new(&dest, b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, tmp.path().join("README-1"));
        assert_eq!(fs::read(&dest).unwrap(), b"existing");
        assert_eq!(fs::read(&written).unwrap(), b"new");
        assert_eq!(listing(tmp.path()), ["README", "README-1"]);
    }

    #[test]
    fn readme_without_clash_stays_readme() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("OTHER"), b"other").unwrap();
        let dest = tmp.path().join("README");

        let written = write_new(&dest, b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, dest);
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert_eq!(listing(tmp.path()), ["OTHER", "README"]);
    }

    #[test]
    fn a_file_appearing_before_the_write_is_not_overwritten_and_the_next_number_is_used() {
        // The file appears after the temporary file is written and right
        // before the rename: only a rename that refuses to replace keeps it.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("logo.png");
        let mut seen = Vec::new();

        let written = write_new_with(
            &dest,
            b"new",
            perms(tmp.path()),
            MAX_NAME_NUMBER,
            &mut |p| {
                if seen.is_empty() {
                    fs::write(p, b"appeared").unwrap();
                }
                seen.push(p.to_path_buf());
            },
        )
        .unwrap();

        assert_eq!(written, tmp.path().join("logo-1.png"));
        assert_eq!(fs::read(&dest).unwrap(), b"appeared");
        assert_eq!(fs::read(&written).unwrap(), b"new");
        assert_eq!(seen, [dest, written]);
        assert_eq!(listing(tmp.path()), ["logo-1.png", "logo.png"]);
    }

    #[test]
    fn with_nothing_appearing_before_the_rename_the_planned_name_is_used() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("logo.png");
        let mut seen = Vec::new();

        let written = write_new_with(
            &dest,
            b"new",
            perms(tmp.path()),
            MAX_NAME_NUMBER,
            &mut |p| seen.push(p.to_path_buf()),
        )
        .unwrap();

        assert_eq!(written, dest);
        assert_eq!(seen, std::slice::from_ref(&dest));
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert_eq!(listing(tmp.path()), ["logo.png"]);
    }

    #[test]
    fn numbering_stops_after_max_name_number_with_the_reason() {
        let tmp = TempDir::new().unwrap();
        for name in ["README", "README-1", "README-2", "README-3"] {
            fs::write(tmp.path().join(name), name).unwrap();
        }
        let mut seen = Vec::new();

        let error = write_new_with(
            &tmp.path().join("README"),
            b"new",
            perms(tmp.path()),
            3,
            &mut |p| seen.push(p.file_name().unwrap().to_owned()),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "no free name in the output folder");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(seen, ["README", "README-1", "README-2", "README-3"]);
        assert_eq!(
            listing(tmp.path()),
            ["README", "README-1", "README-2", "README-3"]
        );
        for name in ["README", "README-1", "README-2", "README-3"] {
            assert_eq!(fs::read(tmp.path().join(name)).unwrap(), name.as_bytes());
        }
    }

    #[test]
    fn a_free_name_below_the_bound_is_used() {
        let tmp = TempDir::new().unwrap();
        for name in ["README", "README-1", "README-2"] {
            fs::write(tmp.path().join(name), name).unwrap();
        }

        let written = write_new_with(
            &tmp.path().join("README"),
            b"new",
            perms(tmp.path()),
            3,
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(written, tmp.path().join("README-3"));
        assert_eq!(fs::read(&written).unwrap(), b"new");
        assert_eq!(temp_files(tmp.path()), Vec::<OsString>::new());
        assert_eq!(
            listing(tmp.path()),
            ["README", "README-1", "README-2", "README-3"]
        );
    }

    #[test]
    fn an_existing_folder_of_that_name_takes_the_next_number() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("logo.png");
        fs::create_dir(&dest).unwrap();

        let written = write_new(&dest, b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, tmp.path().join("logo-1.png"));
        assert!(dest.is_dir());
        assert_eq!(fs::read(&written).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_at_the_name_is_neither_followed_nor_replaced() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target.png");
        let dest = tmp.path().join("logo.png");
        std::os::unix::fs::symlink(&target, &dest).unwrap();

        let written = write_new(&dest, b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, tmp.path().join("logo-1.png"));
        assert!(!target.exists());
        assert!(
            fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&written).unwrap(), b"new");
    }

    #[test]
    fn a_name_too_long_to_number_fails_and_leaves_no_temp_file() {
        // 250 + ".png" is 254 bytes, within the usual 255-byte name limit;
        // with "-1" it is 256, and the rename fails with another error.
        let tmp = TempDir::new().unwrap();
        let name = format!("{}.png", "a".repeat(250));
        let dest = tmp.path().join(&name);
        fs::write(&dest, b"existing").unwrap();

        let error = write_new(&dest, b"new", perms(tmp.path())).unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        assert_eq!(fs::read(&dest).unwrap(), b"existing");
        assert_eq!(listing(tmp.path()), [OsString::from(name)]);
    }

    #[test]
    fn write_new_refuses_a_destination_without_a_file_name() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("sub").join("..");

        let error = write_new(&dest, b"new", perms(tmp.path())).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(listing(tmp.path()), Vec::<OsString>::new());
    }

    #[test]
    fn write_new_creates_a_missing_output_folder() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("exports").join("logo.png");

        let written = write_new(&dest, b"new", perms(tmp.path())).unwrap();

        assert_eq!(written, dest);
        assert_eq!(listing(&tmp.path().join("exports")), ["logo.png"]);
    }

    // --- failure reasons carry no path ---------------------------------------

    /// A folder `locked-folder` in `dir` that nothing can be created in, or
    /// `None` when running as root, where every folder stays writable.
    #[cfg(unix)]
    fn read_only_folder(dir: &Path) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let folder = dir.join("locked-folder");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(folder.join("probe"), b"").is_ok() {
            eprintln!("skipped: the folder stays writable (running as root)");
            return None;
        }
        Some(folder)
    }

    /// `message` names no part of `path` and no temporary file: the reason a
    /// file failed reaches the window, which never receives a path.
    fn assert_no_path(message: &str, path: &Path) {
        assert!(!message.contains("at path"), "{message}");
        assert!(!message.contains(".tmp"), "{message}");
        for part in path.components() {
            if let std::path::Component::Normal(name) = part {
                let name = name.to_string_lossy();
                assert!(!message.contains(&*name), "{name:?} in {message:?}");
            }
        }
    }

    #[test]
    fn a_temp_file_error_without_an_os_code_keeps_its_kind_and_loses_the_path() {
        // What tempfile returns when every name it tries is taken: its own
        // error, with the folder's path added.
        let folder = Path::new("/home/someone/private-folder");
        let error = io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("too many temporary files exist at path {folder:?}"),
        );

        let mapped = without_path(error);

        assert_eq!(mapped.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(mapped.raw_os_error(), None);
        assert_no_path(&mapped.to_string(), folder);
        assert!(!mapped.to_string().contains('/'), "{mapped}");
    }

    #[test]
    fn a_temp_file_error_with_an_os_code_keeps_the_code() {
        let mapped = without_path(io::Error::from_raw_os_error(13));

        assert_eq!(mapped.raw_os_error(), Some(13));
        assert_eq!(
            mapped.to_string(),
            io::Error::from_raw_os_error(13).to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomically_into_a_read_only_folder_gives_the_os_permission_error() {
        let tmp = TempDir::new().unwrap();
        let Some(folder) = read_only_folder(tmp.path()) else {
            return;
        };

        let error = write_atomically(&folder.join("out.bin"), b"x", perms(tmp.path())).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        let code = error.raw_os_error().expect("the system's error code");
        assert_eq!(
            error.to_string(),
            io::Error::from_raw_os_error(code).to_string()
        );
        assert_no_path(&error.to_string(), &folder);
    }

    #[cfg(unix)]
    #[test]
    fn write_new_into_a_read_only_folder_gives_the_os_permission_error() {
        let tmp = TempDir::new().unwrap();
        let Some(folder) = read_only_folder(tmp.path()) else {
            return;
        };

        let error = write_new(&folder.join("out.bin"), b"x", perms(tmp.path())).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        let code = error.raw_os_error().expect("the system's error code");
        assert_eq!(
            error.to_string(),
            io::Error::from_raw_os_error(code).to_string()
        );
        assert_no_path(&error.to_string(), &folder);
    }

    #[cfg(unix)]
    #[test]
    fn write_new_gives_the_output_exactly_the_permissions_it_is_handed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        for mode in [0o600, 0o644, 0o640] {
            let dest = tmp.path().join(format!("out-{mode:o}.bin"));
            let written = write_new(&dest, b"x", fs::Permissions::from_mode(mode)).unwrap();
            let actual = fs::metadata(&written).unwrap().permissions().mode() & 0o777;
            assert_eq!(actual, mode);
        }
    }
}
