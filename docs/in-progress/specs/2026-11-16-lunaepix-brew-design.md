# Lunaepix brew formula — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-16

## Handoff

Decided in conversation (2026-11-16). The user's words are quoted as they were said.

- **Spec approved by the user (2026-11-16):** "Approved", including the "Calls I made" list.
- **Start:** "Brew formula can already be started". This is the roadmap item "Apple support: a brew formula", split off from the Mac download earlier the same day ("Later, own roadmap item").
- **No separate repository:** "If we can't do the brew part, we won't make it. I do not want a separate repo just to allow that". Homebrew can tap this repository itself when the address is given once, so the formula lives here.
- **What it installs:** "Formula in this repo, both app and command line".
- **How the app is built:** offered (1) Tauri's own bundler, the same `cargo tauri build` as the Mac workflow, with the pinned Tauri command line built inside the formula's build folder (about 5–10 extra minutes on first install), or (2) plain cargo plus assembling the `.app` by hand (faster, but a second copy of the bundling logic). The user chose "1".
- **Getting the app into Applications:** offered linking (follows `brew upgrade`, but Spotlight and Launchpad do not list a linked app) or copying (found by Spotlight, but stays on the old version after an upgrade). The user chose "Link it".
- **Context from earlier the same day:** the repository is public ("We are going public"), so Actions runs on standard runners cost no minutes. `main` is for releases and `development` is the working branch ("Now, we use main for releases, and development as the branch we work on till we are ready for release that is stable enough").
- **Starting point:** `development` at 9324ccd (0.1.4, the Mac download, merged). This work goes on `feature/lunaepix-brew` from `development`, and merges back into `development`. `main` gets it only with a release the user asks for.

