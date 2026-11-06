# Lunaepix WebP support and header logo — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-06

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lunaepix optimises `.webp` files losslessly (metadata stripping for every WebP, a level-9 lossless re-encode for still lossless ones), and the window shows the moon logo beside its heading.

**Architecture:** A new `webp` module in `crates/core` with three parts. `riff` is the container walk, size check, stripping and assembly, in safe Rust. `codec` is the libwebp FFI, the only unsafe part. The module root chooses between the candidates, behind a `Codec` trait so that the failure branches can be reached with a double. Front-end changes are text, plus one `<img>` in the header.

**Tech Stack:** Rust 2024 (MSRV 1.90), `libwebp-sys =0.14.4` (vendored libwebp, built with `cc`), `image =0.25.10` with its `webp` feature as the independent test decoder, Tauri 2, vanilla JS, cargo-about.

**Spec:** [2026-11-06-lunaepix-webp-design.md](../../in-progress/specs/2026-11-06-lunaepix-webp-design.md). Read its Handoff before anything else.

## Handoff

Copied verbatim from the spec's Handoff, plus what was decided while planning. Give every implementer these lines as they are. An implementer that wants to deviate from one stops and asks instead of choosing.

- **Plan approved by the user (2026-11-06):** "approved". This includes the calls made while planning (listed below). Execute in a fresh session, subagent-driven, on `feature/lunaepix-webp`.
- **Spec approved by the user (2026-11-06):** "Approved, and also include the new logo for it".
- "Is it possible to also add support for webP?" — **Scope: "Optimise .webp files"** (user, 2026-11-06). Accept `.webp` as input, like PNG/JPG/SVG today. Output stays WebP (`optimized-<name>.webp`), never larger than the input, and the original is never touched. Ruled out: making WebP from PNG, alongside this or instead of it.
- **Lossy and animated WebP: "Strip metadata, else copy"** (user, 2026-11-06). Remove EXIF/XMP but keep the colour profile, so the picture looks the same. If that gains nothing, copy the file as it is, the same "never larger" rule PNG and JPG follow today. Ruled out: skipping lossy and animated WebP files.
- **Logo in the window header** (user, 2026-11-06, chosen over "only ship the app icon"): show the moon logo next to the "Lunaepix" heading inside the window, made from the same SVG. The approved sketch in the spec's Layout section is binding: logo about 32 px tall, decorative (`alt=""`), the heading text stays as it is. The moon app icon (commit `dd0849e`, already on this branch) ships in the same release.
- The desktop-app spec's decisions stand: lossless only, one worker process per file, `MAX_PIXELS` checked from the header before decoding, hostile inputs in `crates/core/tests/hostile_input.rs`, licences limited to the `about.toml` list, structured files checked with a parser.
- The Jugoo CMS makes lossy WebP (GD quality 100, no metadata); Lunaepix copies those unchanged. That is expected, not a defect.
- **Task 1 is a hard gate.** No product code is written before the user has ruled on its numbers. If the user drops the re-encode there, Tasks 5 and 6 are removed whole, and `libwebp-sys` stays a test-only dependency.

Decided while planning (2026-11-06; approved with the plan):

- **Branch:** `feature/lunaepix-webp`, branched from `feature/lunaepix-desktop-app` at `dd0849e`, so the moon icon commit comes along. Dev URL: none (a desktop app, tested by hand on Linux and through the rc4 Windows build).
- **Test fixtures:** lossless WebP from the `image` crate's encoder, which is pure Rust and fast, so it leaves room to optimise. Lossy and lossy-with-alpha WebP come from `libwebp-sys`'s simple `WebPEncodeRGBA` API, in a test helper. Animated WebP is assembled by the test helper from `ANIM`/`ANMF` chunks around lossless frames. The safe `webp` wrapper crate is **not** used: it links `libwebp-sys 0.9`, a second copy of libwebp's C symbols.
- **`smallest`** moves from `jpg.rs` to the crate root, with its three tests, so JPG and WebP share one implementation (the security rules treat duplicated logic as a defect).

## Execution

Fresh session, subagent-driven. Invoke `sdd-overrides` first. The `dev-*` agents are written for Jugoo PHP work, so expect a general-purpose agent for the Rust and the window tasks.

