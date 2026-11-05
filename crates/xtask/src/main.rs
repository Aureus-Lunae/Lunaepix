//! Build helper for the bundle. `cargo tauri build --config
//! src-tauri/tauri.bundle.conf.json` runs `cargo run --locked -p
//! lunaepix-xtask -- sidecar` first (its `beforeBuildCommand`): that builds
//! the release worker and copies it to
//! `src-tauri/binaries/lunaepix-worker-<triple>[.exe]`, the name Tauri's
//! `externalBin` looks for. Tauri then installs it beside the app as
//! `lunaepix-worker[.exe]`.
//!
//! A crate of its own, std only, so that it builds in a second: a binary in
//! `lunaepix-cli` would compile all of `lunaepix-core` in debug first, only
//! to start a release build of it.

use std::env;
use std::env::consts::EXE_SUFFIX;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The variable tauri-cli sets for its `beforeBuildCommand`.
const TRIPLE_VAR: &str = "TAURI_ENV_TARGET_TRIPLE";

/// The triple this helper was built for (see `build.rs`).
const HOST_TRIPLE: &str = env!("LUNAEPIX_HOST_TRIPLE");

/// The worker binary's name in `lunaepix-core`.
const WORKER: &str = "lunaepix-worker";

/// The cargo arguments that build the worker.
const BUILD_ARGS: &[&str] = &[
    "build",
    "--release",
    "--locked",
    "-p",
    "lunaepix-core",
    "--bin",
    WORKER,
];

/// `lunaepix-worker-<triple>`, with `.exe` for a Windows triple: the file
/// name Tauri expects under `src-tauri/binaries/`. A missing or empty
/// triple is refused, and so is one that is not a plain triple: it becomes
/// part of a path.
fn sidecar_name(triple: Option<&str>) -> Result<String, String> {
    let triple = triple.map(str::trim).unwrap_or_default();
    if triple.is_empty() {
        return Err(format!(
            "{TRIPLE_VAR} is not set: run this through `cargo tauri build --config src-tauri/tauri.bundle.conf.json`"
        ));
    }
    let plain = triple
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !plain || triple.contains("..") || !triple.contains('-') {
        return Err(format!("{TRIPLE_VAR} is not a target triple: {triple:?}"));
    }
    let suffix = if triple.contains("-windows-") {
        ".exe"
    } else {
        ""
    };
    Ok(format!("{WORKER}-{triple}{suffix}"))
}

/// The bundle must be for the machine the worker is built on: the worker is
/// built without `--target`.
fn check_host(triple: &str, host: &str) -> Result<(), String> {
    if triple == host {
        Ok(())
    } else {
        Err(format!(
            "the bundle is for {triple}, but the worker would be built for {host}: cross builds are not supported"
        ))
    }
}

/// Where `cargo build --release` puts the worker: `release/` beside the
/// profile folder this helper runs from. That follows `CARGO_TARGET_DIR`,
/// a `<triple>/` layout, and any target folder cargo was told to use.
fn built_worker(helper_exe: &Path) -> Option<PathBuf> {
    let profile = helper_exe.parent()?;
    let target = profile.parent()?;
    Some(target.join("release").join(format!("{WORKER}{EXE_SUFFIX}")))
}

/// Build the worker with `build` (given [`BUILD_ARGS`]), check that it is
/// there, and copy it to `<root>/src-tauri/binaries/<sidecar name>`. Checks
/// the triple before it builds, and copies nothing when anything fails.
fn stage(
    triple: Option<&str>,
    host: &str,
    helper_exe: &Path,
    root: &Path,
    build: impl FnOnce(&[&str]) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let name = sidecar_name(triple)?;
    check_host(triple.unwrap_or_default().trim(), host)?;
    let built = built_worker(helper_exe).ok_or_else(|| {
        format!(
            "cannot find the target folder from {}",
            helper_exe.display()
        )
    })?;
    build(BUILD_ARGS)?;
    if !built.is_file() {
        return Err(format!(
            "the build finished, but there is no {WORKER} at {}",
            built.display()
        ));
    }
    let folder = root.join("src-tauri").join("binaries");
    fs::create_dir_all(&folder)
        .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
    let staged = folder.join(name);
    fs::copy(&built, &staged).map_err(|error| {
        format!(
            "cannot copy {} to {}: {error}",
            built.display(),
            staged.display()
        )
    })?;
    Ok(staged)
}