**Standing process rules** (the user's, from earlier Lunaepix plans; they apply here):

- Tables before code: a test table (behaviour → positive → negative → reached by → fails on its own because…) and a failure-branch table, from every implementer and every fix round.
- Structured files (YAML, JSON, TOML, HTML, property lists) are checked with a real parser. Text that is not a data format (a shell `run:` body, the Ruby formula) may be read as text, and the check then says in a comment that it is a lint.
- Comments follow the code: docs of guards, profiles and modules whose use the change widens are updated in the same change.
- Always bump the version: 0.1.5.
- Every behaviour has a positive and a negative test. No mutation runs.

## Problem

The Mac download (0.1.4) is an unsigned `.dmg`. On first launch macOS blocks it, and the user has to find "Open Anyway" in System Settings. A program built on the Mac itself is not quarantined, so Gatekeeper never asks. Homebrew can do that build: the user types two commands and waits for the compile.

## Goals

1. On an Apple Silicon Mac, these two commands build Lunaepix from source and install it:

   ```
   brew tap aureus-lunae/lunaepix https://github.com/Aureus-Lunae/Lunaepix
   brew install aureus-lunae/lunaepix/lunaepix
   ```

   Afterwards `lunaepix` (the command line) is on the PATH, and `Lunaepix.app` is in the formula's folder.
2. The app opens with no Gatekeeper question at all, the first time and every time.
3. The note Homebrew prints after the install tells the user how to link the app into Applications, once.
4. After a release, `brew upgrade` builds the new version, and the link in Applications follows it without being redone.
5. Every tag's Mac run proves that the formula builds this commit's source, passes `brew test`, and passes Homebrew's own checks of the formula.
6. The README's Mac section explains the brew route next to the download.

## Non-goals

- Homebrew's main repository (homebrew-core). It has a popularity bar and does not take window apps.
- A separate tap repository (the user ruled it out).
- A cask. Homebrew's cask repository no longer accepts unsigned apps, and a cask would install the same quarantined download.
- Intel Macs. The Mac download is Apple Silicon only, and on Intel the JPEG library would also need NASM. The formula refuses Intel with Homebrew's own message.
- Homebrew on Linux. The command line builds on Linux already with `cargo`; the formula is for the Mac.
- Prebuilt "bottles". They would need hosting and would no longer be a local build.
- Bumping the formula automatically at a release. That belongs to the roadmap item "Automated draft release". Here the bump is a documented hand step.
- A `--HEAD` install from `development`. `development` can be mid-change; users get releases.

## Anti-goals

- A second copy of the bundling logic: no hand-built `Info.plist`, no hand-placed worker, no separate signing step. The app comes out of `cargo tauri build` exactly as the Mac workflow builds it.
- Anything outside Homebrew's folders: the formula never writes to `/Applications`, never to the user's `~/.cargo`, and never asks for a password.
- A credential, an Apple account or a write permission anywhere.

## Design

### The formula: `Formula/lunaepix.rb`

A tap given by address reads formulas from the `Formula/` folder at the repository's root, on the default branch (`main`).

- **Source:** `url "https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v0.1.5.tar.gz"` with its `sha256`, `license "MIT"`, `homepage` the repository, and a one-line `desc`.
- **Requirements:** `depends_on "rust" => :build`, `depends_on arch: :arm64`, and `depends_on macos: :big_sur` (macOS 11, the same minimum as `tauri.macos.conf.json`; it also limits the formula to macOS). No NASM and no Node: the Mac needs neither (see the Mac build spec).
- **Checksum between releases:** on `development`, and in the commit a release tag points at, `sha256` holds a placeholder of 64 zeros. The real checksum only exists after the tag (see "The release step"). The guards accept exactly the placeholder or 64 lower-case hex characters.
- **The app is left as built:** `skip_clean "Lunaepix.app"`. After an install, Homebrew's clean-up may strip or rewrite Mach-O files in the formula's folder; any change to a file inside the app breaks its seal, and `codesign --verify --deep --strict` then fails. `skip_clean` is not a signing step, so it stays within the anti-goal. If the installed app still fails the check, re-signing it would be a separate signing step: stop and ask.
- **Install, in this order:**
  1. The command line: `cargo install` of `crates/cli` with Homebrew's standard arguments plus `--target-dir target`, so `lunaepix` lands in the formula's `bin` and the libraries it compiles are reused by the app build in step 3 instead of compiled a second time.
  2. The Tauri command line, at exactly the version both workflows pin (2.12.4 today), installed with `--locked` into a folder inside the build folder, and that folder's `bin` put first on the build's PATH (Homebrew filters PATH, and `cargo tauri` looks for `cargo-tauri` there). It never reaches the user's `~/.cargo`, and it is gone after the install.
  3. `cargo tauri build` with `--bundles app` and `--config src-tauri/tauri.bundle.conf.json`. This is the Mac workflow's build command without the disk image. It runs the worker staging step, bundles the app, and signs it ad hoc with the worker sealed in, exactly as the download.
  4. `Lunaepix.app` from `target/release/bundle/macos/` goes into the formula's folder.
- **The note after install (caveats).** This is the exact text:

  ```
  Lunaepix.app is installed in:
    #{opt_prefix}/Lunaepix.app

  To open it from the Applications folder, link it there once:
    ln -sfn "#{opt_prefix}/Lunaepix.app" /Applications/Lunaepix.app

  The link keeps working after `brew upgrade`. Spotlight and Launchpad
  do not list a linked app: open Lunaepix from the Applications folder.
  ```

  `opt_prefix` (`/opt/homebrew/opt/lunaepix`) always points at the installed version, so the link survives upgrades. `-n` stops `ln` from following an existing link into the app.
- **`brew test`** runs against the installed copy in Homebrew's folder, not the build folder:
  - `bin/lunaepix` compresses a small PNG that the test writes itself under the test's own folder, with the output folder set there too, and the output file exists.
  - `Lunaepix.app/Contents/MacOS/lunaepix-worker` exists and is executable.
  - `codesign --verify --deep --strict` passes on the app.

### Proving it on every tag: a "Formula" job in `macos.yml`

A third job in the Mac workflow, with `needs: test` (strict, as the Installer) and no build cache: it builds the way a user's Mac does.

- The published formula points at a release archive that does not exist yet for an rc tag, and whose checksum is only known after the release tag is pushed. So the job does two things, both in local taps (Homebrew does not install a loose formula file):
  - **Checks the committed formula as it is:** `brew audit` (offline) and `brew style` on `Formula/lunaepix.rb` unmodified, so the real `url` line and the placeholder checksum are what gets checked.
  - **Builds this commit's source with the formula's own steps:** it makes a source archive of the checkout, writes a copy of the formula whose only change is that archive's local address and checksum, and runs `brew install --build-from-source`, then `brew test` on the installed result.
- On an rc tag such as `v0.1.5-rc1` the formula's `url` still names `v0.1.5`: the guard compares it with the workspace version, never with the tag.
- Everything else the Mac workflow's rules require holds for this job too: pinned actions, read-only token, no `${{ }}` inside a run script, a timeout (60 minutes).

### Guards (`src-tauri/tests/workflow.rs`, or a sibling test file)

- The Mac workflow's profile requires the Formula job, with `needs: test`, and its steps pass the same rules as the other jobs.
- **One version, three places:** the Tauri command line version in the formula equals the version pinned in `macos.yml` and `windows.yml`. A drift would build the brew app with another bundler than the checked download.
- **The formula's source is this release:** its `url` is this repository's tag archive for `v` + the workspace version.
- The formula is Ruby, not a data format, so these checks read it as text and say so in a comment (a lint), as the existing `run:` checks do. Each has a positive and a negative fixture.

### The release step for the formula (documented, by hand)

At a release (`development` → `main`, tag `v0.1.5`), the formula's checksum is filled in by one commit on `main` right after the tag: "Formula: 0.1.5". It cannot be in the tagged commit itself, because the checksum is of that commit's archive. Until that commit is pushed, `brew install` from the tap fails with a checksum mismatch. The window is minutes; the README's release notes for the maintainer say so. The same commit goes back into `development`.

This step is written down in the README's maintainer part (or a short `docs/releasing.md`, the builder's call), as a checklist: tag, download the archive, `shasum -a 256`, edit the formula, commit, push both branches.

