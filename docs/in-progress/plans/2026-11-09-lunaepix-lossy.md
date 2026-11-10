# Lunaepix lossy compression — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-09

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each run is lossless or lossy, with a quality from 0 to 100, for JPG, PNG and WebP (still and animated). Adding files and pressing Run are separate steps. Output can go to a chosen folder. The command line gets the same options.

**Architecture:**
- **Core library.** Each format gets a pure `optimize_lossy(bytes, quality)` next to its lossless `optimize`. They share one `lossy` module for the quality type, the notes, the "same shape" check and the "never larger" rule.
- **Worker.** The settings travel to the worker process as arguments, and the result record moves to version 2 to carry a note.
- **Output folder.** The planner learns about the output folder and names clashing files with numbers. A no-replace write makes sure no existing file is overwritten.
- **App.** The backend keeps the file list and the output folder, and runs on `start_run`. The window becomes the one-panel layout from the binding mock-up.

**Tech Stack:**
- Rust 2024 (MSRV 1.90).
- `mozjpeg-sys =2.2.3`, with its NASM SIMD feature turned on.
- `libwebp-sys =0.14.4`, moved into the product.
- `quantizr =1.4.3` (new).
- The `png` crate, moved into the product to decode and to write indexed PNGs.
- `oxipng =10.2.1`.
- Tauri 2, vanilla JS (`node --test`), cargo-about.

**Spec:** [2026-11-09-lunaepix-lossy-design.md](../../in-progress/specs/2026-11-09-lunaepix-lossy-design.md) and its binding mock-up [2026-11-09-lunaepix-lossy-mockup.html](../../in-progress/specs/2026-11-09-lunaepix-lossy-mockup.html). Read the spec's Handoff before anything else.

## Handoff

Copied verbatim from the spec's Handoff, plus what was decided while planning. Give every implementer these lines as they are. An implementer that wants to deviate from one stops and asks instead of choosing.