Every implementation task is test-first. Each implementer's report starts with the RED run (the command and its failure output, taken before the production code exists). It then gives the test table and the failure-branch table per the memory rule "Test + failure-branch tables first", then the positive and negative test per behaviour, then the Reuse survey block. Exact test counts are not fixed here; the implementer establishes them.

## Reuse survey (done while planning)

```
## Reuse survey
Found:      plan::format_of / Format (crates/core/src/plan.rs:183-206); plan::check_pixels (plan.rs:230);
            jpg::smallest and its three tests (jpg.rs:86, 665-690); png::check_chunks as the chunk-walk pattern (png.rs:52);
            the hostile-input helpers write/assert_refused (crates/core/tests/hostile_input.rs:35-75);
            the html5ever StartTag reader start_tags / StartTag::attr (src-tauri/tests/config.rs:410-483);
            the licence-text checks has_libdeflate_notice and its fixture test (config.rs, licence section);
            the libdeflate-sys clarify pattern (about.toml); the IJG block in about.hbs:18.
Reused:     format_of, check_pixels, the hostile-input helpers, start_tags, the clarify pattern, the about.hbs block pattern as is;
            smallest moved to lib.rs and shared.
Wrote new:  the RIFF walk and assembly (no container code exists for RIFF), the libwebp FFI, the header logo markup.
```

## Global Constraints

- Lossless: a still image's decoded RGBA is byte-identical, including RGB under alpha 0. Lossy, lossy-with-alpha and animated image chunks are copied byte for byte.
- Output never larger than input; when no candidate is smaller, the original bytes are written.
- `libwebp-sys = "=0.14.4"`; `image` stays `=0.25.10`, with the `webp` feature added (dev-dependency only).
- Error texts, verbatim: "not a WebP file (the content is not WebP data)"; "could not read the WebP data: …"; "the re-encoded WebP did not decode to the same pixels"; the size refusal is `plan::check_pixels`'s own text.
- Lossless preset: libwebp `WebPConfigLosslessPreset(&mut config, 9)` and then `config.exact = 1`.
- Kept chunks: `VP8X`, `ICCP`, `ANIM`, `ANMF`, `ALPH`, `VP8 `, `VP8L`. Dropped: `EXIF`, `XMP `, and every other FourCC.
- `unsafe` only in `webp::codec` (and in test helpers). `lib.rs`'s doc line about unsafe names `jpg`, `platform` and `webp::codec`.
- UI texts, verbatim: "Not a PNG, JPG, SVG or WebP file"; "PNG · JPG · SVG · WebP"; "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)"; dialog filter "PNG, JPG, SVG and WebP images".
- The header logo is `<img alt="">` and the heading text "Lunaepix" (an `h1`) stays as it is. Class names are BBEM (`lunaepixHeader__logo`). CSS gets no tests.
- No `debug()`-style leftovers (`dbg!`, `println!` in library code). Build output (`target/`) is never edited.

## Review Focus

Inputs the spec implies but no test row names, most likely to bite first. Each one's test is added to the owning task below.

1. **An odd-sized chunk** (a `VP8 ` or `ICCP` of odd length, followed by its padding byte) must keep its padding in the stripped output, or every later chunk shifts and the file breaks. → Task 3, `keeps_the_padding_byte_of_an_odd_sized_chunk`.
2. **Bytes after the RIFF size** (some tools append data) must not reach the stripped output, and the result must still decode. → Task 3, `drops_bytes_after_the_riff_size`.
3. **`VP8X` flags that disagree with the chunks** (an EXIF chunk present but its flag clear, or the reverse) must still be stripped, and the output flags must match the chunks it actually holds. → Task 3, `rewrites_flags_that_disagree_with_the_chunks`.
4. **An extended lossless file without ICC** (`VP8X` + `VP8L`, no flags) must take the re-encode path, not the lossy one. → Task 5, `re_encodes_an_extended_lossless_file_without_icc`.
5. **An empty or near-empty `.webp`** (0 bytes, or only `RIFF`) must fail with the WebP message, not panic. → Task 2, in `refuses_what_is_not_webp_data`.

---

### Task 1: Measure before building (HARD GATE: no product code)

