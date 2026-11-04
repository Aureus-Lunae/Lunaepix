//! Atomic file output, shared by the PNG, JPG and SVG optimisers.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use tempfile::NamedTempFile;

/// Write `bytes` to `dest` so that `dest` is either the complete new file or
/// untouched. The bytes go to a temporary file in `dest`'s own folder (same
/// filesystem, so the rename is atomic), which is then renamed onto `dest`.
///
/// - The parent folder is created if it is missing, and only now, once there
///   is something to write. A folder created here is left in place if the
///   write then fails: an empty `optimized/` folder is harmless.
/// - The parent folder must be a real directory. If it is a symlink (or
///   anything else), the write is refused: a link planted at `optimized/`
///   would otherwise send the output to wherever it points.
/// - A symlink at `dest` itself is replaced, never followed: the rename swaps
///   the directory entry, so the link's target is not written to.
/// - The file gets exactly `permissions` (the caller passes the source's, so
///   the output is never more readable than the original).
/// - The data is synced to disk before the rename, so a crash cannot leave
///   `dest` pointing at an empty or partial file.
/// - On any failure the temporary file is removed and `dest` is not changed.
pub(crate) fn write_atomically(
    dest: &Path,
    bytes: &[u8],
    permissions: fs::Permissions,
) -> io::Result<()> {
    let parent = match dest.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    // `symlink_metadata` does not follow a link, so a symlink reports as one.
    if !fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the destination folder is a symbolic link or not a folder",
        ));
    }

    // Dropping a `NamedTempFile` deletes it, so every early return below
    // (including a failed `persist`) leaves no temporary file behind.
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().set_permissions(permissions)?;
    temp.as_file().sync_all()?;
    temp.persist(dest).map_err(|failed| failed.error)?;
    Ok(())
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
        let mut names: Vec<_> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
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
    fn refuses_a_destination_folder_that_is_a_symlink_and_writes_nothing_behind_it() {
        let tmp = TempDir::new().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let link = tmp.path().join("optimized");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

        let result = write_atomically(&link.join("a.png"), b"x", perms(tmp.path()));

        assert!(result.is_err());
        assert_eq!(listing(&elsewhere), Vec::<OsString>::new());
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
}
