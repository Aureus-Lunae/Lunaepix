# Lunaepix Mac build — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-13

## Handoff

These lines are copied verbatim from the spec's Handoff. The standing process rules and the calls made while planning follow them. Give every implementer these lines as they are. An implementer that wants to deviate from one stops and asks instead of choosing.

- **Plan approved by the user (2026-11-13):** "approved", including the calls made while planning (listed below). Execute in a fresh session, subagent-driven, on `feature/lunaepix-apple`.
- **Spec approved by the user (2026-11-13):** "Yes", including the "Calls I made" list.
- **Correction after approval (2026-11-13, while planning):** "Running the existing tests on a Mac" named the wrong files for the `/proc/<pid>` tests. They are in `run.rs` (core, unit and integration), `session.rs` and `session_run.rs`, not in `thumb.rs`. The paragraph was corrected. The intent did not change.
- **Scope:** "Git needs the apple build as well for the apple support. And that is what we will add." This work is the Mac build on GitHub Actions, the first part of the roadmap item "Apple support".
- **Who it is for:** "Apple Silicon mac. A few coworkers working on a mac."
- **First playback confirmed:** "Correct." That covered a macOS job next to the Windows one that runs the tests and then builds an unsigned `.dmg` as a run artifact, the same guards as the Windows workflow, and the "Open Anyway" step explained in the README.
- **Intel Macs:** the user asked "But since it will be open source, how common are intel macs still?" I recommended Apple Silicon only (reasons under "Calls I made") and the user went on without objecting. This is not an explicit decision by the user. It is listed as my call for them to check.
- **Repository visibility:** "Stay private for now". So the brew formula waits too, because it needs a public download link.
- **Hand test:** "Short check, a coworker will test it after it gets built". The user downloads the artifact and passes the `.dmg` on. The coworker reports back to the user.
- **How the builder gets Mac runs:** asked how, since "Run workflow" only works for a workflow that is already on `main`. The user chose "rc tags on branch (Recommended)": push `feature/lunaepix-apple` and tag `v0.1.4-rc1`, `rc2` and so on. The builder may push and tag for this without asking each time. Each tag also runs the Windows build.
- **Actions minutes:** asked whether minutes are a constraint (Mac minutes count about ten times on a private repository). The user chose "Limited: keep runs few". The full Mac tests stay. The plan caps the number of rc runs, caches the build in the Installer job too, and the builder fixes as much as possible before each tag.
- **Budget (2026-11-13, after approval):** "I already used 3.90 from 12$ this month". Asked what the rc-tag cap should be, given that each tag costs about $3–5 (Mac plus Windows). The user chose "1 now, then ask": one rc tag, then a report with what it showed and its real cost before any further tag. This replaces the cap of three.
- **Starting point:** `main` = origin/main at 97a1e15 (0.1.3). This work goes on `feature/lunaepix-apple`, from `main` at 97a1e15.
- **Installer after a red test (2026-11-13, final review before the rc tag):** asked whether the Installer job should still run when a Mac test fails. The user chose "Keep needs: test strict".
- **NASM on the Mac (2026-11-13, final review):** the review found mozjpeg needs NASM only for x86; on Apple Silicon it assembles NEON with clang. The user chose "Drop it before the tag (Recommended)": no NASM step in `macos.yml`, and the guard refuses one there.
- **Build cache on rc tags (2026-11-13, final review):** rc tags cannot restore each other's cache, only `main`'s. The user chose "Save only from main (Recommended)": the Mac cache steps save only on `refs/heads/main`; Windows unchanged.