**Files:**
- Create (throwaway, outside git): `.superpowers/sdd/2026-11-06-lunaepix-webp/gate/` (a scratch cargo project that depends on `libwebp-sys =0.14.4` and `image =0.25.10`). `.superpowers/` is already git-ignored.

- [ ] **Step 1:** Make lossless WebP test files from every image in `samples/png` and `samples/jpg`, at libwebp's default lossless settings (`WebPConfigInit` plus `lossless = 1`) and at its fastest (`WebPConfigLosslessPreset(…, 0)`). Make lossy WebP files at quality 75 and 100, each once with and once without an EXIF block of about 10 KB and an XMP block of about 2 KB, added by hand as chunks.
- [ ] **Step 2:** For each lossless file, time the whole re-encode candidate (decode, level-9 encode with `exact = 1`, decode again, compare RGBA) and record the size before and after. Use a release build and `/usr/bin/time -v` for the peak resident memory. For each lossy file, record the size after stripping metadata.
- [ ] **Step 3:** Time and measure the largest sample, or a sample scaled to about 25 megapixels, and scale time and memory linearly to 100 megapixels. Flag a scaled time over **3 minutes** or a scaled memory over **4 GB**.
- [ ] **Step 4:** Report a table to the user and **stop**. The table has one row per file: kind, size before, size after, saving %, time and peak memory. The report states the two flags and asks the user to rule: build the re-encode as specified; build it with a different level or limit (the user names it); or drop the re-encode, which removes Tasks 5 and 6. Record the ruling in the spec's and the plan's Handoff before Task 2 starts.

### Task 2: Recognise WebP and walk the container

**Files:**
- Modify: `crates/core/src/plan.rs` (`Format::Webp`; `format_of` matches `webp`; doc line)
- Modify: `crates/core/src/lib.rs` (`mod webp;`; the `Format::Webp` arm in `optimize_file` calling `webp::optimize`; module and enum docs)
- Create: `crates/core/src/webp.rs` (module root; for now `optimize` only parses and returns the original bytes)
- Create: `crates/core/src/webp/riff.rs`
- Modify: `crates/core/tests/hostile_input.rs`
- Modify: `crates/core/Cargo.toml` (`image` dev-dependency gains the `webp` feature)

**Interfaces:**
- Produces:
  - `pub(crate) enum Kind { Lossy, Lossless, Animated }` in `riff`. Lossless means a still `VP8L` image. Lossy means a still `VP8 ` image, with or without `ALPH`.
  - `pub(crate) struct Chunk<'a> { pub fourcc: [u8; 4], pub data: &'a [u8] }`
  - `pub(crate) struct WebpFile<'a> { pub chunks: Vec<Chunk<'a>>, pub kind: Kind }`
  - `pub(crate) fn parse(bytes: &[u8]) -> Result<WebpFile<'_>, String>`: the content check, the chunk walk and the pixel check, in that order. The walk stops at the RIFF size; bytes after it are ignored.

Header layouts the walk reads (from the WebP container spec, little endian):

```text
RIFF header : "RIFF" u32(size of everything after these 8 bytes) "WEBP"
chunk       : fourcc[4] u32(payload size) payload, plus 1 zero pad byte when the size is odd
VP8X payload: flags u8 (ICC 0x20, alpha 0x10, EXIF 0x08, XMP 0x04, animation 0x02), 3 reserved bytes,
              canvas width-1 u24, canvas height-1 u24
VP8L payload: 0x2F, then 32 bits: width-1 (14 bits), height-1 (14 bits), alpha hint (1), version (3)
VP8  payload: 3-byte frame tag, start code 9D 01 2A, width u16 & 0x3FFF, height u16 & 0x3FFF
```