/// Runs `cargo <args>` in `root`, with the cargo that runs this helper.
fn cargo(root: &Path, args: &[&str]) -> Result<(), String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(&cargo)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|error| format!("cannot start cargo: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed: {status}", args.join(" ")))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args != ["sidecar"] {
        eprintln!("usage: lunaepix-xtask sidecar");
        return ExitCode::from(2);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let triple = env::var(TRIPLE_VAR).ok();
    let staged = env::current_exe()
        .map_err(|error| format!("cannot find this helper's own file: {error}"))
        .and_then(|helper| {
            stage(triple.as_deref(), HOST_TRIPLE, &helper, &root, |args| {
                cargo(&root, args)
            })
        });
    match staged {
        Ok(path) => {
            eprintln!("lunaepix-xtask: staged {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lunaepix-xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::env::consts::EXE_SUFFIX;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const LINUX: &str = "x86_64-unknown-linux-gnu";
    const WINDOWS: &str = "x86_64-pc-windows-msvc";

    /// A fresh, empty folder under the system's temporary folder, removed
    /// when dropped. std only, so no tempfile crate.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lunaepix-xtask-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // --- the sidecar's name ---------------------------------------------------

    #[test]
    fn sidecar_name_is_the_worker_and_the_triple() {
        assert_eq!(
            sidecar_name(Some(LINUX)).unwrap(),
            "lunaepix-worker-x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn a_windows_triple_gets_the_exe_suffix() {
        assert_eq!(
            sidecar_name(Some(WINDOWS)).unwrap(),
            "lunaepix-worker-x86_64-pc-windows-msvc.exe"
        );
        assert!(!sidecar_name(Some(LINUX)).unwrap().ends_with(".exe"));
    }

    #[test]
    fn a_missing_or_empty_triple_is_refused() {
        for triple in [None, Some(""), Some("   ")] {
            let error = sidecar_name(triple).unwrap_err();
            assert!(error.contains("TAURI_ENV_TARGET_TRIPLE"), "{error}");
        }
    }

    #[test]
    fn a_triple_that_could_leave_the_folder_is_refused() {
        for triple in [
            "../x86_64-unknown-linux-gnu",
            "x86_64/unknown-linux-gnu",
            "x86_64\\pc-windows-msvc",
            "x86_64-unknown linux-gnu",
            "x86_64..linux",
            "linux",
        ] {
            assert!(sidecar_name(Some(triple)).is_err(), "{triple}");
        }
    }

    // --- only a build for this machine ------------------------------------------

    #[test]
    fn the_host_triple_is_accepted() {
        assert_eq!(check_host(LINUX, LINUX), Ok(()));
    }

    #[test]
    fn another_triple_is_refused_as_a_cross_build() {
        let error = check_host(WINDOWS, LINUX).unwrap_err();
        assert!(error.contains(WINDOWS) && error.contains(LINUX), "{error}");
    }

    // --- where cargo put the worker ----------------------------------------------

    #[test]
    fn the_built_worker_sits_in_release_beside_the_helpers_profile_folder() {
        let worker = format!("lunaepix-worker{EXE_SUFFIX}");
        assert_eq!(
            built_worker(Path::new("/t/debug/lunaepix-xtask")),
            Some(Path::new("/t/release").join(&worker))
        );
        // A target folder of its own, and the `<triple>/` layout.
        assert_eq!(
            built_worker(Path::new(
                "/shared/target/x86_64-unknown-linux-gnu/debug/lunaepix-xtask"
            )),
            Some(Path::new("/shared/target/x86_64-unknown-linux-gnu/release").join(&worker))
        );
    }

    #[test]
    fn a_helper_path_too_short_to_have_a_target_folder_gives_none() {
        assert_eq!(built_worker(Path::new("lunaepix-xtask")), None);
        assert_eq!(built_worker(Path::new("/")), None);
    }

    // --- build, check, copy -------------------------------------------------------

    /// A target folder with the helper in `debug/` and, when `built`, a
    /// worker in `release/`. Returns the helper's path.
    fn target_with(scratch: &Scratch, built: bool) -> PathBuf {
        let target = scratch.0.join("target");
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::create_dir_all(target.join("release")).unwrap();
        if built {
            let worker = target
                .join("release")
                .join(format!("lunaepix-worker{EXE_SUFFIX}"));
            fs::write(worker, b"worker bytes").unwrap();
        }
        target.join("debug").join("lunaepix-xtask")
    }

    #[test]
    fn staging_copies_the_built_worker_to_src_tauri_binaries() {
        let scratch = Scratch::new();
        let helper = target_with(&scratch, true);
        let root = scratch.0.join("repo");
        let given = RefCell::new(Vec::new());

        let staged = stage(Some(LINUX), LINUX, &helper, &root, |args| {
            given.borrow_mut().push(args.join(" "));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            *given.borrow(),
            ["build --release --locked -p lunaepix-core --bin lunaepix-worker"]
        );
        let expected = root
            .join("src-tauri")
            .join("binaries")
            .join("lunaepix-worker-x86_64-unknown-linux-gnu");
        assert_eq!(staged, expected);
        assert_eq!(fs::read(&expected).unwrap(), b"worker bytes");
    }

    #[test]
    fn staging_fails_and_copies_nothing_when_the_build_fails() {
        let scratch = Scratch::new();
        let helper = target_with(&scratch, true);
        let root = scratch.0.join("repo");

        let error = stage(Some(LINUX), LINUX, &helper, &root, |_| {
            Err("cargo exited with 101".to_owned())
        })
        .unwrap_err();

        assert!(error.contains("cargo exited with 101"), "{error}");
        assert!(!root.exists(), "nothing may be copied");
    }

    #[test]
    fn staging_fails_when_the_built_worker_is_missing() {
        let scratch = Scratch::new();
        let helper = target_with(&scratch, false);
        let root = scratch.0.join("repo");

        let error = stage(Some(LINUX), LINUX, &helper, &root, |_| Ok(())).unwrap_err();

        assert!(error.contains("lunaepix-worker"), "{error}");
        assert!(!root.exists());
    }

    #[test]
    fn staging_fails_without_the_triple_or_for_another_system_and_never_builds() {
        let scratch = Scratch::new();
        let helper = target_with(&scratch, true);
        let root = scratch.0.join("repo");
        for (triple, host) in [(None, LINUX), (Some(WINDOWS), LINUX)] {
            let built = RefCell::new(false);
            let result = stage(triple, host, &helper, &root, |_| {
                *built.borrow_mut() = true;
                Ok(())
            });
            assert!(result.is_err(), "{triple:?}");
            assert!(!*built.borrow(), "{triple:?}: built anyway");
        }
        assert!(!root.exists());
    }
}
