# Lunaepix single image mode: preview, compare and rename — design

**Owner:** Aureus-Lunae · **Started:** 2026-11-11

## Handoff

Decided in conversation (2026-11-11). The user's words are quoted as they were said.

- **Spec approved by the user (2026-11-11):** "Approved, write the plan". The calls listed under "Calls I made" were shown with the design and approved with it. Plan: `docs/in-progress/plans/2026-11-11-lunaepix-single-image.md`.
- **Plan approved by the user (2026-11-11):** "yes", including its calls made while planning. Execute in a fresh session, subagent-driven.
- **Starting point:** the Re-add + thumbnails work was finalized, merged into `main` and pushed (`main` = origin/main at 13715c4, version 0.1.1). This work is on `feature/lunaepix-single-image`, from `main` at 13715c4.
- **Roadmap item:** "Preview single image compression -> This means a gui change. Like in top the option to switch between batch and single image."
- **Start mode:** "Default open single image, still able to select or drop a file in it."
- **What the preview shows:** "How much the selection change, so one can compare. Also the expected file size and savings."
- **Layout:** shown mock-up A (settings beside the picture) and B (settings under it). The user answered "B", then: "But, with an option for A, so both actually, default B. User can switch between it depending on their preferences."
- **Mock-up:** `docs/in-progress/specs/2026-11-11-lunaepix-single-image-mockup.html` (the one the user picked B from) is binding for the arrangement of both layouts; see Layout.
- **Zoom:** "Shrunk to fit the window, as that is the most used case (For websites, one won't notice), but also the option to zoom in."
- **Renaming:** asked "Should renaming be part of this work, or stay its own roadmap item?" The user answered "Part of this work, single mode only". This absorbs the roadmap item "Export rename (single file only)".
- **More than one file dropped:** the user asked "Only 1 image allowed, what do most programs do in that case?" I recommended: load the first image and show a note that the others were left out, with a "Switch to Batch" button that takes all the dropped files into batch mode; a dropped folder loads nothing and gets the same kind of note and button; the file picker picks one file. The user answered "Yes".
- **When the preview updates:** I proposed that the compressed side and the sizes update a moment after the slider is let go (not while dragging), with a "Compressing…" note while the old picture stays until the new one is ready. The user answered "Yes that is fine".
- **Not objected to, but my guesses rather than the user's words** (listed under "Calls I made" below): the expected size is exact because the image is really compressed; Save writes exactly that result; each mode keeps its own content when switching; the layout choice is remembered; the name box starts with today's name; a name the user typed that is taken gets a warning instead of a number.

## Intent

Today Lunaepix works on a list of files and writes them all at once. You only see what lossy compression did after the files are written, and only as numbers. Single image mode lets you load one image, see the original and the compressed version side by side with a divider you drag, see the exact size it will have, and change the settings until you are happy, before anything is written. Then you save it, under a name you choose.

Most images in this app end up on websites, where the picture is shown smaller than it is. So the comparison is made on the whole picture fitted in the window; zooming in to the pixels is there for when you want to look closer.

## Goals

- A switch at the top of the window between **Single image** and **Batch**. The window opens in Single image on every launch.
- In Single image: pick one file or drop one; see original and compressed side by side with a draggable divider; see original size, compressed size and the saving; change Lossless/Lossy and Quality and see the result update; zoom in and move around; save under a chosen name.
- Two layouts for Single image: settings under the picture (B, the default) and beside it (A). The user switches between them.
- Batch keeps working exactly as today.

## Non-goals

