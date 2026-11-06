# Lunaepix WebP support — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-06

## Handoff

Decided in conversation (2026-11-06):

- **Spec approved by the user (2026-11-06):** "Approved, and also include the new logo for it".
- **Plan approved by the user (2026-11-06):** "approved". Plan: `docs/in-progress/plans/2026-11-06-lunaepix-webp.md`, on branch `feature/lunaepix-webp` (from `feature/lunaepix-desktop-app` at `dd0849e`). Task 1 is a hard measurement gate.
- **Gate ruling (user, 2026-11-06): "Drop the re-encode".** The measurement gave level 9 +0.66% over libwebp's default lossless (larger on 4 of 8 samples) and +10.4% over the fastest preset, at 4.3–7.5 min and about 5 GB scaled to 100 MP. Only the stripped candidate is built, for every WebP: pure safe Rust, no libwebp in the product, no FFI module, no C build, no licence change (see "First step: measure before building"). The "Re-encoded" candidate, the "Library" section and the re-encode, hidden-colour, re-encode-check, libwebp-failure and licence rows of the Tests table no longer apply.
- The user tested the Windows build (`v0.1.0-rc3`) and it worked. WebP is the change that comes after that test; rebuilding both platforms and tagging rc4 waits until this work is done.
- "Is it possible to also add support for webP?" — **Scope: "Optimise .webp files"** (user, 2026-11-06). Accept `.webp` as input, like PNG/JPG/SVG today. Output stays WebP (`optimized-<name>.webp`), never larger than the input, and the original is never touched. Ruled out: making WebP from PNG, alongside this or instead of it.
- **Lossy and animated WebP: "Strip metadata, else copy"** (user, 2026-11-06). Remove EXIF/XMP but keep the colour profile, so the picture looks the same. If that gains nothing, copy the file as it is, the same "never larger" rule PNG and JPG follow today. Ruled out: skipping lossy and animated WebP files.
- **Logo in the window header** (user, 2026-11-06, chosen over "only ship the app icon"): show the moon logo next to the "Lunaepix" heading inside the window, made from the same SVG. The approved sketch in the Layout section is binding: logo about 32 px tall, decorative (`alt=""`), the heading text stays as it is. The moon app icon (commit `dd0849e`, already on this branch) ships in the same release.
- This reverses one line of the desktop-app spec ("WebP is not handled here (the sites make WebP on the server)"). Its other decisions stand: lossless only, one worker process per file, `MAX_PIXELS` checked from the header before decoding, hostile inputs in `crates/core/tests/hostile_input.rs`, licences limited to the `about.toml` list, structured files checked with a parser.
- Found while writing this spec (not a user decision): the Jugoo CMS makes its WebP with GD `imagewebp($image, $destination, 100)` (`application/libraries/libImages.php:29`). Quality 100 in GD is **lossy** WebP at top quality, with no metadata. Lunaepix will therefore copy those files unchanged. The saving comes from lossless WebP and from WebP files that carry EXIF/XMP, such as camera, phone and editor exports.

## Problem

Lunaepix skips every `.webp` file ("Not a PNG, JPG or SVG file"). A WebP file can carry wasted bytes in two places. A lossless WebP saved at a fast setting can usually be encoded smaller without changing a pixel. Any WebP can carry EXIF and XMP metadata that the picture does not need.

## Design

### What stays the same

- Lossless. A still image's pixels stay exactly the same. That includes the colour values under fully transparent pixels, which most tools treat as free to change.
- The original is never modified. Output names and folders follow the existing rules: `optimized-<name>` beside a file, `optimized/<name>` inside a folder.
- The output is never larger than the input. If nothing is gained, the original bytes are written, as PNG and JPG do today.
- One worker process per file, the same per-file limits (`MAX_FILE_BYTES`, `MAX_PIXELS`, `WORKER_TIMEOUT`) and the same report row.

### What a WebP file is

A WebP is a RIFF container (`RIFF`, size, `WEBP`, then chunks). It takes one of three shapes:

- **Simple lossy:** a single `VP8 ` chunk.
- **Simple lossless:** a single `VP8L` chunk.
- **Extended:** a `VP8X` header chunk with flags (ICC, alpha, EXIF, XMP, animation), followed by optional `ICCP`, then either one image (`VP8L`, or `VP8 ` with an optional `ALPH` alpha chunk) or `ANIM` plus `ANMF` frames, then optional `EXIF` and `XMP ` chunks.

### How each file is handled