- [ ] **Step 1: Write the failing tests** in `riff.rs` (`#[cfg(test)]`) and `hostile_input.rs`:
  - `refuses_what_is_not_webp_data`: PNG bytes, a `RIFF…WAVE` file, 0 bytes and a file of only `RIFF` each give exactly "not a WebP file (the content is not WebP data)" (Review Focus 5).
  - `parses_a_simple_lossless_file_as_lossless`: a fixture from `image`'s WebP encoder gives `Kind::Lossless` and one `VP8L` chunk. The negative is `parses_a_lossy_file_as_lossy`: a hand-built `VP8 ` header (frame tag + start code + size) gives `Kind::Lossy`, not lossless.
  - `parses_an_animated_file_as_animated`: `VP8X` (animation flag) + `ANIM` + two `ANMF`. Negative: the same chunks without `ANIM`/`ANMF` are not `Animated`.
  - `refuses_a_vp8x_that_is_not_first` and `refuses_a_second_vp8x`: both errors start with "could not read the WebP data:". Positive counterpart: a `VP8X` that comes first, exactly once, parses.
  - `refuses_a_file_without_an_image_chunk`: `VP8X` + `EXIF` only.
  - `refuses_a_chunk_past_the_end`: a chunk size of `u32::MAX`, and a RIFF size larger than the file. Each gives a "could not read the WebP data" error with no panic, within 5 s.
  - `refuses_a_canvas_over_the_pixel_limit`: a `VP8X` of 20000×20000 gives `check_pixels`'s "image too large: 20000×20000 exceeds 100 megapixels" within 5 s. Negative: 10000×10000 passes the size check and fails as unreadable, not as "too large". Also a `VP8X` canvas within the limit over a `VP8L` header over the limit is refused (both sizes are checked).
  - In `hostile_input.rs`, through `optimize_file` and `assert_refused`: the `u32::MAX` chunk, the long RIFF size, a truncated `VP8L`, the misplaced `VP8X`, the second `VP8X` and the `RIFF`-only file. Each is refused with no output written. Positive counterpart: `still_optimises_a_normal_webp`, which writes an output that `image` decodes.
- [ ] **Step 2: Run** `cargo test -p lunaepix-core webp` and `cargo test -p lunaepix-core --test hostile_input`. Expected: they fail to compile or fail (no `webp` module yet). Keep this output for the report.
- [ ] **Step 3: Implement** `Format::Webp`, `format_of`, `riff::parse` (on the pattern of `png::check_chunks`, with checked arithmetic on every offset), and `webp::optimize(bytes: &[u8]) -> Result<Vec<u8>, String>`, which for now returns the original bytes after a successful parse. Also replace the plan test `skips_gif_and_webp_as_unsupported_and_plans_nothing` with `skips_gif_and_lookalike_extensions_as_unsupported`: `a.gif`, `a.webpx` and `a.web` are skipped. Add its positive `plans_webp_in_either_letter_case`: `a.webp` and `a.WEBP` are planned as `optimized-a.webp` and `optimized-a.WEBP`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: all pass.
- [ ] **Step 5: Commit** with the message "Recognise WebP files and walk their RIFF container".

### Task 3: Strip metadata and choose the smallest

**Files:**
- Modify: `crates/core/src/webp/riff.rs` (`stripped`)
- Modify: `crates/core/src/webp.rs` (`optimize` uses the stripped candidate)
- Modify: `crates/core/src/lib.rs` and `crates/core/src/jpg.rs` (move `smallest` and its three tests to `lib.rs` as `pub(crate) fn smallest<'a>(original: &'a [u8], candidates: &'a [Vec<u8>]) -> &'a [u8]`; `jpg.rs` calls it)
- Create: `crates/core/tests/optimize_webp.rs`, `crates/core/tests/common/webp_fixtures.rs` (fixture helpers; `mod common;` in the test files that use them)
- Modify: `crates/core/Cargo.toml` (`libwebp-sys = "=0.14.4"` as a dev-dependency here; Task 5 adds it as a normal dependency)

**Interfaces:**
- Consumes: `riff::parse`, `WebpFile`, `Chunk`, `Kind` (Task 2).
- Produces: `impl WebpFile<'_> { pub(crate) fn stripped(&self) -> Vec<u8>; pub(crate) fn icc(&self) -> Option<&[u8]> }`. The stripped file keeps only the kept chunks (see Global Constraints) in their original order, padding included. Its `VP8X` flags for ICC, EXIF and XMP are recomputed from the chunks present, alpha and animation are kept as they were, and the RIFF size is rewritten. A simple file (no `VP8X`) stays simple.
- Produces (test helpers in `webp_fixtures.rs`): `lossless(rgba, w, h) -> Vec<u8>` (via `image`); `lossy(rgba, w, h, quality: f32) -> Vec<u8>` (via `libwebp_sys::WebPEncodeRGBA` and `WebPFree`; with alpha this gives `VP8X`+`ALPH`+`VP8 `); `with_chunks(file, &[([u8;4], &[u8])]) -> Vec<u8>` (adds chunks and sets the matching `VP8X` flags, turning a simple file into an extended one); `animated(frames: &[Vec<u8>], w, h) -> Vec<u8>`; `chunk_payload(file, fourcc) -> Option<Vec<u8>>`.

