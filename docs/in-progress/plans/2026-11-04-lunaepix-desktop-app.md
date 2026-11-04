# Lunaepix desktop app — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-04

Spec (read its Handoff first): [2026-11-04-lunaepix-desktop-app-design.md](../../in-progress/specs/2026-11-04-lunaepix-desktop-app-design.md)

## Handoff

Everything decided in conversation is in the spec's Handoff. Do not re-open it. In short: Tauri, Rust core, Linux tested locally, Windows built in GitHub Actions, no macOS, SVG via a Rust crate (stop and report if none is close enough to svgo), approved layout is binding, MIT licence already in the repo.

**Plan approved by the user on 2026-11-04**, including these additions made while writing it:

- Two binaries share one core library: the window app and a CLI. The user's shell functions `optimg` and `optimgdir` are rewritten to call the CLI, so terminal and window give identical results.
- Limits (Tier 2, bounds): 100 MiB per file and 2000 files per run. Over the limit means skipped with a reason, not an error. These are constants in one place and easy to change.
- Writes go to a temporary file in the destination folder and are renamed into place, so a failure never leaves a half-written `optimized-<name>`.
- The compressor evaluation (Step 2) runs **before** any other code and is a hard gate.
- The plan names no crate versions. The implementer picks current versions and records them.

**Execution (2026-11-04):** Step 2 ran before Step 1, because both handoffs say the gate comes before any other code. The gate rulings (SVG edge-noise tolerance, JPG always smallest-lossless, MPL-2.0 allowed) are in the spec's Handoff.

Not in scope: macOS, auto-update, code signing, lossy modes, recursive folders, WebP.

## Execution

Fresh session, subagent-driven. Invoke `sdd-overrides` first; it decides which agent takes which task. The specialist `dev-*` agents are written for Jugoo PHP/Smarty work, so expect the controller to use a general-purpose agent for the Rust tasks.

Tasks 3 to 8 are test-first. Each implementer's report starts with the RED run, then the positive and negative test per behaviour, then the Reuse survey block. The exact number of tests is not fixed here; the implementer establishes it.

## Reuse survey (done while planning)

Found:      the shell functions `_optimg_one`, `optimg`, `optimgdir` in `~/.bashrc.d/90-miscCommands.sh:29-65`: they define the formats, naming, flags (`oxipng -o 4 --strip safe`, `jpegoptim`, `svgo`) and the report layout.
Reused:     their behaviour as the reference; nothing is copied as code.
Wrote new:  everything else. The repo is empty apart from `LICENSE` and `docs/`.

## Needs creating in the CMS

None. No CMS dependency.

## Layout

Binding layout is the sketch in the spec's `## Layout` section (approved 2026-11-04). The implementer compares the finished window against it. Class names follow BBEM (`~/.claude/css-conventions.md`): camelCase components, `__element`, modifiers as sibling `-modifier` classes. Planned names: `lunaepixDrop`, `lunaepixDrop__button`, `lunaepixResults`, `lunaepixResults__row` with `-skipped` and `-failed`, `lunaepixSummary`. The same names appear in HTML, CSS and JS.

## File structure

```
Cargo.toml                      workspace
crates/core/                    lunaepix-core   (library, all logic, all tests)
crates/cli/                     lunaepix-cli    (binary `lunaepix`)
src-tauri/                      Tauri app (thin: commands, window config, capabilities)
ui/index.html, ui/app.css, ui/app.js   vanilla, no bundler, no framework
samples/                        gitignored; user-supplied evaluation files
.github/workflows/windows.yml   Windows build
```

## Steps

### Step 0 — Prerequisites (user runs these; do not install silently)

Rust (`rustup`), the Tauri CLI, and Tauri's Linux system packages through `dnf` (webkit2gtk 4.1 and GTK development packages, librsvg, openssl, xdo). The implementer looks up the current Fedora package list on tauri.app's prerequisites page at that time, gives the user the exact commands, and waits.

### Step 1 — Workspace scaffold

Create the workspace, the three crates and `ui/`. `lunaepix-core` exposes:

```rust
pub struct Outcome { pub before: u64, pub after: u64, pub millis: u64 }
pub enum Skip { UnsupportedType, TooLarge, ... }          // reason shown to the user
pub fn optimize_file(src: &Path, dest: &Path) -> Result<Outcome, OptimizeError>;
pub fn plan_files(inputs: &[PathBuf]) -> Vec<Job>;        // Job { src, dest } or a Skip
pub fn plan_folder(dir: &Path) -> Vec<Job>;
```

