# Lunaepix "Re-add" and thumbnails — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-11

## Handoff

Decided in conversation (2026-11-11). The user's words are quoted as they were said.

- **Spec approved by the user (2026-11-11):** "Otherwise, approved", after one change to the button (next line). Plan: to be written in `docs/pending-approval/plans/`.
- **The button is "Re-add", not "Reuse files":** "We already have clear to clear the list. We can just use readd". Spelled "Re-add"; the command is `readd_list`, the permission `allow-readd-list`, the button `#readdList`.
- **Starting point:** "Tested, and latest build works on windows." The lossy branch was finalized and merged into `main` locally (f54b5a2, not pushed). This work is on `feature/lunaepix-run-again-thumbnails`, from `main` at f54b5a2.
- **Request 1, run the same batch again:** "We can't rerun the same batch right now. We need to reselect all. Possible to just readd all the ones that ran for if one both want a lossy and lossless compression, or different lossless ones."
- **Request 2, preview:** "Is a small preview possible of the image files that will get compressed?"
- **Renaming is out of scope:** "renaming is another part of the roadmap. For a batch job, that is a prefix / postfix, as it is not good to rename all files in a batch job."
- **Numbering next to the originals:** asked "Should 'next to the originals' also number a taken name (`optimized-holiday-1.jpg`) instead of replacing it?" The user answered "Yes".
- **Correction to my own example (2026-11-11):** the existing numbering starts at `-2` (`optimized-holiday-2.jpg`), not `-1` as I wrote in that question; the unnumbered name counts as the first. Put back to the user before approval.
- **Numbering starts at `-1` everywhere:** "Start at -1, as everything does normally on a computer on a duplicate name." This changes the existing numbering too (chosen folder, the command line's `--out`), not only next to the originals.
- **Thumbnail placement:** "Small thumbnail before the filename."
- **Order of work:** asked whether to finish the lossy branch first and start this on a new branch from `main`. The user answered "Yes".
- **Not decided by the user, my calls (listed under "Calls I made", approved with the spec):** the button's place, the command line staying unchanged, and the technical choices under "Thumbnails".

## Problem

After a run, the window shows the results with "Open folder". To run the same files again with other settings (lossy after lossless, or lossy at another quality), every file has to be picked or dropped again: the first add after a run starts a new list.

Next to the originals, a run writes `optimized-<name>` and replaces an earlier one. So even with the files re-added, a second run overwrites the first, and two settings cannot be compared side by side.

The list shows only file names. Nothing shows which picture a name stands for.

## Goals

1. After a run, one button puts the same files back in the list, ready for Run with other settings.
2. Next to the originals, a run never overwrites an earlier output: a taken name gets a number, as in a chosen output folder (`optimized-holiday.jpg`, then `optimized-holiday-1.jpg`, `optimized-holiday-2.jpg`). The numbering itself now starts at `-1` everywhere.
3. Every row shows a small thumbnail before the file name: in the list, during a run and in the results.

## Non-goals

- Renaming outputs, or a prefix or suffix per batch. That is the roadmap item "Export rename".
- The before/after slider. That is the roadmap item "Preview single image compression".
- Running again with one click. The button brings the list back; Run is still pressed by hand after changing the settings.
- Remembering the list between launches.
- Thumbnails in the command line.

## Anti-goals

- **A file path reaches the window.** The window still gets names, numbers and now thumbnail keys, never a path. A thumbnail address names a row, never a file.
- **A broken image takes the app down.** Decoding a hostile or damaged file must not crash the window, just as compression cannot.
- **The list waits for thumbnails.** The list appears at once; thumbnails fill in as they are made.
- **Outputs of the last run sneak into the re-added list.** "Re-add" gives back exactly the files that were in the list, not the `optimized-*` files the run wrote beside them.
- **A wrong picture beside a name.** A thumbnail from an earlier list must never show on a row of a new one.

## Design

### 1. "Re-add"

**Today:** the backend keeps the list after a run and marks it finished. The next add (dialog or drop) clears it first. The window shows the summary and "Open folder" in place of the settings and Run.

**Changes:**

- **New command `readd_list`** (no argument). It clears the list's "finished" mark and sends `list-changed` with the list planned again at that moment. Rows are what they are now: a file deleted since becomes "Not a readable file", as `start_run` already does. It rejects only with `"busy"` (a run is going, or a dialog is open), with a `run-busy` event as for every other command.
- **It works in any state that is not busy.** In a ready state it changes nothing but re-sending the list. With an empty list it sends `{ "files": [], … }`. The window shows the button only after a finished run.
- **The list holds the paths as they were added.** A dropped folder was expanded into its images when it was dropped, so the re-added list does not pick up the `optimized-*` files the run wrote into that folder.
- **After a re-add, adds go to the end of the list** (the "finished" mark is gone), and Clear empties it as today.
- **Window:** the summary row gets a "Re-add" button. Clicking it calls `readd_list`. `list-changed` brings the window back to ready: the list, the settings and Run. The settings keep the mode and quality the window already holds, which are those of the last run. Focus on "Re-add" moves to Run when the summary hides.
- The gate is `canChoose` (not running), like the other list buttons. The button keeps `aria-disabled` while a run is going.

### 2. Numbering: never replace, start at `-1`

**Today:** a chosen output folder uses the no-replace write (`WriteMode::NoReplace`), and a taken name gets a number before the last extension (`atomic::numbered`), starting at `-2`: the unnumbered name counts as the first. Next to the originals the app uses `WriteMode::Replace` (`PreparedRun::options` in `src-tauri/src/session.rs`).

**Changes:**

- **The app always uses `WriteMode::NoReplace`.** Next to the originals: `optimized-holiday.jpg`, then `optimized-holiday-1.jpg`, then `-2`.
- **The numbering starts at `-1` everywhere** (the user's ruling). The first taken name tries `-1`, then `-2`, up to the existing bound `MAX_NAME_NUMBER` (9999). Both places that number names change: the no-replace write (`atomic.rs`, today `numbered(name, 2)` first) and the planner's free name in an output folder (`plan.rs:219`, today `2..=MAX_NAME_NUMBER`). This also changes the chosen folder in the app and the command line's `--out`: a clash there now gives `logo-1.png`, not `logo-2.png`.
- **Every text and test that says `-2` for the first clash** changes with it: `README.md`, the doc comments in `atomic.rs`, `lib.rs` and `plan.rs`, and the tests in `atomic.rs`, `lib.rs`, `crates/core/tests/output_folder.rs`, `crates/cli/tests/cli.rs` and `src-tauri/src/session.rs`. The builder finds them with `git grep -nE -- '-2\.(png|jpg|webp|svg)|numbered\(name, 2\)|2\.\.=MAX_NAME_NUMBER'`.
- Nothing else changes in how names are made.
- The command line keeps replacing next to the originals (my call: scripts rely on a fixed output name; see "Calls I made").
- `INTERFACE.md` and the README say that the app never replaces an output.
- What already happens stays: a folder dropped later lists earlier `optimized-*` files like any image.

### 3. Thumbnails

#### What the window gets

`list-changed` gains one key per file row, `thumb`: a short key for that row's thumbnail, or `null` for a row that will be skipped (and for the over-the-limit row).

```json
{ "files": [
  { "name": "holiday.jpg", "size": 2400000, "skip": null, "thumb": "3-0" },
  { "name": "logo.svg", "size": 4100, "skip": null, "thumb": "3-1" },
  { "name": "notes.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file", "thumb": null }
], "notice": null }
```

After Clear (unchanged apart from the shape it shares):

```json
{ "files": [], "notice": null }
```

What the shapes share: `thumb` is in every file row, always a string or `null`. The window does not take it apart, but it accepts only a string matching `^\d{1,10}-\d{1,4}$` and reads anything else as `null`, so no other text ever reaches an image address.

- The key is `<list>-<index>`. (Not a slash: `convertFileSrc` percent-encodes its argument, so a `/` would arrive as `%2F`; a digit and `-` pass through unchanged.) `list` is a number the backend raises every time a new list starts (Clear, and the first add after a finished run). `index` is the row's place in the list as that `list-changed` sent it. The backend keeps, per `list-changed`, the key → path snapshot of the rows it sent, so the handler never plans the list again per request. Adds keep the earlier rows' keys, and so does "Re-add", so thumbnails already made are not made again.
- The window turns the key into an address with `window.__TAURI__.core.convertFileSrc(key, 'thumb')`. That gives `thumb://localhost/3-0` on Linux and `http://thumb.localhost/3-0` on Windows.
- The run rows take the `thumb` of the list row in their place, the same way they already take its name and size. Done, skipped and failed rows keep the `thumb` of the row they replace.

#### What the backend answers

A custom URI scheme `thumb`, registered on the Tauri builder with an asynchronous handler.

- The path must match `^/(\d{1,10})-(\d{1,4})$` exactly, as received: it is not percent-decoded first. The list number must be the current list's, and the index a row of it whose `thumb` is not `null`. Anything else gets `404` with an empty body. The handler never reads a path or a file name from the address.
- **Found:** `200`, `Content-Type: image/png`, the thumbnail's bytes. For an SVG source: `200`, `Content-Type: image/svg+xml`, the file's own bytes (see below).
- **Not possible** (unreadable, damaged, too large, the worker crashed or timed out): `404`, empty body. The window shows an empty square. Nothing is logged to the window's status line.

Example exchange:

```text
GET thumb://localhost/3-0     → 200, image/png, 2 314 bytes
GET thumb://localhost/3-1     → 200, image/svg+xml, 4 100 bytes
GET thumb://localhost/2-0     → 404 (list 2 is gone)
GET thumb://localhost/3-2     → 404 (notes.txt is a skipped row)
GET thumb://localhost/3%2D0   → 404 (encoded; not a key as received)
GET thumb://localhost/3-x     → 404 (not a key)
```

#### How a thumbnail is made

- **In a worker process, like compression, SVG included.** The app starts the same program it uses for compression workers (the sidecar in a release build, which starts about ten times faster than the app binary) as `<worker> --lunaepix-thumb <src>`. `worker::serve_if_requested` answers the flag, so both binaries do. The window process never opens the source file. A decoder crash takes down only that worker.
- **The worker's answer** on stdout: one line `lunaepix-thumb 1 png` or `lunaepix-thumb 1 svg`, a line feed, then the bytes, exit 0. Or a non-zero exit: `3` for "cannot be shown" (not an image, damaged, over the limits, not a regular file), `2` for a usage error. The parent reads at most 2 MiB plus the header line, and kills the worker after 10 seconds. The handler spawns the worker and waits for it on the same thread, so the worker's tie to its parent (`die_with_parent`, `crates/core/src/platform.rs`) holds.
- **Size:** the longest side is 64 pixels (sharp at 2× display scaling), aspect ratio kept, RGBA PNG. An image smaller than that is not enlarged.
- **Upright:** a JPG or WebP with an EXIF orientation shows the way a photo viewer shows it, for all eight orientation values, mirrored ones included. This needs a small EXIF orientation reader (there is none for JPG yet, and `webp/exif.rs` only answers whether the picture turns); the builder writes it with its own bounds checks.
- **Formats, with the decoders the product already ships** (no new decoding library):
  - JPG: `mozjpeg-sys`, decoding at a reduced scale (1/2 to 1/8) where the size allows, so a large photo is quick.
  - PNG: the `png` crate. An animated PNG shows its default image.
  - WebP: `libwebp-sys`, with its scaled decode. An animated WebP shows its first frame.
  - SVG: no rendering library ships in the product, so the worker passes the SVG file's own bytes through (`svg` header) and the handler serves them as `image/svg+xml`; the webview draws it in an `<img>`. An image element runs no script and loads nothing from outside, and the window's CSP applies as well. An SVG over 2 MiB, or one whose bytes are not UTF-8 text starting (after whitespace or a BOM) with `<`, gets exit 3. The handler serves `image/svg+xml` only for a row whose name ends in `.svg` and a `svg` header, and `image/png` only for a `png` header whose bytes start with the PNG signature; anything else is `404`.
- **Too large:** a source over 100 megapixels gets `404` without decoding.
- **At most two thumbnail workers at a time.** The requests wait their turn; a run's workers are not slowed by more than that.
- **Cache:** the backend keeps the thumbnails of the current list in memory and drops them when a new list starts. A worker that answered exit 3 ("cannot be shown") is remembered, so a broken file is not tried again for every render. A timeout, a kill, a crash, a spawn failure or an over-long answer is not remembered: the next request for that key tries again.

#### What changes in the window's settings

- **CSP:** `img-src 'self' thumb: http://thumb.localhost`. Nothing else in the CSP changes. The config test that guards the CSP is updated to allow exactly these two, and its negative cases still refuse `data:`, `blob:` and any other host.
- **Permissions:** the scheme needs no permission (it is not a command). `readd_list` is a command: it gets `allow-readd-list` (in `build.rs`'s command list and `capabilities/default.json`), and the config test that guards "exactly these commands" names seven.

## Layout

The binding layout is the current window (`ui/index.html`) with these changes.

**Each row of the table:** the name cell starts with a square of 32 × 32 CSS pixels, then the name. The square holds the thumbnail, fit inside the square (`object-fit: contain`) on the table's own background, so a wide picture is letterboxed, not cropped. A row with `thumb: null`, a thumbnail still loading, or one that failed shows the same square empty (a thin border in `--rowLine`), so every name starts at the same place. Text in the other cells stays top-aligned, as today. A note (`kept lossless: …`) stays under the name, lined up with the name, not with the square.

```text
 File                              Size
 ┌──┐ holiday.jpg               2.4 MB
 └──┘
 ┌──┐ logo.svg                    4 KB
 └──┘
 ┌──┐ notes.txt   Will be skipped: not a PNG, JPG, SVG or WebP file
 └──┘                                     (empty square)
```

**The summary row after a run:** the total on the left. On the right, "Re-add" and then "Open folder", side by side, in the same button style.

```text
 Saved 1.8 MB on 2 files            [ Re-add ] [ Open folder ]
```

- At narrow widths (the window's minimum is 360 px), the two buttons wrap under the text, together, in the same order.
- No change to the settings, Run or the drop area.

**Markup and class names (BBEM):**
- The square: `<span class="lunaepixResults__thumb">` inside the name cell, before the name text. The `<img>` inside it: `class="lunaepixResults__thumbImage"`, `alt=""`, `width="32"`, `height="32"`, `loading="lazy"`, `decoding="async"`. It is decorative: the file name beside it says what it is.
- The button: `<button class="lunaepixSummary__button" id="readdList" type="button">Re-add</button>`, before `#openFolder`.
- Changing a row's status (waiting → compressing → done) must not make its thumbnail flicker. Keep the same `<img>` element, or rely on the webview's cache; the builder chooses.

## Tests

CSS is not tested (`unit-testing.md`). Every behaviour below gets a positive and a negative test.

### Backend (`src-tauri`, `cargo test`)

| Behaviour | Positive | Negative |
|---|---|---|
| `readd_list` brings back the list after a run | After a finished run of 3 files, `readd_list` sends `list-changed` with the same 3 names, in order, with their `thumb` keys | Without `readd_list`, the first add after a run still starts a new list with only the new files |
| Adds after a re-add append | After `readd_list`, an add gives the 3 old rows plus the new one | After a run and no re-add, the add gives only the new one (same as above, kept as its own test) |
| The re-added list is the added paths, not the outputs | Drop a folder of `a.png`, run next to the originals (writes `optimized-a.png`), re-add: one row, `a.png` | Drop the same folder again after the run without re-add: two rows, `a.png` and `optimized-a.png` (the existing behaviour, which the re-add must not inherit) |
| Rows are planned again on re-add | A file deleted after the run is "Not a readable file" after re-add, with `thumb: null` | A file still present keeps its `thumb` key |
| `readd_list` is refused while busy | During a run it rejects `"busy"` and sends `run-busy` | When not busy it resolves and sends `list-changed` |
| Numbering starts at `-1` | A first clash gives `-1` in the no-replace write and in the planner (output folder), in the core and through the CLI's `--out`; with `-1` taken too, the next is `-2` | No name ends in `-0`; `-2` is never used while `-1` is free; at `MAX_NAME_NUMBER` taken the file still fails with "no free name" |
| Next to the originals, no replace | Two runs on `a.png` with no output folder leave `optimized-a.png` and `optimized-a-1.png`, the first unchanged | The command line, run twice on `a.png`, leaves one `optimized-a.png` (it still replaces) |
| Thumbnail keys | Rows get `<list>-<index>`; an add keeps the earlier keys | Clear, and the first add after a run, raise the list number; a skipped row and the over-the-limit row get `null` |
| The handler answers only current keys | `thumb://localhost/<current>-<i>` of an image row gives `200 image/png` | An earlier list number, an index past the end, a skipped row's index, `3-x`, `-1`, `3/0`, `3%2D0`, `3-0-1`, an 11-digit list number, a 5-digit index, an extra segment, a query string: each `404` with an empty body |
| The handler never touches a path from the address | A key resolves through the list | An address that looks like a path (`thumb://localhost/%2Fetc%2Fpasswd`, `thumb://localhost/../a.png`) is `404` and no worker is started (a double records no call) |
| Failures become `404` | A worker that answers exit 3 gives `404`, and a second request starts no worker (remembered) | A worker that times out, crashes, cannot be spawned, writes more than 2 MiB plus the header, writes a wrong header, or writes a `png` header with bytes that are not a PNG (a double each) gives `404`, and a second request starts a worker again (not remembered) |
| The worker gets the right file | A request for `<list>-1` starts the worker with row 1's path (a double records the argument), and the handler answers with the bytes the worker wrote | — the double's recorded argument is not row 0's or row 2's path |
| SVG content type | An SVG row whose worker answers `svg` is served as `image/svg+xml` | An `svg` answer for a row not named `.svg` is `404`; a `png` answer is never served as SVG |
| Concurrency | Two requests start two workers | A third waits until one has finished (a double records the order) |
| Cache | A second request for the same key starts no worker | A new list drops the cache: the same index of the new list starts a worker |

### Worker (`crates/core`, `cargo test`)

| Behaviour | Positive | Negative |
|---|---|---|
| Size | A 400 × 200 JPG, PNG and WebP each give a 64 × 32 RGBA PNG | A 20 × 10 image gives 20 × 10 (not enlarged) |
| Upright | A JPG and a WebP with EXIF orientation 6 give a portrait thumbnail; orientation 5 (mirrored) gives the mirrored portrait (pixel check on a corner) | The same picture with orientation 1 or no EXIF stays landscape; a damaged EXIF block is ignored and the thumbnail is still made |
| Animation | An animated PNG and an animated WebP each give their first frame (pixel check) | — the second frame's colour is not in the thumbnail |
| Bounds | An image of exactly 100 megapixels is decoded (header-only fixture where possible) | 100 megapixels + 1 gives a non-zero exit without decoding; a damaged file, an empty file and a text file give a non-zero exit |
| SVG | An SVG under 2 MiB gives a `svg` header and its own bytes | An SVG of 2 MiB + 1 byte, a binary file named `.svg` and a folder named `a.svg` give exit 3, nothing on stdout |
| Usage | `--lunaepix-thumb <src>` is accepted, by the app binary and by the sidecar | No source, two sources, an unknown option: usage error, exit 2, nothing on stdout |

### Window (`node --test`)

| Behaviour | Positive | Negative |
|---|---|---|
| `thumb` is read from `list-changed` | `"3-0"` and `"4294967295-1999"` are kept on the row | A number, an object, an empty string, a missing key, `"3/0"`, `"3-0-1"`, `"../a"`, `"3-x"`, `"thumb://localhost/3-0"` and an 11-digit list number each read as `null` |
| Run rows keep the thumbnail | `run-started` and every `file-done` keep the list row's `thumb` | A row with `thumb: null` stays `null` after `file-done` |
| The row shows a square | A row with a key gets the square with an `<img>` whose `src` is the converted key and `alt=""` | A row with `null` gets the empty square and no `<img>` |
| "Re-add" shows after a run only | Finished: the button shows and is usable | Ready and running: hidden; running: `aria-disabled="true"` if it is ever shown, and a click sends nothing |
| A click sends `readd_list` | Finished: one `readd_list` call | Running: no call |
| Settings survive a re-add | After a lossy 60 run and re-add, the mode is lossy and the quality 60 | A Clear and a new add do not reset them either (existing behaviour, kept) |
| Focus | Focus on "Re-add" moves to Run when the summary hides | Focus on another element does not move |
| The address | The window builds the address with `convertFileSrc(key, 'thumb')` | A row never gets a `src` that is not from `convertFileSrc` (a double records each call) |

### Config (`src-tauri/tests/config.rs`, parsed, not scanned)

| Behaviour | Positive | Negative |
|---|---|---|
| CSP | `img-src` is exactly `'self' thumb: http://thumb.localhost` | `data:`, `blob:`, `*`, `https:` or another host in `img-src` are each refused; every other directive is unchanged |
| Commands | The capability allows exactly the seven commands, `allow-readd-list` among them | A fixture with six (no `allow-readd-list`) or with an eighth is refused |
| The button | `index.html` has `#readdList` with the text "Re-add", before `#openFolder`, inside the summary | A fixture without it, with it after `#openFolder`, or outside the summary is refused |

### Edges the tests cover

- **More content than a demo:** a list of 2000 rows gets a key per row, and the over-the-limit row gets `null`.
- **Optional parts present and absent:** with and without an output folder (numbering both ways); with a list that has only skipped rows (no thumbnails, Run not usable, "Re-add" still brings it back).
- **Bounds:** 64 px exactly and 65 px; 100 megapixels exactly and one more; 2 MiB SVG exactly and one byte more; worker output of 2 MiB plus the header exactly and one byte more.

## Calls I made

These were not settled in conversation; each is my choice, open to change.

1. **The button sits before "Open folder"** in the summary row. (Its label "Re-add" is the user's.)
2. **The command line keeps replacing** next to the originals. Your "Yes" was about the app's window; scripts that run the CLI again expect the same output name.
3. **Thumbnails are made in a worker process,** as compression is, so a damaged image cannot crash the window.
4. **No new decoding library.** The decoders the product already ships cover JPG, PNG and WebP. SVG is shown by the webview itself.
5. **The limits:** 64 px longest side, 32 px on screen, 100 megapixels, 2 MiB for an SVG, two workers at a time, 10 seconds per thumbnail.
6. **The thumbnail worker is the compression worker's program** (the sidecar in a release build), so 2000 thumbnails take seconds, not a minute.
7. **Only "cannot be shown" is remembered;** a timeout or crash is tried again on the next request.
8. **The key is `<list>-<index>`,** not `<list>/<index>`, because `convertFileSrc` encodes a slash.

## Known limit

When a file in the list has become the same file as an earlier row between adding it and Run (a link made in between), the run has one row fewer than the list. That already shifts the names of later rows until their `file-done` arrives; with this change their thumbnails shift too, for the rest of that run. This case needs a link made by hand during that window and is not handled.

## CMS changes

No CMS changes.