- [ ] **Step 1: Write the failing tests** (`riff.rs` unit tests, plus `optimize_webp.rs` through `optimize_file`):
  - `removes_exif_and_xmp_and_keeps_icc_byte_identical`: the flags match afterwards. Negative: `adds_nothing_to_a_file_without_metadata` (no chunk and no flag gained; the ICC chunk is still there).
  - `writes_the_original_when_stripping_gains_nothing`: output bytes == input bytes. Negative: the same file with a 10 KB EXIF comes out smaller.
  - `keeps_every_anmf_chunk_of_an_animated_file`: every `ANMF` is byte-identical, EXIF/XMP are gone, and every frame decodes with `image` to the same pixels. Negative: an animated file without metadata is copied as it is.
  - `keeps_vp8_and_alph_of_a_lossy_file_with_alpha_byte_identical`: negative, the output is not larger than the input.
  - `drops_unknown_chunks`: a chunk `ABCD` is gone. Negative: no kept chunk is gone.
  - Review Focus 1, `keeps_the_padding_byte_of_an_odd_sized_chunk`: an odd-length `ICCP`. The output parses, and `image` decodes it to the input's pixels.
  - Review Focus 2, `drops_bytes_after_the_riff_size`: 100 trailing bytes. The stripped candidate holds none of them and decodes to the same pixels. Negative: a file whose only waste is the trailing bytes comes out smaller.
  - Review Focus 3, `rewrites_flags_that_disagree_with_the_chunks`: an EXIF chunk with its flag clear, and an XMP flag set with no XMP chunk. Afterwards the flags match exactly the chunks present.
  - `optimize_file` end to end: a `.webp` in a folder run lands in `optimized/` under its own name, and the source bytes are unchanged. Negative: PNG bytes named `.webp` fail with no output.
  - The three moved `smallest` tests pass unchanged from their new place.
- [ ] **Step 2: Run** `cargo test -p lunaepix-core`. Expected: the new tests fail (no `stripped` yet).
- [ ] **Step 3: Implement** `stripped`, `icc`, the move of `smallest`, and `webp::optimize` = parse, then `smallest(original, &[file.stripped()])`.
- [ ] **Step 4: Run** `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: all pass, no warnings.
- [ ] **Step 5: Commit** with the message "Strip WebP metadata and keep the smallest result".

### Task 4: Name WebP in every text

**Files:**
- Modify: `crates/core/src/lib.rs` (`Skip::UnsupportedType` text, docs, the `skip_reasons_are_human_readable` test)
- Modify: `src-tauri/src/lib.rs:30,97` (`IMAGE_EXTENSIONS` gains `"webp"` and `"WEBP"`; filter "PNG, JPG, SVG and WebP images")
- Modify: `ui/index.html:18`, `ui/state.js:17`
- Modify: the texts' tests: `src-tauri/src/session.rs:1002`, `src-tauri/src/events.rs:236`, `crates/cli/src/report.rs:275`, `crates/cli/tests/cli.rs:244,457`, `tests/ui/state.test.js:25,92,210,366-370`
- Modify: `README.md` first line, the `description` in `crates/core/Cargo.toml`, the doc comments in `crates/cli/src/main.rs:1` and `crates/core/src/atomic.rs:1`

- [ ] **Step 1: Update the tests first** to the new texts (Global Constraints). Add `the_dialog_filter_lists_webp_in_both_cases` in `src-tauri/src/lib.rs` (`IMAGE_EXTENSIONS` contains `webp` and `WEBP`). Its negative is `no_text_names_only_png_jpg_and_svg`: a scan of `ui/` and the `Skip` texts for the old phrase "PNG, JPG or SVG" finds nothing. The same scan, run on a fixture string that holds the phrase, finds it.
- [ ] **Step 2: Run** `cargo test --workspace` and `node --test "tests/ui/*.test.js"`. Expected: the updated tests fail.
- [ ] **Step 3: Change the texts.**
- [ ] **Step 4: Run** both suites. Expected: all pass.
- [ ] **Step 5: Commit** with the message "Name WebP in the window, the CLI and the skip reason".

### Task 5: Re-encode still lossless WebP with libwebp (removed if the gate drops it)

**Files:**
- Modify: `crates/core/Cargo.toml` (`libwebp-sys = { version = "=0.14.4" }` as a normal dependency, with a comment in the style of the other codec pins; the default features `std` and `parallel` stay, and no SIMD-flag features are added)
- Create: `crates/core/src/webp/codec.rs` (`#[allow(unsafe_code)] mod codec;` in `webp.rs`)
- Modify: `crates/core/src/webp/riff.rs` (`assemble_lossless`), `crates/core/src/webp.rs` (the trait and the choice), `crates/core/src/lib.rs` (the unsafe doc line)

