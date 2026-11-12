# Lunaepix single image mode — implementation plan

**Owner:** Aureus-Lunae · **Started:** 2026-11-11

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Single image / Batch switch; in Single image, one image is loaded, compressed in a worker with the current settings, compared with its original behind a draggable divider with zoom, shown with exact sizes, and saved under a chosen name that never replaces a file.

**Architecture:**
- **Core.** A third worker mode, `--lunaepix-probe <src>`, reads a file's header in a worker process and answers its shown pixel size or Batch's refusal. Two small public helpers: write a file only under exactly this name (`save_new`), and find the first free name beside a taken one (`first_free_name`).
- **Backend.** A new `single.rs` holds the Single image state behind the session: the view, the loaded image (a private copy taken at load), the compressed versions, the in-flight preview's stop handle and the last drop's left-out items. Five new commands, six new events, and a `preview` URI scheme beside `thumb`. Preview runs one job through the existing runner (`run_until_stopped`) with `WriteMode::NoReplace` into the private folder.
- **Window.** New pure modules `ui/names.js`, `ui/zoom.js`, `ui/prefs.js`, `ui/single.js` (state, gates, messages) and `ui/single-view.js` (writes to the page); `ui/window.js` wires (`startWindow`, tested) and `ui/app.js` only starts it (see Handoff, "Window wiring"). One `#settings` block is moved between the two views.

**Tech Stack:** Rust 2024 (MSRV 1.90), Tauri 2 (custom URI schemes, `tauri-plugin-dialog`), `tempfile 3.27`, `serde_json`, vanilla JS (`node --test`).

**Spec:** [2026-11-11-lunaepix-single-image-design.md](../../in-progress/specs/2026-11-11-lunaepix-single-image-design.md), with its binding mock-up [2026-11-11-lunaepix-single-image-mockup.html](../../in-progress/specs/2026-11-11-lunaepix-single-image-mockup.html). Read the spec's Handoff before anything else; the spec's "How it works", "Interface" and "Layout" sections are requirements for every task.

## Handoff

Copied verbatim from the spec's Handoff, plus the standing process rules and the calls made while planning. Give every implementer these lines as they are. An implementer that wants to deviate from one stops and asks instead of choosing.

- **Plan approved by the user (2026-11-11):** "yes", including the calls made while planning (listed below). Execute in a fresh session, subagent-driven, on `feature/lunaepix-single-image`.
- **Spec approved by the user (2026-11-11):** "Approved, write the plan". The calls listed under "Calls I made" were shown with the design and approved with it.
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
- **Always raise the version** (user, 2026-11-11): "you updated the version wrong. Always update a version". This work is 0.1.2.
- **Standing process rules (user, from the lossy plan, 2026-11-10), apply to every task here:**
  - "Controller RED audit": before each review the controller compares the RED run's test list with the commit's final tests; a test missing from the RED run goes back as a concern before review. The test table has a "fails on its own because…" column.
  - "Branch table covers choices": the failure-branch table also lists every if/else driven by a setting or option, each side with a named test.
  - "Add a cross-module row": the branch table gets one row for every core function the task newly calls, naming the branches inside it the new caller can reach.
  - "No 'glue' exemption": code that decides what is sent or shown is moved into a function a test can reach; only true pass-through calls may stay untested.
- **Controller gate (user, 2026-11-11):** before each review the controller reads the failure-branch table, and any "untested" row on a branch that decides what the caller gets goes back to the implementer unless it names how a test could reach it and why that does not work.
- **Pass-through means unchanged (user, 2026-11-11):** a call counts as untested pass-through only if it hands its input on unchanged; picking a field (path rather than path + query, one request field, a default) is a decision and must be moved into a tested function. The controller checks pass-through rows line by line before review.
- **Decisions made during execution (user, 2026-11-12), each the answer to a repeated defect class or a question:**
  - Trigger coverage: "Trigger column (Recommended)": the test table gets a "reached by" column, every trigger its own test; the controller checks rows with a single trigger before review.
  - Atomicity: "Seam + one section (Recommended)": a decision and its event or commit sit in one critical section, with a Shared-state table, a test hook between decide and act, and a race test that injects the competing call.
  - Windows: "Windows check here (Recommended)": every Rust task runs `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`.
  - Stale docs: "Widen the rule (Recommended)": the comment rule also covers docs of types, enum variants, guards, constants and INTERFACE sections whose use a change widens.
  - Window wiring: "A: test app.js itself (Recommended)": the wiring lives in `ui/window.js` (`startWindow`, taking the page, Tauri, timers, storage, ResizeObserver and a log); `ui/app.js` is only the entry that passes the real globals. Node tests drive every listener with strict fakes. This replaces "`app.js` only wires" below.
  - New text: "The picture could not be shown." (status line, a picture the webview cannot decode): "Keep it (Recommended)".
  - Async window actions (after the final review): "A: rule + fix all (Recommended)": the window records which picture each side actually shows and Save requires it to equal the ready version; previews go one at a time, a change made during one is sent when it settles, and a busy answer is retried; every async window action states its in-flight and late-answer policy, with a test for each.
  - Save text: "New text (Recommended)": a save with no folder chosen that fails with output-missing says "The original's folder cannot be written to. Choose a folder in Save to."; Batch and saves to a chosen folder keep today's text.
  - Private folder left by a close during a load: "Note it (Recommended)": INTERFACE.md and the README say so; no code.

Calls made while planning (not user decisions; flagged for the plan review):

