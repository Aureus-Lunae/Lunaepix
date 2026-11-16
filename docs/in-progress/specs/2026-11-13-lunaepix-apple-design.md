# Lunaepix Mac build — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-13

## Handoff

Decided in conversation (2026-11-13). The user's words are quoted as they were said.

- **Plan approved by the user (2026-11-13):** "approved". Plan: `docs/in-progress/plans/2026-11-13-lunaepix-apple.md`. Execute in a fresh session, subagent-driven.
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
- **rc1 result and going public (2026-11-16):** the user: "Mac failed on the test. We are going public". The repository is public now. The one Mac failure was `every_accepted_fixture_name_saves`: both spellings of "vakantie-café" (one character, and e + combining accent) went into one folder, and macOS treats them as one name. Asked how to fix it; the user chose "Fresh folder + Mac test (Recommended)": each fixture name saves into its own folder, plus a Mac-only test that the other spelling of an existing name is refused and the existing file stays untouched, with the Linux counterpart (both saved) as the negative.
- **rc tags after going public (2026-11-16):** the cap of one tag was set for cost; public runs cost no Actions minutes. The user chose "Tag as needed (Recommended)": the builder fixes, pushes and tags rc2, rc3 and so on itself until the Mac build is green, and reports then. A real behaviour difference still stops and asks. This replaces "1 now, then ask".
- **Brew formula (2026-11-16):** the user chose "Later, own roadmap item (Recommended)": this plan finishes with the Mac download; the brew formula becomes a separate roadmap item, brainstormed and planned on its own.
- **Branches (2026-11-16):** the user: "Now, we use main for releases, and development as the branch we work on till we are ready for release that is stable enough." `development` was created from `main` at 97a1e15. This branch merges into `development` when finished, not into `main`; `main` gets a release only when the user asks.

## Problem

Lunaepix is built for Linux (locally) and for Windows (GitHub Actions, `.github/workflows/windows.yml`). There is no Mac build. A few coworkers with Apple Silicon Macs cannot use it.

There is no Apple developer account. Paying for one ($99 a year) is ruled out on the roadmap. So the app cannot be signed with a developer certificate or notarized. macOS blocks the first launch of such an app until the user allows it once under System Settings › Privacy & Security › "Open Anyway".

## Goals

1. A GitHub Actions workflow builds an Apple Silicon `.dmg` of Lunaepix. It starts the same way as the Windows build: by hand, or when a `v*` tag is pushed. The `.dmg` is kept as a run artifact for 30 days.
2. Before the `.dmg` is built, the whole workspace is checked on a Mac: formatting, clippy, every Rust test and the window script tests.
3. The `.dmg` holds an app that a coworker can open after one "Open Anyway". Opening it never ends in "Lunaepix is damaged and can't be opened".
4. The app in the `.dmg` includes the worker sidecar, so it compresses, makes thumbnails and previews at the same speed as on Linux and Windows.
5. The README explains how to get the Mac app and how to get past the first-launch block.
6. Version 0.1.4.

## Non-goals

- **The brew formula.** It waits until the repository is public. A formula needs a public link to the source.
- **Making the repository public**, or a separate public release repository.
- **An Intel or universal build.**
- **The automated draft release.** That is its own roadmap item. The Mac build keeps its `.dmg` as a run artifact, as Windows does.
- **Signing with an Apple developer certificate, or notarization.**
- **Any change to how the app behaves.** Platform code changes only where the Mac test run shows a real defect, and then only after asking the user (see "Stop and ask").
- **Tauri's updater, the Mac App Store, and a Mac menu bar beyond what Tauri gives by default.**

## Anti-goals

These would make the work a failure even if a `.dmg` comes out.

- **A credential in the workflow.** No secret, no certificate, no `contents: write`, no pull-request trigger. The token can only read the repository, as in the Windows workflow.
- **"Lunaepix is damaged".** On Apple Silicon, code with no signature at all does not run, and Gatekeeper reports it as damaged. That message has no "Open Anyway" route.
- **A `.dmg` without the worker.** The app would still run, but each file would be about ten times slower, and nothing would show why.
- **The Windows build changes.** `windows.yml` and what it produces stay as they are, apart from changes needed to share the workflow guards.
- **A Mac-only test fix that hides a real behaviour difference.** For example, adjusting a test because the Mac's file system ignores case, when the app itself would then overwrite a file on a Mac.

## Design

### The workflow: `.github/workflows/macos.yml`

This is a new file kept separate from `windows.yml` (my call). Its shape and comments follow `windows.yml`:

