# Lunaepix "Re-add" and thumbnails — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-11

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A "Re-add" button after a run puts the same files back in the list; the app never replaces an output and numbers a taken name from `-1` everywhere; every row shows a 32 px thumbnail before the file name.

**Architecture:**
- **Naming.** The two numbering loops in the core start at 1. The app's run options always use `WriteMode::NoReplace`.
- **Re-add.** `FileList` gets a way to clear its "finished" mark; a new command `readd_list` does that and sends `list-changed`. The window shows the button in the summary row.
- **Thumbnails.** A pure core module makes a thumbnail from a file (raster formats decoded with the decoders the product already ships, scaled to 64 px; SVG passed through). The worker binary answers a new flag `--lunaepix-thumb` with a one-line header and the bytes. The app keeps a key → path snapshot per `list-changed`, serves `thumb://` requests through a two-slot pool of those workers, and caches the answers for the current list. The window turns each row's key into an address with `convertFileSrc`.

**Tech Stack:** Rust 2024 (MSRV 1.90), Tauri 2 (custom URI scheme), `mozjpeg-sys =2.2.3`, `libwebp-sys =0.14.4`, `png 0.17`, vanilla JS (`node --test`).

**Spec:** [2026-11-11-lunaepix-run-again-thumbnails-design.md](../../in-progress/specs/2026-11-11-lunaepix-run-again-thumbnails-design.md). Read its Handoff before anything else.

## Handoff

Copied verbatim from the spec's Handoff, plus the standing process rules and the calls made while planning. Give every implementer these lines as they are. An implementer that wants to deviate from one stops and asks instead of choosing.

