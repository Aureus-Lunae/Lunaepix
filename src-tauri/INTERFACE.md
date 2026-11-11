# Window ↔ backend interface

The contract between the window (`ui/`, vanilla JS) and the Rust backend
(`src-tauri/src/lib.rs`, with the logic in `session.rs` and the payloads in
`events.rs`; the thumbnails in `thumbs.rs`). The window never sends a path,
and it never receives one: it gets file names, the output folder's name,
numbers and thumbnail keys.

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

Nothing is remembered between launches.

## Commands

Each resolves to `null` or rejects with one bare string. Only `start_run`
takes an argument.

| Command | Argument | Does | Rejects with |
|---|---|---|---|
| `choose_files` | none | Opens a native multi-file dialog (PNG, JPG, SVG, WebP; "All files" as a second filter). The files picked are added to the list. Resolves as soon as the dialog is open; the outcome arrives as events. | `"busy"` |
| `clear_list` | none | Empties the list. | `"busy"` |
| `readd_list` | none | After a run, puts the files that were in the list back: sends `list-changed` with the list as it was added, planned again now. It adds nothing and removes nothing; it only ends the "run has finished" state, so the next add appends. | `"busy"` |
| `choose_output_folder` | none | Opens a native folder dialog; the folder picked becomes the output folder. Resolves as soon as the dialog is open. | `"busy"` |
| `clear_output_folder` | none | Back to "next to the originals". | `"busy"` |
| `start_run` | `{ "settings": { "mode": "lossy", "quality": 75 } }` | Runs the list with these settings and the output setting. It writes next to the originals (`optimized-a.png`) or into the chosen folder (`a.png`), and never replaces an existing output: a taken name gets a number before the extension, from `-1` (`optimized-a-1.png`, `a-1.png`), then `-2`, up to `-9999`; when all of those are taken the file fails ("no free name in the output folder"). Resolves once the run has started; its progress arrives as events. | `"busy"`, `"invalid-settings"`, `"empty-list"`, `"output-missing"` |
| `open_output_folder` | none | Opens the output folder of the last completed run in the file manager: the chosen output folder, or, with none, the folder of the first file written. | `"no-output"`, `"open-failed"` |

```js
await invoke('start_run', { settings: { mode: 'lossy', quality: 75 } });
```

- `"busy"`: a run is active, or a dialog for one is open. A `run-busy` event
  goes out as well. Every command but `open_output_folder` can be busy.
- `"invalid-settings"`: `settings` is missing or `null`, or is not exactly
  an object with the two
  keys `mode` (`"lossless"` or `"lossy"`) and `quality` (a whole JSON number
  from 0 to 100). A float (also `75.0`), a string (`"75"`), `null`, a
  missing or an extra key, or another mode word is refused. Send `quality`
  also for a lossless run (the current value, 75 unless changed); it is
  ignored there.
- `"output-missing"`: the chosen output folder is gone, is no longer a
  folder, or cannot be written to. `start_run` does not create it again.
  A folder deleted once the run has started is created again by the next
  file written into it.
- `"empty-list"`: no file in the list can be compressed (the list is
  empty, or every row is skipped).
- `"no-output"`: no completed run wrote anything yet. `"open-failed"`: the
  system could not open the folder, for example because it was removed.

`start_run` checks, in this order: busy, the settings, the output folder,
then the list. The list is planned again at that moment, so a file deleted
since it was added runs as a skipped row ("Not a readable file") and the
rest still runs. A row that is a folder by then (a dropped folder that
could not be read, or a path that did not exist when it was added) stays
one "Not a readable file" row: the run never looks inside it. To add its
images, drop the folder again. Nothing runs when it rejects.

A dialog closed without a choice changes nothing and sends `run-cancelled`.

Drag and drop needs no command. A drop on the window adds to the list,
exactly like `choose_files` with those paths. A dropped folder adds the PNG,
JPG, SVG and WebP files directly inside it (not its subfolders), earlier
`optimized-*` outputs included. While a run is going, a drop is refused with
`run-busy`.

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

A command or a drop was refused because a run, or a dialog for one, is
active. Payload `null`.

### `run-cancelled`

A file or folder dialog was closed without a choice. Nothing changes.
Payload `null`.

### `drag-state`

Files are being dragged over the window, or not any more. It is sent only
when the value changes. A drop also sends `{ "over": false }`, just before
its `list-changed`.

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
  an empty square then; nothing goes to the status line.
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

## Rules for the window

- **Runs never overlap, so events are strictly ordered.** Handle them in the
  order they arrive. The backend frees a run only after its `run-finished`
  has gone out. Run N's events are therefore always `run-started`, then per
  row `file-started` (compressed rows only) and `file-done`, then
  `run-finished`, and all of them come before any event of run N+1. A
  `run-finished` always belongs to the last `run-started`, so no run number
  is needed.
- **Listen only to the events above.** Do not listen to Tauri's own
  `tauri://drag-enter`, `tauri://drag-over` or `tauri://drag-drop`: Tauri
  puts the dropped paths in them, and they reach the page only when a
  listener asks for them. `drag-state` is the replacement.
- **Permissions:** the seven commands (`allow-choose-files`,
  `allow-clear-list`, `allow-readd-list`, `allow-choose-output-folder`,
  `allow-clear-output-folder`, `allow-start-run`,
  `allow-open-output-folder`), `core:event:allow-listen` and
  `core:event:allow-unlisten`, nothing more (`capabilities/default.json`; a
  test guards it). There is no `emit`, no window API and no plugin API, and
  `alert()` and `confirm()` do not work (the dialog plugin overrides them,
  and the window has no dialog permission). Show messages in the page.
- **CSP** (`tauri.conf.json`): `default-src 'self'; script-src 'self';
  style-src 'self'; img-src 'self' thumb: http://thumb.localhost; font-src
  'self'; connect-src ipc: http://ipc.localhost; object-src 'none';
  base-uri 'none'; form-action 'none'; frame-src 'none'`. So: scripts and
  styles come from files in `ui/` only. No inline `<script>`, no `on…=`
  attributes, no `style="…"` attributes, no `eval` or `new Function`,
  nothing remote, no `data:` images; images come from `ui/` or from the
  `thumb` scheme (`thumb:` on Linux and macOS, `http://thumb.localhost` on
  Windows). Setting `element.style.foo` from JS is allowed; `setAttribute('style', …)`
  is not.
- **Do not lock the buttons while a dialog is open** in the page's own state.
  The backend refuses a second request with `"busy"`, and the page can say
  so.
