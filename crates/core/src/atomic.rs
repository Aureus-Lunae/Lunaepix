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
/// - A symlink at `dest` is replaced, never followed: the rename swaps the
///   directory entry, so the link's target is not written to.
/// - On any failure the temporary file is removed and `dest` is not changed.
pub(crate) fn write_atomically(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match dest.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;

    // Dropping a `NamedTempFile` deletes it, so every early return below
    // (including a failed `persist`) leaves no temporary file behind.
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    // `NamedTempFile` is created private (0600); an output image should look
    // like any other file the user makes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file().set_permissions(fs::Permissions::from_mode(0o644))?;
    }
    temp.persist(dest).map_err(|failed| failed.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::TempDir;

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
        write_atomically(&dest, b"hello").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert_eq!(listing(tmp.path()), [OsString::from("out.bin")]);
    }

    #[test]
    fn replaces_the_whole_content_of_an_existing_file() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");
        fs::write(&dest, b"a much longer previous content").unwrap();
        write_atomically(&dest, b"new").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
    }

    #[test]
    fn creates_missing_parent_folders() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("a").join("b").join("out.bin");
        write_atomically(&dest, b"x").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"x");
    }

    #[test]
    fn leaves_no_temp_file_and_keeps_the_target_when_the_rename_fails() {
        // `dest` is a non-empty folder: the rename onto it must fail.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("inside"), b"keep").unwrap();

        assert!(write_atomically(&dest, b"x").is_err());

        assert_eq!(listing(tmp.path()), [OsString::from("out.bin")]);
        assert_eq!(fs::read(dest.join("inside")).unwrap(), b"keep");
    }

    #[test]
    fn fails_without_creating_anything_when_a_parent_is_a_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("blocker"), b"file").unwrap();
        let dest = tmp.path().join("blocker").join("out.bin");
        assert!(write_atomically(&dest, b"x").is_err());
        assert_eq!(listing(tmp.path()), [OsString::from("blocker")]);
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_symlink_without_writing_to_its_target() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target.bin");
        fs::write(&target, b"target").unwrap();
        let dest = tmp.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &dest).unwrap();

        write_atomically(&dest, b"new").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(fs::symlink_metadata(&dest).unwrap().file_type().is_file());
        assert_eq!(fs::read(&dest).unwrap(), b"new");
    }
}
