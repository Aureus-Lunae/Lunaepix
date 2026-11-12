# Window ↔ backend interface

The contract between the window (`ui/`, vanilla JS) and the Rust backend
(`src-tauri/src/lib.rs`, with the logic in `session.rs` and the payloads in
`events.rs`; Single image mode's state in `single.rs`; the thumbnails in
`thumbs.rs`). The window never sends a path, and it never receives one: it
gets file names, the output folder's name, numbers, thumbnail keys and image
keys.

The window has two modes, Single image and Batch, and opens in Single image
at every launch. The backend is told which one shows (`set_view`), because a
drop goes to the mode that shows.

Use the global API (`app.withGlobalTauri` is on):

```js
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
```

## The run settings

A run has three settings, fixed when Run is pressed. The window keeps mode
and quality itself and sends them with `start_run`; the backend keeps the
list and the output folder.

| Setting | Values | Starts as (every launch) |
|---|---|---|
| Mode | `"lossless"`, `"lossy"` | `"lossless"` |
| Quality | integer 0–100 | 75 (used only when mode is `"lossy"`) |
| Output | next to the originals, or one chosen folder | next to the originals |

Nothing is remembered between launches, except one choice the window keeps
itself: where Single image mode shows the settings (under or beside the
picture), in the window's own storage (`localStorage`, key
`lunaepix.layout`). The backend stores nothing.

## Commands

Each resolves to `null` or rejects with one bare string. Only `start_run`,
`preview_image`, `save_image` and `set_view` take an argument.