**Standing process rules** (the user's, from earlier Lunaepix plans; they apply to every task here):

- **Tables before code.** Every implementer and every fix round writes two tables before any code:
  - A test table: behaviour → positive test → negative test → "reached by", which lists every kind of input that reaches the behaviour (success, refusal, failure, and so on), each with its own test, → "fails on its own because…".
  - A failure-branch table: every `Err` path, fallback, kill/reap, "log and skip", and every if/else driven by a setting or platform. Each gets the test that reaches it, or "untested" with the reason.
- **Controller gate.** Before each review, the controller sends back:
  - any "untested" row on a branch that decides what the caller gets, unless the row names how a test could reach it and why that does not work;
  - any single-trigger row that gives no reason.
- **Pass-through means unchanged.** A call counts as untested pass-through only if it hands its input on unchanged.
- **Controller RED audit.** The controller compares the RED run's test list with the commit's final tests. A test missing from the RED run goes back before review.
- **Parse structured files.** Every check on YAML, JSON, TOML, HTML or a property list uses a real parser. Only a shell `run:` body may be read as text, and then it is documented as a lint. Reviewers probe other spellings.
- **Comments follow the code.** The comment check covers doc comments of functions, types, constants and guards whose use the change widens. Here that means `workflow.rs`'s module doc, which today says "the Windows build workflow", and `BUNDLE_TARGETS`.
- **Always bump the version.** 0.1.4, in Task 6.

**Calls made while planning** (not the user's decisions; flagged for the plan review):

1. **The check step also opens the `.dmg`.** It mounts the disk image read-only and verifies the app inside it, not only the app in `bundle/macos/`. The coworker gets the app inside the `.dmg`, and a seal broken by packaging would only show up on their Mac.
2. **One `process_is_gone(pid)` test helper per test crate,** using `kill(pid, 0)`. A zombie still counts as present, so "gone" proves the process was reaped, as the `/proc/<pid>` check did. `libc` becomes a `cfg(unix)` dev-dependency of `lunaepix-core`.
3. **A test for two sources whose names differ only in case,** run on Windows and Mac. It writes `a.png` and `A.png` from two folders into one output folder, and neither output may replace the other. This is Review Focus 3.
4. **The upload guard applies to Windows too.** It checks the artifact name, the exact paths and 30 days, with Windows's current values. It costs one profile field, and both files stay under one rule.
5. **The Mac runner is exactly `macos-latest`,** as Windows checks exactly `windows-latest`. The spec says "a `macos-*` runner"; this is the stricter form of the same rule.
6. **The Mac config may hold `$schema`** next to `bundle`, as the other config files do. `bundle.macOS` may hold only `signingIdentity` and `minimumSystemVersion`, and the guard checks both values.
7. **Two items added to the coworker's check:** a HEIC photo from an iPhone is listed as skipped and nothing crashes; and the picture is sharp at 100 % zoom on a Retina screen. These are Review Focus 4 and 5.
8. **Every check before the tag** that can run off the Mac is listed in Task 7. With one tag, a failure that Linux could have shown wastes a run.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Invoke `sdd-overrides` first.

**Goal:** a GitHub Actions workflow that tests Lunaepix on an Apple Silicon Mac and builds an ad-hoc-signed `.dmg` with the worker inside, plus the licences, the README and version 0.1.4.

**Architecture:**
- `macos.yml` mirrors `windows.yml`, with two jobs: Test, then Installer with a check step.
- The Mac bundle settings live in `src-tauri/tauri.macos.conf.json`, which Tauri merges over `tauri.conf.json` on the Mac only.
- `workflow.rs` gets a per-platform profile, so every shared guard runs on both workflow files and only a few rules differ.
- The `/proc`-based kill-and-reap tests move to a `kill(pid, 0)` helper, so they run on every Unix.

**Tech stack:** GitHub Actions (`macos-latest`, Apple Silicon), Tauri 2 / `tauri-cli` 2.12.4, Rust 2024 (MSRV 1.90), `yaml_rust2`, `serde_json`, `toml`, `libc`, cargo-about.

**Spec:** [2026-11-13-lunaepix-apple-design.md](../../in-progress/specs/2026-11-13-lunaepix-apple-design.md). Read its Handoff before anything else.

## Global Constraints

- **Mac workflow:** `.github/workflows/macos.yml`, name "macOS build", triggers `workflow_dispatch` and `push` of tags `v*` only. `permissions: contents: read`, concurrency group `${{ github.workflow }}-${{ github.ref }}`, `defaults.run.shell: bash`, runner `macos-latest`.
- **Actions:** the same pinned hashes and release comments as `windows.yml`: checkout `3d3c42e5…` (`persist-credentials: false`), rust-toolchain `b725eda5…`, rust-cache `d1527c8f…`, setup-node `949feb24…` (Node 22), upload-artifact `cf430e03…`.
- **NASM on the Mac:** `brew install nasm`, then `nasm -v`, before the first cargo step of every job.
- **Installer:**
  - `cargo install tauri-cli --locked --version 2.12.4`, then `cargo tauri build --config src-tauri/tauri.bundle.conf.json`.
  - The Installer job has its own rust-cache.
  - Upload: artifact `lunaepix-macos-dmg`, path `target/release/bundle/dmg/*.dmg`, `if-no-files-found: error`, `retention-days: 30`.
- **Timeouts:** Test 45 minutes, Installer 60 minutes.
- **`tauri.macos.conf.json`:** `bundle.targets` `["app", "dmg"]`, `bundle.macOS.signingIdentity` `"-"`, `bundle.macOS.minimumSystemVersion` `"11.0"`. Nothing else besides `$schema`.
- **`about.toml` targets:** `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`. Never widen `accepted`. A refused licence means stop and report.
- **`windows.yml` behaviour is unchanged.** Only its guard code is shared.
- **Version** `0.1.4`.
- **No secret, no write permission, no release** anywhere.
- **Every behaviour has a positive and a negative test.** No mutation runs, and no production code is broken to prove a test.

## Review Focus

1. **The app inside the `.dmg` is sealed and ad-hoc signed,** not only the one in `bundle/macos/`. Otherwise the coworker sees "damaged". The test is the check step in Task 5, which mounts the `.dmg`.
2. **The worker is inside the app's seal.** If it isn't, macOS can refuse the worker after "Open Anyway". The app then quietly uses its slower fallback, and the coworker cannot see why. The test is the `plutil` check in Task 5.
3. **Two sources whose names differ only in case go into one output folder** (`x/a.png` and `y/A.png`). On the Mac's case-insensitive file system neither may replace the other. The test is in Task 1, run on Windows and Mac.
4. **An iPhone HEIC photo is dropped on the window.** It is listed as skipped with a reason, and nothing crashes. The format refusal is the same code on every platform and is already tested, so this is item 7 of the coworker's check (Task 6).
5. **A Retina screen** (device pixel ratio 2): the preview at 100 % zoom is sharp, not upscaled. No test runs on a HiDPI Mac, so this is item 8 of the coworker's check (Task 6).

---

### Task 1: The kill-and-reap tests run on the Mac too

**Files:**
- Modify: `crates/core/Cargo.toml` (`[target.'cfg(unix)'.dev-dependencies] libc = "0.2.191"`).
- Modify the tests:
  - `crates/core/src/run.rs`: the two `a_worker_past_the_timeout_is_failed_and_reaped` (thumb and probe modules).
  - `crates/core/tests/run.rs`: `stopping_a_run_kills_and_reaps_its_worker_and_starts_no_further_file` with its `wait_for_lines` helper, and `stops_and_reaps_a_worker_that_runs_past_its_timeout`.
  - `src-tauri/src/session.rs`: `stopping_a_session_as_the_window_closes_kills_its_worker_and_finishes_once`.
  - `src-tauri/tests/session_run.rs`: `mod running` and its `alive`.
- Test: the same files, plus `crates/core/tests/optimize_png.rs` or `crates/core/tests/output_folder.rs` for the case test, whichever already holds the output-folder naming tests.

**Interfaces:**
- Produces: `fn process_is_gone(pid: u32) -> bool`, once per test crate (core unit tests, core `tests/`, src-tauri unit tests, src-tauri `tests/`). Where a crate's `tests/` already has a shared module, it goes there.

- [ ] **Step 1: Write the tables, then the failing tests.**
  - `process_is_gone`, in each crate that gets one:
    - `a_reaped_child_is_gone` (spawn `sleep 60`, kill, `wait()` → `true`);
    - `a_running_child_is_not_gone` (→ `false`);
    - `a_killed_child_that_was_not_waited_for_is_not_gone` (kill, wait until it is a zombie, then `false`; reap afterwards). This one proves "gone" means reaped.
  - The case test (Review Focus 3), `cfg(any(windows, target_os = "macos"))`: `two_sources_whose_names_differ_only_in_case_both_reach_one_output_folder`. Sources `x/a.png` and `y/A.png` go to one output folder. Assert two output files whose bytes match their own source's optimised output, and that neither was replaced. Its negative pair is `cfg(target_os = "linux")`: `on_linux_names_that_differ_only_in_case_are_both_kept_as_they_are`, which gives `a.png` and `A.png`, neither numbered.
- [ ] **Step 2:** Run `cargo test --workspace`. The new helper tests fail (no helper yet). Record the output.
- [ ] **Step 3:** Write `process_is_gone`. This is the exact shape, because `EPERM` means "exists":

  ```rust
  fn process_is_gone(pid: u32) -> bool {
      let pid = libc::pid_t::try_from(pid).expect("pid fits pid_t");
      // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
      let answer = unsafe { libc::kill(pid, 0) };
      answer == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
  }
  ```

  Then change the listed tests from `cfg(target_os = "linux")` to `cfg(unix)`, and replace each `/proc/<pid>` existence check with `process_is_gone`. If a listed test also relies on something Linux-only (`PR_SET_PDEATHSIG`, the OOM score), keep that part Linux-only and give the reason in a comment and in the report. The tests of the OOM score, `die_with_parent`, `/proc/self/status` and the case-sensitive folder stay `cfg(target_os = "linux")`.
- [ ] **Step 4:** Run these; all must pass on Linux:
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`

  The case test runs first on the Windows and Mac runs. If it fails there, that is a behaviour difference: stop and ask.
- [ ] **Step 5:** Commit "Run the kill-and-reap tests on every Unix, through kill(pid, 0)".

### Task 2: The Mac bundle settings

**Files:**
- Create: `src-tauri/tauri.macos.conf.json`.
- Modify: `src-tauri/tests/config.rs`. Add the new guard next to `bundle_target_problems`, and update the doc comment of `BUNDLE_TARGETS` to say that the Mac has its own list in `tauri.macos.conf.json`.

**Interfaces:**
- Produces: `const MACOS_BUNDLE_TARGETS: &[&str] = &["app", "dmg"];` and `fn macos_config_problems(config: &Value) -> Vec<String>`.

- [ ] **Step 1: Write the tables, then the failing tests.**
  - `the_macos_config_targets_app_and_dmg_signs_ad_hoc_and_needs_macos_11`: the real file reads as JSON, `macos_config_problems` is empty, and the three values are exactly as in the Global Constraints.
  - `a_macos_config_with_other_targets_is_caught_in_a_fixture`: targets missing, `"all"`, `["app"]`, `["app", "dmg", "deb"]`, `["dmg", 1]`.
  - `a_macos_config_without_an_ad_hoc_signature_is_caught_in_a_fixture`: identity missing, `""`, `"Developer ID Application: X"`, `null`.
  - `a_macos_config_with_another_minimum_version_is_caught_in_a_fixture`: missing, `"10.13"`, `11`.
  - `a_macos_config_that_sets_anything_else_is_caught_in_a_fixture`: top-level `app`, `bundle.resources`, `bundle.macOS.entitlements`, `bundle.macOS.providerShortName`.
  - `a_macos_config_with_or_without_a_schema_passes_a_fixture`: the positive pair of the line above.
- [ ] **Step 2:** Run `cargo test -p lunaepix --test config`. The tests fail (no file, no function). Record the output.
- [ ] **Step 3:** Create the file. This is the exact content:

  ```json
  {
    "$schema": "https://schema.tauri.app/config/2",
    "bundle": {
      "targets": ["app", "dmg"],
      "macOS": {
        "signingIdentity": "-",
        "minimumSystemVersion": "11.0"
      }
    }
  }
  ```

  Then write `macos_config_problems`.
- [ ] **Step 4:** `cargo test --workspace` and clippy (Linux and windows-gnu) pass.
- [ ] **Step 5:** Commit "Mac bundle settings: app and dmg, ad-hoc signature, macOS 11".

### Task 3: Licences for the Mac build

**Files:**
- Modify: `about.toml` (`targets`, and the comment above it).
- Modify: `THIRD-PARTY-LICENSES.html` (regenerated, never edited by hand).
- Modify: `src-tauri/tests/config.rs` (`about_problems`, or a new `about_target_problems(about_toml: &str) -> Vec<String>` beside it).

- [ ] **Step 1: Write the tables, then the failing tests.**
  - `the_real_about_toml_lists_the_linux_windows_and_mac_targets`.
  - `about_toml_targets_without_one_platform_or_with_another_are_caught_in_a_fixture`: Mac missing, Linux missing, an extra `x86_64-apple-darwin`, `targets = "aarch64-apple-darwin"` (not a list), and a non-string entry.
  - If `config.rs` already parses `THIRD-PARTY-LICENSES.html`, extend that parse:
    - `the_licence_page_names_a_crate_only_the_mac_build_uses`, using `objc2`, after confirming with `cargo tree --target aarch64-apple-darwin -i objc2` that it is Mac-only and shipped;
    - its fixture negative, a page without it, which is reported.

    If no such parse exists, say so in the report and do not add text matching.
- [ ] **Step 2:** Run the config tests and record the RED output.
- [ ] **Step 3:** Change `targets` and its comment ("Linux, Windows and Apple Silicon macOS; no Intel Mac, no mobile"). Then run `cargo about generate -c about.toml about.hbs -o THIRD-PARTY-LICENSES.html`.
  - **If cargo-about refuses a licence:** stop and report the crate and the licence to the user. Never touch `accepted`.
- [ ] **Step 4:** `cargo test --workspace` passes. `git diff --stat THIRD-PARTY-LICENSES.html` shows the page grew.
- [ ] **Step 5:** Commit "List the licences of the crates the Mac build ships".

### Task 4: Workflow guards per platform (Windows unchanged)

**Files:**
- Modify: `src-tauri/tests/workflow.rs` (module doc, `workflow()`, `structure_problems`, `nasm_problems`, `every_check`, and the real-file tests).

**Interfaces:**
- Produces:

  ```rust
  enum BuildCache { Forbidden, Required }
  enum NasmInstall { ChocolateyPinned, Homebrew }
  struct Platform {
      file: &'static str,            // "windows.yml"
      runner: &'static str,          // "windows-latest"
      build_cache: BuildCache,
      nasm: NasmInstall,
      artifact: &'static str,        // "lunaepix-windows-installer"
      artifact_paths: &'static [&'static str],
  }
  const WINDOWS: Platform;
  fn workflow(platform: &Platform) -> String;
  fn structure_problems(text: &str, platform: &Platform) -> Vec<String>;
  fn nasm_problems(text: &str, platform: &Platform) -> Vec<String>;
  fn upload_problems(text: &str, platform: &Platform) -> Vec<String>;   // new
  fn every_check(text: &str, platform: &Platform) -> Vec<(&'static str, Vec<String>)>;
  ```

- [ ] **Step 1: Write the tables, then the failing tests.** Every existing test keeps its assertions and passes `&WINDOWS` where a signature now asks for it. The new behaviour is `upload_problems`, tested on Windows:
  - `the_upload_names_the_platform_artifact_its_paths_and_thirty_days_in_a_fixture` (positive);
  - `an_upload_with_another_name_path_or_retention_is_caught_in_a_fixture`: name `lunaepix`, a missing `.msi` path, an extra path, `retention-days: 90`, and no `retention-days`.

  `the_workflow_passes_every_check` still passes on the real `windows.yml`.
- [ ] **Step 2:** Run `cargo test -p lunaepix --test workflow`. It fails to compile, or the new tests fail. Record the output.
- [ ] **Step 3:** Implement. `ChocolateyPinned` keeps today's rule exactly (`choco install nasm`, `--version=`, `GITHUB_PATH`). `Forbidden` keeps today's no-cache rule. `Homebrew` and `Required` are written in Task 5. The module doc says it guards both workflows.
- [ ] **Step 4:** `cargo test --workspace` and clippy pass. `git diff .github/workflows/windows.yml` is empty.
- [ ] **Step 5:** Commit "Workflow guards take a platform profile; the upload guard checks name, paths and retention".

### Task 5: The Mac workflow

**Files:**
- Create: `.github/workflows/macos.yml`.
- Modify: `src-tauri/tests/workflow.rs` (`MACOS`, the `Homebrew` and `Required` rules, `mac_check_step_problems`, a Mac fixture, and the real-file tests for `macos.yml`).

**Interfaces:**
- Consumes: Task 4's `Platform`, `every_check`, `structure_problems`, `nasm_problems` and `upload_problems`.
- Produces: `const MACOS: Platform` (`"macos.yml"`, `"macos-latest"`, `Required`, `Homebrew`, `"lunaepix-macos-dmg"`, `&["target/release/bundle/dmg/*.dmg"]`), and `fn mac_check_step_problems(text: &str) -> Vec<String>`. `every_check(text, &MACOS)` includes it.

- [ ] **Step 1: Write the tables, then the failing tests.**
  - **The real file:** `the_mac_workflow_parses_and_passes_every_check`, and `the_checks_of_the_real_mac_file_are_not_vacuous` (as the Windows one: it counts references and scripts and contains `target/release/bundle/`).
  - **A good Mac fixture** (`good_mac()`, with a `with_mac(from, to)` that fails when `from` is absent) passes every check.
  - **The shared guards are wired to the Mac,** one negative each on the Mac fixture:
    - a write permission;
    - a `pull_request` trigger;
    - a tag-pinned action;
    - a `secrets.X`;
    - a `${{ }}` in a run script;
    - `cargo test --workspace` without `--no-fail-fast`;
    - a node step without `if: success() || failure()`;
    - a build without `--config src-tauri/tauri.bundle.conf.json`;
    - a release step;
    - an upload with `if-no-files-found: warn`.
  - **Mac rules,** each with its positive in the good fixture and these negatives:
    - runner `windows-latest`, and runner `macos-13`;
    - the Installer job without `Swatinem/rust-cache` (Mac) — the Windows "build restores a cache" test stays its pair;
    - no `brew install nasm`, `brew install nasm` after the first cargo step, no `nasm -v`, `choco install nasm` instead;
    - the upload with name `lunaepix-windows-installer`, or with path `target/release/bundle/macos/*.app`.
  - **The check step:**
    - no check step;
    - the check step after the upload;
    - the check step before `cargo tauri build`;
    - `codesign --verify --deep --strict` only in a shell comment;
    - no `plutil -extract`;
    - no `hdiutil attach`.
- [ ] **Step 2:** Run `cargo test -p lunaepix --test workflow` and record the RED output.
- [ ] **Step 3:** Write `macos.yml` from `windows.yml`, with the changes the spec lists and comments in the same voice. The check step's script must keep this shape (it is the gate, so it is given here). Bash on Actions runs with `-e -o pipefail`, so every failing line fails the step.

  ```bash
  app=target/release/bundle/macos/Lunaepix.app
  check_app() {
    test -x "$1/Contents/MacOS/lunaepix-worker"
    codesign --verify --deep --strict --verbose=2 "$1"
    details=$(codesign -dv "$1" 2>&1)
    echo "$details"
    grep -qx 'Signature=adhoc' <<<"$details"
    grep -q '^Sealed Resources version=2' <<<"$details"
    plutil -extract 'files2.MacOS/lunaepix-worker' xml1 -o /dev/null \
      "$1/Contents/_CodeSignature/CodeResources"
  }
  check_app "$app"
  dmg=$(ls target/release/bundle/dmg/*.dmg)
  mount=$(mktemp -d)
  hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg"
  trap 'hdiutil detach "$mount"' EXIT
  check_app "$mount/Lunaepix.app"
  ```

  The step's own failure arms (worker missing, verification failing, not ad hoc, not in the seal) can only fire on a broken Mac build. They are not staged, because producing a broken build on purpose is breaking production code to prove a test. The branch table lists them as "untested: needs a broken bundle; guarded by `mac_check_step_problems` for presence and order".

The worker's name inside the bundle (`lunaepix-worker`) is what Tauri gives an `externalBin` there. If the first run shows another name, the name changes in the script and the guard together; that is a fixture fix. `grep` reads `codesign`'s free-text output, which is not a structured file. `CodeResources` is a property list and is read with `plutil`.
- [ ] **Step 4:** `cargo test --workspace` and clippy pass. `git diff .github/workflows/windows.yml` is empty.
- [ ] **Step 5:** Commit "macOS build workflow: tests on Apple Silicon, then an ad-hoc signed dmg with the worker sealed in".

### Task 6: README, version 0.1.4 and the coworker's check

**Files:**
- Modify: `README.md` (new section "Getting the Mac app" after "Getting the Windows installer", worded as in the spec, with the artifact name and the Open Anyway steps). Also change any sentence that says the app is built for Linux and Windows only.
- Modify: `Cargo.toml` (`version = "0.1.4"`) and `Cargo.lock`, which `cargo build` brings along.

- [ ] **Step 1:** Write the README section and bump the version.
- [ ] **Step 2:** Run the full checks:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`
  - `cargo test --workspace`
  - `node --test "tests/ui/*.test.js"`
  - the `debug(` grep from `~/.claude/CLAUDE.md`

  All pass.
- [ ] **Step 3:** Write the coworker's check into the report, not the repository. It is the spec's six items plus:
  - 7: drop an iPhone `.heic` photo; it is listed as skipped with a reason, and nothing crashes.
  - 8: in Single image at 100 % zoom on the Mac's own screen, the picture is sharp.
- [ ] **Step 4:** Commit "Document the Mac download; version 0.1.4".

### Task 7: The Mac run (controller, not an implementer)

**One tag only. Then report and ask** ("1 now, then ask").

- [ ] **Step 1: Everything off the Mac first.**
  - Run the Task 6 Step 2 checks on the merged branch.
  - Read every `cfg(unix)`, `cfg(target_os = …)`, `/proc`, `/dev/`, `sleep`, `head -c` and `chmod` use in the tests. List the ones the Mac meets for the first time, each with why it should hold on macOS (BSD tools, APFS, no `/proc`). If one cannot hold, it becomes a fixture fix through an implementer, with tables, before the tag.
- [ ] **Step 2:**
  - `git push -u origin feature/lunaepix-apple`
  - `git tag -a v0.1.4-rc1 -m "Lunaepix 0.1.4 rc1: first Mac build"`
  - `git push origin v0.1.4-rc1`
- [ ] **Step 3:** Follow the runs with `gh run list --limit 4` and `gh run watch <id>`, for both the "macOS build" and the "Windows build" runs.
  - **Green:** note the artifact `lunaepix-macos-dmg` and its expiry.
  - **Red:** `gh run view <id> --log-failed`. Sort each failure into fixture or behaviour difference (the spec's "Stop and ask").
- [ ] **Step 4: Report to the user and stop.** The report covers:
  - each run's result and duration;
  - the real cost (Mac minutes × price, or the billing page);
  - every failure, sorted;
  - the proposed fixes;
  - the coworker's check list if the run was green.

  A second tag needs the user's go-ahead.

After the coworker's check passes, `/finalize-plan` closes the plan. It moves the spec and plan to `docs/finished/` and updates the roadmap row to "Mac download built on GitHub (0.1.4); brew formula waits for a public repository". Merging and pushing `main` happen only when the user asks.

## Implementer report

Every implementer and every fix round writes, **before any code**:

1. **The test table:** behaviour → positive → negative → reached by → fails on its own because….
2. **The failure-branch table:** each branch → the test that reaches it, or "untested" with the reason.

Then:
- the RED run (command and output, taken before the production code existed);
- the GREEN run;
- the "Reuse survey" block;
- the files changed.

Reviewers check the diff against both tables and probe other spellings in every structured-file check. Lunaepix has no Grunt build and no devsync deployment, so nothing is uploaded to a dev server.

## CMS changes

No CMS changes.