**Interfaces:**
- Consumes: `parse`, `stripped`, `icc`, `smallest` (Tasks 2–3).
- Produces:
  - `pub(crate) struct Rgba { pub width: u32, pub height: u32, pub pixels: Vec<u8> }`, with 4 bytes per pixel and no stride padding.
  - `pub(crate) trait Codec { fn decode_rgba(&self, webp: &[u8]) -> Result<Rgba, String>; fn encode_lossless(&self, image: &Rgba) -> Result<Vec<u8>, String>; }`. `encode_lossless` returns a complete simple WebP file (RIFF + `VP8L`), as libwebp's memory writer produces.
  - `pub(crate) struct Libwebp;` implements `Codec`. Decoding uses `WebPDecodeRGBA` and `WebPFree`, refusing a null result. Encoding uses `WebPConfigInit`, then `WebPConfigLosslessPreset(&mut config, 9)`, then `config.exact = 1`, then `WebPValidateConfig`, then `WebPPictureImportRGBA` with `WebPMemoryWriter`; it refuses any zero return and frees the picture and the writer on every path.
  - `pub(crate) fn assemble_lossless(vp8l: &[u8], icc: Option<&[u8]>, width: u32, height: u32, has_alpha: bool) -> Vec<u8>` in `riff`. Without ICC it gives a simple file. With ICC it gives `VP8X` (ICC flag, plus the alpha flag when `has_alpha`, and the canvas) + `ICCP` + `VP8L`. `has_alpha` is true when any decoded alpha byte is below 255.
  - `pub(crate) fn optimize_with(bytes: &[u8], codec: &impl Codec) -> Result<Vec<u8>, String>`; `optimize` calls it with `&Libwebp`. For `Kind::Lossless` the candidates are `[stripped, re-encoded]`, otherwise `[stripped]`. The re-encoded candidate counts only when `codec.decode_rgba(&candidate)` equals the source's decode (width, height and every byte). Otherwise the whole call returns "the re-encoded WebP did not decode to the same pixels".