- **Name:** "macOS build".
- **Triggers:** `workflow_dispatch`, and `push` of tags matching `v*`. A `v*` tag therefore starts both workflows.
- **Settings shared with Windows:** `permissions: contents: read`, the same `concurrency` group pattern, `CARGO_TERM_COLOR: always` and `shell: bash`.
- **Runner:** `macos-latest`, which is an Apple Silicon runner (my call, mirroring `windows-latest`).
- **Actions:** the same as Windows, at the same pinned commit hashes with their release comments: checkout (`persist-credentials: false`), rust-toolchain, rust-cache (both jobs, see "Build cache"), setup-node (Node 22) and upload-artifact.
- **NASM:** none. No NASM on the Mac: mozjpeg assembles its Arm SIMD with clang; NASM is only for x86 (decided 2026-11-13, see Handoff). The Windows build still installs it.
- **Test job:** formatting (`cargo fmt --all --check`), lints (`cargo clippy --workspace --all-targets -- -D warnings`), then `cargo test --workspace --no-fail-fast`. After that come the window script tests (`node --test "tests/ui/*.test.js"`) with `if: success() || failure()`, the same as Windows.
- **Installer job,** which `needs: test`:
  - Install `tauri-cli` at `--locked --version 2.12.4`.
  - Run `cargo tauri build --config src-tauri/tauri.bundle.conf.json`.
  - Then a check step (below). It fails the job if the app, the worker or the signature is wrong.
  - Then upload the `.dmg`: artifact name `lunaepix-macos-dmg`, path `target/release/bundle/dmg/*.dmg`, `if-no-files-found: error`, `retention-days: 30`.
- **Build cache:** both jobs use `Swatinem/rust-cache`, at the same pinned hash as Windows. The Installer job's cache is its own (release profile), and it also keeps the installed `tauri-cli` in `~/.cargo/bin`, so later runs skip that install. This differs from Windows, which has no Installer cache. Here Mac minutes cost more than the cache upload does. Both cache steps save only from `main` (`save-if: ${{ github.ref == 'refs/heads/main' }}`), because rc tags cannot restore each other's cache, only `main`'s (decided 2026-11-13, see Handoff). The "Rust tests" step also carries `if: success() || failure()`, so one rc run reports a failed formatting or lint step and the test failures together.
- **Timeouts:** the same as Windows (45 and 60 minutes). The builder may raise them if a first run shows they are too short.

### Getting Mac runs while building

The workflow only exists on the feature branch, and GitHub's "Run workflow" button needs it on `main`. So runs come from rc tags, as with 0.1.3:

- **How:** push `feature/lunaepix-apple`, then tag `v0.1.4-rc1`, `v0.1.4-rc2` and so on (annotated) on it. The builder may push and tag without asking each time. Each tag also runs the Windows build.
- **Minutes are limited.** Mac minutes count about ten times on a private repository, so:
  - **One** rc tag (`v0.1.4-rc1`), then a report to the user with what the run showed and its real cost, read from the billing page or the run's timing. Every further tag needs the user's go-ahead ("1 now, then ask").
  - Before each tag, the builder does everything that can be done off the Mac. That means the full Linux suite, clippy, and the Windows-gnu clippy check used before. It also means reading every `cfg(unix)`, `cfg(target_os = …)` and `/proc` use that the Mac will meet for the first time, because Linux clippy cannot see Mac-only paths.
  - One rc run reports every failure at once (`--no-fail-fast`, and the node step runs even after a failed Rust step). All of them are fixed before the next tag.
- **Done** means a green Mac run (Test and Installer), the check step passing, and a downloadable `lunaepix-macos-dmg`. The coworker's hand check comes after that.

**The check step** runs on the built `Lunaepix.app` under `target/release/bundle/macos/`. It fails the job when any of these is not true:

1. `Contents/MacOS/` holds both the app and the worker (`lunaepix-worker`, with or without the triple suffix, whichever name Tauri gives it inside the bundle).
2. `codesign --verify --deep --strict` succeeds on the `.app`. This is the check that matters. It fails when the worker was changed or added after the app was sealed.
3. `codesign -dv` on the `.app` reports an ad-hoc signature (`Signature=adhoc`) with sealed resources. This proves Tauri signed the bundle itself. The linker's own signature on each loose binary does not count, because Apple Silicon's linker signs every binary ad hoc anyway.
4. The app's seal (`Contents/_CodeSignature/CodeResources`) lists the worker. This proves the worker is part of the signed app, not just signed on its own. The file is a property list, so it is read with `plutil`, never by a text search.

### Bundle configuration: `src-tauri/tauri.macos.conf.json`

Tauri merges a platform file like this over `tauri.conf.json` on that platform only. Linux and Windows builds never read it (my call; it keeps the Mac settings out of the shared file). It sets:

