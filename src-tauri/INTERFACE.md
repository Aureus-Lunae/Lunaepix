# Window ↔ backend interface

The contract between the window (`ui/`, vanilla JS) and the Rust backend
(`src-tauri/src/lib.rs`). The window never sends a path, and it never
receives one: it gets file names and numbers.

Use the global API (`app.withGlobalTauri` is on):

```js
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
```

## Commands

None of them takes an argument. Each resolves to `null` or rejects with one
bare string.

| Command | Does | Resolves | Rejects with |
|---|---|---|---|
| `choose_files` | Opens a native multi-file dialog (PNG, JPG, SVG; "All files" as a second filter). The files picked are optimised to `optimized-<name>` next to each one. | `null` as soon as the dialog is open. The result arrives as events. | `"busy"` |
| `choose_folder` | Opens a native folder dialog. The files in the folder (not its subfolders) are optimised into `<folder>/optimized/`. | `null` as soon as the dialog is open. | `"busy"` |
| `open_output_folder` | Opens the output folder of the last completed run in the file manager: `<folder>/optimized` for a folder, or the folder of the first file written. | `null` | `"no-output"`: no completed run wrote anything yet. `"open-failed"`: the system could not open it, for example because it was removed. |

`"busy"` means a run is active, or a dialog for one is open. A `run-busy`
event goes out as well.

A dialog closed without a choice runs nothing and sends `run-cancelled`.

Drag and drop needs no command. A drop on the window starts a run, exactly
like `choose_files` with those paths; a dropped folder is handled like
`choose_folder`.

## Events

Every payload is JSON data. Show names and reasons with `textContent`, never
with `innerHTML`. A file name may contain any character.

### `run-started`

A run starts. `total` is the number of `file-done` events that follow,
skipped files included. It can be 0, for an empty folder; the window then
says "No PNG, JPG or SVG files directly in this folder (subfolders are not
included)".

```json
{ "total": 3 }
```

### `file-done`

One per file, in order, `index` from 0 to `total - 1`. `status` is `"done"`,
`"skipped"` or `"failed"`.

- `done`: `before` and `after` in bytes, `millis` in ms. No `reason`.
- `skipped` and `failed`: `reason`, a sentence for the user. No sizes.

`name` is the file name alone, never a path. It is empty only for a path
with no file name.

A run optimises at most 2000 files. The files left out for that reason do
not get a row each: after every other row comes one `skipped` row named
`More files`, whose reason counts them ("3 more files not optimised: over
the 2000-file limit", or "1 more file ..."), and `total` counts it as one.
Files skipped for any other reason keep a row of their own.

A file whose worker was stopped because the window closed is `failed` with
the reason "the run was stopped"; no later file runs.

```json
{ "index": 0, "name": "logo-black.svg", "status": "done", "before": 21828, "after": 9631, "millis": 22 }
{ "index": 1, "name": "notes.txt", "status": "skipped", "reason": "Not a PNG, JPG or SVG file" }
{ "index": 2, "name": "broken.png", "status": "failed", "reason": "the optimiser crashed on this file (it gave no valid result)" }
```

A file can grow (`after > before`). Show the negative saving and do not
clamp it.

### `run-finished`

The run is over. `optimised` counts the `done` files, and `saved_bytes` is
their total saving (a file that grew counts 0). `has_output` says whether
`open_output_folder` has a folder to open. Before this event goes out, the
backend stores the output folder, so "Open folder" works as soon as the
event arrives. The run is freed right after the event goes out. In the
moment between the two, a drop or pick gets `"busy"` / `run-busy`; a retry
works.

```json
{ "optimised": 2, "saved_bytes": 341210, "has_output": true }
```

Every `run-started` is followed by exactly one `run-finished`. That holds
even when the run breaks off inside the backend. Then fewer than `total`
`file-done` events arrive, and the summary counts only those. `has_output`
is true exactly when `open_output_folder` has a folder.

### `run-busy`

A drop or a pick was refused because a run, or a dialog for one, is active.
Payload `null`.

### `run-cancelled`

A file or folder dialog was closed without a choice. Nothing runs. Payload
`null`.

### `drag-state`

Files are being dragged over the window, or not any more. It is sent only
when the value changes. A drop also sends `{ "over": false }`, just before
its run starts.

```json
{ "over": true }
```

## Rules for the window

- **Runs never overlap, so events are strictly ordered.** Handle them in the
  order they arrive. The backend frees a run only after its `run-finished`
  has gone out. Run N's events are therefore always `run-started`, then its
  `file-done`s, then `run-finished`, and all of them come before any event
  of run N+1. A `run-finished` always belongs to the last `run-started`, so
  no run number is needed. A `run-started` begins a new run: clear the table
  and reset the counters.
- **Listen only to the events above.** Do not listen to Tauri's own
  `tauri://drag-enter`, `tauri://drag-over` or `tauri://drag-drop`: Tauri
  puts the dropped paths in them, and they reach the page only when a
  listener asks for them. `drag-state` is the replacement.
- **Permissions:** the three commands, `core:event:allow-listen` and
  `core:event:allow-unlisten`, nothing more (`capabilities/default.json`; a
  test guards it). There is no `emit`, no window API and no plugin API, and
  `alert()` and `confirm()` do not work (the dialog plugin overrides them,
  and the window has no dialog permission). Show messages in the page.
- **CSP** (`tauri.conf.json`): `default-src 'self'; script-src 'self';
  style-src 'self'; img-src 'self'; font-src 'self'; connect-src ipc:
  http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action
  'none'; frame-src 'none'`. So: scripts and styles come from files in `ui/`
  only. No inline `<script>`, no `on…=` attributes, no `style="…"`
  attributes, no `eval` or `new Function`, nothing remote, no `data:`
  images. Setting `element.style.foo` from JS is allowed; `setAttribute('style', …)`
  is not.
- **Do not lock the buttons while a dialog is open** in the page's own state.
  The backend refuses a second request with `"busy"`, and the page can say
  so.