- **Plan approved by the user (2026-11-09):** "approved", including the calls made while planning (listed below). Execute in a fresh session, subagent-driven, on `feature/lunaepix-lossy`. `nasm` 3.02 is installed.
- **Spec approved by the user (2026-11-09):** "approved". Branch `feature/lunaepix-lossy`, from `main` at `c3e38e4`.
- Roadmap item started: "Lossy image compression".
- **Formats and the choice:** "Webp and jpg, but we do want them in general. A choice for people, to choose between lossless and lossy. Lossy must support a quality."
- **PNG included:** "But yes, png as well."
- **Quality setting is part of this feature**, not a separate roadmap item: "Why not. Make them at once."
- **PNG library:** "Check others", then "Transparency needs to be supported." quantette is ruled out; quantizr is the pick.
- **JPG speed-up:** "Can't we add the speed-up code?" Yes: mozjpeg's SIMD code is turned on; `nasm` becomes a build requirement.
- **libwebp in the product:** "Fine".
- **Run button:** "Incorrect, it will only run when pressing run."
- **Two checks:** "Correct, there is a lossless that checks the no pixes changed, and there is a lossy one for the new changes".
- **One setting per run:** "One setting for the batch job."
- **Lossy behaviour per format** (play-back confirmed with "Correct."): result must open as an image of the same size; not smaller → original copied; kept details as lossless today; SVG gets the lossless treatment in a lossy run.
- **Animated WebP:** "Yes" to including it in this feature, with a cap on frames × pixels.
- **No memory between launches:** "Start fresh".
- **Starting quality:** "75 is fine."
- **Layout:** option B, selected in the browser; the mock-up is binding.
- **While running:** "It starts running, showing which files are done and which are currently in progress."
- **Folders:** "No longer select an entire folder." Rest of the screen play-back: "Yes is correct".
- **A dropped folder:** "Add the images inside it as loose files."
- **Output folder:** "Also, give an option for an output folder."
- **Names in the output folder:** "Original name, add a number when names clash".
- **Command line:** "Yes, it should get parameters for that".
- **Earlier outputs in a dropped folder:** "No" to leaving `optimized-*` files out; they are listed like any other image.
- Calls made after the spec's builder check (not user decisions): the animation check compares canvas size and total duration, not frame count. `list-changed` carries an optional `notice`. The lossy JPG drops the source's JFIF APP0 and Adobe APP14.
- Put off: the before/after preview slider, export rename, the "Clean and clear UI" design pass.
- **Task 1 is a hard gate.** No product code is written before the user has ruled on its numbers and set the animation cap.
- **Task 1 ruling by the user (2026-11-09):** for the cap the user chose "1,000,000,000 (Recommended)", so `MAX_ANIMATION_PIXELS` = 1,000,000,000 frames × canvas pixels (time binds: worst measured 171 ms per million, with alpha). Asked whether lossy animated WebP also gets a canvas limit, because its memory grows with the canvas (about 50 MB per megapixel with alpha, about 5 GB at 100 MP), the user chose "Yes, 50 MP (Recommended)", so `MAX_ANIMATION_CANVAS_PIXELS` = 50,000,000 canvas pixels. An animation over either limit gets the lossless treatment with the existing note "kept lossless: animation too large for lossy"; no new note. Measurements: `.superpowers/sdd/2026-11-09-lunaepix-lossy/task-1-report.md`.
- **Lossy WebP EXIF flag (user, 2026-11-10):** asked what lossy should do for a WebP without `VP8X` that carries a rotated `EXIF` (readers ignore it there; lossy output adds `VP8X` and would set the EXIF flag), the user chose "Keep source's flag (Recommended)": the `EXIF` chunk is kept, but its flag stays clear for a source that had no `VP8X`, so it renders exactly as before. Ruled out: "Flag matches chunk".
- **16384-wide WebP in a lossy run (user, 2026-11-10):** a valid lossless WebP 16384 px wide cannot be encoded lossy (libwebp's limit is 16383). The user chose "Clearer error (Recommended)": check the width and height before decoding and fail with a plain reason naming the 16383 px limit. No new note. Ruled out: "Lossless with a note", "Leave as is".
- **Test remedy (user, 2026-11-10):** after tests written after their code (Tasks 4, 6) and negatives that cannot fail on their own (Tasks 3, 5) recurred, the user chose "Controller RED audit (Recommended)": before each review the controller compares the RED run's test list with the commit's final tests, and a test missing from it goes back as a concern before review; the test table gets a "fails on its own because…" column. Ruled out: "Late tests become concerns", "Accept with reviewer check".
- **Lossy PNG validation (user, 2026-11-10):** after the lossy PNG path (lenient `png` crate plus hand-written checks) twice accepted damaged files the lossless path refuses (a bad-CRC chunk in Task 4; image data longer than the IHDR in Task 8a, whose size check a crafted IDAT split then bypassed), the user chose "oxipng parses first (Recommended)": before decoding, the lossy path runs oxipng's own parser (`oxipng::internal_tests::PngData::from_slice`, doc-hidden, oxipng pinned at `=10.2.1`) and refuses whatever it refuses, with its message; the hand-written CRC and image-size pre-checks go. Ruled out: "Patch + comparison test", "Lossless decides first".
- **A PNG oxipng accepts but the `png` decoder refuses, in a lossy run (user, 2026-11-10):** for example a PNG with two `tRNS` chunks (lossless optimises it; the decoder's error even names the wrong chunk, "Chunk PLTE must appear at most once"). The user chose "Refuse, clearer message": the lossy run keeps refusing such a file, with the reason worded "could not decode the PNG for lossy: …". No new note. Ruled out: "Lossless with a note", "Refuse as now".

Calls made while planning (not user decisions; flagged for the plan review):

- `start_run` always carries an integer quality 0–100, also for a lossless run, where it is ignored. The window always sends its current quality (75 unless changed). This keeps the one validation rule from the spec ("quality not an integer in 0–100" is invalid) without a lossless exception. `run-started` still reports `quality: null` for a lossless run, as the spec's example shows.
- The worker protocol goes to record version 2 (`lunaepix-worker 2 …`). The runner and the worker always come from the same build, so the version number guards against a stale sidecar rather than keeping old workers working.
- The final output name is not reported back by the worker. Open folder only needs the folder, and the CLI prints the source name as today.
- Numbering tries at most `MAX_NAME_NUMBER` = 9999 names (`logo-2` … `logo-9999`). Past that, the file fails with "no free name in the output folder". This bound is required by security.md (explicit upper bounds on loops).

## Global Constraints

- Lossless runs stay byte-identical to today, including all existing tests. A lossless run never calls a lossy path.
- The original is never modified, and nothing existing in a chosen output folder is overwritten.
- A lossy output is never larger than its original. When it would be, the original bytes are written with the note "original kept: lossy was not smaller".
- Quality is an integer 0–100. It starts at 75 at every launch, and the app starts on Lossless with the output next to the originals.
- Per-file limits are unchanged: `MAX_FILE_BYTES` 100 MB, `MAX_PIXELS` 100 megapixels (checked from the header before decoding), `WORKER_TIMEOUT` 10 minutes, `MAX_RUN_FILES` 2000. The animated-WebP cap comes on top, with its value set at the Task 1 gate.
- Exact user-facing notes:
  - "kept lossless: animated PNG"
  - "kept lossless: CMYK JPG"
  - "kept lossless: animation too large for lossy"
  - "original kept: lossy was not smaller"
- The window never sends or receives a path. The output folder reaches it as its folder name only.
- Licences stay within `about.toml`'s `accepted` list, and the list is never extended to make a run pass.
- `unsafe` is allowed only in `jpg`, `platform` and the new libwebp FFI module, and the crate doc in `lib.rs` names all three.
- Structured files (TOML, JSON, YAML, HTML) are checked with a parser in tests, never as text.
- CSP stays as it is: no inline styles or scripts, and every text goes in through `textContent`. Class names follow BBEM.
- Accessibility follows `~/.claude/a11y-rules.md`:
  - Compression is a `fieldset` with a `legend`.
  - Quality is a labelled range input plus a labelled number input.
  - Every action is a `<button>`.
  - The list table keeps its `caption`.
  - Text is 14 px minimum.
- Every behaviour has a positive and a negative test, and every failure branch is reached by a test, through a double where needed.

## Review Focus

1. **A greyscale JPG in a lossy run.** It should stay greyscale (one component), not grow into a three-channel colour file. The test is in Task 3.
2. **An already-indexed PNG with transparency (`PLTE` + `tRNS`).** It should keep its transparent pixels after lossy, and not fall back to opaque black. The test is in Task 4.
3. **An output folder that exists but cannot be written to.** `start_run` should refuse it with `"output-missing"` before any file runs, instead of failing every row. The test is in Task 10.
4. **Non-ASCII and non-UTF-8 names in the output folder.** `Mobilis Logo – zwart é.png` should clash with `MOBILIS LOGO – ZWART É.PNG` and become `Mobilis Logo – zwart é-2.png`. A non-UTF-8 name should still get its number without being mangled. The test is in Task 8.
5. **A listed file deleted before Run is pressed.** The run should show that file as skipped ("Not a readable file") and still run the rest. The test is in Task 10.

---

## Before Task 1 (the user, not an implementer)

The user installs `nasm` (`sudo dnf install nasm`); done on 2026-11-09. Task 1 still starts by checking `command -v nasm` and stops if it is missing.

### Task 1: Measure, then stop (hard gate)

**Files:** none in the repository. A throwaway Cargo project goes in `.superpowers/sdd/2026-11-09-lunaepix-lossy/measure/`, and the report in `.superpowers/sdd/2026-11-09-lunaepix-lossy/task-1-report.md`. Both are gitignored scratch.

- [ ] **Step 1:** Check `command -v nasm`. Stop and report if it is missing.
- [ ] **Step 2:** Build the throwaway measurer:
  - Dependencies: `mozjpeg-sys =2.2.3` with `nasm_simd`, `libwebp-sys =0.14.4`, `quantizr =1.4.3`, `png`, `oxipng =10.2.1`.
  - It runs each lossy path the spec describes at quality 75.
  - JPG: decode, encode at the quality.
  - PNG: decode, then quantizr to 256 colours with dithering, then indexed PNG, then oxipng.
  - Still WebP: decode, then `WebPEncodeRGBA`.
  - Animated WebP: `WebPAnimDecoder`, then `WebPAnimEncoder` with lossy config.
- [ ] **Step 3:** Measure and write into the report:
  - The saving per file and in total at quality 75, for `samples/jpg`, `samples/png` and WebP files made from them.
  - The wall time and peak memory (`/usr/bin/time -v`) at about 25 megapixels for JPG, PNG and still WebP, scaled to `MAX_PIXELS`. Flag anything over 3 minutes or 4 GB.
  - For animated WebP: the time and peak memory for at least three animations of different canvas sizes and frame counts, expressed per million (frames × canvas pixels).
  - The proposed cap: the largest frames × canvas pixels whose scaled time is ≤ 3 minutes on this machine (so ≤ about 9 minutes on a laptop three times slower, within `WORKER_TIMEOUT`) and whose peak memory is ≤ 4 GB.
- [ ] **Step 4:** Stop. Report the numbers and the proposed cap to the user. Do not pick a cap, a format limit or a memory limit yourself. Record the user's ruling as a new line in this plan's Handoff before Task 2 starts, with the cap as `MAX_ANIMATION_PIXELS`.

### Task 2: The shared lossy core

**Files:**
- Create: `crates/core/src/lossy.rs`
- Modify: `crates/core/src/lib.rs` (module, re-exports, `Outcome` gains `note`)
- Test: unit tests in `lossy.rs`

**Interfaces:**
- Produces:
  - `pub struct Quality(u8)`, with:
    - `Quality::new(u8) -> Option<Quality>` (`None` above 100)
    - `Quality::parse(&str) -> Option<Quality>`: ASCII digits only, no sign, no fraction, no spaces, 0–100.
    - `Quality::get(self) -> u8`
    - `pub const DEFAULT_QUALITY: Quality` = 75
  - `pub enum Note { KeptLosslessAnimatedPng, KeptLosslessCmyk, KeptLosslessAnimationTooLarge, OriginalKept }`. `Display` gives the four exact notes from Global Constraints. `Note::code(self) -> &'static str` and `Note::from_code(&str) -> Option<Note>` serve the worker record. The codes are `apng`, `cmyk`, `anim-too-large`, `original-kept`.
  - `pub(crate) struct Shape { width: u32, height: u32, duration_ms: Option<u64> }`. `duration_ms` is `Some(total)` for an animation only.
  - `pub(crate) fn check_shape(source: Shape, output: Shape) -> Result<(), String>`. On a mismatch, the message names what differs, for example "the lossy result has a different size (100×80, expected 100×90)".
  - `pub(crate) struct Lossy { bytes: Vec<u8>, note: Option<Note> }`
  - `pub(crate) fn finish(original: &[u8], candidate: Vec<u8>) -> Lossy`: the candidate when it is strictly smaller, otherwise the original bytes with `Some(Note::OriginalKept)`.
  - `pub(crate) fn kept_lossless(bytes: Vec<u8>, note: Note) -> Lossy`
  - `Outcome { before, after, millis, note: Option<Note> }`. Every existing constructor passes `note: None`.

- [ ] **Step 1: Write the test and failure-branch tables** (see "Implementer report" at the end). Then write the failing tests:
  - `quality_accepts_0_75_and_100`
  - `quality_rejects_101_and_text_signs_fractions_and_spaces`, covering `"101"`, `"-1"`, `"+5"`, `"7.5"`, `"abc"`, `""`, `" 75"` and `"1000"`
  - `notes_read_as_the_exact_sentences` and `note_codes_round_trip`, plus `an_unknown_note_code_is_none`
  - `check_shape_accepts_the_same_size_and_duration`
  - `check_shape_rejects_another_width_height_or_duration`
  - `check_shape_rejects_a_still_output_for_an_animation`: `duration_ms` is `None` against `Some`, and the other way round
  - `finish_keeps_a_smaller_candidate_without_note`
  - `finish_keeps_the_original_with_the_note_when_the_candidate_is_equal_or_larger`, at equal size and at larger size
- [ ] **Step 2:** Run `cargo test -p lunaepix-core lossy` and expect it to fail (the module does not exist yet). Record the output for the report.
- [ ] **Step 3:** Implement `lossy.rs`, add `note` to `Outcome`, and update every `Outcome { … }` in the workspace.
- [ ] **Step 4:** Run `cargo test --workspace` and expect everything to pass.
- [ ] **Step 5:** Commit: "Add the shared lossy core: quality, notes, shape check".

### Task 3: Lossy JPG (and the SIMD speed-up)

**Files:**
- Modify: `crates/core/Cargo.toml` (add the `nasm_simd` feature to `mozjpeg-sys`, and update the comment that says SIMD is not needed)
- Create: `crates/core/src/jpg/lossy.rs`, a child module of `jpg` that reuses its guards, error manager and `run_libjpeg`
- Modify: `crates/core/src/jpg.rs` (`mod lossy;` and `pub(crate) use lossy::optimize_lossy;`)
- Modify: `.github/workflows/windows.yml` (install nasm in both jobs), `src-tauri/tests/workflow.rs` (only if a rule check needs to know about the new step)
- Test: `crates/core/tests/optimize_jpg.rs` and unit tests in `jpg/lossy.rs`

**Interfaces:**
- Consumes `Quality`, `Note`, `Shape`, `check_shape`, `finish` and `kept_lossless` from Task 2.
- Produces `pub(crate) fn optimize_lossy(bytes: &[u8], quality: Quality) -> Result<Lossy, String>`, built on `fn optimize_lossy_with(bytes, quality, encode: impl FnOnce(&Decoded, Quality) -> Result<Vec<u8>, String>) -> Result<Lossy, String>`. The real encoder is one argument, and the tests pass doubles for the others.

**Behaviour:**
- It checks pixels from the header with `plan::check_pixels`, the same way the lossless path does.
- A source whose colour space is not YCbCr/RGB or greyscale (CMYK, YCCK) returns `kept_lossless(jpg::optimize(bytes)?, Note::KeptLosslessCmyk)`.
- **Decoding:** RGB or greyscale scanlines.
- **Encoding:**
  - `jpeg_set_defaults`, then `jpeg_set_quality(quality, TRUE)`, with mozjpeg's default profile, so it is progressive with optimised tables.
  - Greyscale stays greyscale.
  - Same `MAX_LIBJPEG_MEMORY` cap.
- **Markers:**
  - `jpeg_save_markers` for APP0–APP15 and COM. Each saved marker is written after `jpeg_start_compress` with `jpeg_write_marker`, in source order.
  - The source's JFIF APP0 (`JFIF\0` identifier) and Adobe APP14 (`Adobe` identifier) are not written.
- **Shape check:** decode the result header and run `check_shape`.
- Then `finish(bytes, result)`.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `a_photo_jpg_gets_smaller_at_75_and_keeps_its_size`. Decode with `image` and compare width and height.
  - `an_already_small_jpg_at_100_keeps_the_original_with_the_note`
  - `exif_with_orientation_6_and_icc_are_kept_byte_for_byte`
  - `a_source_without_exif_gains_none`
  - `an_adobe_app14_with_transform_0_is_not_copied_and_colours_stay_close`. The mean absolute difference per channel against the source decode stays under 8.
  - `a_greyscale_jpg_stays_one_component` (Review Focus 1)
  - `a_cmyk_jpg_is_kept_lossless_with_the_note`. The output equals `jpg::optimize`'s output.
  - `an_rgb_jpg_in_the_same_run_is_encoded_lossy`. The output differs from the lossless output.
  - `quality_30_is_smaller_than_quality_90`, and its negative `quality_90_is_not_smaller_than_quality_30`
  - `quality_0_gives_a_valid_jpg_of_the_same_size`
  - `an_encoder_double_returning_a_different_size_fails_with_the_reason`
  - `an_encoder_double_returning_garbage_fails_as_not_decodable`
  - `an_encoder_double_returning_an_error_fails_with_its_reason`
  - `the_same_double_returning_a_valid_jpg_gets_it_back`
  - `a_header_over_max_pixels_is_refused_before_decoding`, within 5 s
  - `a_corrupt_jpg_fails_with_a_reason_without_panic`. This one goes in `hostile_input.rs`, in its lossy section.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core jpg` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement. Turn on `nasm_simd`. In `windows.yml`, add a step before the cargo steps of both jobs that installs NASM with `choco install nasm --no-progress -y` and appends `C:\Program Files\NASM` to `$GITHUB_PATH`. The step uses no `${{ }}` and no new action, so `workflow.rs` keeps passing.
- [ ] **Step 4:** Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`, and expect both to pass.
- [ ] **Step 5:** Commit: "Add lossy JPG with mozjpeg's SIMD build".

### Task 4: Lossy PNG

**Files:**
- Modify: `crates/core/Cargo.toml`. Add `quantizr = "=1.4.3"`. Move `png = "0.17.16"` into `[dependencies]`; the version stays.
- Create: `crates/core/src/png/lossy.rs`
- Modify: `crates/core/src/png.rs` (`mod lossy;` and the re-export; `check_chunks` also reports whether an `acTL` chunk is present)
- Test: `crates/core/tests/optimize_png.rs` and unit tests

**Interfaces:**
- Produces `pub(crate) fn optimize_lossy(bytes: &[u8], quality: Quality) -> Result<Lossy, String>`, built on a `_with` variant whose quantise step can be replaced by a test double.

**Behaviour:**
- Chunks are checked and pixels checked from IHDR before decoding, as the lossless path does.
- An `acTL` chunk returns `kept_lossless(png::optimize(bytes)?, Note::KeptLosslessAnimatedPng)`.
- **Decoding:** RGBA8. Expand palette, `tRNS` and greyscale, and reduce 16-bit to 8-bit.
- **Quantising:** quantizr with `max_colors = colors_for(quality)` and dithering level 1.0.
- **Colour mapping:** `fn colors_for(q: Quality) -> i32` is monotonic, with 256 at 100 and at least 2 at 0. The builder picks the curve and documents it in a doc comment.
- **Writing:** an indexed PNG (`PLTE` plus `tRNS` when any entry has alpha < 255), at the smallest bit depth that holds the palette.
- **Chunks carried over:** the source's ancillary chunks before IDAT, re-inserted so that oxipng's `StripChunks::Safe` can keep or drop them exactly as it does today. That covers at least `iCCP`, `sRGB`, `gAMA`, `cHRM`, `cICP` and `pHYs`.
- Then oxipng with today's options (reuse `png::run_oxipng`).
- **Shape check:** from the output's IHDR.
- Then `finish`.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `a_gradient_rgba_png_gets_smaller_as_indexed_with_the_same_size`
  - `an_indexed_16_colour_png_keeps_the_original_with_the_note`
  - `alpha_0_stays_0_and_alpha_128_stays_within_8`. Include the fixture self-check `the_fixture_really_has_alpha_0_and_128`.
  - `an_indexed_png_with_trns_keeps_its_transparent_pixels` (Review Focus 2)
  - `quality_100_gives_256_colours_for_a_many_colour_image`
  - `quality_0_gives_no_more_colours_than_quality_50`
  - `colors_for_is_monotonic_over_0_to_100`, and its negative `colors_for_never_exceeds_256_or_drops_below_2`
  - `an_icc_profile_is_kept`, and its negative `a_source_without_iccp_gains_none`
  - `an_apng_is_kept_lossless_with_the_note`, and its negative `a_still_png_is_encoded_lossy`
  - `a_16_bit_png_is_reduced_and_gets_smaller`
  - `a_quantiser_double_returning_an_error_fails_with_its_reason`
  - `a_quantiser_double_returning_a_wrong_size_buffer_fails`
  - `a_valid_double_result_is_written`
  - Over `MAX_PIXELS` refused before decoding, within 5 s.
  - A truncated PNG in a lossy run fails without panic. This one goes in `hostile_input.rs`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core png` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy, and expect both to pass.
- [ ] **Step 5:** Commit: "Add lossy PNG with quantizr".

### Task 5: Lossy still WebP

**Files:**
- Modify: `crates/core/Cargo.toml` (move `libwebp-sys = "=0.14.4"` into `[dependencies]`)
- Create: `crates/core/src/webp/codec.rs`. This is the libwebp FFI, `#[allow(unsafe_code)]`, return codes only.
- Create: `crates/core/src/webp/lossy.rs`
- Modify: `crates/core/src/webp.rs`, `crates/core/src/webp/riff.rs` (an assembly function), `crates/core/src/lib.rs` (crate doc names the `webp::codec` unsafe module)
- Test: `crates/core/tests/optimize_webp.rs` and unit tests

**Interfaces:**
- Produces:
  - `pub(crate) fn optimize_lossy(bytes: &[u8], quality: Quality) -> Result<Lossy, String>`. It routes animated files to Task 6's `optimize_animated_lossy`; until Task 6 lands, that function returns lossless with `KeptLosslessAnimationTooLarge`. That is a stub between two commits on the branch, never released, and Task 6 replaces it.
  - A `trait Codec { fn decode_rgba(&self, bytes: &[u8]) -> Result<Rgba, String>; fn encode_lossy(&self, rgba: &Rgba, quality: Quality) -> Result<Vec<u8>, String>; }`, with `LibWebp` as the real implementation. The tests use doubles.
  - `riff::with_metadata(encoded: &[u8], source: &WebpFile) -> Result<Vec<u8>, String>`. It takes libwebp's simple or VP8X+ALPH output, adds the source's `ICCP`, and adds its `EXIF` only under today's orientation rule (the same predicate `stripped()` uses). `XMP ` is never added. It writes `VP8X` with flags matching the chunks, keeping the source's ICC flag rule from the WebP Handoff, and rewrites the RIFF size.

**Behaviour:**
- Header pixel check from today's `riff::parse`.
- Decode, then encode lossy at the quality. libwebp's default alpha quality is 100.
- `with_metadata`, then the shape check (`WebPGetInfo`), then `finish`.
- Lossless and lossy sources both go this way.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `a_lossless_webp_becomes_a_smaller_vp8_with_the_same_size`
  - `an_alpha_webp_keeps_an_alpha_channel`
  - `a_small_lossy_webp_at_100_keeps_the_original_with_the_note`
  - `iccp_kept_byte_for_byte`
  - `exif_orientation_6_kept`
  - `xmp_dropped`
  - `exif_orientation_1_dropped`
  - `a_source_without_iccp_gains_none`
  - `vp8x_flags_match_the_chunks_written`
  - `quality_30_is_smaller_than_quality_90`, plus its negative
  - `quality_0_gives_a_valid_webp_of_the_same_size`
  - Codec doubles: a decode error, an encode error, an encode returning another size, an encode returning bytes that are not WebP, and a valid result. Each fails or succeeds as the core tests in Task 3 do.
  - Over `MAX_PIXELS` refused before decoding.
  - In `hostile_input.rs`: the existing hostile WebP cases run in a lossy run too.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core webp` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy, and expect both to pass.
- [ ] **Step 5:** Commit: "Add lossy still WebP through libwebp".

### Task 6: Lossy animated WebP

**Files:**
- Modify: `crates/core/src/webp/codec.rs` (`WebPAnimDecoder`, `WebPAnimEncoder`), `crates/core/src/webp/lossy.rs`, `crates/core/src/plan.rs` (`pub const MAX_ANIMATION_PIXELS: u64 = 1_000_000_000` and `pub const MAX_ANIMATION_CANVAS_PIXELS: u64 = 50_000_000`, the values the user set at Task 1, both re-exported from `lib.rs`)
- Test: `crates/core/tests/optimize_webp.rs`

**Interfaces:**
- Produces `pub(crate) fn optimize_animated_lossy(bytes: &[u8], file: &WebpFile, quality: Quality) -> Result<Lossy, String>`. The `Codec` trait gains `decode_animation` and `encode_animation`, with a frame type that holds RGBA plus `timestamp_ms`, so tests can use doubles.

**Behaviour:**
- **Cap check before decoding:** number of `ANMF` chunks × canvas width × height from `VP8X`, read by `riff::parse`, against `MAX_ANIMATION_PIXELS`; and canvas width × height against `MAX_ANIMATION_CANVAS_PIXELS`. Over either gives `kept_lossless(webp::optimize(bytes)?, Note::KeptLosslessAnimationTooLarge)`, and no decoder is called.
- **Decoding:** every frame as a full canvas with its timestamp.
- **Encoding:** `WebPAnimEncoderOptions`, with:
  - loop count and background colour from the source `ANIM`;
  - `allow_mixed = 0`;
  - a lossy `WebPConfig` at the quality.
- Each frame is added at its timestamp, and the end timestamp is the source's total duration.
- `with_metadata` (animated `VP8X` flag kept).
- **Shape check:** canvas width and height plus total duration (the last frame's end timestamp). The frame count is not compared.
- Then `finish`.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `distinct_frames_keep_canvas_loop_count_frame_count_and_durations`
  - `two_identical_frames_in_a_row_merge_into_one_with_the_same_total_duration`
  - `an_animation_just_over_the_cap_is_kept_lossless_with_the_note_and_never_decoded`. A double records no decode call.
  - `an_animation_just_under_the_cap_is_decoded`
  - `a_canvas_just_over_50_mp_is_kept_lossless_with_the_note_and_never_decoded`, and its negative `a_canvas_of_exactly_50_mp_within_the_frame_cap_is_decoded`
  - `an_encode_double_with_another_total_duration_fails_with_the_reason`
  - `an_encode_double_with_another_canvas_fails`
  - `a_decode_error_and_an_encode_error_fail_with_their_reasons`
  - `iccp_kept_and_xmp_dropped_on_an_animation`
  - In `hostile_input.rs`: `an_animation_with_a_frame_running_off_the_canvas_fails_without_panic_within_5_s`
- [ ] **Step 2:** Run `cargo test -p lunaepix-core webp` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy, and expect both to pass.
- [ ] **Step 5:** Commit: "Add lossy animated WebP".

### Task 7: Licences for the new product libraries

**Files:**
- Modify: `about.toml`, `about.hbs`, `THIRD-PARTY-LICENSES.html` (regenerated), `README.md` (licence section), `src-tauri/tests/config.rs` (if its checks cover the clarifications)

**Behaviour:**
- **`libwebp-sys`:** clarified as `MIT AND BSD-3-Clause`. The BSD part comes from `vendor/COPYING` with SHA-256 `5aec868f669e384a22372a4e8a1a6cd7d44c64cd451f960ca69cc170d1e13acf`, and the MIT part uses cargo-about's standard text.
- **Google's `PATENTS`:** a fixed block in `about.hbs`, with a comment giving its SHA-256 `cc3273e0694ea5896145e0677699b53471b03ea43021ddc50e7923fbb9f5023c`. This follows `docs/finished/specs/2026-11-06-lunaepix-webp-design.md`, "Licence notice".
- **`quantizr`:** clarified as `MIT` from its `LICENSE` file with that file's SHA-256, which the implementer computes from the crate source.
- **`png` and its dependencies:** these were already in the tree through dev-dependencies and are now shipped. Their licences must already be in `accepted`. If one is not, stop and report it.
- Regenerate with `cargo about generate -c about.toml about.hbs -o THIRD-PARTY-LICENSES.html`.

- [ ] **Step 1:** Write the tables, then the failing tests in `config.rs`. Parse `about.toml` with `toml` and the HTML with `html5ever`:
  - `about_toml_clarifies_libwebp_sys_and_quantizr_with_checksums`, and its negative: a fixture without the quantizr entry is flagged.
  - `the_licence_page_names_libwebp_quantizr_and_the_patents_grant`, and its negative: a fixture page without them is flagged.
- [ ] **Step 2:** Run `cargo test -p lunaepix-app --test config` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Edit and regenerate. If `cargo about` reports a licence outside `accepted`, stop and report it.
- [ ] **Step 4:** Run `cargo test --workspace` and expect it to pass.
- [ ] **Step 5:** Commit: "Ship the licences of libwebp and quantizr".

### Task 8: Run options, worker protocol v2, output folder and no-replace writes

**Files:**
- Modify: `crates/core/src/lib.rs` (`optimize_file` signature, the lossy dispatch, the destination rule)
- Modify: `crates/core/src/worker.rs` (arguments and record v2), `crates/core/src/run.rs` (`Worker::with_options`, the on-start hook), `crates/core/src/plan.rs` (output-folder planning, numbering), `crates/core/src/atomic.rs` (no-replace write)
- Test: `crates/core/tests/{run.rs,worker.rs}`, unit tests in `plan.rs` and `atomic.rs`, and a new `crates/core/tests/output_folder.rs`

**Interfaces:**
- Produces:
  - `pub enum Compression { Lossless, Lossy(Quality) }`
  - `pub enum WriteMode { Replace, NoReplace }`
  - `pub struct Options { pub compression: Compression, pub write: WriteMode }`
  - `pub fn optimize_file(src: &Path, dest: &Path, options: &Options) -> Result<Outcome, OptimizeError>`. `Lossless` keeps exactly today's path. `Lossy(q)` dispatches as follows: JPG, PNG and WebP go to their `optimize_lossy`, and SVG goes to `svg::optimize` with no note. With `WriteMode::NoReplace` the destination folder may be any folder, because nothing existing can be overwritten. With `Replace` the existing rule (the source's folder or its `optimized/` folder) stays.
  - `pub(crate) fn write_new(dest: &Path, bytes: &[u8], permissions: fs::Permissions) -> io::Result<PathBuf>`. It writes a temp file in the folder, then `persist_noclobber`. On `AlreadyExists` it tries `numbered(dest, n)` for n = 2…`MAX_NAME_NUMBER`. It returns the path written, or fails with "no free name in the output folder".
  - `pub(crate) fn numbered(name: &OsStr, n: u32) -> OsString`. The number goes before the last extension (`logo.png` → `logo-2.png`), or at the end for a name without one (`README-2`). The bytes stay intact for non-UTF-8 names.
  - `pub fn plan_files_to(inputs: &[PathBuf], out: Option<&Path>) -> Vec<Job>` and `pub fn plan_folder_to(dir: &Path, out: Option<&Path>) -> Vec<Job>`. With `None`, they behave as `plan_files` and `plan_folder` today, and those two become thin wrappers. With `Some(out)`, the destination is `out/<original name>`. The name is numbered when it clashes, case-insensitively (compared after full Unicode lowercasing of the lossy UTF-8 form), with an existing entry in `out` or with an earlier job's destination.
  - `pub fn images_in_folder(dir: &Path) -> io::Result<Vec<PathBuf>>`: the regular files directly in `dir` with a supported extension, sorted. This is for the app's dropped folders.
  - The worker arguments are `<WORKER_FLAG> [--lossy <0-100>] [--no-replace] <src> <dest>`, in that order. Anything else (a missing quality, a value `Quality::parse` refuses, an unknown option, a repeated option) exits with `EXIT_USAGE` and writes nothing.
  - `Worker::with_options(&self, options: &Options) -> Worker` appends those arguments.
  - Record v2 is `lunaepix-worker 2 ok <before> <after> <millis> <note-code or ->`. The error records are unchanged apart from the version. `parse_record` refuses version 1.
  - `pub fn run_observed(jobs, worker, stop, on_start: impl FnMut(usize), on_progress: impl FnMut(usize, &Path, &FileResult)) -> Vec<FileResult>`. `on_start(index)` is called right before each `Job::Optimize`'s worker starts, and never for a `Job::Skip`. `run_until_stopped` calls it with a no-op.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - **Lossless unchanged:** `a_lossless_run_gives_todays_bytes` (the existing fixture tests, through the new signature), and its negative `a_lossless_run_never_reaches_a_lossy_path`. A double for the lossy dispatch records no call; use a `_with` variant of `optimize_file`.
  - **SVG in a lossy run:** `an_svg_in_a_lossy_run_gets_the_lossless_output_without_note`, and its negative `the_lossy_dispatch_double_records_no_call_for_svg`.
  - **Worker settings:**
    - `the_worker_accepts_lossy_0_75_and_100`
    - `the_worker_refuses_a_missing_quality_minus_1_101_75_5_abc_an_unknown_option_and_a_repeat`. Each case exits 2 and writes no output file.
  - **Record v2:** `a_v2_ok_record_with_a_note_round_trips`, and its negatives `a_v1_record_is_refused` and `an_unknown_note_code_is_refused`.
  - **On-start hook:** `on_start_fires_once_per_optimize_job_before_its_progress`, and its negative `on_start_never_fires_for_a_skip`.
  - **Output folder:**
    - `original_names_land_in_the_output_folder`, and its negative `no_optimized_prefix_in_the_output_folder`.
    - `an_existing_logo_png_makes_logo_2_and_stays_byte_identical`, and its negative `no_clash_no_number`.
    - `two_sources_named_logo_png_give_logo_and_logo_2`, and its negative `different_names_get_no_number`.
    - `an_existing_Logo_PNG_makes_logo_png_become_logo_2`, and its negative `logo_png_and_logo_jpg_do_not_clash`.
    - `non_ascii_names_clash_case_insensitively_and_keep_their_bytes`, plus a non-UTF-8 name on Linux (Review Focus 4).
    - `a_file_appearing_before_the_write_is_not_overwritten_and_the_next_number_is_used`: create the file between planning and `write_new`. Its negative is `with_nothing_appearing_the_planned_name_is_used`.
    - `the_originals_own_folder_writes_photo_2_and_leaves_photo_unchanged`, and its negative `another_folder_writes_photo_without_number`.
    - `readme_clashes_to_readme_2`, and its negative `readme_without_clash_stays_readme`.
    - `numbering_stops_after_max_name_number_with_the_reason`: use a test-only small bound passed into a `_with` variant. Its negative is `a_free_name_below_the_bound_is_used`.
  - **Next to the originals:** `without_an_output_folder_optimized_name_replaces_as_today`, and its negative `nothing_is_written_outside_the_originals_folder`.
  - **Replace rule:** `replace_mode_still_refuses_a_destination_outside_the_two_allowed_folders`, and its negative `no_replace_mode_accepts_any_folder`.
  - **Folder images:** `images_in_folder_lists_supported_files_only_sorted_and_skips_subfolders`, with `optimized-a.jpg` beside `a.jpg` listed. Its negative is `a_folder_with_no_images_gives_an_empty_list`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement. Update the existing callers (CLI and app) only as far as needed to compile, passing lossless and `Replace`. Their features come in Tasks 9 and 10.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy, and expect both to pass.
- [ ] **Step 5:** Commit: "Pass run options to the worker and add the output folder".

### Task 9: Command line

**Files:**
- Modify: `crates/cli/src/args.rs`, `crates/cli/src/main.rs`, `crates/cli/src/report.rs`
- Test: `crates/cli/tests/cli.rs` and unit tests in `args.rs` and `report.rs`

**Interfaces:**
- Produces `pub struct Args { pub request: Request, pub compression: Compression, pub out: Option<PathBuf> }`. `parse(...) -> Result<Args, UsageError>`, and `USAGE` becomes the two lines from the spec's "Command line" section.

**Behaviour:**
- **Options:**
  - `--lossy` turns on lossy.
  - `--quality <v>` needs `--lossy` and goes through `Quality::parse`. Without it, the quality is `DEFAULT_QUALITY`.
  - `--out <folder>` is created with `create_dir_all` before planning, and outputs are planned with `plan_files_to` / `plan_folder_to`.
- The worker gets `with_options` with `NoReplace` when `--out` is given, and `Replace` otherwise.
- **Report:**
  - A lossy run prints `Lossy, quality <q>` once, on stdout, before the first block.
  - A row with a note prints the note on the line after its block.
  - Lossless output is unchanged.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `parses_lossy_quality_0_and_100_out_with_files_and_with_dir`
  - `usage_error_for_quality_without_lossy_101_minus_1_7_5_abc_repeated_quality_lossy_or_out_and_out_without_value`
  - `lossy_alone_uses_75`, and its negative `without_lossy_the_output_is_byte_identical_to_todays`
  - `out_creates_a_missing_folder_with_parents_and_writes_original_names`, and its negative `out_never_overwrites_an_existing_file`
  - `dir_without_out_writes_to_dir_optimized`, and its negative `dir_with_out_writes_nothing_to_dir_optimized`
  - `a_lossy_run_prints_the_mode_once_and_notes_after_their_row`, and its negative `a_lossless_run_prints_no_mode_line_and_no_note`
- [ ] **Step 2:** Run `cargo test -p lunaepix-cli` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement, and update the crate doc in `main.rs`.
- [ ] **Step 4:** Run `cargo test --workspace` and expect it to pass.
- [ ] **Step 5:** Commit: "Add --lossy, --quality and --out to the command line".

### Task 10: App backend — list, settings, output folder, events

**Files:**
- Modify: `src-tauri/src/lib.rs` (commands, drop handling, dialogs), `src-tauri/src/session.rs` (list, settings validation, run session), `src-tauri/src/events.rs` (new and changed events), `src-tauri/capabilities/default.json`, `src-tauri/INTERFACE.md` (rewritten to the spec's interface section)
- Test: unit tests in `session.rs` and `events.rs`, plus `src-tauri/tests/config.rs` (permissions)

**Interfaces:**
- Consumes `plan_files_to`, `images_in_folder`, `Options`, `Worker::with_options` and `run_observed` from Task 8.
- Produces:
  - **Commands:** `choose_files`, `clear_list`, `choose_output_folder`, `clear_output_folder`, `start_run(settings: serde_json::Value)` and `open_output_folder`. `choose_folder` is removed.
  - `pub fn parse_settings(value: &serde_json::Value) -> Result<Compression, CommandError>`. It accepts only an object with exactly the keys `mode` (`"lossless"` or `"lossy"`) and `quality` (a JSON integer 0–100, never a float, string or null). Anything else gives `CommandError::InvalidSettings`.
  - `CommandError` gains `InvalidSettings`, `EmptyList` and `OutputMissing`, serialised as `"invalid-settings"`, `"empty-list"` and `"output-missing"`.
  - `pub struct FileList`, with:
    - `add(&mut self, paths: &[PathBuf]) -> Option<String>`. A folder expands through `images_in_folder`. Entries are deduplicated by canonical path. The return value is the notice, "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)", when an add changed nothing because of an empty folder.
    - `clear`
    - `rows(&self, out: Option<&Path>) -> Vec<Job>`, through `plan_files_to`.
    - `has_work(&self, out) -> bool`
    - `mark_finished(&mut self)`, called when a run ends: the next `add` first empties the list, so the first add after a finished run starts from an empty list.
  - **Events** (serde, matching the spec's JSON exactly):
    - `ListChanged { files: Vec<ListRow>, notice: Option<String> }`, where `ListRow { name, size: Option<u64>, skip: Option<String> }`
    - `OutputChanged { folder: Option<String> }`
    - `FileStarted { index }`
    - `RunStarted { total, mode, quality: Option<u8> }`
    - `FileDone` gains `note: Option<String>`, skipped in the JSON when `None`.

**Behaviour:**
- A drop adds to the list, or is refused with `run-busy` while a run is going.
- **`start_run`**, in this order:
  1. busy check;
  2. `parse_settings`;
  3. the output folder still a directory and writable (create and delete a temp file in it with `tempfile`), else `"output-missing"`;
  4. re-plan the list now, so files deleted since show as `NotAFile` skips;
  5. `"empty-list"` when no `Job::Optimize` remains;
  6. run with `run_observed` on the existing run thread, emitting `file-started` and `file-done`.
- Open folder uses the chosen output folder when there is one.
- `output-changed` sends `file_name()` of the chosen folder only.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `adding_three_files_sends_three_rows`, and its negative `the_same_file_twice_is_listed_once`
  - `a_dropped_folder_adds_its_images_including_optimized_ones_but_not_subfolders`, and its negatives `notes_txt_in_the_folder_is_not_listed` and `an_empty_folder_sends_the_notice_and_adds_nothing`
  - `parse_settings_accepts_lossy_0_and_100_and_lossless_75`, and its negative `parse_settings_rejects_minus_1_101_75_5_string_75_null_missing_key_extra_key_and_mode_fast`
  - `a_list_with_one_compressible_file_starts`, and its negative `an_empty_list_or_only_skipped_rows_is_empty_list`
  - `after_run_finished_every_command_works_again`, and its negative `every_command_but_open_output_folder_is_busy_during_a_run`
  - `file_started_precedes_file_done_per_file`, and its negative `no_file_started_for_a_skipped_row`
  - `the_first_add_after_a_finished_run_lists_only_the_new_files`, and its negative `the_old_rows_are_not_in_it`
  - `a_deleted_file_shows_as_skipped_and_the_rest_runs` (Review Focus 5)
  - `a_list_of_2001_files_gives_2000_rows_and_one_over_limit_row`, and its negative `a_list_of_2000_files_has_no_over_limit_row`
  - `an_existing_writable_output_folder_starts`, with its negatives `a_deleted_output_folder_is_output_missing` and `a_read_only_output_folder_is_output_missing` (Review Focus 3; Unix permissions, skipped on Windows with a note)
  - `output_changed_sends_the_folder_name_only`, and its negative `no_event_payload_contains_the_temp_dir_path`. Serialise every event of a full session and search for the path. A fixture payload holding the path is found by the same search.
  - `run_started_names_mode_and_quality_and_null_quality_for_lossless`
  - `file_done_serialises_note_only_when_present`
  - In `config.rs`: `the_capability_grants_exactly_the_six_commands_and_event_listening` (parsed JSON), and its negative `a_capability_listing_choose_folder_is_flagged`
- [ ] **Step 2:** Run `cargo test -p lunaepix-app` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement, and rewrite `INTERFACE.md` to the spec's commands, events and examples, keeping its window rules (ordering, no Tauri drag events, CSP, permissions).
- [ ] **Step 4:** Run `cargo test --workspace` and clippy, and expect both to pass.
- [ ] **Step 5:** Commit: "Keep a file list and run it with the chosen settings".

### Task 11: The window

**Files:**
- Modify: `ui/index.html`, `ui/state.js`, `ui/app.js`, `ui/app.css`, `ui/format.js` (only if a new text helper is needed)
- Test: `tests/ui/state.test.js`, `tests/ui/format.test.js`, plus `src-tauri/tests/config.rs` (the existing element-id and HTML structure checks must keep passing)

**Layout** (binding mock-up, structure only; colours and sizes from `app.css`):
- **Drop area:** "Choose folder" is removed. Its title reads "Drop more images or a folder here" while the list has rows.
- **One panel `lunaepixPanel`, hidden while the list is empty**, holding in this order:
  1. The heading line, with the Clear button on the right.
  2. The table, keeping its `caption`.
  3. The settings `lunaepixSettings`:
     - `fieldset` "Compression" with two radios;
     - Quality: a range input plus a number input, each labelled, min 0, max 100, step 1;
     - Save to: the folder name or "Next to the originals", the "Choose output folder" button, and a "Next to the originals" button while a folder is chosen.
  4. Run, at the bottom right.
- **Finished:** the settings and Run are hidden; the summary and "Open folder" sit below the table.
- **Class names:** BBEM elements such as `lunaepixPanel__heading` and `lunaepixSettings__row`. States are separate modifier classes (`-waiting`, `-busy`), toggled with `classList`.

**Interfaces** (`ui/state.js`):
- **State fields:**
  - `phase`: `'empty' | 'ready' | 'running' | 'finished'`
  - `rows`
  - `mode`: `'lossless' | 'lossy'`
  - `quality` (number) and `qualityText` (what the number field holds)
  - `folder`: `string | null`
  - `current`: the index compressing
- **New actions:**
  - backend events: `list-changed`, `output-changed`, `file-started`;
  - local actions: `set-mode` and `set-quality-text`. The range input dispatches the same action with its value.
- **New exports:**
  - `canRun(state)`
  - `runSettings(state)`, which returns `{ mode, quality }`, or `null` when `qualityText` is not an integer 0–100 (`/^\d{1,3}$/` and ≤ 100)
  - `headingLine(state)`
- `rowView(row)` gains the views `waiting` and `compressing` and the `note`.

- [ ] **Step 1:** Write the tables, then the failing tests:
  - `launch_state_is_lossless_75_no_folder_empty`, and its negative `quality_cannot_change_while_lossless`
  - `list_changed_fills_rows_and_writes_a_notice_to_the_status`, and its negative `an_empty_list_hides_the_panel_and_notice_null_leaves_the_status`
  - `can_run_with_one_compressible_row`, and its negative `cannot_run_empty_only_skipped_or_running`
  - `file_started_marks_compressing_and_later_rows_waiting`, and its negative `a_done_row_never_returns_to_compressing`
  - `run_settings_sends_mode_and_integer_quality`, and its negative `text_fraction_or_out_of_range_blocks_run_and_sends_nothing`
  - `range_and_number_follow_each_other`, and its negative `an_invalid_number_does_not_move_the_range`
  - `heading_reads_compressing_2_of_3_lossy_75_then_done_lossy_75_and_lossless_without_number`, and its negative `a_lossless_run_never_shows_a_quality`
  - `a_note_shows_on_its_row`, and its negative `no_note_no_text`
  - `a_list_changed_after_run_finished_replaces_the_rows`, and its negative `an_add_before_run_finished_is_busy_and_keeps_the_rows`
  - `a_name_with_img_markup_stays_text`, and its negative `it_creates_no_element`. This one sits in `config.rs`'s JS lint and the `textContent` rule.
- [ ] **Step 2:** Run `node --test "tests/ui/*.test.js"` and expect the new tests to fail. Record the output.
- [ ] **Step 3:** Implement. Keep `aria-disabled` on buttons that cannot be used right now. Use `disabled` on the quality inputs while Lossless is picked. Announce progress only through the existing `role="status"` line. Add no animation.
- [ ] **Step 4:** Run `node --test "tests/ui/*.test.js"` and `cargo test --workspace` (config checks), and expect both to pass.
- [ ] **Step 5:** Commit: "Show the file list, the settings and Run in one panel".

### Task 12: Documentation and the hand-test list

**Files:**
- Modify: `README.md` (what Lunaepix does, building with `nasm`, the CLI usage, the window flow), `docs/roadmap.md` (status), crate descriptions in `crates/core/Cargo.toml` and the `lib.rs` doc ("Lossless" becomes "Lossless and lossy")
- Create: none

- [ ] **Step 1:** Update the texts.
- [ ] **Step 2:** Run the whole suite: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `node --test "tests/ui/*.test.js"`. All must pass.
- [ ] **Step 3:** Write the user's hand-test list into the report, not the repository:
  - a lossy run at 75 over `samples/` with one transparent PNG, one rotated phone JPG, one animated WebP and one SVG;
  - quality 0 and 100;
  - an output folder holding a file of the same name;
  - the originals' own folder as the output folder;
  - a dropped folder;
  - 2000+ files;
  - a 200-character file name;
  - Clear;
  - the Windows build from the workflow.
- [ ] **Step 4:** Commit: "Document lossy compression".

## Implementer report

Every implementer and every fix round writes, **before any code**:

1. **A test table:** behaviour → positive test → negative test, one row per behaviour.
2. **A failure-branch table:** every `Err` path, fallback, `catch` and "log and skip" the task adds or touches → the test that reaches it, or "untested" with the reason.

The report then gives:
- the RED run (command and failure output, taken before the production code existed);
- the GREEN run;
- the "Reuse survey" block from `~/.claude/CLAUDE.md`;
- the files changed.

Reviewers check the diff against both tables and probe alternative spellings in any structured-file check. Lunaepix has no Grunt build and no devsync deployment, so there is nothing to build or upload beyond `cargo` and `node`.