- `bundle.targets`: `["app", "dmg"]`. On a Mac this replaces the Linux and Windows list.
- `bundle.macOS.signingIdentity`: `"-"`, which is an ad-hoc signature. It needs no certificate and no account. Apple Silicon needs it to run the app at all, and it is what turns "damaged" into "Open Anyway".
- `bundle.macOS.minimumSystemVersion`: `"11.0"`. macOS 11 is the first version for Apple Silicon (my call).

The icon is already there (`src-tauri/icons/icon.icns`).

The worker is built for the host triple by `crates/xtask`, which refuses cross builds. The Apple Silicon runner builds `lunaepix-worker-aarch64-apple-darwin`, which matches the app. No change to xtask is expected.

### Guard tests

**`src-tauri/tests/workflow.rs`** today checks `windows.yml` only. It is extended so that every guard that applies to any workflow runs on both files:

- actions pinned to a full commit hash with a release comment;
- a read-only token, and no secrets;
- no pull-request trigger, and no `${{ }}` inside a run script;
- the triggers are `workflow_dispatch` plus `v*` tags;
- no release is made;
- NASM is installed in every job that runs cargo, before the first cargo step (Windows; on the Mac no step may install or run NASM);
- `--no-fail-fast`, and the node step running after a failed Rust step.

The NASM guard keeps its Windows rule (a pinned Chocolatey version). On the Mac it refuses any step that installs or runs NASM. The Mac profile also requires every `rust-cache` step to have exactly the `save-if` above and the `cargo test` step to carry `if: success() || failure()`; Windows has neither rule.

New guards for `macos.yml` only:

- each job runs on a `macos-*` runner;
- the Installer job builds with the bundle config file;
- the check step is present between the build and the upload, and contains `codesign --verify --deep --strict`;
- the upload step uses the artifact name and path above, with `if-no-files-found: error` and 30 days.

Each guard gets a positive test (the good file passes) and a negative test (a version of the good file with that one rule broken is reported), in the style of the existing `good()` and `with()` fixtures. The YAML is read with the YAML parser the file already uses, never by text search on the YAML.

**`src-tauri/tests/config.rs`** pins `bundle.targets` to `["deb", "rpm", "nsis", "msi"]` today. It gains a guard for `tauri.macos.conf.json`, with a positive and a negative test for each part:

- the file exists and parses as JSON;
- its `bundle.targets` is exactly `["app", "dmg"]`;
- `bundle.macOS.signingIdentity` is `"-"`;
- the file sets nothing outside `bundle.targets` and `bundle.macOS`. This keeps the Mac file from quietly changing the window, the security policy or the resources (my call).

The guard on the shared `tauri.conf.json` stays as it is.

### Running the existing tests on a Mac

The first Mac run is the first time the suite runs on macOS, and it will probably show failures. How to treat them:

- **A test fixture that assumes Linux or Windows** (a path, a permission, a `/proc` read, a command that differs on the Mac): fix the fixture so the test checks the same behaviour on the Mac. Examples are the `cfg(target_os = "linux")` tests that check a stopped or timed-out worker is gone through `/proc/<pid>`. They are in `crates/core/src/run.rs`, `crates/core/tests/run.rs`, `src-tauri/src/session.rs` and `src-tauri/tests/session_run.rs`, and the plan lists them. They should get a Mac way to show the process is gone (for example `kill(pid, 0)` reporting no such process), so the kill-and-reap behaviour is tested on the Mac too. Tests of Linux-only features stay Linux-only: the OOM score, `PR_SET_PDEATHSIG`, the `/proc/self/status` growth test in `thumb.rs`, and the case-sensitive folder tests.
- **A real behaviour difference in the app** (for example, case-insensitive APFS making two output names collide that differ only in case): stop and ask. See below.

Clippy on the Mac also sees the `cfg(unix)` code paths, which Windows clippy skips. A clippy finding there is fixed in the code like any other lint.

### README

A new section "Getting the Mac app" goes after "Getting the Windows installer", in the same style:

1. Open Actions, choose "macOS build" and press "Run workflow". A `v*` tag starts it too.
2. When the run is green, download the `lunaepix-macos-dmg` artifact. GitHub delivers it as a zip, and the zip holds the `.dmg`.
3. Open the `.dmg` and drag Lunaepix into Applications.
4. It is for Macs with Apple Silicon (M1 or later) and macOS 11 or later.
5. The app is **not signed with an Apple developer certificate**, so macOS blocks the first launch. Open it once, close the warning, then go to System Settings › Privacy & Security, scroll to Security, and choose "Open Anyway" next to the message about Lunaepix. After that it opens normally.
6. Right-click › Open no longer gets past the block since macOS Sequoia.

The "Building on Linux" section is not changed. Building locally on a Mac is not documented in this round (my call: nobody here builds on a Mac).

### Third-party licences

