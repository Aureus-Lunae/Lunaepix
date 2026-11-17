# Lunaepix brew formula — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-16

## Handoff

These lines are copied verbatim from the spec's Handoff. The standing process rules and the calls made while planning follow them. Give every implementer these lines as they are. An implementer that wants to deviate from one stops and asks instead of choosing.

- **Spec approved by the user (2026-11-16):** "Approved", including the "Calls I made" list.
- **Plan approved by the user (2026-11-16):** "approved", including the calls made while planning. Execute in a fresh session, subagent-driven, on `feature/lunaepix-brew`.
- **Download users and the link (2026-11-16, at plan approval):** a coworker who installed the 0.1.4 `.dmg` has a real `Lunaepix.app` folder in Applications, so `ln -sfn` would put the link inside it. Asked whether to add "If Lunaepix from the download is in Applications, move it to the Bin first." to the after-install note, with a guard. The user chose "Add the sentence (Recommended)".
- **Start:** "Brew formula can already be started". This is the roadmap item "Apple support: a brew formula", split off from the Mac download earlier the same day ("Later, own roadmap item").
- **No separate repository:** "If we can't do the brew part, we won't make it. I do not want a separate repo just to allow that". Homebrew can tap this repository itself when the address is given once, so the formula lives here.
- **What it installs:** "Formula in this repo, both app and command line".
- **How the app is built:** offered (1) Tauri's own bundler, the same `cargo tauri build` as the Mac workflow, with the pinned Tauri command line built inside the formula's build folder (about 5–10 extra minutes on first install), or (2) plain cargo plus assembling the `.app` by hand (faster, but a second copy of the bundling logic). The user chose "1".
- **Getting the app into Applications:** offered linking (follows `brew upgrade`, but Spotlight and Launchpad do not list a linked app) or copying (found by Spotlight, but stays on the old version after an upgrade). The user chose "Link it".
- **Context from earlier the same day:** the repository is public ("We are going public"), so Actions runs on standard runners cost no minutes. `main` is for releases and `development` is the working branch ("Now, we use main for releases, and development as the branch we work on till we are ready for release that is stable enough").
- **Starting point:** `development` at 9324ccd (0.1.4, the Mac download, merged). This work goes on `feature/lunaepix-brew` from `development`, and merges back into `development`. `main` gets it only with a release the user asks for.