1. **Recognised by extension**, `webp` case-insensitively, in `plan::format_of`, which both the planner and `optimize_file` use. This adds `Format::Webp`.
2. **Content check:** the bytes must start with `RIFF`, a 4-byte size, then `WEBP`. Otherwise the error is "not a WebP file (the content is not WebP data)", matching the PNG and JPG messages.
3. **Chunk walk** in safe Rust, on the same pattern as `png::check_chunks`. It refuses the following with a clear "could not read the WebP data: …" message:
   - a chunk that runs past the end of the file or past the RIFF size;
   - a `VP8X` that is not the first chunk, or a second `VP8X`;
   - a file with no image chunk at all.
4. **Pixel limit from the header, before decoding:** the canvas size comes from `VP8X`, `VP8L` or the `VP8 ` frame header, and goes through `plan::check_pixels`. When a file has both a `VP8X` canvas and an image header, both are checked. That gives the same "image too large" message PNG gives. Animated files are checked too, by canvas size.
5. **Two candidates, smallest wins:**
   - **Stripped:** the original chunks without `EXIF`, `XMP ` and unknown chunks, with the `VP8X` flags updated to match and the RIFF size rewritten. `ICCP` and every image chunk (`VP8 `, `VP8L`, `ALPH`, `ANIM`, `ANMF`) are copied byte for byte. Every WebP gets this candidate.
   - **Re-encoded:** only for a *still, lossless* image (a `VP8L` image, simple or extended, not animated). libwebp decodes it to RGBA. It is re-encoded with libwebp's strongest lossless preset (level 9: method 6, quality 100, what `cwebp -lossless -z 9` uses), with `exact = 1` so the colour under transparent pixels is kept. The result is wrapped with the original `ICCP` when there was one (a `VP8X` with the ICC flag, and the alpha flag when the image has alpha). Before this candidate counts, its own decode must give exactly the same RGBA bytes as the source's decode. If it does not, the file fails with "the re-encoded WebP did not decode to the same pixels", because that would mean a bug.

   The smaller candidate is written. When neither is smaller than the original, the original bytes are written.
6. **Lossy, lossy with alpha, and animated files** only ever get the stripped candidate. Their image data is never decoded or re-encoded. Recompressing the `ALPH` chunk of a lossy file with alpha is out of scope.

### Library

- **libwebp**, Google's reference encoder and decoder, through `libwebp-sys` **0.14.4**. The crate is MIT; it vendors libwebp's C code (BSD-3-Clause, with Google's `PATENTS` grant) and compiles it with `cc`, as `mozjpeg-sys` does, so the Windows runner needs nothing new. Pin it with `=`, like the other codec crates.
- libwebp reports failure through return codes, not through an error callback, so nothing has to unwind through C. The FFI lives in its own module with `#[allow(unsafe_code)]`, and the crate doc in `lib.rs` names it alongside `jpg` and `platform`. The chunk walk, the stripping and the choice of candidate are safe Rust and are testable without libwebp.
- Not chosen: the pure-Rust `image-webp` encoder. It compresses less than libwebp's lossless encoder, so it would rarely beat a file that libwebp wrote in the first place.
- **Licence notice:** `libwebp-sys` declares only MIT, but ships BSD-3-Clause C code (`vendor/COPYING`) and Google's patent grant (`vendor/PATENTS`). The crate package has no `LICENSE` file of its own, so the `libdeflate-sys` pattern cannot be copied exactly. My call:
  - `about.toml` clarifies the crate as `MIT AND BSD-3-Clause`. The BSD part comes from `vendor/COPYING` with its SHA-256 (`5aec868f669e384a22372a4e8a1a6cd7d44c64cd451f960ca69cc170d1e13acf` in 0.14.4). The MIT part uses cargo-about's standard MIT text, because the crate ships no copy of its own.
  - `PATENTS` is not a licence, so it gets a fixed block in `about.hbs`, the same way the IJG sentence is already there (line 18). The block contains the file's text, and a comment beside it gives the file's SHA-256 (`cc3273e0694ea5896145e0677699b53471b03ea43021ddc50e7923fbb9f5023c`) so an upgrade knows to re-check it.
  - Regenerate `THIRD-PARTY-LICENSES.html` afterwards. BSD-3-Clause is already on the accepted list, so the policy does not change. The README's licence section names libwebp.

### Text that changes

| Where | Today | After |
|---|---|---|
| `Skip::UnsupportedType` (`crates/core/src/lib.rs`) | Not a PNG, JPG or SVG file | Not a PNG, JPG, SVG or WebP file |
| Drop-zone hint (`ui/index.html`) | PNG · JPG · SVG | PNG · JPG · SVG · WebP |
| Empty-folder message (`ui/state.js` `noFiles`) | No PNG, JPG or SVG files directly in this folder (subfolders are not included) | No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included) |
| File dialog (`src-tauri/src/lib.rs`) | filter "PNG, JPG and SVG images", extensions without webp | "PNG, JPG, SVG and WebP images", plus `webp`, `WEBP` |
| README first line, crate descriptions, `lib.rs` doc | PNG, JPG and SVG | PNG, JPG, SVG and WebP |
| Window header (`ui/index.html`, `ui/app.css`) | the heading "Lunaepix" alone | the moon logo beside the heading (see Layout) |