`THIRD-PARTY-LICENSES.html` is installed with the app. Today it is generated for Linux and Windows only: `about.toml` has `targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]`. So the crates that only the Mac build uses are missing from it, for example the `objc2` family under Tauri's window code. A `.dmg` without their notices breaks their licences, even when it only goes to coworkers.

The changes:

- Add `aarch64-apple-darwin` to `targets` in `about.toml`, and update the comment above it.
- Regenerate the HTML with `cargo about`, as the README's licence section describes.
- Extend the guard on `about.toml` in `config.rs` so that the targets list includes the three triples, with a positive and a negative test.

If `cargo about` refuses a licence outside `accepted`, the builder stops and reports it to the user, as `about.toml` itself says. The `accepted` list is never widened to make a run pass.

### Version

Workspace `Cargo.toml` goes to `version = "0.1.4"`, with `Cargo.lock` following.

### Roadmap

When this work is finalized, `docs/roadmap.md` gets the following:

- The "Apple support" row reads "Mac download built on GitHub (0.1.4); brew formula waits for a public repository".
- The section describes what was built.

## Hand check (the coworker)

This is a short check on an Apple Silicon Mac, as the user asked. The user gives the coworker the `.dmg` from a green run. The coworker reports back to the user:

1. The `.dmg` opens, and Lunaepix can be dragged into Applications.
2. The first launch is blocked. "Open Anyway" in Privacy & Security lets it open, and the second launch opens without a question. It never says "damaged".
3. Batch: drop a PNG, a JPG and a WebP on the window. Each row gets a thumbnail.
4. Run lossless, and the results show smaller sizes. "Open folder" opens Finder at the output.
5. Run lossy at quality 75 on the same files with "Re-add". This works and does not overwrite the first outputs.
6. Single image: load one picture. The before/after divider and zoom work, and "Save as" under a new name writes the file.

If the coworker cannot get past the first-launch block, the README text is wrong. That is a finding, not a coworker error.

## Stop and ask

The builder stops and asks the user instead of choosing in these cases:

- A Mac test failure that is a real behaviour difference in the app, not a fixture.
- The `.dmg` step fails on the runner for a reason that is not a simple fix, for example Tauri's disk-image script needing a Finder session the runner lacks. A fallback, such as shipping a zipped `.app`, changes what the coworker receives, so it is the user's call.
- Ad-hoc signing cannot be had through `signingIdentity: "-"` in Tauri 2.12. Signing by hand afterwards with `codesign --force --deep -s -` is a different shape, so it is the user's call.
- A change to `windows.yml` beyond what sharing the guards needs.
- `cargo about` refusing a Mac-only crate's licence.

## Left to the builder

- How `workflow.rs` is split into guards for any workflow and guards per platform, and how the fixtures are shared.
- The exact shell of the check step, and the name of the worker inside the bundle, which the first build shows.
- How each Linux- or Windows-assuming test fixture gets its Mac form.
- Timeouts, if a first run shows the Windows values are too short.

## Calls I made

None of these was decided by the user. Check them while reading:

1. **Apple Silicon only, no Intel or universal build:**
   - Nobody here has an Intel Mac, so an Intel build could never be checked by hand.
   - macOS 26 Tahoe is the last macOS for Intel Macs, and Apple stopped selling them in 2023.
   - The brew formula (later) builds from source and would cover Intel Macs anyway.
   - If an Intel user asks for a download, a second job can be added then.
2. **A separate `macos.yml`** next to `windows.yml`, not one workflow with two platforms. Each file stays short, and a Mac failure does not mark the Windows run red.
3. **`macos-latest`** as the runner, as Windows uses `windows-latest`.
4. **No NASM on the Mac.** (Changed 2026-11-13 after the final review; the user chose "Drop it before the tag": see Handoff.) mozjpeg assembles its Arm SIMD with clang; NASM is only for x86.
5. **`tauri.macos.conf.json`** for the Mac settings, guarded so it can only touch `bundle.targets` and `bundle.macOS`.
6. **macOS 11 as the minimum.**
7. **A check step after the build** that proves the worker is inside the app and that both are signed ad hoc. Without it, a missing worker or signature only shows up on the coworker's Mac.
8. **No local Mac build instructions** in the README.
9. **Artifact name `lunaepix-macos-dmg`**, matching `lunaepix-windows-installer`.
10. **Mac crates in the licence page.** `about.toml` gains the Mac target and the HTML is regenerated, because the `.dmg` ships code whose licences require their notice.
11. **The signature check proves the seal.** The check proves the worker is inside the app's seal, not only that each binary carries some signature (the reviewer pointed out the linker already signs them).
12. **A build cache in the Installer job** (from "Limited: keep runs few"). The tag cap itself is the user's: "1 now, then ask".