- **Plan approved by the user (2026-11-11):** "approved", including the calls made while planning (listed below). Execute in a fresh session, subagent-driven, on `feature/lunaepix-run-again-thumbnails`.
- **Spec approved by the user (2026-11-11):** "Otherwise, approved", after one change to the button (next line).
- **The button is "Re-add", not "Reuse files":** "We already have clear to clear the list. We can just use readd". Spelled "Re-add"; the command is `readd_list`, the permission `allow-readd-list`, the button `#readdList`.
- **Starting point:** "Tested, and latest build works on windows." The lossy branch was finalized and merged into `main` locally (f54b5a2, not pushed). This work is on `feature/lunaepix-run-again-thumbnails`, from `main` at f54b5a2.
- **Request 1, run the same batch again:** "We can't rerun the same batch right now. We need to reselect all. Possible to just readd all the ones that ran for if one both want a lossy and lossless compression, or different lossless ones."
- **Request 2, preview:** "Is a small preview possible of the image files that will get compressed?"
- **Renaming is out of scope:** "renaming is another part of the roadmap. For a batch job, that is a prefix / postfix, as it is not good to rename all files in a batch job."
- **Numbering next to the originals:** asked "Should 'next to the originals' also number a taken name (`optimized-holiday-1.jpg`) instead of replacing it?" The user answered "Yes".
- **Numbering starts at `-1` everywhere:** "Start at -1, as everything does normally on a computer on a duplicate name." This changes the existing numbering too (chosen folder, the command line's `--out`), not only next to the originals.
- **Thumbnail placement:** "Small thumbnail before the filename."
- **Order of work:** finish the lossy branch first, start this on a new branch from `main`: "Yes".
- **Calls approved with the spec (not user decisions):** the button sits before "Open folder"; the command line without `--out` keeps replacing; thumbnails are made in a worker process with the decoders the product already ships; limits 64 px / 32 px on screen / 100 megapixels / 2 MiB SVG / two workers / 10 s; only "cannot be shown" is remembered; the key is `<list>-<index>`.
- **Standing process rules (user, from the lossy plan, 2026-11-10), apply to every task here:**
  - "Controller RED audit": before each review the controller compares the RED run's test list with the commit's final tests; a test missing from the RED run goes back as a concern before review. The test table has a "fails on its own because…" column.
  - "Branch table covers choices": the failure-branch table also lists every if/else driven by a setting or option, each side with a named test.
  - "Add a cross-module row": the branch table gets one row for every core function the task newly calls, naming the branches inside it the new caller can reach.
  - "No 'glue' exemption": code that decides what is sent or shown is moved into a function a test can reach; only true pass-through calls may stay untested.
- **During execution (user, 2026-11-11, after the Task 5 review found the second untested branch rated Important):** remedy "Controller gate (Recommended)": before each review the controller reads the failure-branch table, and any "untested" row on a branch that decides what the caller gets goes back to the implementer unless it names how a test could reach it and why that does not work. Task 5's fix: "Fix thumbnail only (Recommended)"; the optimiser's matching untested arm goes on the final review's list.

Calls made while planning (not user decisions; flagged for the plan review):

1. **A key never changes its file.** The list number is raised on Clear, on the first add after a finished run, and also whenever a new snapshot would give an existing key a different path (a row dropped from the middle of the list by re-planning). So a key, once sent, always names the same file, and the webview's image cache can never show a wrong picture.
2. **CMYK and YCCK JPGs get the empty square** (worker exit 3). Converting CMYK for a 32 px preview is not worth the code; they are rare and already "kept lossless".
3. **The thumbnail scaling is a plain area average** in sRGB on RGBA, not a linear-light filter. At 64 px the difference does not show.
4. **The window's `#readdList` reuses `canChoose`** and the summary's existing button class; no new gate function.
5. **The sidecar's usage message** names both modes.

## Global Constraints

- The window never sends or receives a path. A thumbnail address names `<list>-<index>`, never a file.
- The window process never opens or decodes a source file for a thumbnail; a worker process does.
- Key shape, in the window and in the handler: `^\d{1,10}-\d{1,4}$` (window), `^/(\d{1,10})-(\d{1,4})$` (handler, on the path as received, not percent-decoded).
- Thumbnail: longest side 64 px, aspect kept, never enlarged, RGBA PNG; shown at 32 × 32 CSS px with `object-fit: contain`.
- Limits: `MAX_PIXELS` (100 megapixels, existing, checked from the header before decoding); SVG at most 2 MiB (`2 * 1024 * 1024` bytes); worker answer read at most 2 MiB plus the header line; 10 s per thumbnail; at most 2 thumbnail workers at a time.
- Worker answer: header `lunaepix-thumb 1 png\n` or `lunaepix-thumb 1 svg\n`, then the bytes, exit 0; exit 3 "cannot be shown"; exit 2 usage error.
- Numbering: first taken name gets `-1`, then `-2`, up to `MAX_NAME_NUMBER` (9999). Never `-0`.
- The app always writes with `WriteMode::NoReplace`. The CLI without `--out` keeps `WriteMode::Replace`.
- CSP `img-src 'self' thumb: http://thumb.localhost`; every other directive unchanged.
- Permissions: exactly seven commands, `allow-readd-list` added; no permission for the scheme.
- Exact texts: button "Re-add"; classes `lunaepixResults__thumb`, `lunaepixResults__thumbImage`; button `class="lunaepixSummary__button" id="readdList" type="button"`, before `#openFolder`.
- Every text into the page through `textContent`; the `<img>` gets `alt=""`, `width="32"`, `height="32"`, `loading="lazy"`, `decoding="async"`.
- **Layout:** the spec's `## Layout` section is binding (square before the name, empty square for no thumbnail, "Re-add" then "Open folder" on the right of the summary row, wrapping together at narrow widths).
- Structured files (JSON, HTML, TOML) are checked with a parser in tests, never as text. CSS is not tested.
- Every behaviour has a positive and a negative test; every failure branch is reached by a test, through a double where needed.

## Review Focus

1. **A new list starts while a thumbnail worker for the old list is still running.** Its answer must not land in the new list's cache, nor be served for the new list's key. The test is in Task 6.
2. **Extreme aspect ratios** (10000 × 1, 1 × 10000). The thumbnail must be 64 × 1 and 1 × 64, never a zero side and never a panic. The test is in Task 3.
3. **PNG colour types the decoder hands back differently:** greyscale, greyscale + alpha, 16-bit, palette with `tRNS`. Each must give a correct RGBA thumbnail (transparent stays transparent). The test is in Task 3.
4. **File names with spaces, `é` and (on Linux) non-UTF-8 bytes.** The worker must get the path as an `OsString` argument and make the thumbnail. The test is in Task 5.
5. **The window is closed while thumbnail workers run.** No worker outlives the 10 s limit; on Linux they die with their thread. The test (a worker that sleeps is killed at the timeout and reaped) is in Task 5.

---

### Task 1: Never replace in the app; number from `-1`

**Files:**
- Modify: `crates/core/src/atomic.rs` (`write_new` start number, doc comments of `MAX_NAME_NUMBER`, `write_new`, `numbered`), `crates/core/src/plan.rs:110,219`, `crates/core/src/lib.rs:176` doc, `src-tauri/src/session.rs:294-307` (`PreparedRun::options`), `crates/cli/src/main.rs` doc only if it names `-2`, `README.md:52`, `src-tauri/INTERFACE.md`.
- Test: `crates/core/src/atomic.rs` (unit), `crates/core/src/lib.rs` (unit), `crates/core/tests/output_folder.rs`, `crates/cli/tests/cli.rs`, `src-tauri/src/session.rs` (unit).

**Interfaces:**
- Produces: `PreparedRun::options()` returns `write: WriteMode::NoReplace` always. `atomic::numbered(name, n)` unchanged; callers start at `n = 1`.

- [ ] **Step 1: Write the tables, then the failing tests.** Change every existing expectation of a first clash from `-2` to `-1` (find them with `git grep -nE -- '-2\.(png|jpg|webp|svg)|numbered\(name, 2\)|2\.\.=MAX_NAME_NUMBER'`) and add:
  - `atomic`: `a_first_clash_gets_minus_one` (`logo.png` taken → `logo-1.png`); `a_second_clash_gets_minus_two` (`logo.png`, `logo-1.png` taken → `logo-2.png`); `minus_two_is_not_used_while_minus_one_is_free` (`logo.png`, `logo-2.png` taken → `logo-1.png`); `no_name_ends_in_minus_zero` (after 3 clashes the folder holds no `logo-0.png`); the existing "no free name" test now fills `logo.png` and `logo-1.png` … `logo-9999.png` (or shrinks the bound through the existing test hook if there is one) and still fails.
  - `plan` / `output_folder.rs`: two inputs named `a.png` into one output folder → `a.png`, `a-1.png`.
  - `cli.rs`: `--out` with `a.png` already there → `a-1.png`; and `the_cli_without_out_still_replaces` (run twice, one `optimized-a.png`, its bytes from the second run).
  - `session.rs`: `the_app_never_replaces_next_to_the_originals` (`PreparedRun::options().write == NoReplace` with no output folder); `the_app_never_replaces_in_an_output_folder` (same with a folder). Plus one end-to-end test through `Session::run_with_worker` and a real worker: two runs on `a.png` with no folder leave `optimized-a.png` (first run's bytes, unchanged) and `optimized-a-1.png`.
- [ ] **Step 2:** Run `cargo test --workspace` and expect the new and changed tests to fail. Record the output.
- [ ] **Step 3:** Implement: `write_new` and `plan::free_name` start at 1 (`1..=MAX_NAME_NUMBER`); `PreparedRun::options` returns `NoReplace`; update the doc comments, `README.md` ("`logo.png`, `logo-1.png`, …"; the app never replaces an output, the command line without `--out` does) and `INTERFACE.md` (`start_run` writes next to the originals without replacing).
- [ ] **Step 4:** Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`; both pass.
- [ ] **Step 5:** Commit: "Number a taken name from -1, and never replace an output in the app".

### Task 2: "Re-add", backend and window

**Files:**
- Modify: `src-tauri/src/session.rs` (`FileList`, `Session`), `src-tauri/src/lib.rs` (command, `generate_handler!`), `src-tauri/build.rs` (command list), `src-tauri/capabilities/default.json` (permission, description "seven"), `src-tauri/tests/config.rs` (commands and button checks), `src-tauri/INTERFACE.md`, `ui/index.html`, `ui/app.js`, `ui/state.js` (only if a view function needs a field), `ui/app.css` (summary buttons side by side, wrapping together).
- Test: `src-tauri/src/session.rs` (unit), `src-tauri/tests/config.rs`, `tests/ui/state.test.js`, `tests/ui/table.test.js` if focus helpers change.

**Interfaces:**
- Produces: `FileList::readd(&mut self)` — clears `finished`, keeps `entries` and `seen`. `Session::readd_list(&self, emit: &mut impl FnMut(Event)) -> Result<(), CommandError>` — claims the guard (busy → `Err(Busy)` and `run-busy`, as `clear_list`), calls `readd`, emits `self.list_changed(None)`. Tauri command `readd_list(app: AppHandle) -> Result<(), CommandError>`.
- Window: `panelView(state).showSummary` already gates the summary; the button uses `canChoose`.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `session.rs`: `readd_after_a_run_brings_back_the_same_rows_in_order` (3 files, run to `run-finished`, `readd_list` → `list-changed` with the 3 names in order); `without_readd_the_first_add_after_a_run_starts_a_new_list` (existing behaviour, kept as its pair); `adds_after_a_readd_append`; `readd_lists_the_added_paths_not_the_outputs` (drop a folder holding `a.png`, run next to the originals, `readd_list` → one row `a.png`) with its pair `dropping_the_folder_again_lists_the_output_too` (two rows); `readd_plans_the_rows_again` (delete one file after the run → that row is "Not a readable file"; the others keep their sizes); `readd_while_busy_is_refused_and_tells_the_window` (`Err(Busy)` and one `run-busy`, no `list-changed`) with its pair `readd_when_idle_resolves_and_sends_the_list`; `readd_of_an_empty_list_sends_an_empty_list`; `readd_of_an_all_skipped_list_brings_it_back` (two `.txt` files: both rows come back as skipped, and the window's `canRun` stays false for that state in `state.test.js`).
  - `config.rs` (parsed JSON / HTML): `the_capability_allows_exactly_the_seven_commands` with negative fixtures (six without `allow-readd-list`; eight with an extra); `build_rs_declares_readd_list` if config.rs already checks `build.rs` (otherwise the capability test covers it, because a missing `allow-readd-list` permission fails the build); `the_summary_has_readd_before_open_folder` with negative fixtures (missing, after `#openFolder`, outside `#summary`, text other than "Re-add").
  - `state.test.js`: the button view — `readd shows only when finished` (finished → shown and usable; ready, running, empty → hidden) and `a readd click sends readd_list only when canChoose` (move the click decision into a tested function, as the other buttons use `onClick(button, gate, command)`; the test asserts finished → one call, running → none); `settings survive a readd` (state after a lossy 60 run, then `list-changed` → `mode === 'lossy'`, `quality === 60`, `phase === 'ready'`), with its pair that a `list-changed` during `running` changes nothing (existing).
  - Focus: `hideKeepingFocus(document, summary, …, run)` — test that focus on `#readdList` moves to Run when the summary hides, and focus elsewhere does not move (extend the existing `hideKeepingFocus` tests in `table.test.js` with the summary case if the fallback argument changes from `chooseFiles` to `run`).
- [ ] **Step 2:** Run `cargo test -p lunaepix-app` and `node --test tests/ui/*.test.js`; the new tests fail. Record the output.
- [ ] **Step 3:** Implement. In `app.js`: look up `#readdList`, wire `onClick(readdList, canChoose, 'readd_list')`, set `aria-disabled` from `canChoose`, and pass Run as the summary's focus fallback. In `index.html`: the button before `#openFolder`, inside `#summary`. In `app.css`: the two summary buttons side by side, wrapping together under the text at narrow widths (no test). Update `INTERFACE.md`: the command table, its rejection, "The first add after a run has finished starts a fresh list, unless `readd_list` was called", and the permissions list of seven.
- [ ] **Step 4:** Run `cargo test --workspace`, `node --test tests/ui/*.test.js` and clippy; all pass.
- [ ] **Step 5:** Commit: "Add Re-add: put the last run's files back in the list".

### Task 3: Make a thumbnail (core, pure)

**Files:**
- Create: `crates/core/src/thumb.rs` (the public entry, sizing, PNG encode), `crates/core/src/thumb/decode.rs` (per-format decode to RGBA, bounded).
- Modify: `crates/core/src/lib.rs` (module, re-exports, crate doc's list of `unsafe` modules if the JPEG decode lives in `thumb/decode.rs`; prefer putting the mozjpeg decode in the existing `jpg` module so the `unsafe` list does not grow), `crates/core/src/jpg.rs` or `crates/core/src/jpg/decode.rs` (scaled JPEG decode), `crates/core/src/webp/codec.rs` (scaled still decode; first frame of an animation through the existing `decode_animation`).
- Test: `crates/core/tests/thumbnail.rs` (new), unit tests beside the code.

**Interfaces:**
- Produces:
  - `pub const THUMB_SIDE: u32 = 64;` `pub const MAX_SVG_THUMB_BYTES: u64 = 2 * 1024 * 1024;`
  - `pub enum Thumbnail { Png(Vec<u8>), Svg(Vec<u8>) }`
  - `pub fn thumbnail(src: &Path) -> Result<Thumbnail, CannotShow>`; `pub struct CannotShow(pub String)` (a reason for logs and tests; never shown to the user).
  - `pub(crate) fn fit(width: u32, height: u32, side: u32) -> (u32, u32)` — longest side to `side`, aspect kept, each side at least 1, never enlarged.
- Consumes: `plan::format_of`, `plan::check_pixels` (`MAX_PIXELS`), `MAX_FILE_BYTES`.

Behaviour:
- Opens `src` only if it is a regular file (not a folder, FIFO or device) of at most `MAX_FILE_BYTES` (SVG: `MAX_SVG_THUMB_BYTES`). Reads the format from the extension (`format_of`) and checks the content matches it.
- Checks the header's width × height with `check_pixels` before decoding.
- JPG: decode with `scale_num = 1`, `scale_denom` the largest of 1, 2, 4, 8 that keeps both sides ≥ the target size; output RGB; greyscale expanded; CMYK/YCCK → `CannotShow`.
- PNG: `png` crate with `Transformations::EXPAND | Transformations::STRIP_16` and greyscale → RGBA; an APNG gives its default image.
- WebP: still — libwebp scaled decode (`WebPDecoderConfig` with `use_scaling`) straight to the target size; animated — the first frame from `decode_animation`.
- Then area-average down to `fit(w, h, THUMB_SIDE)` and encode an 8-bit RGBA PNG with the `png` crate.
- SVG: the bytes are returned as `Thumbnail::Svg` when they are valid UTF-8 and the first non-whitespace character (after an optional BOM) is `<`; otherwise `CannotShow`.
- Orientation is Task 4; this task returns the picture as stored.

- [ ] **Step 1: Write the tables, then the failing tests** (fixtures made in the test with the `image` dev-dependency, `png` and the existing helpers in `crates/core/tests/common`):
  - `fit`: `(400, 200) → (64, 32)`, `(200, 400) → (32, 64)`, `(20, 10) → (20, 10)`, `(64, 64) → (64, 64)`, `(65, 64) → (64, 63)`, `(10000, 1) → (64, 1)`, `(1, 10000) → (1, 64)`; negative: no output side is 0, none exceeds 64, none exceeds its input.
  - `a_400x200_jpg_png_and_webp_each_give_a_64x32_rgba_png` (decode the output with `image`; check size and colour type) / `a_20x10_image_is_not_enlarged`.
  - Colour fidelity: a red left half / blue right half 400 × 200 gives a thumbnail whose left pixel is red-ish and right pixel blue-ish (tolerance 16 per channel) — so a channel swap fails.
  - PNG colour types (Review Focus 3): greyscale, greyscale + alpha, 16-bit RGB, palette with `tRNS` → each an RGBA thumbnail; the transparent pixel of the palette fixture stays alpha 0 / its negative: an opaque palette fixture gives alpha 255 everywhere.
  - `a_greyscale_jpg_gives_a_grey_thumbnail` / `a_cmyk_jpg_cannot_be_shown`.
  - Animation: `an_animated_png_gives_its_default_image` and `an_animated_webp_gives_its_first_frame` (first frame red, second blue) / negative: no blue pixel in either thumbnail.
  - Bounds: `an_image_of_exactly_max_pixels_passes_the_check` (call the header check on 10000 × 10000; a header-only PNG fixture where decoding is not needed) / `one_pixel_more_cannot_be_shown_without_decoding` (10000 × 10001 header with no image data → `CannotShow` whose reason names the size, proving the check came first).
  - Not images: an empty file, a text file named `.png`, a truncated JPG, a folder named `a.png` → `CannotShow`, never a panic.
  - SVG: an SVG under 2 MiB → `Thumbnail::Svg(same bytes)`; with a BOM and leading whitespace → still `Svg`; exactly `MAX_SVG_THUMB_BYTES` → `Svg` / one byte more → `CannotShow`; binary bytes named `.svg` → `CannotShow`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core thumb`; the new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy; both pass.
- [ ] **Step 5:** Commit: "Make a 64 px thumbnail from a JPG, PNG or WebP; pass an SVG through".

### Task 4: Upright thumbnails (EXIF orientation)

**Files:**
- Modify: `crates/core/src/webp/exif.rs` (expose `pub(crate) fn orientation(tiff: &[u8]) -> Option<u8>`, built on its existing bounded TIFF reader), `crates/core/src/thumb.rs` (find the EXIF block: JPEG `APP1` with `Exif\0\0`; WebP `EXIF` chunk through `webp/riff.rs`; apply the orientation before scaling or after, the builder's choice).
- Test: `crates/core/tests/thumbnail.rs`, unit tests in `exif.rs`.

**Interfaces:**
- Consumes: `thumbnail` from Task 3. Produces: `pub(crate) fn orientation(tiff: &[u8]) -> Option<u8>` (1–8, `None` for missing, damaged or out-of-range), `pub(crate) fn orient(rgba, width, height, orientation: u8) -> (rgba, width, height)`.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `orientation`: little- and big-endian TIFF with tag 0x0112 = 6 → `Some(6)`; each of 1–8 → itself; value 0 or 9 → `None`; a truncated IFD, an offset past the end, an entry count past the end → `None`, never a panic.
  - `orient` on a 3 × 2 image with distinct corner colours: each of the 8 values puts the top-left source pixel in the corner the EXIF standard says (table in the test), and 5–8 swap width and height. Negative: 1 leaves the buffer unchanged.
  - End to end: a 400 × 200 JPG and a WebP with orientation 6 → 32 × 64 thumbnail; orientation 5 → 32 × 64 with the mirrored corner (pixel check); orientation 1 and no EXIF → 64 × 32; a damaged EXIF block → 64 × 32 and no error.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core thumb exif`; the new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy; both pass.
- [ ] **Step 5:** Commit: "Show thumbnails upright for all eight EXIF orientations".

### Task 5: The thumbnail worker and its runner

**Files:**
- Modify: `crates/core/src/worker.rs` (answer `THUMB_FLAG`), `crates/core/src/run.rs` (runner side), `crates/core/src/lib.rs` (re-exports), `crates/core/src/bin/lunaepix-worker.rs` (usage text names both modes).
- Test: `crates/core/tests/worker.rs` (real binaries through `CARGO_BIN_EXE_lunaepix-worker` and the CLI binary), unit tests in `worker.rs` and `run.rs`.

**Interfaces:**
- Produces:
  - `pub const THUMB_FLAG: &str = "--lunaepix-thumb";` `pub const THUMB_TIMEOUT: Duration = Duration::from_secs(10);` `pub const MAX_THUMB_ANSWER_BYTES: usize = 2 * 1024 * 1024;`
  - Worker: `serve_if_requested()` also answers `<exe> --lunaepix-thumb <src>`: the same start-up (`die_with_parent`, OOM score, crash dialogs) as the optimiser, then `thumbnail(src)`, then writes the header and bytes and exits 0, or exits 3 with nothing on stdout. Anything but exactly one argument after the flag: exit 2, nothing on stdout.
  - Runner: `pub enum ThumbAnswer { Png(Vec<u8>), Svg(Vec<u8>), CannotShow, Failed }` and `pub fn run_thumb(program: &Path, src: &Path) -> ThumbAnswer` with the timeout and cap above. `CannotShow` is exit 3 only; everything else that is not a valid answer is `Failed` (spawn error, timeout, crash, other exit code, over-long answer, missing or wrong header, a `png` header whose bytes do not start with the PNG signature `89 50 4E 47 0D 0A 1A 0A`). It reuses `run.rs`'s spawn, `read_capped`, `wait_with` and `kill_and_reap`, and spawns and waits on the calling thread.

Header, exactly:

```text
lunaepix-thumb 1 png\n<bytes>
lunaepix-thumb 1 svg\n<bytes>
```

- [ ] **Step 1: Write the tables, then the failing tests:**
  - Worker (unit, with `serve_with`-style injection of `thumbnail` and the output): a `Png` answer writes the `png` header and the bytes, exit 0; `Svg` likewise; `CannotShow` → exit 3 and nothing written; no source, two sources, an unknown option → exit 2 and nothing written.
  - Real binaries: the sidecar and the CLI binary both answer `--lunaepix-thumb` for a PNG fixture (header + a decodable 64 px PNG) / both exit 3 for a text file.
  - Names (Review Focus 4): a PNG named `Mobilis Logo – zwart é.png` and, on Unix only, a name with byte `0xE9` → a thumbnail.
  - `run_thumb` with double programs (small scripts or the test helper binary the existing `run.rs` tests use): a valid `png` answer → `Png(bytes)`; exit 3 → `CannotShow`; exit 1, a crash by signal, a program that does not exist → `Failed`; an answer of exactly `MAX_THUMB_ANSWER_BYTES` plus header → `Png` / one byte more → `Failed`; a header `lunaepix-thumb 2 png` or none → `Failed`; a `png` header with non-PNG bytes → `Failed`.
  - Timeout (Review Focus 5): a program that sleeps past the limit (run the test with a shortened limit through an internal `run_thumb_with(program, src, timeout, cap)`) → `Failed`, and the child is reaped (no zombie: `kill_and_reap` was reached; reuse how the existing timeout test checks it).
- [ ] **Step 2:** Run `cargo test -p lunaepix-core worker run`; the new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy; both pass.
- [ ] **Step 5:** Commit: "Make thumbnails in a worker process".

### Task 6: Thumbnail keys and the `thumb://` scheme in the app

**Files:**
- Create: `src-tauri/src/thumbs.rs` (keys, snapshot, cache, two-slot pool, the answer to one request).
- Modify: `src-tauri/src/events.rs` (`ListRow.thumb: Option<String>`), `src-tauri/src/session.rs` (`list_rows` gets the keys; `Session::list_changed` updates the snapshot; Clear, a new list after a finished run and `readd` behave as below), `src-tauri/src/lib.rs` (register the scheme; keep the worker program chosen by `choose_worker` in `AppState` for `run_thumb`), `src-tauri/tauri.conf.json` (CSP), `src-tauri/tests/config.rs` (CSP check), `src-tauri/INTERFACE.md` (`list-changed` example with `thumb`, the scheme, the CSP line).
- Test: `src-tauri/src/thumbs.rs` (unit), `src-tauri/src/session.rs` (unit), `src-tauri/tests/config.rs`.

**Interfaces:**
- Consumes: `run_thumb`, `ThumbAnswer` (Task 5); `readd` (Task 2).
- Produces:
  - `pub struct Thumbs` with `pub fn new() -> Self`; `pub fn snapshot(&self, rows: &[Option<PathBuf>]) -> Vec<Option<String>>` — takes the image path per row (`None` for a skip and the over-limit row), returns the key per row, raising the list number when a row index already in the current snapshot would get a different path; `pub fn new_list(&self)` — raises the list number and drops the cache; `pub fn answer(&self, uri_path: &str, make: impl Fn(&Path) -> ThumbAnswer) -> ThumbResponse`.
  - `pub struct ThumbResponse { pub status: u16, pub content_type: Option<&'static str>, pub body: Vec<u8> }` — 200 `image/png`, 200 `image/svg+xml`, or 404 with no type and an empty body.
  - `pub fn parse_key(uri_path: &str) -> Option<(u64, usize)>` with the regex `^/(\d{1,10})-(\d{1,4})$` (no percent-decoding).
- Rules:
  - `Session::clear_list` and the first add after a finished run call `new_list()`. `readd_list` does not.
  - The cache maps key → answer for the current list number only. A `CannotShow` is cached as a 404; a `Failed` is not cached.
  - An answer computed for a list number that is no longer current when it returns is not cached and is answered 404.
  - At most two `make` calls run at a time (a `Mutex<usize>` + `Condvar`, or an equivalent); a third request waits.
  - `image/svg+xml` only when the row's file name ends in `.svg` (ASCII case-insensitive) and the answer is `Svg`; `image/png` only for `Png`. Anything else → 404.
- Registration in `lib.rs`: `.register_asynchronous_uri_scheme_protocol("thumb", …)`; the handler moves the request path and an `AppHandle` onto a new thread (`std::thread::Builder`, named `thumbnail`), calls `thumbs.answer(path, |src| run_thumb(&program, src))` there, and responds with the `ThumbResponse`. `make` runs on that thread, so the worker is spawned and waited on by the same thread.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `parse_key`: `/3-0`, `/4294967295-1999`, `/0-9999` → `Some` / `/3-x`, `/-1`, `/3/0`, `/3%2D0`, `/3-0-1`, `/12345678901-0`, `/3-12345`, `/3-0/extra`, `/3-0?x=1`, `3-0` (no slash), `/%2Fetc%2Fpasswd`, `/../a.png`, empty → `None`.
  - Keys via `Session` (`list_changed` payloads): 3 images → `"N-0"`, `"N-1"`, `"N-2"`; a skipped row and the over-limit row (2001 files) → `null`; an add keeps the earlier keys; Clear raises N; the first add after a finished run raises N; `readd_list` keeps N; re-planning that drops a middle row (make an entry a folder, as the existing `rows()` test does) raises N so no key changes its path.
  - `answer` with a recording `make` double: current key of an image row → 200 `image/png` with the double's bytes and the double got that row's path (not row 0's or 2's) / an earlier list number, an index past the end, a skipped row's index, every bad shape above → 404, empty body, and the double recorded no call.
  - Failures: `CannotShow` → 404 and a second request makes no call / `Failed` → 404 and a second request calls again.
  - SVG: a `.svg` row answered `Svg` → 200 `image/svg+xml` / a `.png` row answered `Svg` → 404 / an `.svg` row answered `Png` → 200 `image/png` (it is a real PNG; only the svg type is restricted).
  - Cache: a second request for the same key makes no call / after `new_list()` the same index makes a call again.
  - Stale answer (Review Focus 1): a `make` double that calls `new_list()` before it returns → 404 and nothing cached; a request for the new list's key then calls again.
  - Pool: three requests on three threads with a double that blocks on a barrier → at most two inside `make` at once (the double counts the peak) / with two requests both run at once (peak 2, so the pool is not a single slot).
  - `config.rs` CSP (parsed): `img-src` is exactly `'self' thumb: http://thumb.localhost` / fixtures with `data:`, `blob:`, `*`, `https:`, `http://evil.localhost` added are refused; every other directive equals today's.
- [ ] **Step 2:** Run `cargo test -p lunaepix-app`; the new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy; both pass.
- [ ] **Step 5:** Commit: "Serve thumbnails per list row over thumb://".

### Task 7: Thumbnails in the window

**Files:**
- Modify: `ui/state.js` (`listRowFrom`, `waitingRow`, the `file-started` and `file-done` cases keep `thumb`; `rowView` passes it on), `ui/table.js` (`buildRow`: the square and the image; `syncRows` keeps an unchanged image), `ui/app.js` (passes a `thumbSrc(key)` built on `window.__TAURI__.core.convertFileSrc` into the table code), `ui/app.css` (square, image, note alignment; no test).
- Test: `tests/ui/state.test.js`, `tests/ui/table.test.js`.

**Interfaces:**
- Consumes: `thumb` in `list-changed` (Task 6).
- Produces: `export function thumbKey(value): string | null` in `state.js` — the value when it is a string matching `^\d{1,10}-\d{1,4}$`, otherwise `null`. Rows carry `thumb: string | null`. `buildRow(doc, row, thumbSrc)` where `thumbSrc(key) → string`.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `thumbKey`: `"3-0"`, `"4294967295-1999"` → kept / `3`, `{}`, `""`, `undefined`, `"3/0"`, `"3-0-1"`, `"../a"`, `"3-x"`, `"thumb://localhost/3-0"`, `"12345678901-0"` → `null`.
  - Rows: `list-changed` keeps `thumb` on each row; `run-started` gives the waiting rows the list row's `thumb`; `file-started` and every `file-done` kind (done, skipped, failed, and a row marked unfinished at `run-finished`) keep it / a row with `thumb: null` stays `null` through the run.
  - `buildRow` with a recording `thumbSrc` double: a row with a key → name cell starts with `span.lunaepixResults__thumb` holding `img.lunaepixResults__thumbImage` with `src` from the double, `alt=""`, `width`/`height` 32, `loading="lazy"`, `decoding="async"`, and the double got that key / a row with `null` → the empty span, no `img`, no call to the double.
  - The name text stays in the `th` (row header) and a note still follows the name (existing note test extended).
  - No flicker: `syncRows` replacing a waiting row by its compressing and done rows keeps the same `img` element (or, if the builder chooses the cache route, assert the `src` is identical and document the choice in the report).
- [ ] **Step 2:** Run `node --test tests/ui/*.test.js`; the new tests fail. Record the output.
- [ ] **Step 3:** Implement. CSS: the square is 32 × 32 with a 1 px `--rowLine` border, `object-fit: contain`, on the table background; the name cell lays out square and text side by side with the note lined up under the text.
- [ ] **Step 4:** Run `node --test tests/ui/*.test.js` and `cargo test --workspace` (config checks); both pass.
- [ ] **Step 5:** Commit: "Show a thumbnail before each file name".

### Task 8: Documentation, the full suite and the hand-test list

**Files:**
- Modify: `README.md` (Re-add, thumbnails, numbering), `src-tauri/INTERFACE.md` (read whole; it matches every change above), `docs/roadmap.md` (item status "Built; being tested by hand").

- [ ] **Step 1:** Update the texts.
- [ ] **Step 2:** Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `node --test tests/ui/*.test.js`. All pass.
- [ ] **Step 3:** Write the user's hand-test list into the report (not the repository), for Linux and Windows:
  - Run lossless, press Re-add, switch to Lossy 60, Run: both `optimized-x.jpg` and `optimized-x-1.jpg` exist next to the originals, the first unchanged.
  - Re-add, then drop one more file: the list has the old files plus the new one.
  - A chosen output folder with a name clash gives `-1`.
  - Thumbnails for a JPG, a rotated phone photo, a transparent PNG, an animated WebP, an SVG, a skipped `.txt` (empty square), a 2000-file folder (list appears at once, thumbnails fill in).
  - Clear, then add other files: no old picture appears beside a new name.
  - The window at 360 px wide: the summary buttons wrap together.
- [ ] **Step 4:** Commit: "Document Re-add, thumbnails and numbering from -1".

## Implementer report

Every implementer and every fix round writes, **before any code**:

1. **A test table:** behaviour → positive test → negative test → "fails on its own because…", one row per behaviour.
2. **A failure-branch table:** every `Err` path, fallback, `catch`, kill/reap, "log and skip", and every if/else driven by a setting or option the task adds or touches → the test that reaches it, or "untested" with the reason. Plus one row per core function the task newly calls, naming the branches inside it the new caller can reach.

The report then gives:
- the RED run (command and failure output, taken before the production code existed);
- the GREEN run;
- the "Reuse survey" block from `~/.claude/CLAUDE.md`;
- the files changed.

Reviewers check the diff against both tables and probe alternative spellings in any structured-file check. Lunaepix has no Grunt build and no devsync deployment, so there is nothing to build or upload beyond `cargo` and `node`.

## CMS changes

No CMS changes.
