# Lunaepix desktop app — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-04

## Handoff

Decided in conversation (2026-11-04):

- **Spec and layout approved by the user (2026-11-04).** Project name: **Lunaepix** (chosen after Optimg, Leanpix and Featherpix were rejected as already in use). Repo: `git@github.com:Aureus-Lunae/Lunaepix.git`, private for now, local checkout `~/projects/Lunaepix`. Git identity is set per repo to Aureus-Lunae (global config is Bitbucket). The remote's first commit is the MIT `LICENSE` created on GitHub (copyright Erwin Korsten); keep it. Before any public release, list every dependency's licence (`cargo-deny` or `cargo-about`) and confirm none conflicts with MIT; add that as a plan step.
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
- The Layout sketch below is approved; it is the binding layout (no mock-up file).

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
- The shell functions `optimg` and `optimgdir` become wrappers that call the app's CLI binary, so the terminal and the window give identical results.

### SVG engine (decided: Rust crate)

Chosen by the user: a Rust SVG optimiser crate, linked into the app. No maintained crate is known to match `svgo`, so the implementer's first task is to evaluate the available crates on the user's real SVGs (e.g. `Mobilis Logo - Black.svg`: 21.316 KiB -> 9.405 KiB with svgo today) and report the numbers.

- Acceptance: within a few percent of svgo's saving on a sample set, and the output renders identically (compare rasterised output before and after).
- Lossless here means the rendered image is identical, not that the XML is byte-identical.
- If no crate meets this, stop and report with the measurements. The fallbacks (Node `svgo` as a sidecar, or `svgo` as WASM) are not chosen; that is the user's call.

### Platforms

- Linux: built and tested locally (AppImage/RPM).
- Windows: built by a GitHub Actions workflow on `windows-latest` using `tauri-action`, producing an unsigned installer (MSI/NSIS) as a downloadable artifact. Run on tag push or manually.
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
