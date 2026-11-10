# Lunaepix lossy compression — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-09

## Handoff

Decided in conversation (2026-11-09). The user's words are quoted as they were said.

- **Plan approved by the user (2026-11-09):** "approved". Plan: `docs/in-progress/plans/2026-11-09-lunaepix-lossy.md`. Task 1 is a hard measurement gate that sets the animated-WebP cap.
- **Spec approved by the user (2026-11-09):** "approved". Branch `feature/lunaepix-lossy`, from `main` at `c3e38e4`.

- Roadmap item started: "Lossy image compression".
- **Formats and the choice:** "Webp and jpg, but we do want them in general. A choice for people, to choose between lossless and lossy. Lossy must support a quality."
- **PNG included:** "But yes, png as well."
- **Quality setting is part of this feature**, not a separate roadmap item: "Why not. Make them at once." The roadmap items "Lossy image compression" and "Choose how much it can get compressed (quality, like JPEG 70%) on lossy" are delivered together.
- **PNG library:** for the GPL-licensed pngquant/libimagequant the user said "Check others". After the comparison: "Transparency needs to be supported." So quantette (no transparency) is ruled out. **quantizr** is my pick from the remaining permissive libraries (see "Libraries"); exoquant (unmaintained since 2016) and color_quant (no dithering) were not chosen. A side-by-side test on the user's own images was offered and not taken up.
- **JPG speed-up:** "Can't we add the speed-up code?" Yes: mozjpeg's SIMD code is turned on, which makes `nasm` a build requirement.
- **libwebp in the product:** "Fine".
- **Run button:** "Incorrect, it will only run when pressing run." Adding files and running become two steps; a drop no longer starts a run.
- **Two checks:** "Correct, there is a lossless that checks the no pixes changed, and there is a lossy one for the new changes".
- **One setting per run:** "One setting for the batch job."
- **Lossy behaviour per format** (my play-back, confirmed with "Correct."): result must open as an image of the same size; if it is not smaller than the original the original is copied; kept details as lossless today; SVG gets the lossless treatment in a lossy run.
- **Animated WebP:** the user asked "is there already a suggestion of it?" (there was not; libwebp's animation encoder was proposed) and answered "Yes" to including it in this feature, with a cap on frames × pixels.
- **No memory between launches:** "Start fresh". Every launch starts on Lossless.
- **Starting quality:** "75 is fine."
- **Layout:** option B, "One ready panel after the drop", selected in the browser. The binding mock-up is [2026-11-09-lunaepix-lossy-mockup.html](2026-11-09-lunaepix-lossy-mockup.html).
- **While running:** "It starts running, showing which files are done and which are currently in progress."
- **Folders:** "No longer select an entire folder." The rest of the screen play-back was confirmed: "Yes is correct".
- **A dropped folder:** "Add the images inside it as loose files."
- **Output folder:** "Also, give an option for an output folder."
- **Names in the output folder:** "Original name, add a number when names clash".
- **Command line:** "Yes, it should get parameters for that" (lossy, quality, output folder).
- **Earlier outputs in a dropped folder:** asked whether files named `optimized-*` should be left out when a folder is dropped (they would be compressed again and become `optimized-optimized-…`), the user said "No". They are listed like any other image.
- **Calls I made after the builder check** (not user decisions): the animation check compares canvas size and total duration, not frame count; `list-changed` carries an optional `notice`; the lossy JPG drops the source's JFIF APP0 and Adobe APP14. `nasm` must be installed by the user (`sudo dnf install nasm`) before the plan runs, so the plan starts with that.
- **Task 1 ruling by the user (2026-11-09):** cap "1,000,000,000 (Recommended)" (`MAX_ANIMATION_PIXELS`, frames × canvas pixels); and a canvas limit for lossy animated WebP, "Yes, 50 MP (Recommended)" (`MAX_ANIMATION_CANVAS_PIXELS` = 50,000,000), because animation memory grows with the canvas, not the frame count. Over either: lossless treatment with the note "kept lossless: animation too large for lossy".
- **Lossy WebP EXIF flag (user, 2026-11-10):** asked what lossy should do for a WebP without `VP8X` that carries a rotated `EXIF` (readers ignore it there; lossy output adds `VP8X` and would set the EXIF flag), the user chose "Keep source's flag (Recommended)": the `EXIF` chunk is kept, but its flag stays clear for a source that had no `VP8X`, so it renders exactly as before. Ruled out: "Flag matches chunk".
- **16384-wide WebP in a lossy run (user, 2026-11-10):** a valid lossless WebP 16384 px wide cannot be encoded lossy (libwebp's limit is 16383). The user chose "Clearer error (Recommended)": check the width and height before decoding and fail with a plain reason naming the 16383 px limit. No new note. Ruled out: "Lossless with a note", "Leave as is".
- **Test remedy (user, 2026-11-10):** after tests written after their code (Tasks 4, 6) and negatives that cannot fail on their own (Tasks 3, 5) recurred, the user chose "Controller RED audit (Recommended)": before each review the controller compares the RED run's test list with the commit's final tests, and a test missing from it goes back as a concern before review; the test table gets a "fails on its own because…" column. Ruled out: "Late tests become concerns", "Accept with reviewer check".
- **Lossy PNG validation (user, 2026-11-10):** after the lossy PNG path (lenient `png` crate plus hand-written checks) twice accepted damaged files the lossless path refuses (a bad-CRC chunk in Task 4; image data longer than the IHDR in Task 8a, whose size check a crafted IDAT split then bypassed), the user chose "oxipng parses first (Recommended)": before decoding, the lossy path runs oxipng's own parser (`oxipng::internal_tests::PngData::from_slice`, doc-hidden, oxipng pinned at `=10.2.1`) and refuses whatever it refuses, with its message; the hand-written CRC and image-size pre-checks go. Ruled out: "Patch + comparison test", "Lossless decides first".
- **A PNG oxipng accepts but the `png` decoder refuses, in a lossy run (user, 2026-11-10):** for example a PNG with two `tRNS` chunks (lossless optimises it; the decoder's error even names the wrong chunk, "Chunk PLTE must appear at most once"). The user chose "Refuse, clearer message": the lossy run keeps refusing such a file, with the reason worded "could not decode the PNG for lossy: …". No new note. Ruled out: "Lossless with a note", "Refuse as now".
- **Branch-table remedy (user, 2026-11-10):** after reviews found untested branches that decide the outcome three times (Task 4 oxipng error, Task 8a decoder error, Task 9 replace-or-not), the user chose "Branch table covers choices (Recommended)": the implementer's failure-branch table also lists every if/else driven by a setting or option, each side with a named test, and the controller checks before review that every row names a test. Ruled out: "Pre-check the test plan", "Keep as is".
- **Task 10 rulings (user, 2026-11-10):** (1) A folder kept in the list as a skipped row (unreadable or missing when added) that is a readable folder by Run: "Stays a skipped row (Recommended)" — at Run it becomes the same "Not a readable file" skip the list showed, so the run matches the list one for one; the user drops the folder again to get its images. Ruled out: "Expand it at Run". (2) start_run with no settings argument: "Make it optional (Recommended)" — `start_run(settings: Option<Value>)`, a missing one refused as `"invalid-settings"` like any other bad shape. Ruled out: "Keep the brief's signature". (3) After a deciding branch inside core planning code, reached through a new path from the app, slipped past the branch table: "Add a cross-module row (Recommended)" — the branch table also gets one row for every core function the task newly calls, naming the branches inside it that the new caller can reach; the controller checks those rows before review. Ruled out: "Keep the current rule".
- Put off, not part of this feature: the before/after preview slider, export rename, and the "Clean and clear UI" design pass (all separate roadmap items).

## Problem

Lunaepix only compresses losslessly. For photos that is often not small enough: a 2.4 MB holiday JPG stays around 2.2 MB. People need a way to trade a change in pixels that the eye should not notice for a much smaller file, and to say how far that may go.

Today a drop starts compressing at once, so there is no moment to make that choice.

## Goals

- A choice per run between **Lossless** (today's behaviour, unchanged) and **Lossy** with a **quality from 0 to 100**.
- Lossy for **JPG, PNG (with transparency) and WebP, still and animated**.
- Adding files and pressing **Run** become separate steps, with the list of files visible before and during the run.
- An optional **output folder**.
- The same options on the **command line**.

## Non-goals

- Converting between formats (a PNG stays a PNG, a JPG a JPG, a WebP a WebP).
- A preview of the result before running (roadmap: "Preview single image compression").
- Renaming the output of a single file (roadmap: "Export rename").
- Remembering settings between launches.
- Lossy SVG, lossy animated PNG.
- Compressing several files at once (files still run one at a time).
- The visual design pass (roadmap: "Clean and clear UI").

## Anti-goals

- **A lossless run that behaves any differently from today**, in output bytes or checks.
- **An original that is changed or overwritten.** Not by a lossy run, not by an output folder that is the originals' own folder.
- **An existing file in the chosen output folder that is overwritten.**
- **A lossy output that is larger than the original.**
- **A transparent PNG that loses its transparency.**
- **A rotated photo that ends up sideways** because its orientation was dropped.

## Design

### The run settings

A run has three settings, fixed when Run is pressed:

| Setting | Values | Starts as (every launch) |
|---|---|---|
| Mode | `lossless`, `lossy` | `lossless` |
| Quality | integer 0–100 | 75 (used only when mode is `lossy`) |
| Output | next to the originals, or one chosen folder | next to the originals |

They reach each worker process as arguments, so a worker optimises exactly one file with exactly one set of settings. The window sends mode and quality as values; the backend validates them (mode one of the two words, quality an integer within 0–100, anything else refused) before a run starts. The window never sends a path: the output folder is chosen in a native dialog on the Rust side, as files are today.

### Lossless mode

Unchanged: every format goes through today's code and today's pixel check.

### Lossy mode, per format

The general rule for every lossy file:

1. The lossy result is built.
2. **The lossy check:** the result is decoded again and must give an image (or animation) of the same width and height, and for an animation the same canvas size and the same total duration. If not, the file fails with a clear reason and nothing is written. This is a bug guard, not a quality judgement. My call, raised by the builder check: libwebp's animation encoder merges a frame that matches the previous one (or, in lossy mode, nearly matches it) into that frame, adding its duration (`anim_encode.c`, `IncreasePreviousDuration`). So the output can have fewer frames than the source, and that is correct; the frame count is not compared.
3. If the result is not smaller than the original, the **original bytes** are written, as lossless does today.
4. Details kept, the same as lossless today: a photo's orientation is never lost, and colour profiles are kept.

| Format | How | Notes |
|---|---|---|
| **JPG** | Decoded and encoded again by mozjpeg at the quality (mozjpeg's own defaults otherwise: optimised Huffman tables, progressive). The source's APPn and COM markers are copied, except its JFIF APP0 and Adobe APP14: the new encode writes its own JFIF, and a copied Adobe marker could tell a decoder the new YCbCr data is RGB (wrong colours). My call, raised by the builder check; EXIF, ICC and XMP are copied as the lossless path does. | A JPG that is already lossy loses a little more each time; that is what the user asked for, and the "not smaller → original" rule still holds. CMYK and other non-RGB/non-grey JPGs get the lossless treatment (see "Fallbacks"). |
| **PNG** | Decoded to RGBA (8 bit per channel; 16-bit sources are reduced), reduced to a palette by quantizr with alpha, with dithering, then written as an indexed PNG and passed through oxipng with today's settings. | Quality maps to quantizr's settings, monotonic: a lower quality never gives more colours. At 100 the palette has 256 entries. The exact mapping is the builder's call, written down in the code. The text chunks today's lossless path keeps (`--strip safe`) and the colour profile are kept. |
| **WebP, still** | Decoded and encoded again by libwebp as lossy at the quality; alpha is kept (libwebp's default alpha quality 100). | Lossless and lossy sources alike. The `ICCP` chunk is kept; `EXIF` is kept when its Orientation is not 1 and dropped otherwise; `XMP ` is dropped. These are today's WebP rules (`webp::riff`), reused, not rewritten. |
| **WebP, animated** | libwebp's animation decoder reads every frame as a full canvas with its timestamp; libwebp's animation encoder writes them again, lossy at the quality. Loop count, frame timings and background colour are kept. Same `ICCP`/`EXIF`/`XMP ` rules as still WebP. | Under a cap on total work (frames × canvas pixels), checked from the headers before decoding. The cap is measured in the plan's first step and set by the user (see "First step"). Over the cap: lossless treatment. |
| **SVG** | Lossless treatment. | SVG has no lossy mode. |

### Fallbacks inside a lossy run

These files get today's lossless treatment in a lossy run, and their row says so (see `file-done`, `note`):

- SVG: no note, since it is expected.
- An animated PNG (has an `acTL` chunk): "kept lossless: animated PNG".
- A JPG that is not RGB or greyscale (CMYK, YCCK): "kept lossless: CMYK JPG".
- An animated WebP over the cap: "kept lossless: animation too large for lossy".

A lossy result that was not smaller gets the note "original kept: lossy was not smaller".

### Libraries

| Library | Use | Licence | Change |
|---|---|---|---|
| mozjpeg (`mozjpeg-sys` `=2.2.3`) | JPG lossy encode | IJG/BSD (already shipped) | Enable its SIMD feature. This needs `nasm` on every build machine: the developer's Fedora machine (`dnf install nasm`), the Windows CI test and build jobs, and the README's build steps. The Windows workflow's install step must keep `src-tauri/tests/workflow.rs` passing (actions pinned by hash, no `${{ }}` in run scripts, read-only token). |
| libwebp (`libwebp-sys` `=0.14.4`) | WebP lossy and animation | MIT crate, BSD-3-Clause vendored C, Google `PATENTS` | Moves from dev-dependency to dependency. The licence handling the WebP spec described for it (`docs/finished/specs/2026-11-06-lunaepix-webp-design.md`, "Library" → "Licence notice": `about.toml` clarification with the `vendor/COPYING` checksum, `PATENTS` block in `about.hbs`) now applies. Its FFI lives in its own module with `#[allow(unsafe_code)]`, named in the crate doc in `lib.rs`. libwebp reports failures through return codes, so nothing unwinds through C. |
| quantizr (`=1.4.3`) | PNG palette reduction with alpha | MIT (declared as `license-file`, so crates.io shows "non-standard") | `about.toml` gets a clarification entry from its `LICENSE` file with that file's SHA-256, the same pattern as `libdeflate-sys`. Pure Rust: no `unsafe` added to Lunaepix. |

`THIRD-PARTY-LICENSES.html` is regenerated, and the README's licence section names libwebp and quantizr. The accepted-licence list in `about.toml` does not change. If any new crate (including the transitive ones) brings a licence outside that list, the builder stops and reports it; the list is never extended to make a run pass.

### Limits

All of today's per-file limits apply in lossy mode too: `MAX_FILE_BYTES` (100 MB), `MAX_PIXELS` (100 megapixels, checked from the header before decoding), `WORKER_TIMEOUT` (10 minutes), `MAX_RUN_FILES` (2000 files per list). The animated-WebP cap comes on top.

### Output location

**Next to the originals** (default): today's rule, unchanged: `optimized-<name>` beside the original, and an existing `optimized-<name>` is replaced.

**A chosen output folder:**

- Each file keeps its **original name**.
- When the name is already taken, a number goes before the extension: `logo.png`, then `logo-2.png`, `logo-3.png`, and so on. "Taken" means: an existing file or folder of that name in the output folder, or an earlier file of the same run that got that name. Names are compared case-insensitively on every platform, so the result is the same on Windows and Linux.
- **No file in the output folder is ever overwritten**, including one that appears there while the run is going: the final write must refuse to replace an existing name (a no-replace rename, or an exclusive create), and on refusal the next number is tried.
- Choosing the originals' own folder is allowed and safe: the outputs get a number.
- A file without an extension gets the number at the end: `README-2`.
- The output folder must still exist and be writable when Run is pressed; if it is gone, the run does not start and the window says so.

### What the window does

See the binding mock-up for the layout. The behaviour:

- **On launch:** the drop area as today, without "Choose folder", and no panel.
- **Adding files:** a drop or "Choose files" adds to the list.
  - The same file (same canonical path) is listed once, however often it is added.
  - A **dropped folder** adds the PNG, JPG, SVG and WebP files directly inside it (not subfolders), as loose files. Other files in that folder are not listed. Earlier outputs named `optimized-*` are listed like any other image (user decision). A folder with none of them adds nothing, and the status line says "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)".
  - A dropped or chosen loose file that will be skipped is listed with its reason ("Will be skipped: not a PNG, JPG, SVG or WebP file").
  - Over 2000 files: as today, one row counts the files left out.
- **Clear** empties the list and closes the panel. The settings keep their values.
- **Run** starts the run with the current settings. Run cannot be used while the list holds no file that will be compressed.
- **While running:** each row shows *Waiting*, *Compressing…* or its result. The heading shows "Compressing 2 of 3 · Lossy 75" (or "· Lossless"). The drop area, "Choose files", the settings, Clear and Run cannot be used; a drop is refused with `run-busy` as today.
- **Finished:** the rows keep their results; the heading says "Done · Lossy 75"; the summary and "Open folder" appear as today; the settings and Run are no longer shown. The next drop or pick starts a fresh list.
- **Open folder** opens the chosen output folder, or, with no output folder, the folder of the first file written (today's rule for loose files).

### Window ↔ backend interface

`src-tauri/INTERFACE.md` is updated to this. The window still never sends a path and never receives one; it gets file names and numbers only. The output folder is shown by its **folder name** only (for example `exports`), my call to keep that rule.

**Commands** (each resolves to `null` or rejects with one bare string, as today):

| Command | Argument | Does | Rejects with |
|---|---|---|---|
| `choose_files` | none | Native multi-file dialog; the files picked are added to the list. | `"busy"` |
| `clear_list` | none | Empties the list. | `"busy"` |
| `choose_output_folder` | none | Native folder dialog; sets the output folder. | `"busy"` |
| `clear_output_folder` | none | Back to "next to the originals". | `"busy"` |
| `start_run` | `{ "mode": "lossy", "quality": 75 }` | Runs the list with these settings and the output setting. | `"busy"`, `"invalid-settings"` (mode not one of the two words, quality not an integer in 0–100, extra or missing keys), `"empty-list"`, `"output-missing"` (the chosen folder is gone or not writable) |
| `open_output_folder` | none | As today. | `"no-output"`, `"open-failed"` |

`choose_folder` is removed. Drag and drop needs no command, as today. Permissions in `capabilities/default.json` list exactly these commands plus the two event permissions; the existing test that guards the list is updated.

**Events.** `run-busy`, `run-cancelled`, `drag-state` and `run-finished` stay as they are.

`list-changed`: sent after every add, Clear, and at the start of a fresh list. The full list each time, in order. `size` is in bytes, or `null` for a skipped row. A skipped row carries `skip`, a sentence for the user; others have `skip: null`. The over-limit row looks like today's. `notice` is a sentence for the status line, or `null`: it says why an add changed nothing, such as a dropped folder with no images (my call, raised by the builder check; today that message rode on `run-started` with `total: 0`, and a drop no longer starts a run).

```json
{ "files": [
  { "name": "holiday.jpg", "size": 2400000, "skip": null },
  { "name": "logo.png", "size": 88000, "skip": null },
  { "name": "notes.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file" }
], "notice": null }
```

After Clear:

```json
{ "files": [], "notice": null }
```

A dropped folder with no images, onto an empty list:

```json
{ "files": [], "notice": "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)" }
```

`output-changed`: the output setting changed. `folder` is the folder's name, or `null` for next to the originals.

```json
{ "folder": "exports" }
{ "folder": null }
```

`run-started`: now names the settings. `total` counts the rows, as today.

```json
{ "total": 3, "mode": "lossy", "quality": 75 }
{ "total": 3, "mode": "lossless", "quality": null }
```

`file-started` (new): the file at `index` is being compressed now. Exactly one per non-skipped file, before its `file-done`.

```json
{ "index": 1 }
```

`file-done`: as today, plus an optional `note` on a `done` row, a sentence for the user, present only when the file was handled differently from the run's mode (see "Fallbacks").

```json
{ "index": 0, "name": "holiday.jpg", "status": "done", "before": 2400000, "after": 610000, "millis": 1210 }
{ "index": 1, "name": "anim.png", "status": "done", "before": 512000, "after": 498000, "millis": 380, "note": "kept lossless: animated PNG" }
{ "index": 2, "name": "notes.txt", "status": "skipped", "reason": "Not a PNG, JPG, SVG or WebP file" }
```

Departures from today's interface, on purpose: `start_run` is the first command that takes an argument (today none does); `run-started` gains `mode` and `quality`; `file-done` gains an optional `note`; `choose_folder` and the `<folder>/optimized/` output of a dropped folder are gone from the app.

What the shapes share: every list is an array of rows in display order; every user-facing text arrives as a whole sentence; the window adds nothing but formatting. Event order within a run stays strictly `run-started`, then per file `file-started` (non-skipped only) and `file-done`, then `run-finished`.

### Command line

```
lunaepix [--lossy [--quality <0-100>]] [--out <folder>] [--] <file>...
lunaepix [--lossy [--quality <0-100>]] [--out <folder>] --dir <dir>
```

- Without `--lossy`: lossless, exactly as today.
- `--quality` defaults to 75. `--quality` without `--lossy`, a value outside 0–100, a value that is not a whole number, or the option given twice is a usage error. So is `--lossy` or `--out` given twice, or `--out` without a value.
- `--out <folder>`: outputs go there under the output-folder rules above. The folder is created if it does not exist (parents included), like today's `optimized/` folder.
- `--dir` stays, unchanged: without `--out` it writes to `<dir>/optimized/` as today.
- The report shows the mode once at the top ("Lossy, quality 75") and the fallback note after a row that has one.

### First step: measure before building (gate)

The plan's first task is a **hard gate**, as in the WebP work. In a throwaway program the implementer measures and reports, then stops until the user has ruled:

- For animated WebP: wall time and peak memory of decode + lossy re-encode at quality 75, for a few animations, scaled to a total of frames × canvas pixels. From that the implementer proposes a cap that keeps a slow laptop (about three times slower than this machine) well inside `WORKER_TIMEOUT`, and under 4 GB of memory.
- For JPG, PNG and still WebP: wall time and peak memory of the lossy path at `MAX_PIXELS`, flagging anything over 3 minutes on this machine or 4 GB.
- The saving at quality 75 on `samples/png` and `samples/jpg` (and WebP files made from them) per format, so the user sees what lossy gains.

The user sets the cap. The implementer does not pick a cap, a lower format limit or a memory limit on its own.

### What is left to the builder

- The module layout in `crates/core/src`, and how decode, encode and check steps are split so each failure branch can be reached by a test double.
- How mode and quality are passed to the worker (arguments after `WORKER_FLAG`), as long as the worker refuses settings it does not understand.
- The exact quality → quantizr mapping (monotonic, 256 colours at 100).
- How the no-overwrite write is made on each platform.
- How long file names wrap in the list (they must not cause horizontal scrolling).

## Layout

The mock-up [2026-11-09-lunaepix-lossy-mockup.html](2026-11-09-lunaepix-lossy-mockup.html) is **binding** for which blocks exist, their order and where each control sits. Colours, font sizes and spacing follow the existing `ui/app.css` (body text 16 px, nothing below 14 px); the design pass is a later roadmap item.

- The drop area stays at the top; "Choose folder" is gone. With files listed it reads "Drop more images or a folder here".
- One panel below the drop area holds, in this order: the heading line with Clear on the right; the file list as a table; the settings; Run at the bottom right.
- The settings are three rows, label on the left: **Compression** (Lossless / Lossy radio buttons), **Quality** (slider with a number field showing the same value), **Save to** (the folder name or "Next to the originals", a "Choose output folder" button, and when a folder is chosen a "Next to the originals" button to go back).
- With Lossless picked, the Quality row stays visible but cannot be changed.
- While running, the list's columns become File, Before, After, Saved, Time; a row that is waiting or compressing shows that word across the last columns.
- Finished: the settings and Run are not shown; the summary line and "Open folder" sit below the list, as today.
- Empty state: no panel at all.

Accessibility (`~/.claude/a11y-rules.md`):

- Compression is a `<fieldset>` with a `<legend>` and two native radio inputs with labels.
- Quality is a native `<input type="range">` with a `<label>`, paired with an `<input type="number">` (min 0, max 100, step 1) with its own label; changing one updates the other. Disabled (with `disabled`, not only greyed) when Lossless is picked.
- Every action is a `<button>`: Clear, Choose output folder, Next to the originals, Run. Buttons that cannot be used right now stay focusable with `aria-disabled`, as today's buttons do.
- The list is a `<table>` with a `<caption>` and `<th scope>` cells, as today's results table.
- Progress ("Compressing 2 of 3") and messages go through the existing status line (`role="status"`); a per-row "Compressing…" is not announced separately, to avoid noise.
- State is never shown by colour alone: "Waiting", "Compressing…", "Skipped" are words.
- No animation is added; if a spinner is added later it respects `prefers-reduced-motion`.
- Class names follow BBEM (`~/.claude/css-conventions.md`): new blocks such as `lunaepixPanel`, `lunaepixSettings`, with elements like `lunaepixSettings__row` and modifiers as separate `-modifier` classes. `ui/app.css` defines them; the CSP allows no inline styles.

## Tests

Every behaviour gets a positive and a negative test (`~/.claude/rules/unit-testing.md`). Rust: `cargo test --workspace`. Window: `node --test "tests/ui/*.test.js"`. Fixtures are generated in the tests, not committed as binaries. The independent decoder in the checks is the `image` crate (dev-dependency), so a library is never trusted to check itself. CSS is not tested.

### Core library

| Behaviour | Positive | Negative |
|---|---|---|
| Lossless unchanged | a lossless run of the existing fixtures gives byte-identical output to today's (the existing tests stay and pass) | a lossless run never calls the lossy path: a double for the lossy encoder records no call |
| JPG lossy | a photo-like 8-bit RGB JPG at quality 75 comes out smaller, decodes (via `image`) to the same width and height | at quality 100 an already small JPG is not smaller, and the original bytes are written with the "original kept" note |
| JPG markers kept | the output carries the source's APP1 EXIF (with Orientation 6) and APP2 ICC byte for byte | a source without EXIF gains none; a source's Adobe APP14 (transform 0) is not in the output, and the output decodes (via `image`) to colours close to the source's |
| JPG CMYK fallback | a CMYK JPG in a lossy run gets the lossless output and the note | an RGB JPG in the same run is encoded lossy (its output differs from the lossless output) |
| Quality order | for JPG, PNG and WebP, quality 30 gives a smaller file than quality 90 on the same photo-like fixture | quality 90 does not give a smaller file than 30 |
| PNG lossy | an RGBA PNG with a gradient comes out smaller, as an indexed PNG of the same size | an already-indexed 16-colour PNG is not smaller; the original is written with the note |
| PNG transparency | a PNG with fully and half transparent pixels keeps them: every pixel that was alpha 0 is still alpha 0, and alpha 128 stays within a small tolerance | the fixture's own assertion that it really has alpha 0 and alpha 128 pixels |
| PNG quality mapping | quality 100 gives a 256-entry palette for an image with more than 256 colours | quality 0 does not give more colours than quality 50 |
| APNG fallback | a PNG with `acTL` in a lossy run gets the lossless output and the note | a still PNG in the same run is encoded lossy |
| WebP still lossy | a lossless WebP fixture becomes a lossy (`VP8 `) WebP, smaller, same size; one with alpha keeps an alpha channel | an already small lossy WebP at quality 100 keeps the original, with the note |
| WebP metadata | `ICCP` kept byte for byte; `EXIF` with Orientation 6 kept; `XMP ` dropped | `EXIF` with Orientation 1 dropped; a source without `ICCP` gains none |
| WebP animated | an animated fixture of distinct frames comes out lossy with the same canvas size, loop count, frame count and frame durations; a fixture with two identical frames in a row comes out with one frame fewer and the same total duration | an animation over the cap gets the lossless output and the note, and the animation decoder is never called (a double records nothing) |
| Animation cap | the cap is checked from the headers: a file declaring frames × canvas just under the cap is decoded | just over is not |
| Lossy check | the check accepts an output with the source's size (and, for an animation, the source's total duration, also with fewer frames) | a double encoder that returns a different size makes the file fail with the reason, and no output is written; same for a different total duration and for an output that does not decode |
| Encoder failure | the same double returning a valid result gets that result written | a double encoder that returns an error makes the file fail with a clear reason, no output written, and the run goes on with the next file |
| SVG in a lossy run | an SVG gets the lossless output, no note | the lossy encoder double records no call for it |
| Pixel limit | a JPG, PNG and WebP declaring more than `MAX_PIXELS` are refused as "image too large" before decoding, in a lossy run | a file within the limit is processed |
| Hostile input (`hostile_input.rs`) | a corrupt JPG, a truncated PNG, a WebP with bad chunks and an animation with a frame running off the canvas each fail with a reason in a lossy run, no panic, within 5 s | a valid file within the limits passes |
| Worker settings | the worker accepts `lossy` with 0, 75 and 100 | it refuses a missing quality, -1, 101, `75.5`, `abc` and an unknown mode, and writes nothing |

### Output folder

| Behaviour | Positive | Negative |
|---|---|---|
| Original names | `a.png` and `b.jpg` land as `a.png` and `b.jpg` in the folder | no `optimized-` prefix appears |
| Clash with an existing file | with `logo.png` already there, the output is `logo-2.png`, and the existing file's bytes are unchanged | with no clash, no number is added |
| Clash within a run | two sources named `logo.png` from different folders give `logo.png` and `logo-2.png` | two sources with different names get no number |
| Case-insensitive | with `Logo.PNG` there, `logo.png` becomes `logo-2.png` | `logo.png` and `logo.jpg` do not clash |
| File appears during the run | a file created at the target name between planning and writing is not overwritten; the output takes the next number (reached with a double for the write step) | when nothing appears, the planned name is used |
| Originals' own folder | choosing the source folder writes `photo-2.jpg` and leaves `photo.jpg` byte-identical | choosing another folder writes `photo.jpg` there, without a number |
| No extension | `README` clashes to `README-2` | without a clash it stays `README` |
| Next to the originals | without an output folder, `optimized-<name>` beside the original, replacing an existing one, as today | nothing is written outside the original's folder, and no number is added |
| Folder gone | a run whose output folder was deleted after choosing does not start (`"output-missing"`) | an existing writable folder starts the run |

### App backend (`src-tauri`)

| Behaviour | Positive | Negative |
|---|---|---|
| Adding | a drop of three files sends `list-changed` with three rows | the same file dropped twice is listed once |
| Dropped folder | its PNG/JPG/SVG/WebP files directly inside are added, `optimized-a.jpg` next to `a.jpg` included; a subfolder's files are not | a `notes.txt` in the folder is not listed; a folder with no images adds nothing and sends `list-changed` with the `notice` |
| Settings validation | `start_run` with `{ "mode": "lossy", "quality": 0 }` and `100` starts a run | `"invalid-settings"` for `-1`, `101`, `75.5`, `"75"`, `null`, a missing key, an extra key, mode `"fast"`; no run starts |
| Empty list | a list with one compressible file starts | `start_run` on an empty list, or a list of only skipped files, rejects with `"empty-list"` |
| Busy | after `run-finished`, every command works again | every command except `open_output_folder` rejects with `"busy"` during a run |
| Event order | per file: `file-started` before `file-done`; none for skipped rows | no `file-started` for a skipped row |
| Fresh list | the first add after `run-finished` sends a `list-changed` with only the new files | the old rows are not in it |
| No paths | no event payload contains a path separator from the test's temporary folder | the test's own check finds one when given a payload that contains a path |
| Permissions | `capabilities/default.json` lists exactly the new commands and the two event permissions | `choose_folder` is not in it |

### Command line (`crates/cli`)

| Behaviour | Positive | Negative |
|---|---|---|
| Parsing | `--lossy`, `--lossy --quality 0`, `--quality 100`, `--out x`, with files or `--dir` | usage error for `--quality 50` without `--lossy`, `--quality 101`, `-1`, `7.5`, `abc`, `--quality` twice, `--lossy` twice, `--out` twice, `--out` without a value |
| Default | `--lossy` alone uses 75 | without `--lossy`, the output is byte-identical to today's lossless |
| `--out` | creates a missing folder (with parents) and writes original names there | an existing file there is never overwritten |
| `--dir` | without `--out`, writes to `<dir>/optimized/` as today | with `--out`, nothing is written to `<dir>/optimized/` |

### Window (`tests/ui/*.test.js`, `ui/state.js`)

| Behaviour | Positive | Negative |
|---|---|---|
| Launch state | mode lossless, quality 75, no output folder, no panel | quality cannot be changed while lossless |
| List | `list-changed` fills the rows; a `notice` is written to the status line | an empty list hides the panel; `notice: null` leaves the status line as it was |
| Run allowed | Run can be used with at least one non-skipped row | not with an empty list or only skipped rows, and not while running |
| Running rows | `file-started` marks that row "Compressing…", later rows "Waiting" | a `file-done` row never goes back to "Compressing…" |
| Settings sent | pressing Run sends exactly `{ mode, quality }` with the current values, quality as an integer | a number field holding text, a fraction or a value outside 0–100 blocks Run and nothing is sent |
| Slider and number | moving one updates the other | an invalid number does not move the slider |
| Heading | "Compressing 2 of 3 · Lossy 75" while running, "Done · Lossy 75" after, "· Lossless" for a lossless run | a lossless run never shows a quality number |
| Note | a `file-done` with a `note` shows it on that row | a row without a note shows none |
| Fresh list | after `run-finished`, a `list-changed` replaces the old rows | before `run-finished`, an add is refused (busy) and the rows stay |
| Text only | a name containing `<img>` shows as text | that name creates no element in the page |

### Edge cases the checks cover

- Quality at its bounds, 0 and 100, in the core, the worker, `start_run` and the CLI.
- The output folder present and absent in every output test; the "Save to" row in both states in the window tests.
- More rows than the mock-up: a list of 2000 files plus the over-limit row; a file name of 200 characters.

## CMS changes

No CMS changes. (Lunaepix is not a Jugoo site.)
