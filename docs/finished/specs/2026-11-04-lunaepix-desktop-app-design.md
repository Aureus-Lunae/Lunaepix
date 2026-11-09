# Lunaepix desktop app — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-04

Plan filename: 2026-11-04-lunaepix-desktop-app.md
Plan Status: Completed
Date plan executed: 2026-11-05

Audits performed:
- None

## Handoff

Decided in conversation (2026-11-04):

- **Spec and layout approved by the user (2026-11-04).** Project name: **Lunaepix** (chosen after Optimg, Leanpix and Featherpix were rejected as already in use). Repo: `git@github.com:Aureus-Lunae/Lunaepix.git`, private for now, local checkout `~/sideprojects/Lunaepix`. Git identity is set per repo to Aureus-Lunae (global config is Bitbucket). The remote's first commit is the MIT `LICENSE` created on GitHub (copyright Erwin Korsten); keep it. Before any public release, list every dependency's licence (`cargo-deny` or `cargo-about`) and confirm none conflicts with MIT; add that as a plan step.
- Today `optimg` and `optimgdir` are bash functions in `~/.bashrc.d/90-miscCommands.sh`. They call `oxipng` (PNG), `jpegoptim` (JPG) and `svgo` (SVG), lossless, and never touch the original. Output is `optimized-<name>` next to the file, or `<dir>/optimized/<name>` for a folder. Each file prints svgo-style: name, `Done in N ms!`, `before KiB - saved% = after KiB`.
- The user wants a **desktop app** that works on different systems. It is a personal tool now; a public release for several people is possible later.
- **Stack: Tauri** (chosen over Electron, which I had recommended). **Repo: `~/projects/optimg`** (new, standalone). **Test on Linux only** for now; keep the code portable.
- Lossless only. WebP is not handled here (the sites make WebP on the server).
- Rust is **not installed** on this machine (`cargo` missing, checked 2026-11-04). Node 26 is.
- **No macOS release** (user, 2026-11-04): building for Mac needs a macOS runner (GitHub) and signing needs an Apple Developer account; the user chose to skip it. Do not add macOS to CI or packaging. Keep the Rust core portable (`std::path` only) so a Mac build stays possible later.
- **Windows build wanted** (user, 2026-11-04), built on GitHub Actions (`windows-latest`, `tauri-action`) from the user's own GitHub account. Windows is not tested locally; the user tries the CI artifact on a Windows machine. Unsigned installer in v1: Windows SmartScreen will warn on first run ("More info" > "Run anyway"); a code-signing certificate is out of scope. Repo remote is the user's own GitHub; per-repo git identity override needed (global config is Bitbucket, see memory `git-identities-bitbucket-github`).
- Not yet discussed, deliberately left out of scope: auto-update, any lossy mode.
- Input is **drag and drop and also a folder picker** (user, 2026-11-04): a "Choose folder" button opens a native folder dialog, in addition to dropping files or a folder.
- **SVG engine: option 2, a Rust SVG optimiser crate** (user, 2026-11-04). No Node sidecar, no WASM. If no crate comes within a few percent of svgo, the implementer stops and reports the numbers; it does not switch to option 1 or 3 on its own.
- **Implementation plan approved (2026-11-04):** `docs/in-progress/plans/2026-11-04-lunaepix-desktop-app.md`. Execute it in a fresh session, subagent-driven; Step 2 (compressor evaluation) is a hard gate before any other code.
- The Layout sketch below is approved; it is the binding layout (no mock-up file).
- **Step 2 evaluation (2026-11-04):** PNG `oxipng` crate 10.2.1 is byte-identical to the CLI; JPG `mozjpeg-sys` 2.2.3 (jpegtran-style coefficient transcode) is byte-identical to jpegoptim on 13/13 files; SVG `oxvg_optimiser`/`oxvg_ast` 0.0.9 is +0.57 % vs svgo in total. Pin oxvg with `=` and commit `Cargo.lock`.
- **SVG "renders identically" means edge noise is accepted** (user, 2026-11-04): neither svgo nor oxvg is pixel-identical, because both round coordinates. Tests accept differences on anti-aliased edges only: no differing pixel inside a flat area, and at most about 0.5 % of pixels different.
- **JPG mode: always smallest-lossless** (user, 2026-11-04, chosen over "match jpegoptim" and over a switch): build both the jpegoptim-equivalent result (same coding mode as the input, optimised Huffman tables) and mozjpeg's tuned progressive result (coefficient transcode, no re-quantisation), keep the smaller of the two, and keep the original bytes if neither is smaller. Output may therefore become progressive. Checked against the Jugoo CMS upload (`m_images.php` plus CI3 `Upload`): progressive JPGs pass every check, and the WebP the CMS makes is the same size either way. The CMS refuses images over 2600 px or 5 MB (defaults), which Lunaepix neither causes nor fixes.
- **Licences: MPL-2.0 is allowed** (user, 2026-11-04): oxvg pulls in MPL-2.0 crates. If the app is ever distributed it will be open source. GPL, LGPL and AGPL remain a stop. Step 13 generates a third-party licences file (it is needed anyway for the IJG credit of mozjpeg).
- **Decompression bombs** (2026-11-04): images over `MAX_PIXELS` = 100 megapixels are refused from the header, before decoding (controller ruling after a security review). The same class of defect recurred in the PNG check, and the user chose to fix the cause, not only the instance: use each library's own limit first (oxipng `max_decompressed_size`, libjpeg's own header parse plus `max_memory_to_use`), keep our checks as an extra layer, and collect crafted inputs for every format in one shared test file, `crates/core/tests/hostile_input.rs`. Step 6 extends it with SVG entity expansion and deep nesting.
- **One process per file** (user, 2026-11-04, after the same defect class appeared a third time: a 200 KB SVG with deeply nested CSS brackets crashed the process with a stack overflow inside oxvg's CSS parser). The batch runner (Step 7) optimises each file in a child process of the app's own binary, in a hidden worker mode. A crash, abort, out-of-memory or timeout marks only that file as failed ("the optimiser crashed on this file"); the window and the other files carry on. The existing format checks stay, for their clear messages. Chosen over "bound CSS nesting only" and "bound CSS nesting plus a 64 MiB stack thread".
- **SVG parsing** (Step 6): roxmltree with `allow_dtd: true` (Illustrator files carry a DOCTYPE), and no entity resolver, so external entities are never read. Limits: element depth 128, 1,000,000 nodes, entity expansion capped at 16 MiB by an over-estimating check whose soundness relies on roxmltree 0.21.2. roxmltree is therefore pinned exactly, with tests that fail if its entity limits change. Scripts are kept, as svgo does.
- **Step 11 dropped (user, 2026-11-04):** "This is an app, not a terminal option." The shell functions `optimg`/`optimgdir` in `~/.bashrc.d/` are not rewritten and the user's shell config is not touched. The `lunaepix` CLI from Step 8 stays in the repo. Found while preparing the step: `~/.bashrc` sources every file in `~/.bashrc.d/`, including `90-miscCommands.sh.bak`, so the old imagemin `optimg` overrides the current one; that is the user's to handle.
- **Gate passed (2026-11-04):** the user ruled on all Step 2 numbers; implementation continues with Step 1.
- **Parse structured files, never scan them as text** (user, 2026-11-05). The defect class "a check that reads a structured format as text, so another spelling passes" recurred in Step 12 (workflow YAML), Step 13 (`about.toml`) and the final review (the window's HTML). Each fix uses a real parser: yaml-rust2 for the workflow, the `toml` crate for `about.toml`, and html5ever's tokenizer for `ui/index.html` and the licences HTML (all test-only dependencies). The JS checks stay text and are described as a lint, not a guarantee.
- **No AppImage** (user, 2026-11-05). An AppImage bundles about 190 LGPL/GPL system libraries (WebKitGTK, GTK, gnutls, nettle, gmp) without notices or a source offer, which the licence policy above forbids. Linux ships as deb and rpm, which use the system's libraries; Windows as NSIS and MSI. The bundle targets are listed explicitly, never `all`.
- **Final review fixes** (user, 2026-11-05), all in this branch before the merge to main. Windows: no crash dialog from a crashing worker, a static CRT, a rerun that works on a read-only source, and an existing `Optimized` folder in another letter case. Over-limit files collapse into one summary row. A run that finds no image says "No PNG, JPG or SVG files directly in this folder (subfolders are not included)". Closing the window stops the running worker. The worker becomes a small separate binary shipped as a Tauri sidecar, instead of a second copy of the full app.

## Problem

The shell function only works where bash, GNU `stat -c`, GNU `date +%s%N`, `awk` and three separately installed tools exist. It fails on Windows and, because of the GNU-only flags, on macOS. It also cannot be handed to someone who does not live in a terminal.

## Design

### What stays the same

- Formats: PNG, JPG/JPEG, SVG (case-insensitive extension). Anything else is skipped with a message.
- Lossless. The original is never modified or overwritten.
- Output names: `optimized-<name>` beside a single file; `optimized/<name>` inside a dropped folder.
- Per-file report: name, time in ms, before size, saved %, after size.

### What changes

- A Tauri window replaces the terminal: drop files or a folder, **or choose a folder with a native folder dialog**, and see results in a table. A folder chosen either way is processed the same: `optimized/<name>` inside it, non-recursive like `optimgdir` today.
- The compressors are **Rust crates linked into the app**, so nothing is installed separately:
  - PNG: the `oxipng` crate, equivalent settings to today (`-o 4`, `--strip safe`).
  - SVG: no maintained Rust equivalent of `svgo` is known. Open question below.
  - JPG: a lossless JPEG optimiser crate (Huffman table optimisation and progressive encode, no re-quantisation), matching `jpegoptim`'s default behaviour.
- The core is one Rust library with a single function that takes a source path and a destination path and returns `{before, after, ms}` or an error. The window and the command line are thin callers, so there is one code path.
- ~~The shell functions `optimg` and `optimgdir` become wrappers that call the app's CLI binary, so the terminal and the window give identical results.~~ (dropped, see Handoff)

### SVG engine (decided: Rust crate)

Chosen by the user: a Rust SVG optimiser crate, linked into the app. No maintained crate is known to match `svgo`, so the implementer's first task is to evaluate the available crates on the user's real SVGs (e.g. `Mobilis Logo - Black.svg`: 21.316 KiB -> 9.405 KiB with svgo today) and report the numbers.

- Acceptance: within a few percent of svgo's saving on a sample set, and the output renders identically (compare rasterised output before and after).
- Lossless here means the rendered image is identical, not that the XML is byte-identical.
- If no crate meets this, stop and report with the measurements. The fallbacks (Node `svgo` as a sidecar, or `svgo` as WASM) are not chosen; that is the user's call.

### Platforms

- Linux: built and tested locally (deb/RPM).
- Windows: built by a GitHub Actions workflow on `windows-latest` using `cargo install tauri-cli` (not `tauri-action`), producing an unsigned installer (MSI/NSIS) as a downloadable artifact. Run on tag push or manually.
- No macOS release.
- The Rust core uses `std::path` only and no GNU tools. Paths dropped or chosen in the window are handled as `PathBuf`, so Windows drive letters, backslashes and spaces work. Test cases include a name with spaces and non-ASCII characters.

## Layout

No mock-up file exists; this sketch was approved by the user on 2026-11-04 and is binding.

```
+--------------------------------------------------+
|  optimg                                          |
|  +--------------------------------------------+  |
|  |                                            |  |
|  |   Drop images or a folder here             |  |
|  |   PNG · JPG · SVG                          |  |
|  |   [Choose files]   [Choose folder]         |  |
|  |                                            |  |
|  +--------------------------------------------+  |
|                                                  |
|  File             Before    After    Saved  Time |
|  logo-black.svg   21.3 KiB  9.4 KiB  55.9%  22ms |
|  hero.png         1.2 MiB   0.9 MiB  24.8%  310ms|
|  notes.txt        skipped: not png/jpg/svg       |
|                                                  |
|  Total: 2 files, saved 0.3 MiB     [Open folder] |
+--------------------------------------------------+
```

- One card for the drop zone, one table for results, one button row below the table.
- The results table is a real table (columns), not a list.
- Empty state: the table is hidden and the drop zone is the only block; text "Drop images or a folder here".
- A skipped or failed file stays in the table with its reason in place of the numbers.
- The two buttons in the drop zone open native dialogs (files, or folder), so the app is fully usable without drag and drop. Requires keyboard operation and visible focus, per `~/.claude/a11y-rules.md`.

## Tests

Core library (Rust, `cargo test`), each with a positive and a negative:

| Behaviour | Positive | Negative |
|---|---|---|
| PNG optimised | output smaller and pixel-identical to the input | corrupt PNG returns an error and writes no output file |
| JPG optimised | output not larger and decoded pixels identical | non-JPEG bytes named `.jpg` return an error, no output |
| SVG optimised | output smaller and still parses as SVG | invalid XML returns an error, no output |
| Original untouched | source file bytes identical afterwards | n/a (covered by the three above also leaving source intact on failure) |
| Output naming | single file gives `optimized-<name>`; folder gives `optimized/<name>` | a `.txt` file is skipped and produces nothing |
| Folder chosen via dialog | PNG/JPG/SVG files in it land in `<dir>/optimized/` | subfolders are not entered; a cancelled dialog processes nothing |
| Awkward file names | name with spaces and non-ASCII characters is optimised to the right `optimized-<name>` | n/a |
| Unsupported extension | `.PNG` upper-case is accepted | `.gif`, `.webp` are skipped |
| Destination exists | an existing `optimized-<name>` is replaced with fresh output | the source is never chosen as destination |

The GUI is verified by hand on Linux; no automated UI tests in v1.

## Needs creating in the CMS

None. This project has no CMS dependency.

## Rules check

Checked:  `~/.claude/CLAUDE.md`, `rules/spec-requirements.md` (Layout section, no CMS dependency, no money or shared rows), `rules/unit-testing.md` (positive and negative per behaviour, no mutation runs), `rules/security.md` (no web endpoint, no SQL; file paths from drop events are used as given only for reading and for writing the computed destination), `rules/devsync.md` (not a Jugoo site, no dev-server upload), `rules/vanilla-js.md` (the window uses vanilla JS, not a framework; no `innerHTML` with file names, use `textContent`), `rules/smarty.md` (not applicable), `rules/cms-defines-fields.md` (not applicable), `rules/processes.md` (nothing here starts background processes), `rules/local-links.md` (link in the reply), `css-conventions.md` and `a11y-rules.md` (listed under Layout; the implementer reads both before writing markup or styles).
Changed:  added the `textContent`-only rule for file names in the table and the keyboard-operable drop zone; noted that Rust must be installed first.

## Prerequisites before implementation

- Install Rust (`rustup`) and Tauri's Linux system packages (webkit2gtk and friends via `dnf`). Ask the user to run these; do not install them silently.