The CLI takes `.webp` files with no change to its arguments.

### First step: measure before building (gate)

This repeats the desktop app's Step 2, and it is a **hard gate**: the implementer reports the numbers and stops, and no product code is written until the user has ruled on them. Before that, the implementer measures with libwebp's level-9 lossless preset in a throwaway program and reports:

- the saving on lossless WebP files made from `samples/png` and `samples/jpg` at libwebp's default lossless setting and at its fastest;
- the saving on lossy WebP files with and without an EXIF/XMP block (expected: only the metadata);
- the wall time **and the peak memory** of the whole re-encode candidate (decode, level-9 encode, decode again, compare) at about 25 megapixels, each scaled to `MAX_PIXELS` (100 megapixels). Flag it in the report if the scaled time is over 3 minutes on this machine (a slow laptop is about three times slower, and `WORKER_TIMEOUT` is 10 minutes), or if the scaled memory is over 4 GB (the PNG path takes about 2 GB at 100 megapixels; libwebp has no memory cap like JPG's `MAX_LIBJPEG_MEMORY`).

At the gate the user may also decide that the level-9 gain over libwebp's default is too small to be worth it. In that case only the stripped candidate is built: pure safe Rust, with no libwebp, no FFI module, no C build and no licence change. The plan is written so that this outcome removes whole tasks, not parts of tasks. The implementer does not choose a lower level, a WebP-specific pixel limit or a memory cap on its own; those are the user's call at the gate.

### What is left to the builder

- Whether an extended file left with no flags becomes a simple file. My call: keep `VP8X`, which is simpler and costs 18 bytes.
- What to do with bytes after the RIFF size. My call: they are not part of the image and are dropped from the stripped candidate. An unchanged file is still the original bytes.
- The module layout inside `crates/core/src`, and how the decode, encode and compare steps are split so their failure branches can be reached by a test double.

## Layout

The window stays as approved on 2026-11-04, with two changes: the texts in the table above, and the logo in the header. This sketch was approved by the user on 2026-11-06 and is binding (no mock-up file):

```
+--------------------------------------------+
|  (moon) Lunaepix                           |
|  +--------------------------------------+  |
|  |  Drop images or a folder here        |  |
|  |  PNG · JPG · SVG · WebP              |  |
|  |  [Choose files]   [Choose folder]    |  |
|  +--------------------------------------+  |
+--------------------------------------------+
```

- The logo sits to the left of the "Lunaepix" heading, on the same line and vertically centred with it. It is about 32 px tall, and smaller on the narrow layout where the heading shrinks too (`app.css` already reduces `lunaepixHeader__title` there).
- It is decorative: `<img alt="">`, because the heading next to it already says "Lunaepix". The heading's text and level (`h1`) do not change.
- **Source:** `src-tauri/icons/lunaepix-icon.svg`, the file the app icons are generated from. The window's content security policy allows images from `'self'` only, so a copy goes into `ui/` (for example `ui/lunaepix-logo.svg`) and is loaded with `<img src>`, not inlined and not fetched from elsewhere. My call: a test asserts that the copy is byte-identical to the icon source, so the two cannot drift apart.
- Class names follow BBEM: the image is an element of the header block (for example `lunaepixHeader__logo`). Where the header needs a flex row for the logo and heading, that goes on `lunaepixHeader` or a new element of it, not on the `h1`.

## Tests

Core library (`cargo test --workspace`). The independent decoder in the tests is the `image` crate with its `webp` feature (pure-Rust `image-webp`), added to the dev-dependency, so the pixel checks do not trust libwebp to check itself. Fixtures are generated in the tests from libwebp at its fastest lossless setting, or from `image`, not committed as binaries.

| Behaviour | Positive | Negative |
|---|---|---|
| Planned by extension | `a.webp` and `a.WEBP` are planned as `optimized-a.webp` / `optimized-a.WEBP` | `a.gif`, `a.webpx` and `a.web` are skipped as unsupported (replaces today's `skips_gif_and_webp_as_unsupported_and_plans_nothing`) |
| Content check | a real WebP passes | PNG bytes named `.webp`, and a `RIFF…WAVE` file, return "not a WebP file"; no output is written |
| Lossless re-encode | a lossless WebP saved at the fastest setting comes out smaller, and its RGBA (decoded by `image`) is byte-identical to the input's | a lossy WebP is never re-encoded: its `VP8 ` chunk is byte-identical in the output |
| Hidden colour kept | a lossless fixture with fully transparent pixels whose RGB is not zero keeps that RGB exactly | the fixture's own assertion: it really has alpha-0 pixels with non-zero RGB, so the positive test can fail |
| Metadata stripped | EXIF and XMP are gone; the `ICCP` chunk is byte-identical; the `VP8X` flags match the chunks present | a file without EXIF/XMP gains no chunk and no flag; ICC is never dropped |
| Never larger | a file that is already optimal and has no metadata is written as the original bytes | the same file with a 10 KB EXIF block comes out smaller |
| Animated | EXIF/XMP removed; every `ANMF` chunk byte-identical; every frame decodes (via `image`) to the same pixels | an animated file without metadata is copied as it is |
| Lossy with alpha | after stripping, the `VP8 ` and `ALPH` chunks are byte-identical | the stripped file is not larger than the original |
| Pixel limit | a `VP8X` declaring 20000×20000 is refused as "image too large" within 5 s, before decoding | a header declaring 10000×10000 passes the size check (and then fails as unreadable, not "too large") |
| Hostile chunks (`hostile_input.rs`) | each case below returns a clear error, no panic, within 5 s: a RIFF size past the end of the file, a chunk size of `u32::MAX`, a truncated `VP8L`, a `VP8X` that is not first, a second `VP8X`, a file of only `RIFF` | a real WebP within the limits passes the same walk |
| Re-encode check | the compare step accepts identical RGBA | the compare step refuses RGBA that differs in one byte, and `optimize` returns the "did not decode to the same pixels" error with no output written. This is reached with a double, not by breaking the encoder |
| libwebp failures | — | a decode that returns null, and an encode that returns failure, each give an error for that file and write no output (doubles) |
| End to end (`optimize_file`, worker) | a `.webp` in a folder run lands in `optimized/` with the same name | the source bytes are unchanged afterwards; an unsupported file still fails as before |
| Texts | the skip reason, the drop hint, the empty-folder message and the dialog filter all name WebP | the old "PNG, JPG or SVG" texts are gone (the existing text tests in `src-tauri/tests` and `tests/ui` are updated, not removed) |
| Header logo | `ui/index.html` has exactly one `<img>` in the header, with `alt=""` and a `src` that resolves to a file inside `ui/` (parsed with the existing html5ever-based test) | the same check on a fixture header whose `<img>` lacks `alt`, or points outside `ui/` (an `http:` URL, `../`), fails |
| Logo copy matches its source | `ui/lunaepix-logo.svg` is byte-identical to `src-tauri/icons/lunaepix-icon.svg` | the comparison helper reports a difference for two files that differ in one byte |
| Licence notice | `THIRD-PARTY-LICENSES.html` holds libwebp's BSD-3-Clause text and the `PATENTS` text (checked with the existing html5ever-based test) | the same check run on a fixture HTML without the `PATENTS` block fails; the `about.toml` accepted list is unchanged (the existing `config.rs` check) |

Every failure branch in the new code (each `Err` return, the null-decode and failed-encode paths, the pixel mismatch) has a test that reaches it. The GUI, including how the logo looks beside the heading, is checked by hand on Linux and, through the rc4 build, on Windows. CSS is not tested.

## CMS changes

No CMS changes. Lunaepix is not a Jugoo site.

## Rules check

Checked:  `~/.claude/CLAUDE.md`; `rules/spec-requirements.md` (Layout section with the approved header sketch, wide and narrow layout; no CMS dependency; no money or shared rows; no endpoint); `rules/unit-testing.md` (a positive and a negative per behaviour, failure branches reached with doubles, no mutation runs); `rules/security.md` (no web endpoint or SQL; untrusted file input bounded by `MAX_FILE_BYTES` and `MAX_PIXELS` from the header, with hostile inputs added to the shared test file; worker isolation for crashes inside libwebp); `rules/vanilla-js.md` (one string in `ui/state.js`; no `innerHTML`); `rules/devsync.md` (not a Jugoo site; nothing uploaded); `rules/smarty.md`, `rules/cms-defines-fields.md` (not applicable); `rules/processes.md` (the gate's throwaway program starts no background process); `rules/local-links.md` (link in the reply); `css-conventions.md` (BBEM element of the header block for the logo); `a11y-rules.md` (decorative image with `alt=""`, heading unchanged, no text in the image).
Changed:  added the decorative `alt=""` and the BBEM naming for the header logo; added the failure-branch rows (null decode, failed encode, pixel mismatch) reached with doubles, and the fixture self-check for the hidden-colour test so that test can fail.