Exact error and skip variants are the implementer's to design. Add `samples/` to `.gitignore`. `cargo test` runs from the root.

### Step 2 — Compressor evaluation (HARD GATE, no production code yet)

Ask the user for a folder of real PNG, JPG and SVG files, and copy a sample set into `samples/` (the set must include `Mobilis Logo - Black.svg`). For each format, measure candidates against today's tools run on the same files:

- PNG: the `oxipng` crate against the `oxipng` command, same settings.
- JPG: find a Rust-reachable lossless route (Huffman optimisation and progressive, no re-quantisation) and compare with `jpegoptim`. Whether a pure-Rust crate can do this is **unknown**; a binding to mozjpeg's lossless transcode is one candidate. The implementer reports what exists.
- SVG: evaluate the available Rust SVG optimiser crates against `svgo`. A Rust port of svgo may exist; verify rather than assume.

Report a table (file, original, today's tool, candidate) and verify losslessness: PNG and JPG by identical decoded pixels, SVG by identical rasterised output (a rasteriser crate such as `resvg` can render both sides).

**Gate.** PNG and JPG must match today's size within a few percent. SVG must be within a few percent of svgo (spec). If any format fails, stop and send the table to the user. Do not fall back to a sidecar or WASM on your own. Do not start Step 3 until the user rules on the numbers.

### Step 3 — Paths, naming and the job plan (test-first)

Pure functions, no compression yet. Behaviour:

- Extension check is case-insensitive: `.png .jpg .jpeg .svg`. Anything else is a skip, never an error.
- A single file's destination is `optimized-<name>` in the same folder; a folder's is `<dir>/optimized/<name>`.
- Folder planning is non-recursive, and ignores the `optimized/` folder itself, subfolders, and symlinks.
- The destination is never equal to the source, after canonicalising both.
- Size limit 100 MiB per file and 2000 files per run, enforced here.
- Paths stay `PathBuf` throughout, never strings.

| Positive | Negative |
|---|---|
| `a.PNG` is planned as `optimized-a.PNG` | `a.gif` and `a.webp` are skipped, nothing planned |
| a folder with 3 images plans 3 jobs into `optimized/` | a subfolder's images are not planned |
| a name with spaces and non-ASCII plans correctly | a file already inside `optimized/` is not planned again |
| a file of exactly 100 MiB is planned | one byte over is skipped as too large |
| 2000 files are planned | the 2001st is skipped as over the run limit |
| the destination differs from the source | a source that is its own destination (symlink or same path) is refused |

### Step 4 — PNG

`optimize_file` for PNG with the settings chosen in Step 2. Writes to a temp file in the destination folder, then renames.

| Positive | Negative |
|---|---|
| output is smaller and decodes to identical pixels | a corrupt PNG returns an error and leaves no output file |
| the source file's bytes are unchanged afterwards | a failed run also leaves the source unchanged and no temp file behind |
| an existing `optimized-<name>` is replaced | a `.png` that holds JPEG bytes is rejected |

### Step 5 — JPG

Same contract, using the route chosen in Step 2. Negative tests mirror Step 4: corrupt JPEG, PNG bytes named `.jpg`, no output on failure, source untouched. Positive: decoded pixels identical and output not larger than the input. An input that is already optimal produces a copy of equal size, not an error.

### Step 6 — SVG

Same contract, using the crate chosen in Step 2. Positive: smaller, parses as SVG, rasterises identically. Negative: invalid XML returns an error with no output; an SVG with a script element or an external reference is **not executed or fetched** during optimisation (test with a fixture that would fail if it were).

### Step 7 — Batch runner

`run(jobs, on_progress) -> Vec<Result<Outcome, ..>>`. Runs files one at a time off the UI thread (a bounded worker pool is optional), reports progress per file, and one file failing never stops the rest.

| Positive | Negative |
|---|---|
| 3 files give 3 outcomes in input order | one corrupt file in the middle gives an error for it, and the other two still succeed |
| progress is reported once per file, with the arguments given | an empty job list reports nothing and creates no `optimized/` folder |
| a cancelled dialog (empty input) does nothing | a skipped file produces no output file |

### Step 8 — CLI

`lunaepix file...` and `lunaepix --dir <dir>` print exactly the layout the shell function prints today: `<name>:`, `Done in N ms!`, `<before> KiB - <saved>% = <after> KiB`, a blank line, with green only on a terminal. Skips and failures go to stderr, and the exit code is non-zero if any file failed. Tests run the formatter as a pure function: a known outcome gives the expected text, and a zero-byte input does not divide by zero.

### Step 9 — Tauri app shell

`src-tauri` registers commands: `plan_and_run(paths)`, and a folder-pick path using the dialog plugin. Progress and results reach the window as events. Capabilities are minimal: the dialog plugin and the window drag-drop event, with no filesystem or shell permission for the frontend. A strict content security policy: no remote content, no inline script. File names are data and go to the window as JSON, never as HTML.

### Step 10 — Window UI (vanilla JS)

Implements the approved layout. Read `~/.claude/css-conventions.md` and `~/.claude/a11y-rules.md` first. Required:

- `<html lang="en">`, a unique `<title>`, one `h1`, landmarks (`header`, `main`).
- The drop zone is a region with two real `<button>`s, **Choose files** and **Choose folder**, so the app works without drag and drop. A visible focus style, targets at least 24x24 px, body text at least 16px, secondary text at least 14px.
- Results are a real `<table>` with a visually hidden `<caption>`, `<th scope="col">` headers, and the file name as `<th scope="row">`. A skipped or failed row carries the reason as text; colour is never the only signal.
- A polite live region announces "Optimised N files, saved X". The summary row repeats it as text.
- Any drag-over highlight transition has a `prefers-reduced-motion: reduce` branch. No `title` attributes, no `autofocus`, no `innerHTML` (use `textContent` and `replaceChildren`).
- Contrast is checked in APCA (Lc), foreground on background, and reported. Works in forced-colours mode.
- Layout does not scroll horizontally at 320 px.

Manual checks (no UI test automation in v1): drop files; drop a folder; both buttons; cancel each dialog; keyboard only from start to finish; a window at 200% text.

### Step 11 — Terminal wrappers

Install the CLI (`cargo install --path crates/cli`) and rewrite `optimg` and `optimgdir` in `~/.bashrc.d/90-miscCommands.sh` to call it, keeping the names and the output. Keep the old bodies in the existing `.bak`. This edits the user's own shell config, which is why it is a separate step. Verify by running the same files through the old and new commands and comparing the lines.

### Step 12 — Windows build workflow

`.github/workflows/windows.yml`: `windows-latest`, builds the Tauri app and uploads the installer as an artifact, on manual dispatch and on a version tag. Unsigned, which the user knows triggers SmartScreen. The workflow cannot be run locally; the implementer checks it with a syntax lint and the user runs it. Paths in the code already use `PathBuf`, and Step 3 carries the spaces and non-ASCII tests.

### Step 13 — Licence check and README

Run `cargo-deny` or `cargo-about` and list every dependency licence. A GPL or other copyleft dependency is a **stop**: report to the user, since it would override the MIT choice. Write a short README (what it does, Linux build, the Windows artifact, the licence).

### Step 14 — Linux package and acceptance

Build the Linux bundle with `cargo tauri build`. Run the full `cargo test`. Report the manual checks from Step 10 as done or not done. The user then tests live. Pushing is a separate "merge and push" instruction from the user.

## Rules check

Checked:  `~/.claude/CLAUDE.md` (handoff first, plan describes and does not carry full code, no building to confirm, link last), `rules/unit-testing.md` (positive and negative table per behaviour, RED run first, no mutation runs), `rules/security.md` (paths from the window are only read and written to computed destinations, destination never equals source, bounds on file size and count, no SVG script execution or fetch, strict CSP and minimal Tauri capabilities, atomic temp-and-rename writes), `rules/vanilla-js.md` (no framework, `textContent` only, no URL built from input), `rules/spec-requirements.md` (Layout section links the approved spec layout, CMS block present, no money or shared rows), `rules/devsync.md` (not a Jugoo site, nothing uploaded), `rules/smarty.md` and `rules/cms-defines-fields.md` (not applicable), `rules/processes.md` (no background process is started by the plan; the implementer saves PIDs if it runs one), `css-conventions.md` (BBEM names listed under Layout), `a11y-rules.md` (checked section by section in Step 10: contrast, font size, no `title`, motion, keyboard and focus, controls, tables, structure, reflow).
Changed:  added the 100 MiB and 2000-file bounds, the atomic write, the SVG no-execute test, and the evaluation gate before any code; added the table `scope` and caption and the live region to Step 10.