| Command | Argument | Does | Rejects with |
|---|---|---|---|
| `choose_files` | none | Opens a native multi-file dialog (PNG, JPG, SVG, WebP; "All files" as a second filter). The files picked are added to the list. Resolves as soon as the dialog is open; the outcome arrives as events. | `"busy"` |
| `clear_list` | none | Empties the list. | `"busy"` |
| `readd_list` | none | After a run, puts the files that were in the list back: sends `list-changed` with the list as it was added, planned again now. It adds nothing and removes nothing; it only ends the "run has finished" state, so the next add appends. | `"busy"` |
| `choose_output_folder` | none | Opens a native folder dialog; the folder picked becomes the output folder. Resolves as soon as the dialog is open. | `"busy"` |
| `clear_output_folder` | none | Back to "next to the originals". | `"busy"` |
| `start_run` | `{ "settings": { "mode": "lossy", "quality": 75 } }` | Runs the list with these settings and the output setting. It writes next to the originals (`optimized-a.png`) or into the chosen folder (`a.png`), and never replaces an existing output: a taken name gets a number before the extension, from `-1` (`optimized-a-1.png`, `a-1.png`), then `-2`, up to `-9999`; when all of those are taken the file fails ("no free name in the output folder"). Resolves once the run has started; its progress arrives as events. | `"busy"`, `"invalid-settings"`, `"empty-list"`, `"output-missing"` |
| `open_output_folder` | none | Opens, in the file manager, the folder of the last save or the output folder of the last completed run, whichever came last. For a run that is the chosen output folder, or, with none, the folder of the first file written; for a save, the folder it wrote to. | `"no-output"`, `"open-failed"` |
| `set_view` | `{ "view": "single" }` or `{ "view": "batch" }` | Tells the backend which mode shows, so a drop goes there. Ends the Switch to Batch offer (see [Single image mode](#single-image-mode)). Sends no event of its own; when it answers `"busy"`, `run-busy` goes out as for every command. | `"busy"`, `"invalid-view"` |
| `choose_image` | none | Opens a native one-file dialog with the same filters as `choose_files`; the file picked is loaded in Single image mode. Resolves as soon as the dialog is open; the outcome arrives as `image-changed`. | `"busy"` |
| `preview_image` | `{ "settings": { "mode": "lossy", "quality": 75 } }` | Makes the compressed version of the loaded image with these settings, in a worker, as Batch would write it (see [Previews](#previews-the-preview-scheme)). A request still going is stopped first; only the latest request's outcome arrives, as `preview-ready` or `preview-failed`. Resolves once `preview-started` has gone out. | `"busy"`, `"invalid-settings"`, `"no-image"` |
| `save_image` | `{ "name": "optimized-holiday", "preview": "7-3" }` | Writes the kept compressed version with that key (the one the window shows) as `name` + the original's extension, in the chosen output folder or, with none, next to the original. Never replaces a file and never numbers the name. Resolves once the file is written and `image-saved` has gone out (see [Single image mode](#single-image-mode)). | `"busy"`, `"invalid-name"`, `"no-image"`, `"not-ready"`, `"output-missing"`, `"name-taken"`, `"save-failed"` |
| `move_drop_to_batch` | none | Switch to Batch: the view becomes Batch and every item of the last Single image drop that left items out is added to the list, exactly as a drop on Batch adds them (`list-changed`). | `"busy"`, `"nothing-to-move"` |

```js
await invoke('start_run', { settings: { mode: 'lossy', quality: 75 } });
await invoke('set_view', { view: 'batch' });
await invoke('preview_image', { settings: { mode: 'lossless', quality: 75 } });
await invoke('save_image', { name: 'optimized-holiday', preview: '7-3' });
```

- `"busy"`: a run is active, a dialog for one is open, an image is being
  loaded, or an image is being saved. A `run-busy` event goes out as well. Every command but
  `open_output_folder` can be busy. Accepting a `preview_image` request
  also holds the guard for a moment (through stopping the previous
  request's worker and sending `preview-started`), so a command in that
  moment answers `"busy"` too; a retry works.
- `"invalid-view"`: the argument is not exactly `{ "view": "single" }` or
  `{ "view": "batch" }`: another spelling (`"Single"`), another type, `null`,
  a missing or an extra key is refused, and the view stays as it was.
- `"no-image"`: `preview_image` or `save_image` was called while no image
  is loaded.
- `"not-ready"`: `save_image`'s `preview` is not the key of the version
  kept for the current image: none is ready yet, the last attempt failed
  and none was ready before, a newer version has become ready, or the key
  is another image's. It is compared as text with the `key` of the last
  `preview-ready` (so `"07-3"` is refused). Also when, after that check
  but before the save read the version, a newer version became ready or
  the window closed (either removes the version's file). Once the save has
  read the version, neither stops it: the save goes on and succeeds.
  Nothing is written when it answers `"not-ready"`.
- `"invalid-name"`: `save_image`'s `name` breaks a name rule (see
  [Single image mode](#single-image-mode)), or the argument is not exactly
  an object with the two string keys `name` and `preview` (a missing or an
  extra key, another type, `null`).
- `"name-taken"`: a file or a folder named `name` + the extension already
  exists in the folder `save_image` would write to (the original itself
  included). Nothing is written, nothing is replaced, and the name is not
  numbered: choose another.
- `"save-failed"`: the file could not be written for another reason (a
  full disk, a folder that refused the write). Nothing is written.
- `"nothing-to-move"`: there is no drop to move: the last Single image drop
  left nothing out, or its offer has ended.
- `"invalid-settings"`: `settings` is missing or `null`, or is not exactly
  an object with the two
  keys `mode` (`"lossless"` or `"lossy"`) and `quality` (a whole JSON number
  from 0 to 100). A float (also `75.0`), a string (`"75"`), `null`, a
  missing or an extra key, or another mode word is refused. Send `quality`
  also for a lossless run (the current value, 75 unless changed); it is
  ignored there.
- `"output-missing"`: the chosen output folder is gone, is no longer a
  folder, or cannot be written to; for `save_image` with no chosen folder,
  the same for the original's own folder. For `save_image` it is also a
  folder removed after its check but before the write. `start_run` and
  `save_image` do not create it again.
  A folder deleted once the run has started is created again by the next
  file written into it.
- `"empty-list"`: no file in the list can be compressed (the list is
  empty, or every row is skipped).
- `"no-output"`: nothing was saved and no completed run wrote anything
  yet, or the last completed run (after any save) wrote nothing. `"open-failed"`: the
  system could not open the folder, for example because it was removed.

`start_run` checks, in this order: busy, the settings, the output folder,
then the list. The list is planned again at that moment, so a file deleted
since it was added runs as a skipped row ("Not a readable file") and the
rest still runs. A row that is a folder by then (a dropped folder that
could not be read, or a path that did not exist when it was added) stays
one "Not a readable file" row: the run never looks inside it. To add its
images, drop the folder again. Nothing runs when it rejects.

`preview_image` checks, in this order: busy, the settings, then the image.
Nothing starts when it rejects.

`save_image` checks, in this order: busy, the argument's shape
(`"invalid-name"`), the image, `preview` (`"not-ready"`), the name rules
(`"invalid-name"`), then the folder (`"output-missing"`); the write itself
can then give `"name-taken"`, `"output-missing"` (the folder went away
since its check) or `"save-failed"`. Nothing is written when it rejects.

A dialog closed without a choice changes nothing and sends `run-cancelled`.

Drag and drop needs no command. A drop goes to the mode that shows (the
last `set_view`; Single image at launch). In Batch it adds to the list,
exactly like `choose_files` with those paths. A dropped folder adds the PNG,
JPG, SVG and WebP files directly inside it (not its subfolders), earlier
`optimized-*` outputs included. In Single image it loads an image (see
[Single image mode](#single-image-mode)). While a run is going, a drop is
refused with `run-busy`.

A file is listed once, however often it is added. When a run has finished,
the list is marked finished: the first add after that starts a fresh list
(only the new files are in it, and its thumbnails get a new list number).
`readd_list` clears that mark, so the next add appends to the list instead.
At any other time adds go to the end of the list.

`readd_list` gives back the paths that were added, not the outputs the run
wrote beside them (`optimized-*`): dropping the folder again after a run does
list those too, a re-add does not. The list is planned again at that moment,
so a file deleted since the run is a skipped row ("Not a readable file"), as
in `start_run`. Called with an empty list, it sends an empty list. The
window's settings (mode, quality) and the output folder are not touched.

## Single image mode

**Loading.** A choice (`choose_image`) or a drop while Single image shows is
a load attempt. The backend copies the file into a private folder (only
this user can read it; at most 100 MiB, Batch's limit, checked before the
copy) and a worker process reads the copy's header: Batch's limits, and the
size the picture is shown at. Everything later works from that copy, so a
file that changes or goes away on disk afterwards changes nothing until it
is loaded again. A loaded image gets a new key: `"1"` for the first of a
launch, then one more each time. The old image's copy is removed then.

**What a drop loads.** The first dropped item that is a file with a PNG,
JPG, SVG or WebP extension (any case). The other folders and image files
are left out and counted in `left_out`; other files are not counted (Batch
cannot use them either). Nothing else is tried when the first image is
refused.

| Dropped | Loads | `left_out` | `notice` |
|---|---|---|---|
| one image | it | 0 | `null` |
| an image and a text file | the image | 0 | `null` |
| a text file, then an image | the image | 0 | `null` |
| two images | the first | 1 | "Single image mode takes one image; 1 other was left out." |
| three images | the first | 2 | "Single image mode takes one image; 2 others were left out." |
| a folder (and text files) | nothing | the folders | "Folders go in Batch." |
| only text files | nothing | 0 | "Not a PNG, JPG, SVG or WebP file." |

A load that is refused (Batch's reason for the file: too large, over the
pixel limit, damaged, not a readable file; or "the optimiser crashed on
this file (it gave no valid result)" when the worker gave no answer) keeps
the current image, and the reason is the `notice`. A choice of a file that
is not a PNG, JPG, SVG or WebP gives "Not a PNG, JPG, SVG or WebP file."

**Switch to Batch.** After a drop with `left_out` above 0, the backend
keeps that drop for `move_drop_to_batch`: all its items, the loaded image
included. The offer ends at the next load attempt (a drop while Single image
shows, a file picked in the image dialog) and at every `set_view`; after
that `move_drop_to_batch` rejects with `"nothing-to-move"`. A closed image
dialog does not end it. The image stays loaded when Batch shows.

**Making a version.** `preview_image` makes the compressed version of the
loaded image from its copy, with the settings given, exactly as a Batch run
would write that file (same mode, quality, notes and reasons). Each request
gets a version number: `1` for the first of a launch, then one more for each
request, whatever the image. Only the latest request counts: a new
`preview_image` stops the worker of the one before, and the backend sends
nothing for the one before after it has sent the new `preview-started`.
Making a version is not busy: other commands and drops work meanwhile.
These stop it: `set_view` to Batch, `move_drop_to_batch`, `start_run`, a
new image loaded, and the window closing. From that moment the backend
sends nothing for the request. Its outcome may have been sent just before;
then it was sent before the stopping call's own event (`image-changed`,
`list-changed`, `run-started`). All of these events are sent off the main
thread, through Tauri's one event queue, so they normally arrive in that
order too; the window still checks each outcome (see
[Rules for the window](#rules-for-the-window)). A load attempt that loads
nothing does not stop it. The
backend keeps one version per image: the last that succeeded. A failure
keeps the version kept before; a new image forgets it.

**Saving.** `save_image` writes the kept version named by `preview` (the
`key` of the `preview-ready` the window shows), byte for byte, as `name` +
the original's extension as it is spelled (`.JPG` stays `.JPG`), with the
original's permissions. It goes to the chosen output folder, or with none
to the original's folder. It never replaces anything: a name that is taken
is `"name-taken"`, also when a file appears there at the last moment, and
the name is never numbered. The name rules are the window's (`ui/names.js`),
checked again here: not empty or only spaces; none of `/ \ : * ? " < > |`
and no control character; not ending in a space or a dot; not a name
Windows reserves (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`,
any case, with or without an extension); at most 200 characters and 255
bytes (UTF-8) with the extension. The name is written exactly as typed
(`vakantie-café` is not normalised). Saving is busy: the guard is held
until `image-saved` has gone out, so no other command, drop or run starts
meanwhile; a version still being made goes on and may become ready. After a
save, `open_output_folder` opens the folder written to, until a later save
or completed run. Closing the window does not wait for a save: a save that
has read its version finishes while the app is still running, but if the
app exits during the write the file may be missing, and a hidden staging
file (`.tmp` and six characters) may be left in the target folder.

## Events

Every payload is JSON data. Show names, reasons, notes and notices with
`textContent`, never with `innerHTML`. A file or folder name may contain any
character.

### `list-changed`

Sent after every add (the first add after a finished run starts a fresh
list), every Clear and every `readd_list`: the full list each time, in
display order.

- `name`: the file name alone, never a path.
- `size`: in bytes, or `null` for a skipped row.
- `skip`: why the row will be skipped, a sentence for the user; `null` for a
  row that will be compressed.
- `thumb`: the key of the row's thumbnail, `<list>-<index>` (for example
  `"3-0"`), or `null` for a skipped row and for the over-the-limit row. See
  [Thumbnails](#thumbnails-the-thumb-scheme).
- `notice`: a sentence for the status line, or `null`. It says why an add
  changed nothing: a dropped folder with no images directly in it. (The
  file dialog picks files only, so a folder arrives by a drop.)

```json
{ "files": [
  { "name": "holiday.jpg", "size": 2400000, "skip": null, "thumb": "3-0" },
  { "name": "logo.svg", "size": 4100, "skip": null, "thumb": "3-1" },
  { "name": "notes.txt", "size": null, "skip": "Not a PNG, JPG, SVG or WebP file", "thumb": null }
], "notice": null }
```

`thumb` is in every file row, always a string or `null`.

After Clear:

```json
{ "files": [], "notice": null }
```

A dropped folder with no images, onto an empty list:

```json
{ "files": [], "notice": "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)" }
```

A run compresses at most 2000 files. The files over that limit do not get a
row each: after every other row comes one row named `(over the limit)` with
`size: null`, whose `skip` counts them ("1 more file not optimised: over the
2000-file limit", or "3 more files ..."). The run ends with the same kind
of row.

The run plans the list again when Run is pressed, so its rows are the
list's rows in the same order, each as things stand at that moment. While
nothing on disk changed since the last `list-changed`, they are the same
rows. Otherwise a row can differ: a file deleted since becomes "Not a
readable file", a file that appeared at a missing path is compressed, and a
row whose file is now the same file as an earlier row's (through a link
made since) has no run row of its own. The run never has a row the list
did not show, and never runs a file inside a folder row.

### `output-changed`

The output setting changed. `folder` is the chosen folder's name (never its
path; empty for a folder without a name, such as a drive root), or `null`
for next to the originals.

```json
{ "folder": "exports" }
{ "folder": null }
```

With an image loaded in Single image mode, a `name-suggested` follows.

### `image-changed`

After every load attempt in Single image mode (a file picked in the image
dialog, or a drop while Single image shows), and only then. `image` is the
image that shows now: the new one, or, when nothing was loaded, the one that
was there (same `key`), or `null` when there is none. A new `key` means a new
image. `left_out` above 0 means Switch to Batch is offered. The shape is the
same in every case; only the values differ.

- `name`: the original's file name alone. `size`: bytes of the copy taken
  at load. `width`, `height`: the size it is shown at (an EXIF-rotated JPG
  gives its upright size, an animation its canvas); `null` for an SVG.
- `key`: the image's key, a whole number as text.
- `suggested`: the name Save as starts with, without the extension: Batch's
  output name (`optimized-holiday` next to the original, `holiday` in a
  chosen folder), or its first free numbered form when that is taken
  (`optimized-holiday-1`). A name that is not valid text is shown with
  `�` in place of the bytes it cannot show.
- `extension`: the original's extension with its dot, as it is spelled.
- `notice`: a sentence for the status line, or `null`.

```json
{ "image": { "name": "holiday.jpg", "size": 2400000, "width": 4032, "height": 3024,
             "key": "7", "suggested": "optimized-holiday", "extension": ".jpg" },
  "left_out": 0, "notice": null }
{ "image": { "name": "holiday.jpg", "size": 2400000, "width": 4032, "height": 3024,
             "key": "8", "suggested": "optimized-holiday", "extension": ".jpg" },
  "left_out": 3, "notice": "Single image mode takes one image; 3 others were left out." }
{ "image": null, "left_out": 1, "notice": "Folders go in Batch." }
{ "image": { "name": "logo.svg", "size": 4100, "width": null, "height": null,
             "key": "9", "suggested": "optimized-logo", "extension": ".svg" },
  "left_out": 0, "notice": null }
```

### `name-suggested`

After `output-changed` while an image is loaded (none without one): the
name Save as starts with for that image in the new output place. The image
and its key stay as they are.

```json
{ "image": "7", "suggested": "holiday" }
```

### `preview-started`

`preview_image` was accepted: version `version` of image `image` is being
made, and is now the latest request. Exactly one `preview-ready` or
`preview-failed` follows for it, unless it is stopped (see
[Single image mode](#single-image-mode)) or a newer request starts.

```json
{ "image": "7", "version": 4 }
```

### `preview-ready`

The latest request's version is ready and is the version kept for the
image. `key` is its key for the `preview` scheme, `<image>-<version>`.
`mode` and `quality` are the request's settings; `quality` is `null` for
lossless. `before` is the copy's size and `after` the version's, in bytes;
`after` can be larger than `before` in lossless mode. `note` is Batch's note
(the same sentences as `file-done`'s, for example "original kept: lossy was
not smaller") or `null`; the key is always there. The backend sends only
the latest request's outcome, and never sends it after a newer request's
`preview-started` or a new image's `image-changed`. Show it only when it
passes the window's check (see
[Rules for the window](#rules-for-the-window)): its `image` is the key of
the current `image-changed`, and its `version` is the last
`preview-started`'s. Otherwise drop it.

```json
{ "image": "7", "version": 4, "key": "7-4", "mode": "lossy", "quality": 75,
  "before": 2400000, "after": 610000, "note": null }
{ "image": "7", "version": 5, "key": "7-5", "mode": "lossy", "quality": 60,
  "before": 512000, "after": 498000, "note": "kept lossless: animated PNG" }
{ "image": "7", "version": 6, "key": "7-6", "mode": "lossless", "quality": null,
  "before": 512000, "after": 498000, "note": null }
```

### `preview-failed`

The latest request gave no version. `reason` is Batch's reason for that
file, a sentence for the user (for example "the optimiser crashed on this
file (it gave no valid result)"). The version kept before, if any, stays
and is still served.

```json
{ "image": "7", "version": 6, "reason": "the optimiser crashed on this file (it gave no valid result)" }
```

### `image-saved`

`save_image` wrote a file. `name` is the saved file's name with its
extension; `folder` is the chosen output folder's name (as in
`output-changed`), or `null` when it went next to the original. `suggested`
is the name for the Save as box now, without the extension: the first free
numbered form of the saved name in that folder (`beach` → `beach-1`, or
`beach-2` when `beach-1` is taken; `beach-2` → `beach-2-1`), so pressing
Save again does not hit `"name-taken"`.

```json
{ "name": "optimized-holiday.jpg", "folder": null, "suggested": "optimized-holiday-1" }
{ "name": "holiday.jpg", "folder": "exports", "suggested": "holiday-1" }
```

### `run-started`

A run starts. `total` is the number of `file-done` events that follow,
skipped rows and the over-limit row included. `mode` and `quality` are the
run's settings; `quality` is `null` for a lossless run.

```json
{ "total": 3, "mode": "lossy", "quality": 75 }
{ "total": 3, "mode": "lossless", "quality": null }
```

### `file-started`

The file at `index` is being compressed now. Exactly one per row that is
compressed, before its `file-done`; none for a skipped row.

```json
{ "index": 1 }
```

### `file-done`

One per row, in order, `index` from 0 to `total - 1`. `status` is
`"done"`, `"skipped"` or `"failed"`.

- `done`: `before` and `after` in bytes, `millis` in ms. No `reason`. A
  `note`, a sentence for the user, only when the file was handled
  differently from the run's mode; otherwise there is no `note` key.
- `skipped` and `failed`: `reason`, a sentence for the user. No sizes, no
  note.

`name` is the file name alone, never a path. It is empty only for a path
with no file name.

The notes are exactly: "kept lossless: animated PNG", "kept lossless: CMYK
JPG", "kept lossless: animation too large for lossy" and "original kept:
lossy was not smaller".

A file whose worker was stopped because the window closed is `failed` with
the reason "the run was stopped"; no later file runs.

```json
{ "index": 0, "name": "holiday.jpg", "status": "done", "before": 2400000, "after": 610000, "millis": 1210 }
{ "index": 1, "name": "anim.png", "status": "done", "before": 512000, "after": 498000, "millis": 380, "note": "kept lossless: animated PNG" }
{ "index": 2, "name": "notes.txt", "status": "skipped", "reason": "Not a PNG, JPG, SVG or WebP file" }
{ "index": 3, "name": "broken.png", "status": "failed", "reason": "the optimiser crashed on this file (it gave no valid result)" }
```

A file can grow in a lossless run (`after > before`). Show the negative
saving and do not clamp it.

### `run-finished`

The run is over. `optimised` counts the `done` files, and `saved_bytes` is
their total saving (a file that grew counts 0). `has_output` says whether
`open_output_folder` has a folder to open. Before this event goes out, the
backend stores the output folder, so "Open folder" works as soon as the
event arrives. The run is freed right after the event goes out. In the
moment between the two, a command or a drop gets `"busy"` / `run-busy`; a
retry works.

```json
{ "optimised": 2, "saved_bytes": 341210, "has_output": true }
```

Every `run-started` is followed by exactly one `run-finished`. That holds
even when the run breaks off inside the backend. Then fewer than `total`
`file-done` events arrive, and the summary counts only those. `has_output`
is true exactly when `open_output_folder` has a folder.

### `run-busy`

A command or a drop was refused because the backend is busy: a run is
active, a dialog is open, an image is being loaded or saved, or another
command is in its short checked moment (see `"busy"` under
[Commands](#commands)). Payload `null`.

### `run-cancelled`

A file or folder dialog was closed without a choice. Nothing changes.
Payload `null`.

### `drag-state`

Files are being dragged over the window, or not any more. It is sent only
when the value changes. A drop also sends `{ "over": false }`, just before
its `list-changed` (Batch) or `image-changed` (Single image).

```json
{ "over": true }
```

## Thumbnails: the `thumb` scheme

Each `list-changed` row with a `thumb` key has a thumbnail at an address of
the custom URI scheme `thumb`. Build it with
`window.__TAURI__.core.convertFileSrc(key, 'thumb')`: that gives
`thumb://localhost/3-0` on Linux and macOS and `http://thumb.localhost/3-0`
on Windows. Accept only a key matching `^\d{1,10}-\d{1,4}$` and treat
anything else as `null`, so no other text reaches an image address.

- **The key** is `<list>-<index>`: `index` is the row's place in that
  `list-changed`; `list` is a number the backend raises when a new list
  starts (Clear, and the first add after a finished run). Adds and
  `readd_list` keep the earlier rows' keys. A key never names another file:
  when re-planning would move a file into a row whose key was already sent
  (a row dropped from the middle of the list), the list number is raised
  too, and every row gets a new key.
- **The answer:** the address must have no query (not even an empty `?`),
  and its path must be exactly `/<list>-<index>` as received (not
  percent-decoded), of the current list, for a row whose `thumb` is not
  `null`. Then `200` with `Content-Type: image/png` and the thumbnail
  (longest side 64 px), or, for a row whose name ends in `.svg`,
  `200` with `Content-Type: image/svg+xml` and the file's own bytes. Anything
  else, and a thumbnail that cannot be made (not an image, damaged, too
  large, the worker failed or timed out), is `404` with an empty body. Show
  an empty square then; nothing goes to the status line. Every answer,
  `200` or `404`, also carries `Content-Security-Policy: default-src
  'none'; style-src 'unsafe-inline'` and `X-Content-Type-Options: nosniff`.
- **How:** a worker process makes each thumbnail (the window process never
  opens the file), at most two at a time; other requests wait their turn.
  Thumbnails and "cannot be shown" are kept for the current list and dropped
  when a new list starts. At most 64 MiB of thumbnails are kept per list: one
  that does not fit is still sent, but made again when it is asked for
  again ("cannot be shown" takes no room). A failure of any other kind is
  tried again on the next request.

```text
GET thumb://localhost/3-0     → 200, image/png
GET thumb://localhost/3-1     → 200, image/svg+xml
GET thumb://localhost/2-0     → 404 (list 2 is gone)
GET thumb://localhost/3-2     → 404 (notes.txt is a skipped row)
GET thumb://localhost/3%2D0   → 404 (encoded; not a key as received)
GET thumb://localhost/3-x     → 404 (not a key)
GET thumb://localhost/3-0?x=1 → 404 (a query)
```

The scheme needs no permission: it is not a command.

## Previews: the `preview` scheme

Single image mode shows the loaded image and its kept version from the
custom URI scheme `preview`. Build the address with
`window.__TAURI__.core.convertFileSrc(key, 'preview')`: that gives
`preview://localhost/7` on Linux and macOS and `http://preview.localhost/7`
on Windows. Accept only a key matching `^\d{1,10}(-\d{1,10})?$` and treat
anything else as `null`, so no other text reaches an image address.

- **The keys:** `<image>` (the `key` of `image-changed`) is the copy taken
  at load; `<image>-<version>` (the `key` of `preview-ready`) is the
  version kept for that image.
- **The answer:** the address must have no query (not even an empty `?`),
  and its path must be exactly `/<image>` or `/<image>-<version>` as
  received (not percent-decoded), for the current image, and for its kept
  version only. Then `200` with the file's bytes and the `Content-Type` of
  its format: `image/png`, `image/jpeg`, `image/webp` or `image/svg+xml`.
  Anything else (another image, an older or a newer version, a version that
  failed or was stopped, a file that can no longer be read) is `404` with
  an empty body. Leading zeros are accepted: `/07-04` is `/7-4`. Every
  answer carries the same two headers as the `thumb` scheme's
  (`Content-Security-Policy: default-src 'none'; style-src
  'unsafe-inline'`, `X-Content-Type-Options: nosniff`), so an SVG opened by
  itself runs no script.
- **How:** the window process reads the file and never decodes it; the
  webview does. The file is read on a thread of its own.

```text
GET preview://localhost/7       → 200, image/jpeg (the copy)
GET preview://localhost/7-4     → 200, image/jpeg (the kept version)
GET preview://localhost/7-3     → 404 (not the kept version)
GET preview://localhost/6       → 404 (not the current image)
GET preview://localhost/7%2D4   → 404 (encoded; not a key as received)
GET preview://localhost/7-x     → 404 (not a key)
GET preview://localhost/7?x=1   → 404 (a query)
```

The scheme needs no permission: it is not a command.

## Rules for the window

- **Runs never overlap, so events are strictly ordered.** Handle them in the
  order they arrive. The backend frees a run only after its `run-finished`
  has gone out. Run N's events are therefore always `run-started`, then per
  row `file-started` (compressed rows only) and `file-done`, then
  `run-finished`, and all of them come before any event of run N+1. A
  `run-finished` always belongs to the last `run-started`, so no run number
  is needed.
- **Check every preview outcome before showing it.** Show a
  `preview-ready` or `preview-failed` only when its `image` equals the key
  of the current `image-changed` and its `version` equals the version of
  the last `preview-started`; drop any other. The backend already sends no
  outcome after a newer request or a new image (see
  [Single image mode](#single-image-mode)). This check is a second guard,
  so that the window never relies on how events are delivered.
- **Listen only to the events above.** Do not listen to Tauri's own
  `tauri://drag-enter`, `tauri://drag-over` or `tauri://drag-drop`: Tauri
  puts the dropped paths in them, and they reach the page only when a
  listener asks for them. `drag-state` is the replacement.
- **Permissions:** the twelve commands (`allow-choose-files`,
  `allow-clear-list`, `allow-readd-list`, `allow-choose-output-folder`,
  `allow-clear-output-folder`, `allow-start-run`,
  `allow-open-output-folder`, `allow-set-view`, `allow-choose-image`,
  `allow-preview-image`, `allow-save-image`, `allow-move-drop-to-batch`),
  `core:event:allow-listen` and
  `core:event:allow-unlisten`, nothing more (`capabilities/default.json`; a
  test guards it). There is no `emit`, no window API and no plugin API, and
  `alert()` and `confirm()` do not work (the dialog plugin overrides them,
  and the window has no dialog permission). Show messages in the page.
- **CSP** (`tauri.conf.json`): `default-src 'self'; script-src 'self';
  style-src 'self'; img-src 'self' thumb: http://thumb.localhost preview:
  http://preview.localhost; font-src 'self'; connect-src ipc:
  http://ipc.localhost; object-src 'none';
  base-uri 'none'; form-action 'none'; frame-src 'none'`. So: scripts and
  styles come from files in `ui/` only. No inline `<script>`, no `on…=`
  attributes, no `style="…"` attributes, no `eval` or `new Function`,
  nothing remote, no `data:` images; images come from `ui/`, from the
  `thumb` scheme (`thumb:` on Linux and macOS, `http://thumb.localhost` on
  Windows) or from the `preview` scheme (`preview:`,
  `http://preview.localhost`). Setting `element.style.foo` from JS is allowed; `setAttribute('style', …)`
  is not.
- **Do not lock the buttons while a dialog is open** in the page's own state.
  The backend refuses a second request with `"busy"`, and the page can say
  so.