- Renaming in Batch. Batch keeps `optimized-<name>` next to the originals and `<name>` in a chosen folder: "For a batch job, that is a prefix / postfix, as it is not good to rename all files in a batch job." (from the Re-add design's Handoff).
- Changing the format (JPG to WebP and so on). The extension of the saved file is the original's.
- Resizing or cropping.
- A preview in Batch.
- The command line: it gets nothing new.
- The look of the whole app ("Clean and clear UI" stays its own roadmap item). Single image uses the colours, fonts and controls the window has today.

## Anti-goals (a failure even if it works)

- The saved file differs from what the preview showed, or from the size it said.
- Saving replaces a file, the original above all. The app never replaces an output today; that stays true.
- The window gets a path. It never sees one today (`src-tauri/INTERFACE.md`); that stays true.
- A large image freezes the window while it compresses.
- An older compression result shows up after a newer one (you set 60, then 80, and the 60 picture arrives last and stays).

## What exists today (read before building)

- `src-tauri/INTERFACE.md` is the window ↔ backend contract: commands resolve to `null` or reject with one bare string; events carry names, numbers and keys, never paths.
- The window (`ui/`) is vanilla JS split into `state.js` (every decision, tested in `tests/ui/state.test.js`), `table.js` (writes to the page, tested), `format.js`, and `app.js` (only wiring). New window logic follows that split: decisions in tested functions, `app.js` only wires.
- The backend (`src-tauri/src/`): `lib.rs` (commands, drops, the `thumb` scheme), `session.rs` (what the commands do), `thumbs.rs` (the scheme's answers), `events.rs` (payloads).
- Compression always runs in a worker process (`crates/core/src/worker.rs`, `run.rs`), so a decoder crash takes down only the worker. Thumbnails use the same binary in `--lunaepix-thumb` mode and are served over the custom `thumb` scheme.
- Output naming: `atomic::write_new` writes without replacing; a taken name gets `-1`, `-2`, … up to `-9999` (`atomic::numbered`, `plan::free_name`).
- Limits in `crates/core/src/plan.rs`: `MAX_FILE_BYTES` (100 MiB), `MAX_PIXELS` (100 megapixels), the animation limits, `WORKER_TIMEOUT` (600 s).
- The window is 760 × 640 at launch, at least 360 × 480. The CSP allows images only from `'self'` and the `thumb` scheme; no inline scripts or `style="…"` attributes.

## How it works

### The switch at the top

- Two choices in the header, "Single image" and "Batch": a radio group in a `fieldset` with a visually hidden legend ("Work on"), styled as two joined buttons. Single image is selected at every launch.
- Each mode keeps its own content. Switching to Batch and back shows the same image, settings and preview; switching to Single image and back shows the same list.
- Mode (Lossless/Lossy), Quality and "Save to" are one set of settings, shared by both modes: what you set in one is set in the other.
- While a batch run is going, the switch cannot be used (it stays focusable and says why, like the other buttons do today). While a single image is being saved, the same.
- The backend knows which mode is showing, because a drop goes to the mode that is showing.
- Making a compressed version is not "busy": switching, choosing, a drop and Save to all still work while it runs. Switching to Batch stops a compressed version that is being made; coming back to Single image asks for it again. So a batch run and a single-image worker never run at the same time.

### Getting an image in

- Single image shows a drop area like Batch's, with "Drop an image here" and a "Choose image" button. The dialog picks one file (the same filter as Batch: PNG, JPG, SVG, WebP; "All files").
- Once an image is loaded, the drop area gives way to the image, its name, its pixel size (`4032 × 3024`) and a "Choose other" button. Dropping a file onto the window still works and replaces the image; it also stops a compressed version still being made for the old image.
- **Loading** reads the file once in a worker process (a new worker mode, beside `--lunaepix-worker` and `--lunaepix-thumb`) that checks Batch's limits and reports the pixel size. The pixel size is the size as shown: an EXIF-rotated JPG gives its upright size, an animated PNG or WebP its canvas size. An SVG has no pixel size, so none is shown (`width`/`height` are `null`). A file over a limit, or one the worker cannot read, is refused with Batch's reason; the current image stays.
- **What was loaded is what is compressed:** at load the backend copies the file into a private temporary folder (only this user can read it; at most `MAX_FILE_BYTES`). Every compressed version, the original side of the picture and the sizes come from that copy, so a file that changes on disk afterwards cannot mix versions. Choose other or a new drop loads the file as it is then.
- **Several items dropped:** the first item that is a PNG, JPG, SVG or WebP file is loaded. A note in the status line says how many were left out ("Single image mode takes one image; 3 others were left out.") with a **Switch to Batch** button. That button switches to Batch and adds *all* the items of that drop to the batch list, under Batch's usual rules (a folder adds the images directly inside it; the first add after a finished run starts a fresh list).
- **How long the offer lasts:** the note and the Switch to Batch button go away, in the window and in the backend, at the next load attempt of any kind (a drop, Choose image, Choose other) and when the user switches mode by hand. After that `move_drop_to_batch` answers `nothing-to-move`. A stale button can never add an old drop's files.
- **Only a folder, or nothing usable, dropped:** nothing is loaded and the current image stays. The note says "Folders go in Batch." (or "Not a PNG, JPG, SVG or WebP file."), with Switch to Batch when there is something to switch with.
- **A file that cannot be compressed** (too large, over the pixel limit, damaged): the same reasons Batch gives, in the status line; the current image stays.

### Comparing

- The picture area shows the original on the left and the compressed version on the right of a vertical divider. Dragging the divider moves it; it starts in the middle. Labels "Original" and "Compressed" sit in the top corners.
- The divider is a real control: it can be focused and moved with the arrow keys, and has a name ("Compare original and compressed").
- The two pictures have `alt` texts: "Original" and "Compressed" (with the setting, "Compressed, lossy quality 75"). When zoomed in, the picture area is a focusable region named "Picture" (`role="region"`, `tabindex="0"`), as `~/.claude/a11y-rules.md` asks for a scrollable region.
- **Fit** (the starting state, and after a new image): the whole picture fits the area, never enlarged above 100%. An SVG has no 100%: it fits the area, and the zoom buttons go up to 8 × Fit.
- The view (zoom, position, divider) resets only when a new image loads, not when settings or the name change.
- **Zoom:** the mouse wheel zooms around the pointer; buttons "Zoom in", "Zoom out", "100%" and "Fit" do the same from the keyboard. When zoomed, dragging the picture moves it, and the arrow keys move it while the picture has focus. Both sides always show the same spot at the same zoom, so the divider compares like with like. Zoom goes from Fit up to 800%.
- No animated zoom or pan when `prefers-reduced-motion: reduce` is set.
- In Lossless mode the two sides look the same, as they should; the sizes still show the saving.
- An animated WebP or PNG plays on both sides, as the browser shows it.
- **Large images:** both sides are served as whole files and decoded by the window's browser engine, off the page's main thread (`img.decode()`), with "Loading picture…" while it decodes; the old picture stays until the new one is decoded, so the window keeps responding. A photo near `MAX_PIXELS` may be slow to appear; the hand test includes one. A smaller stand-in picture for Fit is not part of this work.

### Settings and when the preview updates

- The settings block is Batch's: Lossless/Lossy, Quality (slider and number), Save to with Choose folder and Clear.
- A new compressed version is made when an image is loaded, when Lossless/Lossy changes, when the Quality slider is let go (not while it is dragged), and when the Quality number has been valid and unchanged for a moment after typing (about 400 ms).
- While it is being made, "Compressing…" shows over the compressed side and in the size box; the previous compressed picture and its sizes stay until the new ones are ready.
- A newer request replaces an older one: the older worker is stopped, and its result is never shown. Nothing on screen ever comes from settings other than the ones shown.
- If compression fails, the reason shows in the size box and Save cannot be used until a later attempt works.

### Sizes

- A size box beside the settings (layout B) or under them (layout A): **Original** (bytes, shown like Batch shows sizes), **Compressed**, and **Saves** (percentage and bytes).
- The numbers are exact: the backend really compresses the image, and Save writes those bytes.
- A file that grows (possible in Lossless) shows the negative saving, not clamped, like Batch.
- A note shows when the file was handled differently from the setting, with Batch's texts ("kept lossless: animated PNG", "original kept: lossy was not smaller", …).

### Name and Save

- Under the settings: **Save as** with a text box for the name and the original's extension shown after it, not editable (`[optimized-holiday].jpg`).
- The box starts with the name Batch would use: `optimized-holiday` next to the original, `holiday` in a chosen folder. When that name is taken, it starts with the first free numbered name (`optimized-holiday-1`), as Batch does. It is filled in again when the image or Save to changes (a `name-suggested` event; see Interface), unless the user has typed in it.
- While typing, the window says at once when the name cannot be used, tied to the box: empty or only spaces; a `/ \ : * ? " < > |` or a control character; ending in a space or a dot; a name Windows reserves (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, any case, with or without an extension); longer than 200 characters with the extension. The same rules apply on Linux, so a name that works on one system works on the other. The backend checks the same rules again on Save.
- **Save** writes the compressed bytes shown under that name, in the Save to place. It never replaces a file. If the name is taken, nothing is written and the message says so, tied to the box: "A file named optimized-holiday.jpg already exists there. Choose another name." The user picked that name on purpose, so it is not quietly numbered.
- Save cannot be used while a new version is being made, when the last attempt failed, or while the name is invalid.
- After saving: the status line says "Saved as optimized-holiday.jpg", and **Open folder** opens the folder it went to (the existing `open_output_folder`, now also set by a save). The name box gets the next free name, so pressing Save again does not hit "already exists".
- Save writes the version made from the copy taken at load. A file that changed on disk since is not looked at again until it is loaded again.

### Limits and safety

- The same limits as Batch (`MAX_FILE_BYTES`, `MAX_PIXELS`, the animation and SVG limits, `WORKER_TIMEOUT`), with the same reasons.
- Loading and compression run in a worker process, as in Batch. The window process never opens the file; the window's browser engine only decodes the pictures the backend serves.
- The window gets no path. The original and the compressed version reach it over a new custom scheme (`preview`, beside `thumb`), by key. The CSP's `img-src` gets that scheme added, and nothing else changes in it.
- Kept by the backend: the copy of the loaded original, the original's name and folder (for Save to "next to the original"), and the latest two compressed versions (the shown one and the one being made). An older version is deleted when a newer one is ready, and all are deleted when another image is loaded and when the app closes. They are kept in the private temporary folder named above.

## Interface (proposed; names may change in the plan, shapes may not without saying so)

New commands follow today's rules: each resolves to `null` or rejects with one bare string, and the outcome arrives as events. Each gets its own permission in `capabilities/default.json`, and the test that guards that file is updated.

| Command | Argument | Does | Rejects with |
|---|---|---|---|
| `set_view` | `{ "view": "single" }` or `"batch"` | Tells the backend which mode shows, so drops go there. | `"busy"`, `"invalid-view"` |
| `choose_image` | none | One-file dialog; the outcome arrives as `image-changed`. | `"busy"` |
| `preview_image` | `{ "settings": { "mode": "lossy", "quality": 75 } }` | Makes the compressed version of the loaded image; replaces any request still going. Resolves once started. | `"busy"`, `"invalid-settings"`, `"no-image"` |
| `save_image` | `{ "name": "optimized-holiday", "preview": "7-3" }` | Writes the compressed version with that key under `name` + the original's extension, never replacing. | `"busy"`, `"no-image"`, `"not-ready"` (the key is not the latest ready version), `"invalid-name"`, `"name-taken"`, `"output-missing"` |
| `move_drop_to_batch` | none | Switches to Batch and adds every item of the last single-mode drop that left something out. | `"busy"`, `"nothing-to-move"` |

`invalid-settings` is exactly today's check for `start_run`. `save_image` names the version by key so the bytes written are the bytes shown; the settings are not sent again.

New events. Names, never paths; show every text with `textContent`.

`image-changed`, after every load attempt (a choice, a drop in single mode, "Choose other"), and only then; a new `image.key` means a new image:

```json
{ "image": { "name": "holiday.jpg", "size": 2400000, "width": 4032, "height": 3024,
             "key": "7", "suggested": "optimized-holiday", "extension": ".jpg" },
  "left_out": 0, "notice": null }
```

Several files dropped, the first image loaded:

```json
{ "image": { "name": "holiday.jpg", "size": 2400000, "width": 4032, "height": 3024,
             "key": "8", "suggested": "optimized-holiday", "extension": ".jpg" },
  "left_out": 3, "notice": "Single image mode takes one image; 3 others were left out." }
```

A folder dropped (the current image, if any, stays; `image` is `null` only when there was none):

```json
{ "image": null, "left_out": 1, "notice": "Folders go in Batch." }
```

An SVG:

```json
{ "image": { "name": "logo.svg", "size": 4100, "width": null, "height": null,
             "key": "9", "suggested": "optimized-logo", "extension": ".svg" },
  "left_out": 0, "notice": null }
```

`left_out` above 0 means Switch to Batch is offered. The shape is the same in every case; only the values differ.

`preview-started`, `preview-ready`, `preview-failed`, each for the latest request only:

```json
{ "image": "7", "version": 3 }
{ "image": "7", "version": 3, "key": "7-3", "mode": "lossy", "quality": 75, "before": 2400000, "after": 612000, "note": null }
{ "image": "7", "version": 3, "reason": "the optimiser crashed on this file (it gave no valid result)" }
```

`quality` is `null` for lossless, as in `run-started`. `note` is `null` or one of Batch's note texts.

`image-saved`:

```json
{ "name": "optimized-holiday.jpg", "folder": null, "suggested": "optimized-holiday-1" }
{ "name": "holiday.jpg", "folder": "exports", "suggested": "holiday-1" }
```

`name-suggested`, after `output-changed` while an image is loaded (the image and its versions stay as they are):

```json
{ "image": "7", "suggested": "holiday" }
```

**The `preview` scheme**, built like `thumb` (`convertFileSrc(key, 'preview')`): `/<image>` is the bytes of the copy taken at load, `/<image>-<version>` a compressed version. Keys are accepted only as `^\d{1,10}(-\d{1,10})?$`, with no query, not percent-decoded, and only for the current image and its kept versions; anything else is `404` with an empty body. `Content-Type` follows the format (`image/png`, `image/jpeg`, `image/webp`, `image/svg+xml`).

The existing `open_output_folder` opens the folder of the last save or the last completed run, whichever came last.

## Layout

The mock-up `docs/in-progress/specs/2026-11-11-lunaepix-single-image-mockup.html` is binding for the **arrangement** of both layouts. Its picture, colours and numbers are made up; colours, fonts and control styles come from the window as it is today.

- **Header:** logo and "Lunaepix" on the left, the Single image / Batch switch on the right.
- **Under the header (single image, image loaded):** a row with the file name, the pixel size and "Choose other"; then the layout switch ("Settings under" / "Settings beside") and the zoom buttons (Zoom out, Zoom in, 100%, Fit).
- **Layout B (default):** the picture at full width; below it a row of two blocks, the settings on the left (Compression, Quality, Save to, Save as) and the size box on the right; Save at the bottom right, under both.
- **Layout A:** the picture on the left taking the remaining width; a column on the right (about 16rem) with the settings, then the size box; Save at the bottom right of the window.
- **Narrow window:** below the width where layout A leaves the picture less room than its column (the builder picks it, around 40rem), A shows as B. The choice itself is kept, so A comes back when the window is wider. In B, below about 30rem, the size box goes under the settings.
- **The picture area** keeps the image's proportions and is never taller than the window allows with the settings in view (B) — the settings never scroll out of sight to show the picture.
- **Empty state (no image):** the drop area takes the picture's place ("Drop an image here", "PNG · JPG · SVG · WebP", Choose image); settings show, Save and the size box do not.
- **The status line** (the existing `role="status"` region) carries the notes: left out, folders, saved, reasons.
- **Batch:** unchanged, under the same header.

## Tests

`~/.claude/rules/unit-testing.md` applies to the Rust and the JS: every behaviour gets a positive and a negative test, failure branches each get a test, CSS gets none. Each implementer writes a test table and a failure-branch table before code.

**Window (node tests, `tests/ui/`):**

- The switch: starts on Single image; switching keeps each mode's content; locked during a batch run and during a save, free again after; a locked switch does nothing when pressed.
- Name check, accepted: `optimized-holiday`, `holiday 2`, a name of exactly 200 characters with the extension, non-ASCII (`vakantie-café`). Refused: empty, `"   "`, each of `/ \ : * ? " < > |`, a control character, a trailing space, a trailing dot, `CON`, `con`, `Con.txt`, `COM1`, `LPT9`, 201 characters with the extension.
- Suggested name: fills in on load and on Save to change; not overwritten after the user typed; becomes the next free name after a save.
- Preview requests: one on load, one on mode change, one on slider release and none while dragging, one after the number is valid and still for the delay and none for an invalid number (`101`, `abc`, empty); quality at 0 and at 100 are sent as such.
- Stale events: a `preview-ready` for an older version or another image changes nothing on screen; the latest one does.
- Save gate: allowed with a ready version and a valid name; refused while compressing, after a failure, with an invalid name, with no image. The `save_image` argument carries the shown version's key.
- Rejections: each new rejection string gives its message (`name-taken` tied to the name box, `nothing-to-move`, …); an unknown string gives the generic message.
- View reset: a new image key resets zoom, position and divider; `name-suggested`, a settings change and a `preview-ready` do not.
- Picture loading: "Loading picture…" shows while a new picture decodes and the old one stays; a decode that fails shows the reason and keeps the old picture.
- Zoom maths: Fit scale for a wide, a tall and a tiny (1 × 1) image never above 100%; zoom clamps at Fit and at 800%; panning clamps at the picture's edges; zoom around the pointer keeps the point under the pointer.
- Layout choice: default B; a stored A is used; a storage read or write that throws, or a stored value that is neither, falls back to B without breaking the window.
- Divider: arrow keys move it, it stays between 0 and 100%.

**Backend (Rust):**

- `set_view`: a drop in Single image goes to the image, in Batch to the list (positive and negative for each). An unknown view is refused.
- Drops in Single image: one image loads; several give the first image and `left_out`; a non-image first and an image second loads the image; a folder loads nothing and keeps the current image; only non-images load nothing; `move_drop_to_batch` adds exactly the items of that drop to the batch list and refuses with `nothing-to-move` when the last drop left nothing out.
- Preview: the result's sizes equal the bytes served and the bytes saved; a newer request stops the older worker and only the newer one sends `preview-ready`; a worker crash, timeout and unreadable file each send `preview-failed`; quality 0 and 100 both work; a lossy request on an SVG and on an animated PNG gives Batch's behaviour and note.
- Limits: an image at `MAX_PIXELS` loads, one pixel over is refused with Batch's reason; the same at `MAX_FILE_BYTES`.
- Save: writes exactly the previewed bytes under the name plus the extension; never replaces (a taken name gives `name-taken` and leaves the existing file byte for byte as it was, the original included); each invalid name from the window list is refused here too; a stale key gives `not-ready`; a missing Save to folder gives `output-missing`; a failed write leaves no partial file.
- Load: the pixel size of a plain JPG, an EXIF-rotated JPG (upright size), an animated WebP (canvas size); an SVG gives no size; an unreadable file and a damaged file are refused with Batch's reason and keep the current image; the copy is taken, and changing the file on disk after load changes neither the served original nor a new compressed version; the copy is removed on the next load and on close.
- The Switch to Batch offer: present after a drop that left something out; gone after the next drop, a choice, Choose other and a mode switch by hand (`nothing-to-move` each time).
- Busy: a compressed version being made does not make `set_view`, `choose_image`, a drop or `choose_output_folder` busy; switching to Batch stops its worker and no `preview-ready` follows; a drop of a new image stops the old image's worker.
- `name-suggested`: sent on `output-changed` with an image loaded, not without one; the image key and the kept versions stay.
- The `preview` scheme: the current image's original and kept versions answer `200` with the right type; an older image's key, a deleted version, an encoded key, a query, and a malformed key answer `404`.
- Clean-up: older versions are deleted when a newer one is ready, all on a new image and on close.
- Config: the CSP has exactly the `preview` scheme added; the permission list has exactly the new commands added.

**Hand test (Linux and Windows):** a file with a very long name (150 characters) in both layouts at 360 px wide: it wraps, nothing scrolls sideways; a large phone photo (fit, zoom to 100% and 800%, pan, divider by mouse and keys); lossy 20 / 75 / 100 and lossless; a photo near 100 megapixels (the window keeps responding while it loads); a transparent PNG; an animated WebP; an SVG; dropping three files and a folder, then Switch to Batch; a taken name; Save twice; switching layouts and restarting the app (A is remembered); the window at 360 px wide in both layouts; Batch still works as before.

## Calls I made (check these while reading)

- The expected size is exact, not an estimate: the image is really compressed, and Save writes those bytes.
- Each mode keeps its own content when switching; Lossless/Lossy, Quality and Save to are shared between the modes.
- The layout choice is remembered between launches, in the window's own storage. This is the first setting that is remembered (`INTERFACE.md` says "Nothing is remembered between launches"), so that sentence changes. Everything else is still not remembered.
- The window opens in Single image on every launch, not in the mode last used.
- The name box starts with Batch's name, numbered when taken; a typed name that is taken is refused with a message, not numbered.
- Names are checked against Windows' rules on every system.
- "Switch to Batch" moves all the items of the drop, the loaded image included, so the batch list matches what was dropped.
- Save to is shared with Batch; there is no separate Save to for Single image.
- Zoom reaches 800%.
- The file is copied at load and everything works from that copy, so a file changing on disk cannot mix versions (instead of checking it again at Save).
- Loading reads the file in a new worker mode that checks the limits and gives the pixel size; an SVG shows no pixel size.
- Both sides are served as whole files and decoded off the main thread; no smaller stand-in picture for Fit. If a 100-megapixel photo turns out too slow in the hand test, a stand-in is a follow-up.
- Making a compressed version is not "busy"; switching to Batch stops it.
- The Switch to Batch offer lasts until the next load attempt or a mode switch by hand.
- Version: raised to 0.1.2 in the workspace `Cargo.toml`, in the plan's last task.

## Left to the builder

- Command, event and scheme names, as long as the shapes above hold or the plan says why not.
- The exact debounce, the narrow-width breakpoints, and the wheel zoom step.
- How the window draws the divider and the zoom (one image clipped over the other, or two layers).
- How `set_view` and Batch's busy state share one lock.