### README

The Mac section gets "With Homebrew" next to "Download":

- the two commands;
- what the first install costs (Rust from Homebrew, and a build of roughly 15–25 minutes on an Apple Silicon Mac — the builder replaces this with the time the Formula job measures);
- the link command for Applications, and that Spotlight does not list a linked app;
- `brew upgrade` after a release;
- that the brew build never asks "Open Anyway".

### Version

0.1.5 (workspace `Cargo.toml`, the lock file, and the licence page's own crates).

### Roadmap

At finalize: "Apple support: a brew formula" → Done, with the spec and plan paths, and a note that it reaches users with the 0.1.5 release.

## Hand check

After the user releases 0.1.5 and the formula commit is on `main`, on an Apple Silicon Mac:

1. The two commands from Goal 1 finish without an error.
2. `lunaepix --help` (or the command line's usage) runs.
3. The link command from the note works, and Lunaepix opens from the Applications folder with no question from macOS.
4. A batch run and a single image save work (the short version of the Mac download's check, items 3–6).

## Stop and ask

- `cargo tauri build` cannot run inside Homebrew's build environment (for example it writes outside the build folder and the sandbox refuses), and the way out would be assembling the app by hand. That is option 2, which the user did not choose.
- Homebrew's checks refuse a formula that installs an `.app`, or require something that contradicts a Handoff line.
- The first build on the runner takes far longer than the README can honestly call acceptable (over 45 minutes).

## CMS changes

No CMS changes.

## Left to the builder

- The exact mechanics of the Formula job's local install (how the archive and the local tap are made), as long as the formula's own install steps are what runs.
- The test PNG, and the exact command line call in `brew test`.
- Whether the guards live in `workflow.rs` or a new test file.
- README wording, and where the release checklist lives.

## Calls I made

1. **Apple Silicon only, macOS 11 or newer,** matching the download.
2. **No `--HEAD`, no bottles, no automatic bump** (see Non-goals).
3. **The Formula job runs on every tag, with `needs: test` and no cache.** Runs are free now; a user's Mac has no cache either.
4. **Version 0.1.5,** so the first formula ships with the 0.1.5 release, not 0.1.4.
5. **The caveat text above,** including `ln -sfn` and the Spotlight sentence.
6. **The release step is by hand for now,** written down as a checklist.
7. **The placeholder checksum is 64 zeros** until the release commit fills it in.
8. **`skip_clean "Lunaepix.app"`** from the start, so Homebrew's clean-up cannot break the app's seal. Re-signing after install stays a stop-and-ask.
9. **The command line shares the app's build folder** (`--target-dir target`), so the heavy libraries compile once.
10. **The committed formula is audited unmodified;** only the build-and-test copy has its source address changed.
11. **rc tags as needed** for this work, as you decided for the Mac build after the repository went public: the builder tags rc1, rc2… itself until the Formula job is green, and a real behaviour difference still stops and asks.