- [ ] **Step 1: Write the failing tests.**
  - `optimize_webp.rs`: `re_encodes_a_fast_lossless_file_smaller_with_identical_pixels`, decoded by `image`. Negative: `never_re_encodes_a_lossy_file`, whose `VP8 ` chunk is byte-identical in the output.
  - `keeps_the_rgb_under_transparent_pixels`. The fixture asserts first that it has alpha-0 pixels with non-zero RGB, then that the output RGBA is byte-identical.
  - `keeps_icc_on_a_re_encoded_file`: `ICCP` byte-identical, the `VP8X` ICC flag set, the alpha flag only when the image has alpha. Negative: an opaque image does not get the alpha flag.
  - Review Focus 4, `re_encodes_an_extended_lossless_file_without_icc`: `VP8X` + `VP8L` with no flags takes the re-encode path (the output is smaller than the stripped candidate alone, and its pixels are identical).
  - In `webp.rs` with a double `FakeCodec` that records the arguments it receives and returns set results: `accepts_a_re_encode_that_decodes_to_the_same_pixels` (the fake was given the source's RGBA). Negatives: `refuses_a_re_encode_that_differs_in_one_byte`, which returns the exact mismatch message, so `optimize_file` writes no output; `a_failed_decode_fails_the_file`; `a_failed_encode_fails_the_file`; `a_failed_decode_of_the_candidate_fails_the_file`. Each failure test asserts the error text and that no output was written.
  - `codec.rs`: `libwebp_round_trips_a_lossless_image_exactly`. Negative: `libwebp_refuses_bytes_that_are_not_webp` (decode of garbage is an `Err`, with no panic).
- [ ] **Step 2: Run** `cargo test -p lunaepix-core webp`. Expected: these tests fail.
- [ ] **Step 3: Implement** `codec.rs`, `assemble_lossless` and `optimize_with`.
- [ ] **Step 4: Run** `cargo test --workspace` and clippy (as in Task 3). Then time a release run of `lunaepix` on the gate's largest lossless sample and confirm it is within the time measured in Task 1.
- [ ] **Step 5: Commit** with the message "Re-encode still lossless WebP at libwebp's strongest lossless setting".

### Task 6: Licence notice for libwebp (removed if the gate drops the re-encode)

**Files:**
- Modify: `about.toml` (`[libwebp-sys.clarify]`)
- Modify: `about.hbs` (a fixed `PATENTS` block beside the IJG block)
- Regenerate: `THIRD-PARTY-LICENSES.html`
- Modify: `src-tauri/tests/config.rs` (licence section), `README.md` (licence section)

- [ ] **Step 1: Write the failing tests** in `config.rs`, beside `the_third_party_licences_carry_libdeflates_mit_notice`:
  - `the_third_party_licences_carry_libwebps_bsd_notice_and_patent_grant`: the page text (via the existing `page_text`) holds libwebp's copyright line "Copyright (c) 2010, Google Inc." with the BSD-3-Clause text, and the first sentence of `PATENTS`.
  - Negative: `a_page_without_the_libwebp_notice_or_patent_grant_is_caught_in_a_fixture`, using two fixture pages, each missing one of the two.
  - The existing `the_real_about_toml_is_read_and_holds_no_copyleft…` must keep passing, with `PERMITTED_LICENCES` unchanged.
- [ ] **Step 2: Run** `cargo test -p lunaepix-app --test config`. Expected: the new tests fail.
- [ ] **Step 3: Configure** `about.toml`:

  ```toml
  # libwebp-sys declares only MIT, but vendors libwebp's C code, which is
  # BSD-3-Clause (vendor/COPYING), with Google's patent grant (vendor/PATENTS,
  # printed by about.hbs). The crate ships no MIT file of its own.
  [libwebp-sys.clarify]
  license = "MIT AND BSD-3-Clause"

  [[libwebp-sys.clarify.files]]
  path = "vendor/COPYING"
  checksum = "5aec868f669e384a22372a4e8a1a6cd7d44c64cd451f960ca69cc170d1e13acf"
  license = "BSD-3-Clause"
  ```

  If cargo-about refuses an MIT term without a file, the implementer stops and reports the message. It does not change `accepted` and it does not drop MIT. Add the `PATENTS` block to `about.hbs`: the full text of `vendor/PATENTS`, under a heading naming libwebp, with an HTML comment giving the file's SHA-256 (`cc3273e0694ea5896145e0677699b53471b03ea43021ddc50e7923fbb9f5023c`) and the crate version. Regenerate with `cargo about generate -c about.toml about.hbs -o THIRD-PARTY-LICENSES.html`. Add libwebp to the README's licence paragraph.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: all pass.
- [ ] **Step 5: Commit** with the message "Credit libwebp's licence and patent grant".

### Task 7: The moon logo in the window header

**Files:**
- Create: `ui/lunaepix-logo.svg` (a byte-identical copy of `src-tauri/icons/lunaepix-icon.svg`)
- Modify: `ui/index.html:11-13`, `ui/app.css` (header block, and the narrow-layout media query at about line 237)
- Modify: `src-tauri/tests/config.rs`

Layout (binding, from the spec): the logo sits to the left of "Lunaepix" on one line, vertically centred, about 32 px tall, and smaller in the narrow layout where the heading shrinks. The flex row goes on `lunaepixHeader` (or a new element of it), not on the `h1`. Markup:

```html
<header class="lunaepixHeader">
  <img class="lunaepixHeader__logo" src="lunaepix-logo.svg" alt="" width="32" height="32">
  <h1 class="lunaepixHeader__title">Lunaepix</h1>
</header>
```

- [ ] **Step 1: Write the failing tests** in `config.rs`, reusing `start_tags`:
  - `the_header_shows_one_decorative_logo_from_the_window_folder`: exactly one `img` in `index.html`. It has `alt=""`, and its `src` is a relative path with no `..` and no scheme, naming a file that exists under `ui/`.
  - Negative: `a_logo_without_alt_or_from_outside_the_window_folder_is_caught_in_a_fixture`, with three fixtures: no `alt`, `src="https://…"`, and `src="../src-tauri/icons/lunaepix-icon.svg"`.
  - `the_window_logo_is_the_icon_source`: the bytes of `ui/lunaepix-logo.svg` equal those of `src-tauri/icons/lunaepix-icon.svg`. Negative: `the_logo_comparison_reports_a_one_byte_difference`, which runs the same helper on two byte strings that differ in one byte.
  - The existing tests `the_index_page_has_the_structure_the_accessibility_rules_need`, `the_content_security_policy_is_strict` and `the_window_files_pass_the_js_lint` must still pass. The `ui_entries` scan must still skip binary files: the SVG is text, so check that the HTML-attribute checks do not trip on it.
- [ ] **Step 2: Run** `cargo test -p lunaepix-app --test config`. Expected: the new tests fail.
- [ ] **Step 3: Implement** the copy, the markup and the CSS. Read `~/.claude/css-conventions.md` and `~/.claude/a11y-rules.md` first.
- [ ] **Step 4: Run** `cargo test --workspace` and `node --test "tests/ui/*.test.js"`. Expected: all pass. Then run `cargo tauri dev` and look at the header in the wide and the narrow window, in light and dark mode; this is a hand check, not a test.
- [ ] **Step 5: Commit** with the message "Show the moon logo beside the window heading".

### Task 8: Build, hand check and release candidate

- [ ] **Step 1:** Merge the `feature/lunaepix-webp` work back as the SDD flow does, then run `cargo test --workspace`, `node --test "tests/ui/*.test.js"` and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: all green.
- [ ] **Step 2:** Build Linux with `cargo tauri build --config src-tauri/tauri.bundle.conf.json`. Install the rpm and check by hand:
  - a lossless WebP gets smaller;
  - a CMS-style lossy WebP is copied unchanged;
  - a lossy WebP with EXIF gets smaller;
  - an animated WebP still animates in a browser;
  - the logo shows beside the heading;
  - the launcher shows the moon icon.
- [ ] **Step 3:** Ask the user before pushing anything. Pushing the branch and a `v0.1.0-rc4` tag starts the Windows build and is outward-facing. After the user's go-ahead, push and report the Actions run. On Windows the user checks the same items as in Step 2, plus that the C build of libwebp linked statically (no missing-DLL dialog).

## CMS changes

No CMS changes. Lunaepix is not a Jugoo site.

## Rules check

Checked:  `~/.claude/CLAUDE.md` (handoff first; plan describes rather than carries code; nothing built while planning; reuse survey; positive and negative tests); `rules/spec-requirements.md` (the Layout is in the spec and binding, with the wide and narrow layouts named in Task 7; no CMS, money or endpoint); `rules/unit-testing.md` (each behaviour has a positive and a negative test; every failure branch is reached with a double; RED run first; no mutation runs; CSS untested); `rules/security.md` (untrusted file input bounded from the header before decoding; checked offsets; hostile inputs in the shared file; unsafe confined to one module; worker isolation); `rules/vanilla-js.md` (one string change; no `innerHTML`); `rules/devsync.md` (not a Jugoo site; nothing uploaded); `rules/processes.md` (the `cargo tauri dev` hand check is started and stopped by the implementer by its own PID); `rules/local-links.md`; `rules/smarty.md` and `rules/cms-defines-fields.md` (not applicable); `css-conventions.md` (`lunaepixHeader__logo`, flex on the block, not on the `h1`); `a11y-rules.md` (decorative `alt=""`, heading unchanged).
Changed:  added the negative half of the text test (the old phrase is found in a fixture); moved `smallest` to the crate root instead of writing a second copy; Task 8 asks before pushing the rc4 tag; the hand check names light and dark mode and the narrow layout.