**Standing process rules** (the user's, from earlier Lunaepix plans; they apply to every task here):

- **Tables before code.** Every implementer and every fix round writes, before any code, a test table (behaviour → positive test → negative test → "reached by", every kind of input that reaches the behaviour with its own test → "fails on its own because…") and a failure-branch table (every `Err` path, fallback and platform- or setting-driven branch → the test that reaches it, or "untested" with the reason).
- **Controller gate.** Before each review the controller sends back any "untested" row on a branch that decides what the caller gets (unless the row says how a test could reach it and why that does not work), and any single-trigger row without a reason.
- **Controller RED audit.** A test missing from the RED run goes back before review.
- **Parse structured files.** YAML, JSON and TOML are read with a real parser. The Ruby formula and shell `run:` bodies are not data formats: they may be read as text, and the check then says in a comment that it is a lint. Reviewers probe other spellings.
- **Comments follow the code.** Docs of guards, profiles and modules whose use the change widens are updated in the same change (here: `workflow.rs`'s module doc and `Platform`, the header comment of `macos.yml`).
- **Always bump the version:** 0.1.5, in Task 1.
- **Every behaviour has a positive and a negative test.** No mutation runs; no production code broken to prove a test.

**Calls made while planning** (not the user's decisions; flagged for the plan review):

1. **The formula's guards live in a new test file, `src-tauri/tests/formula.rs`,** not in `workflow.rs`: they read the formula, both workflows and two config files, and `workflow.rs` is about the workflow files alone. The Formula job's own rules go into `workflow.rs`, beside the other job rules.
2. **The release checklist lives in `docs/releasing.md`,** linked from the README's maintainer part.
3. **Version 0.1.5 is set in Task 1,** together with the formula, because the formula's `url` guard compares with the workspace version.
4. **The guards also pin two more facts the formula repeats:** the macOS floor (a bare `depends_on :macos` while `minimumSystemVersion` in `tauri.macos.conf.json` is not newer than Homebrew's oldest macOS, Big Sur 11.0; above it, `depends_on macos: :<name>` naming that version — changed in Task 4, see "Changes during execution") and the bundle build line (`--bundles app` and `--config src-tauri/tauri.bundle.conf.json`, as in the Installer job). A repeated fact that drifts is a defect (`~/.claude/rules/security.md`, "Duplication is a security defect").
5. **The caveat's extra sentence** (Review Focus 1): the user agreed at plan approval (see Handoff).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Invoke `sdd-overrides` first.

**Goal:** a Homebrew formula in this repository that builds Lunaepix (the app and the command line) from source on an Apple Silicon Mac, proven on every tag by a Formula job in the Mac workflow, with version 0.1.5, the README and a release checklist.

**Architecture:** `Formula/lunaepix.rb` builds the command line with `cargo install` and the app with the same Tauri bundler build as the Mac workflow, calling the pinned `cargo-tauri` binary directly; text-lint guards in `src-tauri/tests/formula.rs` tie its repeated facts to the workflows and configs; a `formula` job in `macos.yml` audits the committed formula and builds `git archive` of the commit through a local tap.

**Tech Stack:** Homebrew (Ruby formula DSL), Rust/cargo, Tauri 2.12.4 CLI, GitHub Actions, `yaml_rust2` / `toml` / `serde_json` in the tests.

## Global Constraints

- **Spec:** `docs/in-progress/specs/2026-11-16-lunaepix-brew-design.md`. Executors read its Handoff and design before their task.
- **Formula file:** `Formula/lunaepix.rb` at the repository root; class `Lunaepix`.
- **Tap commands (exact):** `brew tap aureus-lunae/lunaepix https://github.com/Aureus-Lunae/Lunaepix` and `brew install aureus-lunae/lunaepix/lunaepix`.
- **Source:** `https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v0.1.5.tar.gz`; `sha256` is 64 zeros until the release commit.
- **Requirements:** `rust` at build time, `arch: :arm64`, and a bare `depends_on :macos` (macOS 11 is both the app's floor and Homebrew's oldest, so `brew style` refuses naming it). No NASM, no Node.
- **Tauri command line:** 2.12.4, `--locked`, installed under the build folder, never in `~/.cargo`.
- **App build:** `system tauri/"bin/cargo-tauri", "build", "--bundles", "app", "--config", "src-tauri/tauri.bundle.conf.json"`: the Mac workflow's `cargo tauri build`, with the installed binary called directly, because `brew style` refuses a `system` call that passes both `"cargo"` and `"build"`.
- **`skip_clean "Lunaepix.app"`.** No re-signing after install (stop and ask).
- **Formula job:** in `.github/workflows/macos.yml`, job id `formula`, `needs: test`, `runs-on: macos-latest`, `timeout-minutes: 60`, no cache step, read-only token, no `${{ }}` inside a run script, actions pinned by hash as in the rest of the file.
- **`windows.yml` is unchanged.** Its guards keep every assertion.
- **Version** `0.1.5`.
- **No secret, no write permission, no release** anywhere. The word "release" must not appear in any workflow value (the existing `release_hits` rule); paths under `target/release/` and `--release` are allowed by that rule.

## Review Focus

1. **The download's app is already in Applications.** A coworker who installed the 0.1.4 `.dmg` has a real folder `/Applications/Lunaepix.app`. `ln -sfn` then creates `Lunaepix.app/Lunaepix.app` inside it, and the Applications folder keeps opening the old download. The fix is copy, not code: the caveat sentence "If Lunaepix from the download is in Applications, move it to the Bin first." (the user agreed, see Handoff). Task 1 writes it and its guard checks it is there.
2. **Homebrew changes a file inside the installed app.** `skip_clean` is the answer; `brew test` on the installed copy (`codesign --verify --deep --strict`) in the Formula job is the proof (Task 2).
3. **The build needs something only the runner has.** The Formula job runs on a runner with Xcode and many tools preinstalled; a user's Mac has only Apple's command line tools. The job installs nothing beyond what the formula declares (no `rustup` step, no Node, no extra `brew install`), so a missing dependency fails the job (Task 2's guard refuses a toolchain or extra install step in the `formula` job).
4. **The source archive misses a file the build needs.** GitHub's tag archive is `git archive` of the tag: untracked or ignored files are absent. The Formula job builds from `git archive` of the checkout for the same reason (Task 2), so a build that only works in a working tree fails there.
5. **Two Tauri versions.** A workflow pin bumped without the formula builds the brew app with another bundler than the checked download. Task 1's guard ties the three together.

---

### Task 1: The formula, version 0.1.5, and its guards

**Files:**
- Create: `Formula/lunaepix.rb`
- Create: `src-tauri/tests/formula.rs`
- Modify: `Cargo.toml` (workspace `version = "0.1.5"`), `Cargo.lock` (the workspace crates), `THIRD-PARTY-LICENSES.html` (regenerated, only the workspace crates' versions change)

**Interfaces:**
- Produces: `Formula/lunaepix.rb`, which Task 2's job audits and builds; the test file's helpers stay private to it.

- [ ] **Step 1: Write the tables, then the guard tests (RED).** In `src-tauri/tests/formula.rs`. Module doc: what is guarded and why; the formula is Ruby, so every check reads it as text and is a lint; the workflows are read with `yaml_rust2`, `Cargo.toml` with `toml`, `tauri.macos.conf.json` with `serde_json` (check `src-tauri/Cargo.toml`'s dependencies for what the tests already use, and add nothing new). Each check is a function from text(s) to `Vec<String>` problems, like `workflow.rs`, so fixtures can feed it.

  | Check function | Real files pass | Fixtures it must catch (one problem each) |
  |---|---|---|
  | `tauri_version_problems(formula, macos_yml, windows_yml)` | yes | formula pins `2.12.3`; one workflow pins another version; the formula has no `tauri-cli` install; the formula installs `tauri-cli` without `--locked`; the version only in a Ruby comment |
  | `url_problems(formula, workspace_version)` | yes | `v0.1.4`; another repository; a branch archive (`refs/heads/main`); a `.zip`; no `url`; two `url` lines |
  | `sha256_problems(formula)` | yes (64 zeros) | 63 hex characters; upper-case hex; no `sha256`; two `sha256` lines. Also a positive fixture with a real-looking 64 lower-case hex value |
  | `macos_floor_problems(formula, tauri_macos_json)` | yes (bare `depends_on :macos` with `"11.0"`, not newer than Homebrew's oldest, Big Sur) | `depends_on macos: :big_sur` with `"11.0"` (the rc2 formula; brew style calls it redundant); no macOS line; both a bare and a named line; config `"12.0"` with a bare line; a named floor that differs from the config (`:monterey` with `"13.0"` or `"12.1"`). Positive above the floor: config `"12.0"` with `:monterey` |
  | `requirement_problems(formula)` | yes | no `depends_on arch: :arm64`; no `depends_on "rust" => :build`; a `depends_on "nasm"`; a `depends_on "node"` |
  | `app_build_problems(formula)` | yes | the `cargo-tauri build` call lacks `--bundles app`; lacks `--config src-tauri/tauri.bundle.conf.json`; no `cargo-tauri build`; a `cargo-tauri` that is not the one under the pinned install's `--root`; no `skip_clean "Lunaepix.app"` |
  | `cargo_build_problems(formula)` (added in Task 4) | yes | `system "cargo", "tauri", "build", …`; `system "cargo", "build", "--release"`; a call broken after `system(`; an assigned `x = system …`. A text lint for plain `system` calls only; `brew style` in the Formula job is the authority, and the forms it does not read are pinned in a fixture |
  | `caveat_problems(formula)` | yes | the `ln -sfn "#{opt_prefix}/Lunaepix.app" /Applications/Lunaepix.app` line missing; `ln -sf` without `n`; the Spotlight sentence missing; the download sentence ("move it to the Bin first") missing |

  Plus `the_real_files_pass_every_formula_check()` and `the_formula_checks_are_not_vacuous()` (the real formula has exactly one `url`, one `sha256`, one `cargo-tauri build` call). The Ruby `system tauri/"bin/cargo-tauri", …` form is an argument list: read the quoted words of that call, so `"--bundles", "app"` and `"--bundles=app"` both pass. Probe and document which spellings each check accepts.

- [ ] **Step 2: Run `cargo test -p lunaepix-app --test formula`** before the formula exists. Expected: every real-file test fails (no `Formula/lunaepix.rb`), every fixture test passes or fails as the stub says. Record the output.

- [ ] **Step 3: Write `Formula/lunaepix.rb`.** This is the exact shape (Homebrew's style rules decide the order of the stanzas; keep this order):

  ```ruby
  class Lunaepix < Formula
    desc "Make PNG, JPG and WebP images smaller, lossless or lossy"
    homepage "https://github.com/Aureus-Lunae/Lunaepix"
    url "https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v0.1.5.tar.gz"
    # The real checksum is filled in by the commit right after the release
    # tag (docs/releasing.md); it cannot be in the tagged commit itself.
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    license "MIT"

    depends_on "rust" => :build
    depends_on arch: :arm64
    # macOS 11 is the app's floor and Homebrew's own, so naming it is
    # redundant (brew style refuses it).
    depends_on :macos

    # Homebrew's clean-up may strip or rewrite Mach-O files after install;
    # any change inside the app breaks its seal.
    skip_clean "Lunaepix.app"

    def install
      system "cargo", "install", "--target-dir", "target", *std_cargo_args(path: "crates/cli")

      # Same version as both workflows (src-tauri/tests/formula.rs checks it),
      # inside the build folder, never in ~/.cargo.
      tauri = buildpath/"tauri-cli"
      system "cargo", "install", "tauri-cli", "--locked", "--version", "2.12.4", "--root", tauri

      # The Mac workflow's build, without the disk image: the worker is
      # staged and sealed in, and the app is signed ad hoc. The binary is
      # called directly because brew style refuses a system call with
      # "cargo" and "build".
      system tauri/"bin/cargo-tauri", "build", "--bundles", "app",
             "--config", "src-tauri/tauri.bundle.conf.json"
      prefix.install "target/release/bundle/macos/Lunaepix.app"
    end

    def caveats
      <<~EOS
        Lunaepix.app is installed in:
          #{opt_prefix}/Lunaepix.app

        To open it from the Applications folder, link it there once. If Lunaepix
        from the download is in Applications, move it to the Bin first.
          ln -sfn "#{opt_prefix}/Lunaepix.app" /Applications/Lunaepix.app

        The link keeps working after `brew upgrade`. Spotlight and Launchpad
        do not list a linked app: open Lunaepix from the Applications folder.
      EOS
    end

    test do
      # A 1×1 PNG written here, then compressed into out/.
      ...
      system bin/"lunaepix", "--out", testpath/"out", testpath/"dot.png"
      assert_path_exists testpath/"out/dot.png"

      app = prefix/"Lunaepix.app"
      assert_predicate app/"Contents/MacOS/lunaepix-worker", :executable?
      system "codesign", "--verify", "--deep", "--strict", app
    end
  end
  ```

  The implementer writes the PNG (a valid 1×1 PNG written with `File.binwrite`; the core's PNG fixtures show a minimal one). If ruby is installed locally, `ruby -c Formula/lunaepix.rb` must say "Syntax OK"; otherwise say so (Task 2's `brew style` is the full check).

- [ ] **Step 4: Bump to 0.1.5.** Workspace `Cargo.toml`, `cargo check` to update `Cargo.lock`, then `cargo about generate` as the README's licence section says, and confirm with `git diff THIRD-PARTY-LICENSES.html` that only the workspace crates' versions changed.

- [ ] **Step 5: Run the checks.** `cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`; the same with `--target x86_64-pc-windows-gnu`; `cargo fmt --all --check`; `node --test "tests/ui/*.test.js"`; `git diff 97a1e15 -- .github/workflows/windows.yml` empty. All pass.

- [ ] **Step 6: Commit** "Brew formula: the app and the command line from source; version 0.1.5".

### Task 2: The Formula job in the Mac workflow

**Files:**
- Modify: `.github/workflows/macos.yml` (a third job, and the header comment's rule list)
- Modify: `src-tauri/tests/workflow.rs` (the job's rules, `Platform`, module doc, the Mac fixture)

**Interfaces:**
- Consumes: `Formula/lunaepix.rb` from Task 1.
- Produces: a `formula` job other work can rely on as "the formula builds and tests on a Mac".

- [ ] **Step 1: Write the tables, then the tests (RED).** A new `Platform` field `formula_job: bool` (`true` for `MACOS`, `false` for `WINDOWS`; doc says what it requires), and a check `formula_job_problems(text, platform)` added to `every_check`. Extend `good_mac()` with a minimal valid `formula` job; Windows fixtures are unchanged.

  `formula_job_problems` requires, when `formula_job` is true:
  - a job `formula` with `needs: test` (as a scalar or a list);
  - its steps `uses` no action but `actions/checkout` (an allowlist; this also refuses a cache step — changed in Task 2's fix round from a list of refused actions);
  - no step in it that installs a toolchain or another package: no `rustup`, and no `brew install` other than `brew install --build-from-source` of the tap's formula (Review Focus 3);
  - its run scripts (without shell comments) contain, in this order: `brew audit`, `brew style` (the committed formula is checked first), `git archive` of `HEAD`, `brew install --build-from-source`, `brew test` (Ruling 1 of the run; see "Changes during execution");
  - `timeout-minutes: 60`.
  When `formula_job` is false, any job named `formula` is a problem ("windows.yml has no formula job").

  Tests, each fixture with exactly one problem: the good Mac fixture and the real file pass; no `formula` job; `needs` missing or naming another job; a cache step; a `rust-toolchain` step; a `brew install node` line; each required command missing in turn; `brew test` before `brew install`; a timeout other than 60; the Windows profile with a `formula` job; the Windows profile without one passes. The existing rules already apply to the new job (pinned actions, runner, injection, release words, NASM absent); add one fixture per rule that shows it reaches the `formula` job.

- [ ] **Step 2: Run `cargo test -p lunaepix-app --test workflow`.** The new tests fail; record the output.

- [ ] **Step 3: Write the job in `macos.yml`.** After the `build` job, `name: Formula`. Steps:
  1. `actions/checkout` with `persist-credentials: false` (the same pinned line as the other jobs).
  2. **Check the committed formula as it is:** make a local tap (`brew tap-new --no-git` with a name of the builder's choice), copy `Formula/lunaepix.rb` into it unchanged, `brew trust --formula <tap>/lunaepix` (Homebrew 6 refuses to load a formula from an untrusted tap; added in Task 4), then `brew audit --formula <tap>/lunaepix` (offline) and `brew style <tap>/lunaepix`.
  3. **Build this commit's source with the formula's steps:** `git archive --format=tar.gz` of `HEAD` into `$RUNNER_TEMP` under a name that keeps the version Homebrew reads from the URL (`v0.1.5.tar.gz`, read from the formula's `url`, not typed again); its `shasum -a 256`; a copy of the formula in a second local tap whose only differences are the `url` (`file://` that archive) and the `sha256`; `brew trust --formula` of that copy; `brew install --build-from-source` of that copy.
  4. **Test the installed copy:** `brew test` of that copy.
  Environment variables the builder needs for Homebrew (`HOMEBREW_NO_AUTO_UPDATE: 1` and similar) go in the job's `env:`. No `${{ }}` inside a script: read values from files or environment variables. Comments in the voice of the rest of the file: why the job builds from `git archive` (GitHub's archive has only tracked files), why it uses no cache (a user's Mac has none), why the committed formula is audited unchanged. Update the header's "Rules this file keeps".

- [ ] **Step 4: Run the checks** of Task 1 Step 5. All pass; `windows.yml` unchanged.

- [ ] **Step 5: Commit** "Mac workflow: a Formula job builds this commit with the brew formula and tests the installed app".

### Task 3: README and the release checklist

**Files:**
- Modify: `README.md` (the Mac section, the maintainer part)
- Create: `docs/releasing.md`

No tests (documentation only; the version and formula facts it repeats are guarded by Task 1).

- [ ] **Step 1: README, "Getting the Mac app".** Add "With Homebrew" before the download:
  - the two tap commands (Global Constraints, exact);
  - first install: Homebrew installs Rust for the build, then the build takes a while (write "about N minutes on an Apple Silicon Mac" with N as `TODO-MINUTES`; Task 4 replaces it with the Formula job's measured time);
  - the link command from the caveat, that the link survives `brew upgrade`, and that Spotlight and Launchpad do not list a linked app (and that a download user moves the download's app to the Bin first);
  - `brew upgrade` after a release; no "Open Anyway" for a brew build;
  - it reaches users from the 0.1.5 release on.
  Keep the download part as it is, under its own heading.
- [ ] **Step 2: `docs/releasing.md`.** A checklist for the user's release of a version `X.Y.Z` from `development`: merge `development` into `main` (`--no-ff`), tag `vX.Y.Z` (annotated), push `main` and the tag; download `https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/vX.Y.Z.tar.gz`, `shasum -a 256`; put the value into `Formula/lunaepix.rb`'s `sha256`; commit "Formula: X.Y.Z" on `main`, push; merge `main` back into `development`, push. Say that `brew install` fails with a checksum mismatch until the formula commit is pushed, and that on `development` the `sha256` goes back to the placeholder with the next version bump (the `url` guard then names the next version). Link it from the README's maintainer part.
- [ ] **Step 3:** `cargo test --workspace` still passes (nothing it reads changed). Commit "Document the brew install and the release checklist".

### Task 4: The Mac run (controller, not an implementer)

**rc tags as needed** (spec call 11): the controller fixes, pushes and tags `v0.1.5-rc1`, `rc2`… until the Formula job is green. A real behaviour difference, or any spec "Stop and ask" item, stops and asks.

- [ ] **Step 1: Everything off the Mac first.** The checks of Task 1 Step 5 on the merged branch, and the `debug(` grep.
- [ ] **Step 2:** `git push -u origin feature/lunaepix-brew`, `git tag -a v0.1.5-rc1 -m "Lunaepix 0.1.5 rc1: brew formula"`, `git push origin v0.1.5-rc1`.
- [ ] **Step 3:** Follow the "macOS build" and "Windows build" runs (`gh run list`, `gh run watch`). Red: `gh run view <id> --log-failed`, sort into fixture/workflow fault (fix through an implementer, new rc tag) or behaviour difference / stop-and-ask item (ask).
- [ ] **Step 4: Green.** Read the Formula job's duration of the `brew install --build-from-source` step, replace `TODO-MINUTES` in the README (round up to 5 minutes), commit "README: the measured brew build time". If the build took over 45 minutes: stop and ask (spec).
- [ ] **Step 5: Report and stop.** Each run's result and duration; the Formula job's build time; any failures and their fixes; the hand check from the spec (after the user's 0.1.5 release). Merging into `development` happens when the user asks.

After the user's hand check passes (after the 0.1.5 release), `/finalize-plan` closes the plan and the roadmap row "Apple support: a brew formula" becomes Done with the spec and plan paths.

### Changes during execution

The lines above are corrected to what was built. What changed and why (the run's ledger, `.superpowers/sdd/2026-11-16-lunaepix-brew/progress.md`, has the full rulings):

- **Formula job step order (Ruling 1, Task 2):** Step 1 of Task 2 and its Step 3 disagreed; the job checks the committed formula first (`brew audit`, `brew style`), then archives and builds this commit. The guard holds that order.
- **Actions allowlist (Task 2 fix round):** the job may use only `actions/checkout`, instead of a list of refused actions.
- **Tap trust (Ruling 7, rc1):** Homebrew 6 refused the local taps' formula ("Refusing to load formula … from untrusted tap"). The job runs `brew trust --formula` for each local tap. Users' `brew install aureus-lunae/lunaepix/lunaepix` needs no extra step, because installing by the full name trusts that formula.
- **`depends_on :macos` (Ruling 8, rc2):** `brew style` refused `depends_on macos: :big_sur` as redundant: Big Sur is the oldest macOS Homebrew supports. The guard requires the bare line while the app's floor is not newer than Homebrew's oldest, and a named floor above it. `docs/releasing.md` says what to change when Homebrew drops Big Sur.
- **Direct `cargo-tauri` call (Ruling 9, rc2):** `brew style` refused `system "cargo", "tauri", "build", …` (its rule: a `system` call with `"cargo"` and `"build"` must be `cargo install`). The formula calls the pinned install's binary, `tauri/"bin/cargo-tauri"`, with the same flags; the `PATH` line is gone. The user's choice of Tauri's own bundler (Handoff) is unchanged.
- **`cargo_build_problems` (rc2 fix round):** an offline text lint for the same `brew style` rule. When a second review found a spelling it did not read, the user chose "Document and stop (Recommended)": it reads only plain `system` calls, `brew style` is the authority, and the forms it does not read are pinned in a fixture.
- **README build time (Step 4):** the rc3 Formula job built the formula in 16 min 36 s, so the README says "about 20 minutes". It no longer says only the first install installs Rust, because Homebrew's clean-up may remove Rust again.

## Implementer report

Every implementer and every fix round writes, **before any code**: the test table and the failure-branch table. Then the RED run (command and output, before the production code existed), the GREEN run, the comment checklist (each comment of every changed function, guard, profile field and workflow step, with what changed or "still true"), the files changed, and the block:

```
## Reuse survey
Found:      <what already exists, with file:line>
Reused:     <what was used as-is or extended>
Wrote new:  <what was written fresh, and why nothing existing fit>
```

Lunaepix has no Grunt build and no devsync deployment, so nothing is uploaded to a dev server.

## CMS changes

No CMS changes.