1. **`"save-failed"`** is added to `save_image`'s rejections, for a write that fails for any reason other than a taken name (disk full, permission). Message: "The file could not be saved."
2. **The name length rule also caps the bytes:** at most 200 characters (Unicode code points) *and* at most 255 UTF-8 bytes, counted on name + extension, because Linux allows 255 bytes per file name.
3. **One fixture for the name rules,** `tests/fixtures/save-names.json`, read by both the Rust and the JS tests, so the two copies of the rules cannot drift apart (`~/.claude/rules/security.md`: duplication is a defect).
4. **One `#settings` block,** moved by the window into the slot of the view that shows; no second copy of the controls.
5. **The divider** is a visually hidden `<input type="range">` (0–100, step 1) that carries the keyboard and the name, plus a handle drawn on the picture that pointer drags move. A drag that starts outside the handle pans the picture.
6. **`left_out`** counts the dropped items not loaded that are folders or have a PNG, JPG, SVG or WebP extension; other files do not count (switching cannot help them). A drop with an image and only non-image files besides has `notice: null`.
7. **A choice or drop that loads nothing** (not an image, refused by the probe) sends `image-changed` with the current image unchanged (same key, or `null` when there was none) and the reason in `notice`.
8. **The layout is stored** under the key `lunaepix.layout` in `localStorage`, values `"under"` and `"beside"`.
9. **The private folder** is made once per launch with `tempfile::Builder::new().prefix("lunaepix-").tempdir()` (owner-only on Unix, the user's own temp folder on Windows), and removed when the window closes. Each image gets a subfolder `image-<key>`; the copy is `original<.ext>`, a version `v<n><.ext>`.
10. **Image keys and versions start at 1** and only go up during a launch.

## Global Constraints

- The window never sends or receives a path. It gets names, numbers and keys.
- The window process never opens a source file; loading (probe) and compression run in a worker process.
- Keys: window side `^\d{1,10}(-\d{1,10})?$`; scheme handler `^/(\d{1,10})(?:-(\d{1,10}))?$` on the path as received, not percent-decoded, no query (not even an empty `?`).
- Limits are Batch's: `MAX_FILE_BYTES`, `MAX_PIXELS`, the animation and SVG limits, `WORKER_TIMEOUT` for a preview, `THUMB_TIMEOUT` (10 s) for a probe.
- Nothing is ever replaced: Save writes only to a name that does not exist (`persist_noclobber`), never numbers a typed name, and never writes over the original.
- CSP after this work, exactly: `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' thumb: http://thumb.localhost preview: http://preview.localhost; font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'`.
- Permissions: exactly twelve commands (today's seven plus `allow-set-view`, `allow-choose-image`, `allow-preview-image`, `allow-save-image`, `allow-move-drop-to-batch`), plus `core:event:allow-listen` and `core:event:allow-unlisten`. Each new command is also declared in `src-tauri/build.rs`.
- Every text into the page through `textContent`; no inline script, no `style="…"` attribute (`element.style.foo` from JS is allowed).
- Exact texts (window): switch legend "Work on" (visually hidden), choices "Single image", "Batch"; drop title "Drop an image here", hint "PNG · JPG · SVG · WebP", button "Choose image"; "Choose other"; layout legend "Settings" (visually hidden), choices "Settings under", "Settings beside"; buttons "Zoom out", "Zoom in", "100%", "Fit"; corner labels "Original", "Compressed"; `alt` "Original" and "Compressed, lossless" / "Compressed, lossy quality 75"; divider label "Compare original and compressed"; region label "Picture"; "Compressing…"; "Loading picture…"; size box terms "Original", "Compressed", "Saves"; "Save as"; button "Save"; button "Open folder"; button "Switch to Batch"; status "Saved as optimized-holiday.jpg".
- Exact texts (backend notices): "Single image mode takes one image; 1 other was left out." / "Single image mode takes one image; 3 others were left out."; "Folders go in Batch."; "Not a PNG, JPG, SVG or WebP file."; refusals use Batch's reason texts unchanged.
- Name problems (window): "Enter a name." · "A name cannot contain / \ : * ? \" < > | or control characters." · "A name cannot end in a space or a dot." · "This name is reserved by Windows." · "This name is too long."
- Rejection messages (window): `name-taken` "A file named <name><ext> already exists there. Choose another name." (tied to the name box) · `invalid-name` the name problem text · `not-ready` "The compressed version is not ready yet." · `no-image` "Load an image first." · `nothing-to-move` "There is nothing to move to Batch any more." · `save-failed` "The file could not be saved." · `output-missing` today's text · `invalid-view` unknown (logged).
- BBEM class names (`~/.claude/css-conventions.md`), new: `lunaepixHeader__views`, `lunaepixHeader__view`; block `lunaepixSingle` with `__drop`, `__file`, `__fileName`, `__fileSize`, `__tools`, `__toolButton`, `__picture`, `__stage`, `__image` (`-original`, `-compressed`), `__label` (`-original`, `-compressed`), `__divider`, `__handle`, `__busy`, `__body` (`-under`, `-beside`), `__settingsSlot`, `__sizes`, `__sizeTerm`, `__sizeValue`, `__note`, `__saveAs`, `__name`, `__extension`, `__nameError`, `__foot`, `__save`, `__openFolder`; `lunaepixStatus__action` for the Switch to Batch button. Modifiers are sibling classes.
- Accessibility (`~/.claude/a11y-rules.md`): both switches are radio groups in a `fieldset` with a `legend`; every button a `<button>`; visible focus on every control (the hidden divider input shows its focus on the handle); targets at least 24 × 24 px; the name error tied with `aria-describedby` and not shown by colour alone; the picture area `role="region"`, `aria-label="Picture"`, `tabindex="0"`; no animation under `prefers-reduced-motion: reduce`; no sideways scroll at 360 px wide.
- **Layout:** the spec's `## Layout` section and the mock-up are binding for the arrangement.
- Structured files (JSON, HTML) are checked with a parser in tests, never as text. CSS is not tested (`~/.claude/rules/unit-testing.md`).
- Every behaviour has a positive and a negative test; every failure branch is reached by a test, through a double where needed.
- Version 0.1.2 in the workspace `Cargo.toml` (Task 10).

## Review Focus

1. **An EXIF-rotated JPG compressed lossy.** If the lossy JPG path drops the Orientation tag, the compressed side shows the photo turned while the original side is upright, and the saved file is turned too. The test is in Task 5 (the version's Orientation equals the original's). If it fails, the implementer stops and reports to the user; changing the lossy JPG path is not part of this plan.
2. **Quality changed many times in a row** (ten requests within a second). Only the last result is shown, no worker is left running, and only the shown version's file remains. The test is in Task 5.
3. **The window closes while a preview runs.** The worker is stopped and the private folder is removed. The test is in Task 5.
4. **Names with `é`, emoji, combining marks and (on Linux) a non-UTF-8 original name.** The suggested name is the lossy text of the original's name; a typed `vakantie-café` saves under exactly that name; a 63-emoji name is refused by the byte cap. The tests are in Tasks 3 and 6.
5. **The Save to folder disappears between choosing it and pressing Save,** or the original's own folder is removed. Save answers `output-missing` and writes nothing. The test is in Task 6.

---

### Task 1: The probe worker mode (core)

**Files:**
- Create: `crates/core/src/probe.rs`
- Modify: `crates/core/src/worker.rs` (new flag, `serve_probe`, module docs), `crates/core/src/run.rs` (`run_probe`), `crates/core/src/thumb/decode.rs` (a header-size function `check_header` reuses), `crates/core/src/lib.rs` (exports), `crates/core/src/bin/lunaepix-worker.rs` (usage text names three modes)
- Test: `crates/core/tests/probe.rs`, unit tests beside the code

**Interfaces:**
- Produces:
  - `pub const PROBE_FLAG: &str = "--lunaepix-probe";`
  - `pub struct Probe { pub width: Option<u32>, pub height: Option<u32> }` (both `None` for an SVG)
  - `pub fn probe(src: &Path) -> Result<Probe, String>` — pure, in-process; `Err` carries Batch's reason text.
  - `pub enum ProbeAnswer { Size(Probe), Refused(String), Failed }`
  - `pub fn run_probe(program: &Path, src: &Path) -> ProbeAnswer` — worker process, `THUMB_TIMEOUT`, output capped at `MAX_WORKER_OUTPUT_BYTES`.
- Worker answer, one line on stdout:

```text
lunaepix-probe 1 size <width> <height>      exit 0   (an SVG: "size - -")
lunaepix-probe 1 refused <message>          exit 3   (message escaped as the optimiser's record: \\ \n \r, cut to MAX_WORKER_MESSAGE_BYTES)
```

  Exit 2 for a usage error (nothing on stdout), 101 for a panic. The runner reads only these two records with these exit codes; anything else is `Failed`.

- [ ] **Step 1: Write the tables, then the failing tests.**
  - `probe`:
    - `a_jpg_gives_its_stored_size` (300 × 200, no EXIF);
    - `an_exif_rotated_jpg_gives_its_upright_size` (stored 300 × 200, Orientation 6 → 200 × 300; also 5, 7, 8 swap and 2, 3, 4 do not);
    - `an_animated_png_and_webp_give_their_canvas_size`;
    - `an_svg_gives_no_size`;
    - `a_file_over_max_pixels_is_refused_with_the_optimisers_reason` (header 10 000 × 10 001; the message equals `check_pixels(10_000, 10_001)`'s);
    - `a_file_at_max_pixels_is_accepted` (10 000 × 10 000 header);
    - `a_file_over_max_file_bytes_is_refused_with_the_planners_reason` (`Skip::TooLarge` text);
    - `a_damaged_header_is_refused`;
    - `a_non_image_name_is_refused_with_the_unsupported_text`;
    - `a_folder_is_refused`.
  - Every header check the optimiser makes before it decodes is made here, with the same message. List them in the report with the function each comes from.
  - Worker and runner:
    - `the_worker_answers_a_size_line_and_exits_0`;
    - `the_worker_answers_refused_with_an_escaped_message_and_exits_3`;
    - `a_probe_with_two_arguments_is_a_usage_error`;
    - `a_path_that_looks_like_an_option_is_still_a_path`;
    - `run_probe_reads_size_and_refused`;
    - `run_probe_counts_a_wrong_version_a_wrong_exit_code_too_much_output_a_timeout_and_a_crash_as_failed`, each through a test program as `run_thumb`'s tests do, with the timeout shortened through a `run_probe_with` like `run_thumb_with`;
    - `a_file_name_with_spaces_and_e_acute_is_probed`;
    - on Linux, `a_non_utf8_file_name_is_probed`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core probe`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement. `serve_if_requested` answers `PROBE_FLAG` as it answers the other two.
- [ ] **Step 4:** Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`. Both pass.
- [ ] **Step 5:** Commit: "Add a probe worker mode: a file's shown size, or Batch's refusal".

### Task 2: Save under exactly this name; first free name (core)

**Files:**
- Modify: `crates/core/src/atomic.rs`, `crates/core/src/plan.rs` (make the batch output name reachable), `crates/core/src/lib.rs` (exports)
- Test: unit tests in `atomic.rs`, `crates/core/tests/output_folder.rs`

**Interfaces:**
- Produces:
  - `pub fn save_new(dest: &Path, bytes: &[u8], permissions: fs::Permissions) -> io::Result<()>`. Writes like `write_new` (temp file in the folder, synced, then a rename that refuses an existing name) but tries only `dest`. A taken name is `ErrorKind::AlreadyExists`, and nothing is left behind. It never creates a missing folder; that is `ErrorKind::NotFound`.
  - `pub fn first_free_name(folder: &Path, name: &OsStr) -> Option<OsString>`: `name` if free, else `numbered(name, 1..=MAX_NAME_NUMBER)`'s first free one; `None` when all are taken.
  - `pub fn batch_output_name(src: &Path, in_chosen_folder: bool) -> OsString`: `optimized-<name>` when `false`, `<name>` when `true` (reuses `prefixed_name`).
  - The Windows read-only handling of `optimize_file`'s output applies to `save_new` too (reuse the existing code, do not copy it).

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `save_new_writes_exactly_the_bytes_under_exactly_the_name`;
  - `save_new_refuses_a_taken_name_and_leaves_that_file_byte_for_byte` (also when the taken name is a folder);
  - `save_new_never_numbers` (the folder holds no `-1` afterwards);
  - `save_new_refuses_a_missing_folder_and_creates_nothing`;
  - `save_new_leaves_no_temporary_file_after_a_refusal` (folder listing before = after);
  - `save_new_a_file_appearing_just_before_the_rename_is_kept` (through the `before_persist` hook);
  - `first_free_name_gives_the_name_when_free`;
  - `first_free_name_gives_minus_one_then_minus_two`;
  - `first_free_name_skips_a_taken_minus_one` (`a.png`, `a-1.png` taken → `a-2.png`);
  - `first_free_name_is_none_when_all_are_taken` (bound shrunk through the existing test hook);
  - `batch_output_name_prefixes_only_next_to_the_original`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-core save_new first_free batch_output_name`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy. Both pass.
- [ ] **Step 5:** Commit: "Add save_new and first_free_name for single image saves".

### Task 3: The name rules, in Rust and in JS, against one fixture

**Files:**
- Create: `src-tauri/src/names.rs`, `ui/names.js`, `tests/fixtures/save-names.json`, `tests/ui/names.test.js`
- Modify: `src-tauri/src/lib.rs` (`pub mod names;`)

**Interfaces:**
- Produces:
  - Rust: `pub fn check_save_name(stem: &str, extension: &str) -> Result<(), NameProblem>` with `pub enum NameProblem { Empty, ForbiddenCharacter, TrailingSpaceOrDot, Reserved, TooLong }`.
  - JS: `export function nameProblem(stem, extension)` returns `null` or one of the five name-problem texts in Global Constraints, in the same order of checks.
- The rules, checked in this order on `stem` (what the user typed) and `stem + extension` (the full name):
  1. `Empty`: `stem.trim()` is empty.
  2. `ForbiddenCharacter`: the stem matches `[\/\\:*?"<>|\u0000-\u001F\u007F]`.
  3. `TrailingSpaceOrDot`: the stem ends in `" "` or `"."`.
  4. `Reserved`: the full name, lower-cased, matches `^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)`.
  5. `TooLong`: the full name has more than 200 code points (JS `[...s].length`, Rust `chars().count()`) or more than 255 UTF-8 bytes.
- Fixture shape (parsed with `serde_json` and `JSON.parse`):

```json
{ "extension": ".jpg",
  "accepted": ["optimized-holiday", "holiday 2", "vakantie-café", "<196 × a>"],
  "refused": { "Empty": [""], "ForbiddenCharacter": ["a/b"], "...": [] } }
```

  The implementer writes the actual strings: long ones are generated by the test from a rule in the fixture, such as `{ "repeat": "a", "times": 196 }`, never typed out.

- [ ] **Step 1: Write the tables, then the fixture and the failing tests.**
  - The fixture's accepted list holds:
    - `optimized-holiday`, `holiday 2`, `vakantie-café`, `.hidden`;
    - a name of exactly 200 code points with the extension;
    - 62 four-byte emoji plus `abc`, which with `.jpg` is exactly 255 bytes.
  - The fixture's refused list holds, per problem:
    - `Empty`: `""`, `"   "`;
    - `ForbiddenCharacter`: each of `/ \ : * ? " < > |`, `"a\u0001b"`, `"a\u007Fb"`;
    - `TrailingSpaceOrDot`: `"a "`, `"a."`;
    - `Reserved`: `CON`, `con`, `Con.txt`, `COM1`, `lpt9`, `nul.tar`;
    - `TooLong`: 201 code points; 63 four-byte emoji, which with `.jpg` is 256 bytes (67 code points, so only the byte cap refuses it).
  - Rust: `every_accepted_fixture_name_passes`; `every_refused_fixture_name_gives_its_problem`; `a_reserved_word_inside_a_name_is_accepted` (`console`, `com10`, `icon`).
  - JS: the same three, plus `each_problem_has_its_own_text`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-app names` and `node --test tests/ui/names.test.js`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement both.
- [ ] **Step 4:** Run `cargo test --workspace`, `node --test tests/ui/*.test.js` and clippy. All pass.
- [ ] **Step 5:** Commit: "Name rules for Save as, one fixture for Rust and JS".

### Task 4: Single image backend — view, loading, drops, Switch to Batch

**Files:**
- Create: `src-tauri/src/single.rs`
- Modify:
  - `src-tauri/src/session.rs` (a `Single` field; `drop_paths` routes by view; `start_run` stops a running preview);
  - `src-tauri/src/events.rs` (`ImageChanged`, `NameSuggested`);
  - `src-tauri/src/lib.rs` (commands `set_view`, `choose_image`, `move_drop_to_batch`; a one-file dialog; close handler clean-up);
  - `src-tauri/build.rs`, `src-tauri/capabilities/default.json`, `src-tauri/tests/config.rs`;
  - `src-tauri/INTERFACE.md` (these three commands, the two events, drops in Single image).
- Test: unit tests in `single.rs` and `session.rs`, `src-tauri/tests/session_run.rs`

**Interfaces:**
- Consumes: `run_probe` (Task 1), `first_free_name`, `batch_output_name` (Task 2), `claim_run`, `FileList::add`, `file_name_of`.
- Produces:
  - `pub enum View { Single, Batch }` (`Single` at start); `CommandError::{InvalidView, NoImage, NothingToMove}`.
  - `Session::set_view(&self, view: Option<&Value>, emit) -> Result<(), CommandError>`. Claims and frees the guard (busy check); switching to Batch stops a running preview.
  - `Session::choose_image(&self, emit, open_dialog: impl FnOnce(RunTicket))`, with a one-file dialog that has the same filters as `choose_files`'s, and `Session::load_image(&self, ticket, picked: Option<PathBuf>, prober: impl Fn(&Path) -> ProbeAnswer, emit)`.
  - `Session::drop_paths`: unchanged signature; in Single view the paths go to `load_drop(ticket, paths, prober, emit)` instead of `add_paths`.
  - `Session::move_drop_to_batch(&self, emit) -> Result<(), CommandError>`.
  - `Single::current(&self) -> Option<LoadedImage>` (key, file name, original folder, extension, copy path, permissions, size, width, height) for Tasks 5 and 6.
  - `Single::shutdown(&self)`: stops a preview and removes the private folder.
  - `ImageChanged` and `NameSuggested` payloads exactly as the spec's JSON. `suggested` is `first_free_name(target folder, batch_output_name(original, folder chosen))` without its extension, falling back to the unnumbered stem when none is free.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `set_view`:
    - `single_is_the_view_at_start`;
    - `a_drop_in_single_view_loads_and_does_not_touch_the_list`;
    - `a_drop_in_batch_view_adds_to_the_list_and_does_not_load`;
    - `set_view_refuses_anything_but_single_or_batch` (`"Single"`, `1`, `null`, missing key, extra key → `invalid-view`);
    - `set_view_is_busy_during_a_run`.
  - Loading:
    - `loading_copies_the_file_and_probes_the_copy` (the prober double records the copy's path, not the original's);
    - `a_new_load_gives_a_new_key_and_removes_the_old_images_folder`;
    - `a_refused_probe_keeps_the_current_image_and_sends_its_reason` (same key in `image-changed`);
    - `a_failed_probe_keeps_the_current_image_with_a_reason`;
    - `a_non_image_choice_keeps_the_current_image_with_the_not_an_image_notice`;
    - `a_closed_dialog_sends_run_cancelled_and_changes_nothing`;
    - `a_file_over_max_file_bytes_is_refused_before_it_is_copied`;
    - `changing_the_original_after_load_does_not_change_the_copy`;
    - `deleting_the_original_after_load_keeps_the_copy`.
  - Drops:
    - `one_image_loads_with_left_out_0_and_no_notice`;
    - `three_images_load_the_first_with_left_out_2_and_the_plural_notice`;
    - `two_images_give_the_singular_notice`;
    - `a_non_image_then_an_image_loads_the_image`;
    - `an_image_and_a_text_file_have_left_out_0_and_no_notice`;
    - `a_folder_loads_nothing_keeps_the_image_and_says_folders_go_in_batch`;
    - `only_text_files_load_nothing_with_left_out_0`.
  - Switch to Batch:
    - `move_drop_to_batch_adds_every_item_of_that_drop_and_switches_to_batch` (the loaded image included; a folder adds its images);
    - `move_drop_to_batch_without_a_left_out_drop_is_nothing_to_move`;
    - `the_offer_is_gone_after_the_next_drop`, the same `after_a_choice`, and `after_set_view_by_hand` (each then `nothing-to-move`).
  - Suggested name:
    - `suggested_is_optimized_name_next_to_the_original`;
    - `suggested_is_the_plain_name_in_a_chosen_folder`;
    - `suggested_is_numbered_when_taken`;
    - `name_suggested_follows_output_changed_with_an_image_loaded`;
    - `no_name_suggested_without_an_image`;
    - `name_suggested_keeps_the_key`;
    - on Linux, `a_non_utf8_original_name_gives_a_lossy_suggested_name`.
  - Clean-up:
    - `shutdown_removes_the_private_folder`;
    - `the_close_handler_calls_shutdown`, through the existing `stop_on_close` seam.
  - Config:
    - `the_capability_grants_exactly_the_twelve_commands_and_event_listening`;
    - the six/eight fixture test becomes eleven/thirteen. (`tauri-build` already fails the build when the capability names a command `build.rs` does not declare.)
- [ ] **Step 2:** Run `cargo test -p lunaepix-app`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement. Loading runs off the main thread, holding the ticket, as `add_paths` does today.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy. Both pass.
- [ ] **Step 5:** Commit: "Single image backend: view switch, loading through a probe, drops, Switch to Batch".

### Task 5: Preview — versions, the events and the `preview` scheme

**Files:**
- Modify:
  - `src-tauri/src/single.rs`;
  - `src-tauri/src/session.rs` (`preview_image`);
  - `src-tauri/src/events.rs` (`PreviewStarted`, `PreviewReady`, `PreviewFailed`);
  - `src-tauri/src/lib.rs` (command, scheme registration);
  - `src-tauri/tauri.conf.json` (CSP);
  - `src-tauri/tests/config.rs`;
  - `src-tauri/INTERFACE.md`.
- Create: `src-tauri/src/previews.rs` (the scheme's answers, like `thumbs.rs`)
- Test: unit tests, `src-tauri/tests/session_run.rs` (end to end with the real worker)

**Interfaces:**
- Consumes: `parse_settings`, `Worker::with_options`, `run_until_stopped`, `StopHandle`, `Single::current` (Task 4), the note text function `events.rs` uses for `file-done`.
- Produces:
  - `Session::preview_image(&self, settings: Option<&Value>, emit, start: impl FnOnce(PreviewJob)) -> Result<(), CommandError>`. Checks, in order: busy (guard claimed and freed at once), settings, an image. Then it stops the previous job, takes version `n + 1`, sends `preview-started`, and hands the job to `start`.
  - `Session::run_preview(&self, job: PreviewJob, worker: &Worker, emit)`: one `Job::Optimize { src: copy, dest: image-<key>/v<n><.ext> }` through `run_until_stopped` with `Options { compression, write: WriteMode::NoReplace }`. Under the `Single` lock, it accepts the result only if `(key, n)` is still the latest request. Then:
    - on success it keeps version `n`, deletes every older version file, and sends `preview-ready`;
    - on failure it sends `preview-failed` with `FileResult::Failed(..).reason()`;
    - when not the latest, it deletes its file and sends nothing.
  - `pub fn previews::serve(single: &Single, uri_path: &str, query: Option<&str>) -> PreviewResponse`:
    - `200` for `/<key>`, with the bytes of the copy;
    - `200` for `/<key>-<n>` when `n` is the kept version;
    - `Content-Type` by format: `image/png`, `image/jpeg`, `image/webp`, `image/svg+xml`;
    - everything else `404` with an empty body.
  - `Single::latest_ready(&self) -> Option<(u64, u64)>` for Task 6.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `preview_image`:
    - `a_preview_without_an_image_is_no_image`;
    - `bad_settings_are_invalid_settings` (the same cases as `start_run`'s);
    - `a_preview_during_a_save_is_busy`;
    - `a_preview_running_does_not_make_choose_image_set_view_drop_or_choose_output_folder_busy`.
  - Results:
    - `preview_ready_sizes_equal_the_served_bytes_length_and_the_original_size`, end to end with the real worker, at quality 0, 75 and 100 and lossless;
    - `a_newer_request_stops_the_older_worker_and_only_the_newer_sends_ready`;
    - `ten_requests_in_a_row_leave_one_version_file_and_no_running_worker`;
    - `a_worker_crash_a_timeout_and_an_unreadable_copy_each_send_preview_failed`, through a test program;
    - `a_failed_preview_keeps_the_previous_ready_version`.
  - Batch behaviour and notes:
    - `a_lossy_preview_of_an_svg_has_batchs_behaviour`;
    - `a_lossy_preview_of_an_animated_png_has_the_kept_lossless_note`;
    - `an_exif_rotated_jpg_keeps_its_orientation_in_the_lossy_version` (Review Focus 1: stop and report if it fails).
  - Stopping:
    - `set_view_batch_stops_a_running_preview_and_no_ready_follows`;
    - `start_run_stops_a_running_preview`;
    - `a_new_load_stops_the_old_images_preview`;
    - `shutdown_during_a_preview_kills_the_worker_and_removes_the_folder`.
  - Scheme:
    - `the_original_and_the_kept_version_answer_200_with_their_type`, for each of the four formats;
    - `an_older_images_key_a_deleted_version_an_encoded_key_a_query_an_empty_query_and_a_malformed_key_answer_404`.
  - Config: `the_csp_is_exactly_the_new_string`, and `a_csp_without_the_preview_scheme_is_flagged_in_a_fixture`.
- [ ] **Step 2:** Run `cargo test -p lunaepix-app`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement. The scheme handler reads the file on a thread of its own, as `thumb_protocol` does, and hands `request.uri().path()` and `request.uri().query()` to `previews::serve` unchanged.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy. Both pass.
- [ ] **Step 5:** Commit: "Preview: compressed versions in a worker, the preview scheme".

### Task 6: Save

**Files:**
- Modify:
  - `src-tauri/src/single.rs`;
  - `src-tauri/src/session.rs` (`save_image`; `last_output` set by a save);
  - `src-tauri/src/events.rs` (`ImageSaved`; `CommandError::{NotReady, InvalidName, NameTaken, SaveFailed}`);
  - `src-tauri/src/lib.rs`;
  - `src-tauri/INTERFACE.md` (`save_image`, `image-saved`, `open_output_folder` now "the last save or the last completed run, whichever came last").
- Test: unit tests, `src-tauri/tests/session_run.rs`

**Interfaces:**
- Consumes: `check_save_name` (Task 3), `save_new`, `first_free_name` (Task 2), `Single::latest_ready` (Task 5), `can_write_in`.
- Produces: `Session::save_image(&self, args: Option<&Value>, emit) -> Result<(), CommandError>`.
  - `args` must be exactly `{ "name": <string>, "preview": <key string> }`; anything else is `invalid-name`.
  - Checks, in order: busy (the ticket is held for the whole save), an image, `preview` equal to `"<key>-<latest ready n>"` (else `not-ready`), the name rules, the target folder writable (the chosen output folder, or the original's folder; else `output-missing`).
  - Then `save_new(folder/<name><ext>, version bytes, original's permissions)`: `AlreadyExists` → `name-taken`, any other error → `save-failed`.
  - On success: `last_output` = the folder, then `image-saved` with `name`, `folder` (the chosen folder's name or `null`), and `suggested` = `first_free_name(folder, saved name)` without its extension.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - Writing:
    - `save_writes_exactly_the_shown_versions_bytes_under_name_plus_extension`;
    - `save_next_to_the_original_and_in_a_chosen_folder`.
  - Never replacing:
    - `a_taken_name_is_name_taken_and_the_existing_file_is_unchanged`;
    - `the_originals_own_name_is_name_taken_and_the_original_is_unchanged`.
  - Refusals:
    - `every_refused_fixture_name_is_invalid_name_and_writes_nothing` (reads `tests/fixtures/save-names.json`);
    - `a_badly_shaped_argument_is_invalid_name` (missing key, extra key, number name, `null`);
    - `a_stale_or_foreign_preview_key_is_not_ready`;
    - `a_save_before_any_ready_version_is_not_ready`;
    - `a_save_without_an_image_is_no_image`;
    - `a_deleted_chosen_folder_is_output_missing` and `a_deleted_original_folder_is_output_missing` (Review Focus 5).
  - Failures:
    - `a_failing_write_is_save_failed_and_leaves_no_file`, through a read-only folder on Unix, or a double for the writer where the OS cannot refuse;
    - `a_save_during_a_run_is_busy`.
  - After saving:
    - `image_saved_gives_the_next_free_name` (`optimized-holiday` → `optimized-holiday-1`; typed `beach` → `beach-1`);
    - `open_output_folder_opens_the_folder_of_the_last_save`, and `a_run_after_a_save_wins`.
  - Names: `a_name_with_e_acute_saves_under_exactly_that_name` (Review Focus 4).
- [ ] **Step 2:** Run `cargo test -p lunaepix-app`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and clippy. Both pass.
- [ ] **Step 5:** Commit: "Save a single image under a chosen name, never replacing".

### Task 7: Window state for Single image

**Files:**
- Create: `ui/single.js`, `tests/ui/single.test.js`
- Modify: `ui/state.js` (`view` in the state; `reduce` hands the six new events and the single-image actions to `reduceSingle`; `REJECTIONS` gets the new strings; the switch gate), `tests/ui/state.test.js`

**Interfaces:**
- Consumes: `nameProblem` (Task 3), `formatSize` and `formatPercent` (`ui/format.js`).
- Produces (all pure):
  - `initialSingle()`, `reduceSingle(state, action) -> state`. The new actions are `set-view`, `set-name-text`, `preview-requested`, `layout-changed`, and the six events.
  - `canSwitchView(state) -> boolean`.
  - `previewArgs(state) -> { settings } | null`: `null` with no image, an invalid quality text, or settings equal to the last requested for this image.
  - `canSave(state)`, `saveArgs(state) -> { name, preview } | null`.
  - `sizeView(state) -> { original, compressed, saves, note, busy }`.
  - `imageAlt(state) -> string`.
  - `pixelSizeText(image) -> string`: `"4032 × 3024"`, or `""` for an SVG.
  - `canOpenFolder` is also true after `image-saved`.
  - `offerView(state) -> { notice, canMove }`.
  - `viewResets(prev, next) -> boolean`.
  - `nameText(state)`.

- [ ] **Step 1: Write the tables, then the failing tests.** Each item names the positive and the negative case.
  - **The switch:**
    - `the_view_is_single_at_start`;
    - `switching_keeps_each_views_content`;
    - `the_switch_is_locked_during_a_run_and_a_save_and_free_after`;
    - `a_locked_switch_does_nothing`.
  - **Image events:**
    - a new key sets the image and resets `nameTyped`;
    - the same key with a notice keeps everything and only sets the message;
    - `left_out > 0` offers Switch to Batch, `0` does not;
    - any later `image-changed` and `set-view` remove the offer.
  - **Preview requests:**
    - one after a new image;
    - one after a mode change;
    - none for the same settings twice;
    - none for an invalid quality text (`101`, `abc`, empty);
    - quality 0 and 100 sent as such.

    The timing (slider release; 400 ms after a valid number) is wired in Task 9. Here, `previewArgs` alone decides whether there is anything to send.
  - **Stale events:**
    - a `preview-ready` for another image, or for a version lower than the last `preview-started`, changes nothing;
    - the latest one sets the sizes and the key.
  - **Save gate:**
    - allowed with a ready key and a valid name;
    - refused while compressing, after `preview-failed`, with an invalid name, and with no image;
    - `saveArgs` carries the ready key and the typed stem.
  - **Names:**
    - the suggestion fills the box until the user types;
    - `name-suggested` does not overwrite typed text;
    - `image-saved` sets the next free name;
    - the box's problem text comes from `nameProblem`.
  - **Sizes:**
    - original, compressed and saves formatted as Batch does;
    - a file that grew shows a negative saving;
    - a note shows when present and not otherwise;
    - "Compressing…" while busy, with the previous numbers kept.
  - **Rejections:**
    - each new string gives its message from Global Constraints, and `name-taken` names the full file name;
    - an unknown string is logged.
  - **`viewResets`:**
    - true for a new key;
    - false for `name-suggested`, a settings change and `preview-ready`.
  - **`imageAlt`:** the lossless and lossy texts.
  - **`pixelSizeText`:** a raster image's size, and nothing for an SVG.
  - **Open folder:** usable after `image-saved`, not before a save or a run.
- [ ] **Step 2:** Run `node --test tests/ui/*.test.js`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `node --test tests/ui/*.test.js`. All pass.
- [ ] **Step 5:** Commit: "Window state for single image mode".

### Task 8: Zoom maths and the stored layout

**Files:**
- Create: `ui/zoom.js`, `ui/prefs.js`, `tests/ui/zoom.test.js`, `tests/ui/prefs.test.js`

**Interfaces:**
- Produces:
  - `zoomLimits(image, area) -> { fit, max }`, where `image` is `{ width, height }` and both are `null` for an SVG, using the decoded size the window passes instead. A raster image has `fit = min(1, areaW / w, areaH / h)` and `max = 8`. An SVG has `fit = min(areaW / w, areaH / h)` and `max = 8 * fit`.
  - `zoomAt(view, factor, point, limits, image, area) -> view`: the image point under `point` stays under it, the scale is clamped to `[fit, max]`, and the pan is clamped.
  - `step(view, direction, limits, image, area)`: one button press, a factor of 1.25 per step, centred.
  - `toActual(view, limits, image, area)` (100%) and `toFit(limits)`.
  - `pan(view, dx, dy, image, area)`: clamped so the picture's edge never moves past the area's edge, and centred when the picture is smaller than the area.
  - `divider(value) -> number`: clamped to 0–100.
  - `readLayout(storage) -> "under" | "beside"`, `writeLayout(storage, value) -> void`. Every storage access sits in try/catch, under the key `lunaepix.layout`.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `fit_of_a_wide_a_tall_and_a_1x1_image_is_never_above_1`;
  - `an_svg_fits_and_may_go_up_to_8_times_fit`;
  - `zoom_clamps_at_fit_and_at_800_percent`;
  - `zooming_at_a_point_keeps_that_point_under_it`;
  - `pan_clamps_at_each_edge_and_centres_a_small_picture`;
  - `step_in_then_out_returns_to_the_same_scale`;
  - `divider_clamps_below_0_and_above_100`;
  - `layout_is_under_by_default`;
  - `a_stored_beside_is_used`;
  - `a_throwing_read_a_throwing_write_and_a_junk_value_give_under_without_throwing`;
  - `write_stores_only_under_or_beside`.
- [ ] **Step 2:** Run `node --test tests/ui/zoom.test.js tests/ui/prefs.test.js`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `node --test tests/ui/*.test.js`. All pass.
- [ ] **Step 5:** Commit: "Zoom maths and the stored layout choice".

### Task 9: The Single image screen

**Files:**
- Create: `ui/single-view.js`, `tests/ui/single-view.test.js`
- Modify: `ui/index.html`, `ui/app.css`, `ui/app.js`, `src-tauri/tests/config.rs` (HTML structure checks, with the `html5ever` tokens `index_html()` already uses)

**Interfaces:**
- Consumes: Tasks 3, 7 and 8; `thumbSource`-style address building for the `preview` scheme (`convertFileSrc(key, 'preview')`, accepting only keys matching the window regex).
- Produces, in `ui/single-view.js` (DOM writes, tested with the fake document from `tests/ui/table.test.js`, moved to `tests/ui/fake-document.js` and shared):
  - `previewSource(convertFileSrc) -> (key) => url | null`;
  - `swapPicture(doc, slot, url, onReady, onFail)`: makes a new `<img>`, awaits `img.decode()`, then replaces the old one; the old one stays until then;
  - `moveSettings(doc, settings, slot)`;
  - `syncSizes(doc, box, sizeView)`;
  - `syncName(input, error, text, problem)`;
  - `applyView(stage, view)`: sets `style.transform` only;
  - `applyDivider(picture, value)`: sets `style.setProperty('--split', value + '%')`.

- [ ] **Step 1: Write the tables, then the failing tests:**
  - `previewSource_accepts_only_window_keys`;
  - `swapPicture_keeps_the_old_image_until_the_new_one_decodes`;
  - `a_failed_decode_keeps_the_old_image_and_reports`;
  - `moveSettings_moves_the_one_block_and_keeps_focus_inside_it`;
  - `syncSizes_writes_text_only` (no markup writes recorded);
  - `syncName_ties_the_error_with_aria_describedby_and_clears_it`;
  - `applyDivider_sets_only_the_custom_property`;
  - In `src-tauri/tests/config.rs`, `index_html_has_both_switches_as_fieldsets_with_legends_the_region_and_the_labelled_divider`, and a fixture string missing the divider's label that the same check flags.
  - The existing `style`-attribute and inline-script checks keep passing on the new markup.
- [ ] **Step 2:** Run `node --test tests/ui/*.test.js`. The new tests fail. Record the output.
- [ ] **Step 3:** Implement.
  - **Markup** per the spec's Layout and the mock-up: the header switch; the Single image section with the drop area, the file row, the tools row, the picture (two stacked images in one stage, corner labels, the divider handle, the hidden range input, the busy text), and the body with the settings slot, the size box, Save as and Save. Batch's section is unchanged apart from its settings slot.
  - **CSS:**
    - layouts B and A as `-under` and `-beside` on `lunaepixSingle__body`;
    - A falls back to B below 40rem;
    - in B the size box goes under the settings below 30rem;
    - the compressed image is clipped with `clip-path: inset(0 0 0 var(--split))`;
    - there are no transitions under `prefers-reduced-motion: reduce`;
    - the focus style of the hidden range input shows on the handle;
    - the picture's height is capped so the settings stay in view.
  - **Wiring in `app.js`:**
    - the view switch sends `set_view`, then dispatches `set-view` and moves `#settings`;
    - Choose image and Choose other send `choose_image`;
    - Switch to Batch sends `move_drop_to_batch` and then switches the view on success;
    - after every state change, `previewArgs` is checked on these triggers only: a new image key, a mode `change`, the Quality slider's `change` event (not `input`), the number field 400 ms after its last valid input (one `setTimeout`, cleared on each input), and the switch to Single image;
    - Save sends `saveArgs`;
    - zoom comes from the wheel (zooming at the pointer), the four buttons, pointer drags (the handle moves the divider, anywhere else pans) and the arrow keys on the focused region;
    - the layout switch writes through `writeLayout`;
    - the window listens to the six new events;
    - `drag-state` highlights the Single image drop area (or the picture once an image is loaded) as it highlights Batch's today;
    - after a save, **Open folder** (`lunaepixSingle__openFolder`, beside Save) sends `open_output_folder`.
- [ ] **Step 4:** Run `node --test tests/ui/*.test.js` and `cargo test --workspace` (config checks). Both pass.
- [ ] **Step 5:** Commit: "Single image screen: switch, compare with zoom, sizes, Save as, two layouts".

### Task 10: Documentation, version, the full suite and the hand-test list

**Files:**
- Modify:
  - `README.md` (Single image mode, renaming, the two layouts);
  - `src-tauri/INTERFACE.md`: read it whole. It must match every change above, including "Nothing is remembered between launches" becoming "Only the layout choice is remembered, in the window's own storage";
  - `docs/roadmap.md`: "Preview single image compression" → "Built; being tested by hand", and "Export rename (single file only)" → "Part of single image mode; built";
  - the workspace `Cargo.toml`: version `0.1.2`. `Cargo.lock` follows.

- [ ] **Step 1:** Update the texts and the version.
- [ ] **Step 2:** Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `node --test tests/ui/*.test.js`. All pass. Then check by reading every `#[cfg(unix)]` / `#[cfg(windows)]` block this work added: Linux clippy cannot see the Windows side (lesson from v0.1.1-rc1).
- [ ] **Step 3:** Write the user's hand-test list into the report (not the repository), for Linux and Windows. It is the spec's hand test, item by item:
  - a 150-character file name at 360 px wide in both layouts;
  - a phone photo: Fit, 100%, 800%, panning, and the divider by mouse and by keys;
  - lossy 20 / 75 / 100 and lossless;
  - a photo near 100 megapixels: the window keeps responding;
  - a transparent PNG, an animated WebP and an SVG;
  - three files dropped, then a folder, then Switch to Batch;
  - a taken name; Save twice;
  - switching layouts, then restarting the app: A is remembered;
  - Batch still works as before;
  - an EXIF-rotated phone photo in lossy: both sides upright.
- [ ] **Step 4:** Commit: "Document single image mode; version 0.1.2".

## Implementer report

Every implementer and every fix round writes, **before any code**:

1. **A test table:** behaviour → positive test → negative test → "fails on its own because…", one row per behaviour.
2. **A failure-branch table:**
   - every `Err` path, fallback, `catch`, kill/reap and "log and skip", and every if/else driven by a setting or option the task adds or touches → the test that reaches it, or "untested" with the reason;
   - plus one row per core function the task newly calls, naming the branches inside it the new caller can reach.

The report then gives:
- the RED run (command and failure output, taken before the production code existed);
- the GREEN run;
- the "Reuse survey" block from `~/.claude/CLAUDE.md`;
- the files changed.

Reviewers check the diff against both tables and probe alternative spellings in any structured-file check. Lunaepix has no Grunt build and no devsync deployment, so there is nothing to build or upload beyond `cargo` and `node`.

## CMS changes

No CMS changes.
