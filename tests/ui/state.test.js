import test from 'node:test';
import assert from 'node:assert/strict';

import {
  MESSAGES,
  afterRejection,
  canChoose,
  canOpenFolder,
  canRun,
  classifyRejection,
  dropTitleText,
  headingLine,
  initialState,
  panelView,
  reduce,
  rowView,
  runSettings,
  saveToText,
  settingsView,
  settledFailures,
  startRunArgs,
  summaryLine,
} from '../../ui/state.js';

function apply(state, ...actions) {
  return actions.reduce((current, action) => reduce(current, action), state);
}

const started = (total, mode = 'lossless', quality = null) => ({
  type: 'run-started',
  payload: { total, mode, quality },
});
const done = (index, name = 'a.png', before = 1000, after = 800, millis = 12) => ({
  type: 'file-done',
  payload: { index, name, status: 'done', before, after, millis },
});
const skipped = (index, name = 'notes.txt', reason = 'Not a PNG, JPG, SVG or WebP file') => ({
  type: 'file-done',
  payload: { index, name, status: 'skipped', reason },
});
const failed = (index, name = 'broken.png', reason = 'the optimiser crashed on this file') => ({
  type: 'file-done',
  payload: { index, name, status: 'failed', reason },
});
const finished = (optimised, savedBytes, hasOutput) => ({
  type: 'run-finished',
  payload: { optimised, saved_bytes: savedBytes, has_output: hasOutput },
});

// initial state

test('the initial state is the empty state: nothing running, no rows, no summary', () => {
  const state = initialState();
  assert.equal(state.phase, 'empty');
  assert.deepEqual(state.rows, []);
  assert.equal(state.summary, null);
  assert.equal(state.hasOutput, false);
  assert.equal(state.dragOver, false);
  assert.equal(state.message, '');
});

// run-started

test('run-started begins a run with the announced total', () => {
  const state = reduce(initialState(), started(3));
  assert.equal(state.phase, 'running');
  assert.equal(state.total, 3);
});

test('run-started clears the table, the summary, the output flag and the message', () => {
  const previous = apply(
    initialState(),
    started(1),
    done(0),
    finished(1, 200, true),
    { type: 'run-busy', payload: null },
  );
  assert.equal(previous.rows.length, 1);
  const state = reduce(previous, started(2));
  assert.deepEqual(state.rows, []);
  assert.equal(state.summary, null);
  assert.equal(state.hasOutput, false);
  assert.equal(state.message, '');
});

test('run-started with a payload that has no usable total counts zero files', () => {
  assert.equal(reduce(initialState(), { type: 'run-started', payload: null }).total, 0);
  assert.equal(reduce(initialState(), started(-4)).total, 0);
  assert.equal(reduce(initialState(), started('7')).total, 0);
});

// file-done

test('file-done adds a done row with the numbers', () => {
  const state = apply(initialState(), started(1), done(0, 'logo.svg', 21828, 9631, 22));
  assert.deepEqual(state.rows, [
    { kind: 'done', name: 'logo.svg', before: 21828, after: 9631, millis: 22 },
  ]);
});

test('file-done adds a skipped row with its reason and no numbers', () => {
  const state = apply(initialState(), started(1), skipped(0));
  assert.deepEqual(state.rows, [
    { kind: 'skipped', name: 'notes.txt', reason: 'Not a PNG, JPG, SVG or WebP file' },
  ]);
  assert.equal('before' in state.rows[0], false);
});

test('file-done adds a failed row with its reason', () => {
  const state = apply(initialState(), started(1), failed(0));
  assert.equal(state.rows[0].kind, 'failed');
  assert.equal(state.rows[0].reason, 'the optimiser crashed on this file');
});

test('file-done keeps rows in the order they arrive', () => {
  const state = apply(initialState(), started(3), done(0, 'a.png'), skipped(1, 'b.txt'), failed(2, 'c.png'));
  assert.deepEqual(
    state.rows.map((row) => [row.name, row.kind]),
    [['a.png', 'done'], ['b.txt', 'skipped'], ['c.png', 'failed']],
  );
});

test('file-done appends to the run\'s own row list instead of copying it', () => {
  const first = apply(initialState(), started(3), done(0));
  const list = first.rows;
  const second = reduce(first, skipped(1));
  assert.equal(second.rows, list, 'the same array, one row longer');
  assert.equal(second.rows.length, 2);
  assert.notEqual(second, first, 'still a new state object');
});

test('run-started gives the new run a new row list and leaves the last run\'s rows alone', () => {
  const previous = apply(initialState(), started(2), done(0), done(1));
  const list = previous.rows;
  const next = apply(previous, started(1), done(0, 'new.png'));
  assert.notEqual(next.rows, list);
  assert.equal(list.length, 2);
  assert.deepEqual(next.rows.map((row) => row.name), ['new.png']);
});

test('a run of more than 2000 files keeps every row, in order', () => {
  const total = 2500;
  let state = reduce(initialState(), started(total));
  const list = state.rows;
  for (let index = 0; index < total; index += 1) {
    state = reduce(state, done(index, `img${index}.png`));
  }
  assert.equal(state.rows, list);
  assert.equal(state.rows.length, total);
  assert.equal(state.rows[0].name, 'img0.png');
  assert.equal(state.rows[total - 1].name, `img${total - 1}.png`);
});

test('file-done ignores a status it does not know', () => {
  const before = apply(initialState(), started(1));
  const state = reduce(before, {
    type: 'file-done',
    payload: { index: 0, name: 'x.png', status: 'weird' },
  });
  assert.deepEqual(state.rows, []);
});

test('file-done ignores a payload that is not an object', () => {
  const before = apply(initialState(), started(1));
  assert.deepEqual(reduce(before, { type: 'file-done', payload: null }).rows, []);
  assert.deepEqual(reduce(before, { type: 'file-done', payload: 'done' }).rows, []);
});

test('file-done with a name that holds markup keeps it as plain text data', () => {
  const state = apply(initialState(), started(1), done(0, '<img src=x onerror=alert(1)>.png'));
  assert.equal(state.rows[0].name, '<img src=x onerror=alert(1)>.png');
});

test('file-done with missing numbers on a done file shows zeros, not NaN', () => {
  const state = apply(initialState(), started(1), {
    type: 'file-done',
    payload: { index: 0, name: 'a.png', status: 'done' },
  });
  assert.deepEqual(state.rows[0], { kind: 'done', name: 'a.png', before: 0, after: 0, millis: 0 });
});

test('file-done with a missing reason says why in words', () => {
  const state = apply(initialState(), started(1), {
    type: 'file-done',
    payload: { index: 0, name: 'a.png', status: 'failed' },
  });
  assert.equal(state.rows[0].reason, 'No reason was given');
});

// run-finished

test('run-finished sets the summary and the announcement', () => {
  const state = apply(initialState(), started(2), done(0), done(1), finished(2, 341210, true));
  assert.equal(state.phase, 'finished');
  assert.deepEqual(state.summary, { optimised: 2, savedBytes: 341210 });
  assert.equal(state.message, 'Optimised 2 files, saved 333.2 KiB');
});

test('run-finished with has_output true allows Open folder', () => {
  assert.equal(apply(initialState(), started(1), finished(1, 5, true)).hasOutput, true);
});

test('run-finished with has_output false keeps Open folder off', () => {
  assert.equal(apply(initialState(), started(1), finished(0, 0, false)).hasOutput, false);
});

test('run-finished without has_output, or with a non-boolean, keeps Open folder off', () => {
  const missing = reduce(initialState(), { type: 'run-finished', payload: { optimised: 1, saved_bytes: 1 } });
  assert.equal(missing.hasOutput, false);
  const text = reduce(initialState(), {
    type: 'run-finished',
    payload: { optimised: 1, saved_bytes: 1, has_output: 'true' },
  });
  assert.equal(text.hasOutput, false);
});

test('run-finished for a single file uses the singular in the announcement', () => {
  const state = apply(initialState(), started(1), finished(1, 100, true));
  assert.equal(state.message, 'Optimised 1 file, saved 100 B');
});

const NO_FILES = 'No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)';

test('a run that planned no files says so, and that subfolders are not included, in the announcement and the summary', () => {
  const state = apply(initialState(), started(0), finished(0, 0, false));
  assert.equal(MESSAGES.noFiles, NO_FILES);
  assert.equal(state.message, NO_FILES);
  assert.equal(summaryLine(state), NO_FILES);
  assert.deepEqual(state.rows, []);
});

test('a run with one file, even a skipped one, gives the usual totals instead', () => {
  const state = apply(initialState(), started(1), skipped(0), finished(0, 0, false));
  assert.equal(state.message, 'Optimised 0 files, saved 0 B');
  assert.equal(summaryLine(state), 'Total: 0 files, saved 0 B');
});

test('the summary line of a run with optimised files is its total', () => {
  const state = apply(initialState(), started(2), done(0), done(1), finished(2, 341210, true));
  assert.equal(summaryLine(state), 'Total: 2 files, saved 333.2 KiB');
});

test('run-finished treats unusable numbers as zero', () => {
  const state = reduce(initialState(), { type: 'run-finished', payload: null });
  assert.deepEqual(state.summary, { optimised: 0, savedBytes: 0 });
  assert.equal(state.hasOutput, false);
});

test('run-finished makes a new announcement even when the text is the same as before', () => {
  const first = apply(initialState(), started(1), finished(1, 100, true));
  const second = apply(first, started(1), finished(1, 100, true));
  assert.equal(first.message, second.message);
  assert.notEqual(first.messageSeq, second.messageSeq);
});

// run-busy and rejections

test('run-busy sets the busy message and leaves a running run alone', () => {
  const running = apply(initialState(), started(4), done(0));
  const state = reduce(running, { type: 'run-busy', payload: null });
  assert.equal(state.message, "Already optimising; try again when it's done.");
  assert.equal(state.phase, 'running');
  assert.equal(state.rows.length, 1);
});

test('a "busy" rejection is handled like run-busy', () => {
  const state = reduce(initialState(), { type: 'command-rejected', payload: 'busy' });
  assert.equal(state.message, MESSAGES.busy);
  assert.equal(state.message, reduce(initialState(), { type: 'run-busy', payload: null }).message);
});

test('a "no-output" rejection says nothing has been written yet', () => {
  const state = reduce(initialState(), { type: 'command-rejected', payload: 'no-output' });
  assert.equal(state.message, 'Nothing has been written yet.');
});

test('an "open-failed" rejection says the folder could not be opened', () => {
  const state = reduce(initialState(), { type: 'command-rejected', payload: 'open-failed' });
  assert.equal(state.message, 'The folder could not be opened.');
});

test('a rejection it does not know gets a generic message and never shows the raw text', () => {
  const state = reduce(initialState(), { type: 'command-rejected', payload: '/home/me/secret/path failed' });
  assert.equal(state.message, MESSAGES.unknown);
  assert.doesNotMatch(state.message, /secret/);
  // Names that exist on every object are not rejections either.
  for (const inherited of ['constructor', 'toString', '__proto__']) {
    const result = reduce(initialState(), { type: 'command-rejected', payload: inherited });
    assert.equal(result.message, MESSAGES.unknown, inherited);
  }
  const fromError = reduce(initialState(), { type: 'command-rejected', payload: new Error('x') });
  assert.equal(fromError.message, MESSAGES.unknown);
});

test('a rejection does not change the table or the run', () => {
  const finishedRun = apply(initialState(), started(1), done(0), finished(1, 200, true));
  const state = reduce(finishedRun, { type: 'command-rejected', payload: 'open-failed' });
  assert.deepEqual(state.rows, finishedRun.rows);
  assert.deepEqual(state.summary, finishedRun.summary);
  assert.equal(state.hasOutput, true);
});

// run-cancelled

test('run-cancelled changes nothing visible', () => {
  const finishedRun = apply(initialState(), started(1), done(0), finished(1, 200, true));
  assert.equal(reduce(finishedRun, { type: 'run-cancelled', payload: null }), finishedRun);
  const initial = initialState();
  assert.equal(reduce(initial, { type: 'run-cancelled', payload: null }), initial);
});

// drag-state

test('drag-state over true sets the highlight', () => {
  assert.equal(reduce(initialState(), { type: 'drag-state', payload: { over: true } }).dragOver, true);
});

test('drag-state over false clears the highlight', () => {
  const over = reduce(initialState(), { type: 'drag-state', payload: { over: true } });
  assert.equal(reduce(over, { type: 'drag-state', payload: { over: false } }).dragOver, false);
});

test('drag-state with a payload that is not exactly over:true does not highlight', () => {
  assert.equal(reduce(initialState(), { type: 'drag-state', payload: { over: 'yes' } }).dragOver, false);
  assert.equal(reduce(initialState(), { type: 'drag-state', payload: null }).dragOver, false);
});

// general

test('an event it does not know changes nothing', () => {
  const state = initialState();
  assert.equal(reduce(state, { type: 'tauri-something', payload: {} }), state);
});

test('reduce never changes the state it was given, other than file-done appending its row', () => {
  const before = apply(initialState(), started(2), done(0));
  const snapshot = JSON.stringify(before);
  for (const action of [
    finished(1, 5, true),
    started(3),
    { type: 'run-busy', payload: null },
    { type: 'command-rejected', payload: 'no-output' },
    { type: 'drag-state', payload: { over: true } },
  ]) {
    reduce(before, action);
    assert.equal(JSON.stringify(before), snapshot, action.type);
  }
  // file-done appends to the run's row list (no copy per event), and
  // changes nothing else of the state it was given.
  const withoutRows = (state) => JSON.stringify({ ...state, rows: null });
  const rowsBefore = JSON.stringify(before.rows);
  const restSnapshot = withoutRows(before);
  reduce(before, done(1, 'b.png'));
  assert.equal(withoutRows(before), restSnapshot);
  assert.equal(before.rows.length, 2);
  assert.equal(JSON.stringify(before.rows.slice(0, 1)), rowsBefore);
});

// rowView

test('rowView of a done row has the formatted numbers', () => {
  assert.deepEqual(rowView({ kind: 'done', name: 'logo.svg', before: 21828, after: 9631, millis: 22 }), {
    kind: 'done',
    name: 'logo.svg',
    before: '21.3 KiB',
    after: '9.4 KiB',
    saved: '55.9%',
    time: '22 ms',
  });
});

test('rowView of a grown file shows a negative saving', () => {
  const view = rowView({ kind: 'done', name: 'a.png', before: 1000, after: 1100, millis: 3 });
  assert.equal(view.saved, '-10.0%');
});

test('rowView of a skipped row has the status word and reason as text, and no numbers', () => {
  const view = rowView({ kind: 'skipped', name: 'notes.txt', reason: 'Not a PNG, JPG, SVG or WebP file' });
  assert.deepEqual(view, {
    kind: 'skipped',
    name: 'notes.txt',
    text: 'skipped: not a PNG, JPG, SVG or WebP file',
  });
  assert.equal('before' in view, false);
});

test('rowView of a failed row has the word "failed" and the reason', () => {
  const view = rowView({ kind: 'failed', name: 'b.png', reason: 'the optimiser crashed on this file' });
  assert.equal(view.text, 'failed: the optimiser crashed on this file');
  assert.equal(view.kind, 'failed');
});

test('rowView gives a nameless file a readable name', () => {
  assert.equal(rowView({ kind: 'skipped', name: '', reason: 'x' }).name, '(no name)');
  assert.equal(rowView({ kind: 'done', name: 'a.png', before: 1, after: 1, millis: 1 }).name, 'a.png');
});

// canChoose and canOpenFolder: the two gates the click handlers and the
// aria-disabled attributes both use

test('canChoose is true when idle', () => {
  assert.equal(canChoose(initialState()), true);
  assert.equal(canChoose(apply(initialState(), started(1), finished(1, 5, true))), true);
});

test('canChoose is false while a run is active', () => {
  assert.equal(canChoose(apply(initialState(), started(2))), false);
  assert.equal(canChoose(apply(initialState(), started(2), done(0))), false);
});

test('canOpenFolder is true after a run that wrote output', () => {
  assert.equal(canOpenFolder(apply(initialState(), started(1), finished(1, 5, true))), true);
});

test('canOpenFolder is false before any run and after a run with no output', () => {
  assert.equal(canOpenFolder(initialState()), false);
  assert.equal(canOpenFolder(apply(initialState(), started(1), finished(0, 0, false))), false);
});

test('canOpenFolder is false when has_output is missing from the last run-finished', () => {
  const state = reduce(initialState(), { type: 'run-finished', payload: { optimised: 1, saved_bytes: 1 } });
  assert.equal(canOpenFolder(state), false);
});

test('canOpenFolder is false while a new run is active, even after an earlier run with output', () => {
  const earlier = apply(initialState(), started(1), finished(1, 5, true));
  assert.equal(canOpenFolder(reduce(earlier, started(3))), false);
  // Also when a state claims both (the gate does not rely on the reducer).
  assert.equal(canOpenFolder({ ...earlier, phase: 'running' }), false);
});

// classifyRejection: what the page does with a rejected command

test('classifyRejection says "busy" for a busy rejection, which run-busy already announces', () => {
  assert.equal(classifyRejection('busy'), 'busy');
});

test('classifyRejection says "known" for the other two rejections of the interface', () => {
  assert.equal(classifyRejection('no-output'), 'known');
  assert.equal(classifyRejection('open-failed'), 'known');
});

test('classifyRejection says "unknown" for anything else, so it can be logged', () => {
  for (const other of ['', 'Busy', 'listen-failed', 'constructor', new Error('x'), null, undefined, 42, ['busy']]) {
    assert.equal(classifyRejection(other), 'unknown', String(other));
  }
});

// settledFailures: the reasons of the rejected results of Promise.allSettled

test('settledFailures lists the reasons of rejected results, in order', () => {
  const results = [
    { status: 'fulfilled', value: () => {} },
    { status: 'rejected', reason: 'first' },
    { status: 'rejected', reason: 'second' },
  ];
  assert.deepEqual(settledFailures(results), ['first', 'second']);
});

test('settledFailures is empty when every result is fulfilled', () => {
  assert.deepEqual(settledFailures([{ status: 'fulfilled', value: 1 }, { status: 'fulfilled', value: 2 }]), []);
  assert.deepEqual(settledFailures([]), []);
});

// ---------------------------------------------------------------------
// The one-panel window: the file list, the settings and Run (Task 11).
// ---------------------------------------------------------------------

const SKIP = 'Not a PNG, JPG, SVG or WebP file';
// How the window shows it after "Will be skipped: " (spec and mock-up).
const SKIP_LOWER = 'not a PNG, JPG, SVG or WebP file';
const NO_IMAGES = 'No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)';
const entry = (name, size = 1000, skip = null) => ({ name, size, skip });
const listed = (files, notice = null) => ({ type: 'list-changed', payload: { files, notice } });
const threeFiles = () =>
  listed([entry('holiday.jpg', 2400000), entry('logo.png', 88000), entry('banner.webp', 410000)]);
const fileStarted = (index) => ({ type: 'file-started', payload: { index } });
const setMode = (mode) => ({ type: 'set-mode', payload: mode });
const setQuality = (text) => ({ type: 'set-quality-text', payload: text });
const outputChanged = (folder) => ({ type: 'output-changed', payload: { folder } });
const doneWithNote = (index, name, note) => ({
  type: 'file-done',
  payload: { index, name, status: 'done', before: 512000, after: 498000, millis: 380, note },
});
const busy = { type: 'run-busy', payload: null };

// launch

test('launch_state_is_lossless_75_no_folder_empty', () => {
  const state = initialState();
  assert.equal(state.phase, 'empty');
  assert.equal(state.mode, 'lossless');
  assert.equal(state.quality, 75);
  assert.equal(state.qualityText, '75');
  assert.equal(state.folder, null);
  assert.equal(state.current, null);
  assert.deepEqual(state.rows, []);
  assert.equal(saveToText(state), 'Next to the originals');
  assert.deepEqual(runSettings(state), { mode: 'lossless', quality: 75 });
});

test('quality_cannot_change_while_lossless', () => {
  const ready = reduce(initialState(), threeFiles());
  const state = reduce(ready, setQuality('40'));
  assert.equal(state.quality, 75);
  assert.equal(state.qualityText, '75');
  assert.deepEqual(runSettings(state), { mode: 'lossless', quality: 75 });
  // The same action does change it once Lossy is picked.
  assert.equal(apply(ready, setMode('lossy'), setQuality('40')).quality, 40);
});

// the list

test('list_changed_fills_rows_and_writes_a_notice_to_the_status', () => {
  const state = apply(
    initialState(),
    listed([entry('holiday.jpg', 2400000), entry('notes.txt', null, SKIP)]),
    // A dropped folder without images onto a list that has rows: the same
    // list again, with a notice.
    listed([entry('holiday.jpg', 2400000), entry('notes.txt', null, SKIP)], NO_IMAGES),
  );
  assert.equal(state.phase, 'ready');
  assert.deepEqual(state.rows, [
    { kind: 'listed', name: 'holiday.jpg', size: 2400000, skip: null },
    { kind: 'listed', name: 'notes.txt', size: null, skip: SKIP },
  ]);
  assert.equal(state.message, NO_IMAGES);
  // Only the row that will be compressed counts.
  assert.equal(headingLine(state), '1 file ready');
  assert.deepEqual(rowView(state.rows[0]), { kind: 'listed', name: 'holiday.jpg', size: '2.3 MiB', text: '' });
  assert.deepEqual(rowView(state.rows[1]), {
    kind: 'listed',
    name: 'notes.txt',
    size: '',
    text: `Will be skipped: ${SKIP_LOWER}`,
  });
});

test('an_empty_list_hides_the_panel_and_notice_null_leaves_the_status', () => {
  const withMessage = apply(initialState(), threeFiles(), busy);
  const state = reduce(withMessage, listed([], null));
  assert.equal(state.phase, 'empty');
  assert.deepEqual(state.rows, []);
  assert.equal(headingLine(state), '');
  assert.equal(state.message, MESSAGES.busy);
  assert.equal(state.messageSeq, withMessage.messageSeq);
});

test('list_changed_with_a_bad_payload_changes_nothing', () => {
  const ready = reduce(initialState(), threeFiles());
  for (const payload of [null, 'files', {}, { files: 'a.png' }, { files: null }]) {
    assert.equal(reduce(ready, { type: 'list-changed', payload }), ready, JSON.stringify(payload));
  }
  // Control: a well-formed list does replace the rows.
  assert.deepEqual(reduce(ready, listed([entry('x.png')])).rows.map((row) => row.name), ['x.png']);
});

test('list_changed_reads_bad_fields_as_no_name_no_size_no_skip', () => {
  const state = reduce(
    initialState(),
    listed([{ name: 42, size: -1, skip: '' }, { name: 'a.png', size: '5', skip: 3 }, null]),
  );
  assert.deepEqual(state.rows, [
    { kind: 'listed', name: '', size: null, skip: null },
    { kind: 'listed', name: 'a.png', size: null, skip: null },
    { kind: 'listed', name: '', size: null, skip: null },
  ]);
  assert.equal(rowView(state.rows[0]).name, '(no name)');
});

// Run

test('can_run_with_one_compressible_row', () => {
  const state = reduce(initialState(), listed([entry('notes.txt', null, SKIP), entry('logo.png')]));
  assert.equal(canRun(state), true);
  assert.equal(canRun(reduce(state, setMode('lossy'))), true);
});

test('cannot_run_empty_only_skipped_or_running', () => {
  assert.equal(canRun(initialState()), false);
  assert.equal(canRun(reduce(initialState(), listed([entry('notes.txt', null, SKIP)]))), false);
  const running = apply(initialState(), threeFiles(), started(3));
  assert.equal(canRun(running), false);
  const finishedRun = apply(running, done(0), done(1), done(2), finished(3, 10, true));
  assert.equal(canRun(finishedRun), false);
});

// running rows

test('file_started_marks_compressing_and_later_rows_waiting', () => {
  const state = apply(
    initialState(),
    threeFiles(),
    started(3, 'lossy', 75),
    fileStarted(0),
    done(0, 'holiday.jpg', 2400000, 610000, 1210),
    fileStarted(1),
  );
  assert.equal(state.current, 1);
  assert.equal(rowView(state.rows[0]).kind, 'done');
  assert.deepEqual(rowView(state.rows[1]), {
    kind: 'compressing',
    name: 'logo.png',
    size: '85.9 KiB',
    text: 'Compressing…',
  });
  assert.deepEqual(rowView(state.rows[2]), {
    kind: 'waiting',
    name: 'banner.webp',
    size: '400.4 KiB',
    text: 'Waiting',
  });
});

test('a_done_row_never_returns_to_compressing', () => {
  const afterFirst = apply(initialState(), threeFiles(), started(3), fileStarted(0), done(0, 'holiday.jpg'));
  const doneRow = afterFirst.rows[0];
  const state = reduce(afterFirst, fileStarted(0));
  assert.equal(state.rows[0], doneRow);
  assert.equal(rowView(state.rows[0]).kind, 'done');
  assert.equal(state.current, null);
  // A skipped or failed result stays as well.
  const withSkip = apply(afterFirst, skipped(1, 'logo.png'), fileStarted(1));
  assert.equal(rowView(withSkip.rows[1]).kind, 'skipped');
});

test('run_started_shows_the_list_rows_as_waiting_and_the_skips_as_skipped', () => {
  const state = apply(
    initialState(),
    listed([entry('holiday.jpg', 2400000), entry('notes.txt', null, SKIP)]),
    started(2, 'lossy', 75),
  );
  assert.equal(state.phase, 'running');
  assert.deepEqual(rowView(state.rows[0]), { kind: 'waiting', name: 'holiday.jpg', size: '2.3 MiB', text: 'Waiting' });
  assert.deepEqual(rowView(state.rows[1]), { kind: 'skipped', name: 'notes.txt', text: `skipped: ${SKIP_LOWER}` });
});

test('run_started_with_fewer_rows_than_the_list_shows_only_the_run_rows', () => {
  const state = apply(initialState(), threeFiles(), started(2));
  assert.deepEqual(
    state.rows.map((row) => row.name),
    ['holiday.jpg', 'logo.png'],
  );
});

test('file_done_outside_a_run_or_past_its_rows_is_ignored', () => {
  const ready = reduce(initialState(), threeFiles());
  assert.equal(reduce(ready, done(0, 'x.png')), ready);
  const running = apply(initialState(), threeFiles(), started(3));
  const rowsBefore = JSON.stringify(running.rows);
  for (const index of [5, -1, '1', 1.5, null]) {
    assert.equal(reduce(running, done(index, 'x.png')), running, String(index));
  }
  assert.equal(JSON.stringify(running.rows), rowsBefore);
});

test('file_started_announces_the_progress_in_the_status_line', () => {
  const runningState = apply(initialState(), threeFiles(), started(3, 'lossy', 75));
  const state = reduce(runningState, fileStarted(0));
  assert.equal(state.message, 'Compressing 1 of 3 · Lossy 75');
  assert.notEqual(state.messageSeq, runningState.messageSeq);
});

test('a_file_started_for_a_finished_or_missing_row_announces_nothing', () => {
  const afterFirst = apply(initialState(), threeFiles(), started(3), fileStarted(0), done(0, 'holiday.jpg'));
  for (const index of [0, 9, -1, '1']) {
    assert.equal(reduce(afterFirst, fileStarted(index)), afterFirst, String(index));
  }
  const ready = reduce(initialState(), threeFiles());
  assert.equal(reduce(ready, fileStarted(0)), ready);
  // Control: the next waiting row is announced.
  assert.notEqual(reduce(afterFirst, fileStarted(1)).messageSeq, afterFirst.messageSeq);
});

test('a_run_that_broke_off_marks_its_unfinished_rows', () => {
  const state = apply(
    initialState(),
    threeFiles(),
    started(3),
    fileStarted(0),
    done(0, 'holiday.jpg'),
    fileStarted(1),
    finished(1, 10, true),
  );
  assert.equal(state.phase, 'finished');
  assert.equal(state.current, null);
  assert.equal(rowView(state.rows[0]).kind, 'done');
  for (const index of [1, 2]) {
    assert.deepEqual(rowView(state.rows[index]), {
      kind: 'failed',
      name: ['holiday.jpg', 'logo.png', 'banner.webp'][index],
      text: 'failed: The run ended before this file was done',
    });
  }
});

test('a_complete_run_keeps_its_rows_as_they_arrived', () => {
  const lastRow = apply(
    initialState(),
    threeFiles(),
    started(3),
    fileStarted(0),
    done(0, 'holiday.jpg'),
    skipped(1, 'logo.png'),
    fileStarted(2),
    failed(2, 'banner.webp'),
  );
  const rows = lastRow.rows;
  const kept = [...rows];
  const state = reduce(lastRow, finished(1, 10, true));
  assert.equal(state.phase, 'finished');
  assert.equal(state.rows, rows);
  assert.deepEqual(state.rows, kept);
  assert.deepEqual(state.rows.map((row) => row.kind), ['done', 'skipped', 'failed']);
});

// settings

test('run_settings_sends_mode_and_integer_quality', () => {
  const ready = apply(initialState(), threeFiles(), setMode('lossy'));
  for (const [text, quality] of [['40', 40], ['0', 0], ['100', 100], ['075', 75]]) {
    const settings = runSettings(reduce(ready, setQuality(text)));
    assert.deepEqual(settings, { mode: 'lossy', quality }, text);
    assert.deepEqual(Object.keys(settings), ['mode', 'quality']);
    assert.equal(Number.isInteger(settings.quality), true);
    assert.equal(canRun(reduce(ready, setQuality(text))), true, text);
  }
});

test('text_fraction_or_out_of_range_blocks_run_and_sends_nothing', () => {
  const ready = apply(initialState(), threeFiles(), setMode('lossy'));
  for (const text of ['', 'abc', '7.5', '101', '-1', '1e2', ' 50', '50 ', '0x10', '1000', '٧٥']) {
    const state = reduce(ready, setQuality(text));
    assert.equal(runSettings(state), null, JSON.stringify(text));
    assert.equal(canRun(state), false, JSON.stringify(text));
  }
});

test('range_and_number_follow_each_other', () => {
  const lossy = apply(initialState(), threeFiles(), setMode('lossy'));
  // The range sends its value; the number field shows it.
  const fromRange = reduce(lossy, setQuality('30'));
  assert.equal(fromRange.quality, 30);
  assert.equal(fromRange.qualityText, '30');
  // The number field sends its text; the range moves to it.
  const fromNumber = reduce(fromRange, setQuality('45'));
  assert.equal(fromNumber.quality, 45);
  assert.equal(fromNumber.qualityText, '45');
});

test('an_invalid_number_does_not_move_the_range', () => {
  const at45 = apply(initialState(), threeFiles(), setMode('lossy'), setQuality('45'));
  for (const text of ['7.5', '101', '', 'abc']) {
    const state = reduce(at45, setQuality(text));
    assert.equal(state.quality, 45, text);
    assert.equal(state.qualityText, text);
  }
});

test('set_mode_switches_to_lossy_and_back', () => {
  const lossy = apply(initialState(), threeFiles(), setMode('lossy'));
  assert.equal(lossy.mode, 'lossy');
  assert.deepEqual(runSettings(lossy), { mode: 'lossy', quality: 75 });
  assert.equal(reduce(lossy, setMode('lossless')).mode, 'lossless');
});

test('set_mode_with_an_unknown_word_or_while_running_changes_nothing', () => {
  const ready = reduce(initialState(), threeFiles());
  for (const word of ['Lossy', 'lossy ', 'fast', null, 1]) {
    assert.equal(reduce(ready, setMode(word)), ready, String(word));
  }
  // Control: the right word, while not running, does switch.
  assert.equal(reduce(ready, setMode('lossy')).mode, 'lossy');
  const running = apply(initialState(), threeFiles(), started(3));
  assert.equal(reduce(running, setMode('lossy')), running);
  const runningLossy = apply(initialState(), threeFiles(), setMode('lossy'), started(3, 'lossy', 75));
  assert.equal(reduce(runningLossy, setQuality('20')), runningLossy);
});

test('switching_to_lossless_puts_back_the_last_valid_quality', () => {
  const state = apply(initialState(), threeFiles(), setMode('lossy'), setQuality('60'), setQuality('7.5'), setMode('lossless'));
  assert.equal(state.qualityText, '60');
  assert.equal(state.quality, 60);
  assert.deepEqual(runSettings(state), { mode: 'lossless', quality: 60 });
  assert.equal(canRun(state), true);
});

test('switching_to_lossless_keeps_a_valid_quality_text_as_typed', () => {
  const state = apply(initialState(), threeFiles(), setMode('lossy'), setQuality('075'), setMode('lossless'));
  assert.equal(state.qualityText, '075');
  assert.equal(state.quality, 75);
});

// the output folder

test('output_changed_shows_the_folder_name_in_save_to', () => {
  const chosen = reduce(initialState(), outputChanged('exports'));
  assert.equal(chosen.folder, 'exports');
  assert.equal(saveToText(chosen), 'exports');
  // A folder without a name, such as a drive root.
  assert.equal(saveToText(reduce(initialState(), outputChanged(''))), '(no name)');
  // A name with markup is kept as text data.
  assert.equal(saveToText(reduce(initialState(), outputChanged('<b>x</b>'))), '<b>x</b>');
  const back = reduce(chosen, outputChanged(null));
  assert.equal(back.folder, null);
  assert.equal(saveToText(back), 'Next to the originals');
});

test('output_changed_with_a_bad_payload_keeps_the_output', () => {
  const chosen = reduce(initialState(), outputChanged('exports'));
  assert.equal(chosen.folder, 'exports');
  for (const payload of [null, 'exports', {}, { folder: 42 }, { folder: ['a'] }]) {
    assert.equal(reduce(chosen, { type: 'output-changed', payload }), chosen, JSON.stringify(payload));
  }
});

// the heading

test('heading_reads_compressing_2_of_3_lossy_75_then_done_lossy_75_and_lossless_without_number', () => {
  const second = apply(
    initialState(),
    threeFiles(),
    setMode('lossy'),
    started(3, 'lossy', 75),
    fileStarted(0),
    done(0, 'holiday.jpg'),
    fileStarted(1),
  );
  assert.equal(headingLine(second), 'Compressing 2 of 3 · Lossy 75');
  const lossyDone = apply(second, done(1, 'logo.png'), fileStarted(2), done(2, 'banner.webp'), finished(3, 10, true));
  assert.equal(headingLine(lossyDone), 'Done · Lossy 75');

  const lossless = apply(initialState(), threeFiles(), started(3, 'lossless', null), fileStarted(0));
  assert.equal(headingLine(lossless), 'Compressing 1 of 3 · Lossless');
  const losslessDone = apply(lossless, done(0), fileStarted(1), done(1), fileStarted(2), done(2), finished(3, 1, true));
  assert.equal(headingLine(losslessDone), 'Done · Lossless');
});

test('a_lossless_run_never_shows_a_quality', () => {
  // Even when a quality comes with a lossless run, it is not shown.
  const running = apply(initialState(), threeFiles(), started(3, 'lossless', 75), fileStarted(0));
  assert.equal(headingLine(running), 'Compressing 1 of 3 · Lossless');
  assert.doesNotMatch(headingLine(running), /75/);
  const finishedRun = apply(running, done(0), skipped(1), skipped(2), finished(1, 1, true));
  assert.equal(headingLine(finishedRun), 'Done · Lossless');
  assert.doesNotMatch(headingLine(finishedRun), /\d/);
});

// notes

test('a_note_shows_on_its_row', () => {
  const state = apply(
    initialState(),
    listed([entry('anim.png', 512000)]),
    started(1, 'lossy', 75),
    fileStarted(0),
    doneWithNote(0, 'anim.png', 'kept lossless: animated PNG'),
  );
  assert.equal(state.rows[0].note, 'kept lossless: animated PNG');
  assert.equal(rowView(state.rows[0]).note, 'kept lossless: animated PNG');
});

test('no_note_no_text', () => {
  const plain = apply(initialState(), listed([entry('a.png')]), started(1), fileStarted(0), done(0, 'a.png'));
  assert.equal('note' in plain.rows[0], false);
  assert.equal('note' in rowView(plain.rows[0]), false);
  for (const note of ['', 42, null]) {
    const state = apply(initialState(), listed([entry('a.png')]), started(1), fileStarted(0), doneWithNote(0, 'a.png', note));
    assert.equal('note' in rowView(state.rows[0]), false, String(note));
  }
  // Control: in the same run, a real note does show on its own row only.
  const mixed = apply(
    initialState(),
    listed([entry('a.png'), entry('cmyk.jpg')]),
    started(2, 'lossy', 75),
    fileStarted(0),
    done(0, 'a.png'),
    fileStarted(1),
    doneWithNote(1, 'cmyk.jpg', 'kept lossless: CMYK JPG'),
  );
  assert.equal('note' in rowView(mixed.rows[0]), false);
  assert.equal(rowView(mixed.rows[1]).note, 'kept lossless: CMYK JPG');
});

// a fresh list

test('a_list_changed_after_run_finished_replaces_the_rows', () => {
  const finishedRun = apply(
    initialState(),
    threeFiles(),
    started(3),
    fileStarted(0),
    done(0, 'holiday.jpg'),
    fileStarted(1),
    done(1, 'logo.png'),
    fileStarted(2),
    done(2, 'banner.webp'),
    finished(3, 10, true),
  );
  const state = reduce(finishedRun, listed([entry('new.png', 2048)]));
  assert.equal(state.phase, 'ready');
  assert.deepEqual(state.rows, [{ kind: 'listed', name: 'new.png', size: 2048, skip: null }]);
  assert.equal(state.summary, null);
  assert.equal(headingLine(state), '1 file ready');
  assert.equal(canRun(state), true);
});

test('an_add_before_run_finished_is_busy_and_keeps_the_rows', () => {
  const running = apply(initialState(), threeFiles(), started(3), fileStarted(0), done(0, 'holiday.jpg'));
  const rows = running.rows;
  const kept = JSON.stringify(rows);
  const refused = reduce(running, busy);
  assert.equal(refused.message, MESSAGES.busy);
  assert.equal(refused.phase, 'running');
  assert.equal(refused.rows, rows);
  assert.equal(JSON.stringify(refused.rows), kept);
  // A list arriving during a run does not replace the run's rows either.
  assert.equal(reduce(running, listed([entry('new.png')])), running);
  assert.equal(JSON.stringify(running.rows), kept);
});

// the run's rejections

test('the_run_rejections_get_their_own_message', () => {
  for (const [reason, message] of [
    ['invalid-settings', MESSAGES.invalidSettings],
    ['empty-list', MESSAGES.emptyList],
    ['output-missing', MESSAGES.outputMissing],
  ]) {
    assert.equal(typeof message, 'string', reason);
    assert.notEqual(message, MESSAGES.unknown, reason);
    assert.equal(classifyRejection(reason), 'known', reason);
    assert.equal(reduce(initialState(), { type: 'command-rejected', payload: reason }).message, message);
  }
  assert.equal(MESSAGES.outputMissing, 'The output folder is gone or cannot be written to. Choose it again, or save next to the originals.');
});

test('a_run_rejection_spelled_differently_stays_unknown', () => {
  for (const reason of ['Empty-list', 'output_missing', 'invalid settings', 'empty-list ']) {
    assert.equal(classifyRejection(reason), 'unknown', reason);
    assert.equal(reduce(initialState(), { type: 'command-rejected', payload: reason }).message, MESSAGES.unknown);
  }
  // Control: the exact spelling is known.
  assert.equal(classifyRejection('empty-list'), 'known');
});

// the drop area

test('the_drop_title_asks_for_more_while_the_list_has_rows', () => {
  const ready = reduce(initialState(), threeFiles());
  assert.equal(dropTitleText(ready), 'Drop more images or a folder here');
  assert.equal(dropTitleText(reduce(ready, started(3))), 'Drop more images or a folder here');
});

test('the_drop_title_has_no_more_when_empty_or_finished', () => {
  assert.equal(dropTitleText(initialState()), 'Drop images or a folder here');
  const finishedRun = apply(initialState(), threeFiles(), started(1), done(0), finished(1, 1, true));
  // The next add starts a fresh list.
  assert.equal(dropTitleText(finishedRun), 'Drop images or a folder here');
});

// purity of the new actions

test('the_new_actions_change_a_new_state_and_leave_the_given_one_as_it_was', () => {
  const ready = apply(initialState(), threeFiles(), setMode('lossy'));
  const snapshot = JSON.stringify(ready);
  for (const [action, key, value] of [
    [listed([entry('x.png')]), 'phase', 'ready'],
    [outputChanged('exports'), 'folder', 'exports'],
    [setMode('lossless'), 'mode', 'lossless'],
    [setQuality('20'), 'quality', 20],
    [started(3, 'lossy', 75), 'phase', 'running'],
  ]) {
    const next = reduce(ready, action);
    assert.equal(next[key], value, action.type);
    assert.notEqual(next, ready, action.type);
    assert.equal(JSON.stringify(ready), snapshot, action.type);
  }
});

// ---------------------------------------------------------------------
// Post-code coverage, ruled by the controller (Task 11 pre-review): the
// branches the first report listed as untested, each with its partner.
// ---------------------------------------------------------------------

// (a) set-mode to the mode already picked

test('set_mode_to_the_mode_already_picked_returns_the_same_state', () => {
  const ready = reduce(initialState(), threeFiles());
  assert.equal(reduce(ready, setMode('lossless')), ready);
  const lossy = reduce(ready, setMode('lossy'));
  assert.equal(reduce(lossy, setMode('lossy')), lossy);
});

test('set_mode_to_the_other_mode_returns_a_new_state', () => {
  const ready = reduce(initialState(), threeFiles());
  const lossy = reduce(ready, setMode('lossy'));
  assert.notEqual(lossy, ready);
  assert.equal(lossy.mode, 'lossy');
  assert.equal(ready.mode, 'lossless');
});

// (b) set-quality-text with a payload that is not a string

test('set_quality_text_with_a_non_string_payload_changes_nothing', () => {
  const lossy = apply(initialState(), threeFiles(), setMode('lossy'));
  for (const payload of [40, null, undefined, ['40'], { value: '40' }]) {
    assert.equal(reduce(lossy, setQuality(payload)), lossy, String(payload));
  }
});

test('set_quality_text_with_a_string_payload_sets_the_text_and_the_quality', () => {
  const lossy = apply(initialState(), threeFiles(), setMode('lossy'));
  const state = reduce(lossy, setQuality('40'));
  assert.equal(state.qualityText, '40');
  assert.equal(state.quality, 40);
});

// (c) run-started with a mode it does not know

test('a_run_with_an_unknown_mode_shows_no_settings_in_the_heading', () => {
  for (const mode of ['fast', 'Lossy', null, undefined]) {
    const running = apply(
      initialState(),
      threeFiles(),
      { type: 'run-started', payload: { total: 3, mode, quality: 75 } },
      fileStarted(0),
    );
    assert.equal(running.run, null, String(mode));
    assert.equal(headingLine(running), 'Compressing 1 of 3', String(mode));
    const finishedRun = apply(running, done(0), done(1), done(2), finished(3, 1, true));
    assert.equal(headingLine(finishedRun), 'Done', String(mode));
  }
});

test('a_run_with_a_known_mode_shows_its_settings_in_the_heading', () => {
  const lossy = apply(initialState(), threeFiles(), started(3, 'lossy', 40), fileStarted(0));
  assert.equal(headingLine(lossy), 'Compressing 1 of 3 · Lossy 40');
  const lossless = apply(initialState(), threeFiles(), started(3, 'lossless', null), fileStarted(0));
  assert.equal(headingLine(lossless), 'Compressing 1 of 3 · Lossless');
});

// (d) and "settingsText lossy without N": a lossy run with no usable quality

test('a_lossy_run_without_a_usable_quality_reads_lossy_without_a_number', () => {
  for (const quality of [null, undefined, 101, -1, 7.5, '75']) {
    const running = apply(initialState(), threeFiles(), started(3, 'lossy', quality), fileStarted(0));
    assert.deepEqual(running.run, { mode: 'lossy', quality: null }, String(quality));
    assert.equal(headingLine(running), 'Compressing 1 of 3 · Lossy', String(quality));
    const finishedRun = apply(running, done(0), done(1), done(2), finished(3, 1, true));
    assert.equal(headingLine(finishedRun), 'Done · Lossy', String(quality));
  }
});

test('a_lossy_run_with_a_quality_at_its_bounds_shows_the_number', () => {
  for (const quality of [0, 100]) {
    const running = apply(initialState(), threeFiles(), started(3, 'lossy', quality), fileStarted(0));
    assert.equal(headingLine(running), `Compressing 1 of 3 · Lossy ${quality}`);
  }
});

// (e) the heading while running before the first file-started, and with no
// row to compress

test('the_heading_before_the_first_file_started_counts_from_one', () => {
  const running = apply(initialState(), threeFiles(), started(3, 'lossless', null));
  assert.equal(headingLine(running), 'Compressing 1 of 3 · Lossless');
  // A skipped first row does not move the count either.
  const skippedFirst = apply(
    initialState(),
    listed([entry('notes.txt', null, SKIP), entry('a.png'), entry('b.png')]),
    started(3),
    skipped(0, 'notes.txt'),
  );
  assert.equal(headingLine(skippedFirst), 'Compressing 1 of 2 · Lossless');
});

test('the_heading_counts_the_started_files_once_they_start', () => {
  const third = apply(
    initialState(),
    threeFiles(),
    started(3),
    fileStarted(0),
    done(0),
    fileStarted(1),
    failed(1),
    fileStarted(2),
  );
  assert.equal(headingLine(third), 'Compressing 3 of 3 · Lossless');
});

test('a_run_with_no_row_to_compress_reads_compressing_without_a_count', () => {
  const running = apply(
    initialState(),
    listed([entry('notes.txt', null, SKIP), entry('readme.md', null, SKIP)]),
    started(2, 'lossy', 75),
  );
  assert.equal(headingLine(running), 'Compressing · Lossy 75');
  assert.doesNotMatch(headingLine(running), / of /);
});

test('a_run_with_a_row_to_compress_reads_compressing_with_a_count', () => {
  const running = apply(
    initialState(),
    listed([entry('notes.txt', null, SKIP), entry('a.png')]),
    started(2, 'lossy', 75),
  );
  assert.equal(headingLine(running), 'Compressing 1 of 1 · Lossy 75');
});

// (f) file-started with a payload that is not an object

test('file_started_with_a_non_object_payload_changes_nothing', () => {
  const running = apply(initialState(), threeFiles(), started(3));
  const rowsBefore = JSON.stringify(running.rows);
  for (const payload of [null, 0, '0', [0]]) {
    assert.equal(reduce(running, { type: 'file-started', payload }), running, JSON.stringify(payload));
  }
  assert.equal(JSON.stringify(running.rows), rowsBefore);
});

test('file_started_with_an_object_payload_marks_its_row', () => {
  const running = apply(initialState(), threeFiles(), started(3));
  const state = reduce(running, { type: 'file-started', payload: { index: 0 } });
  assert.equal(state.current, 0);
  assert.equal(state.rows[0].kind, 'compressing');
});

// (g) file-done for a row other than the one compressing

test('file_done_for_another_row_keeps_the_current_row', () => {
  const running = apply(initialState(), threeFiles(), started(3), fileStarted(1));
  const state = reduce(running, skipped(0, 'holiday.jpg'));
  assert.equal(state.current, 1);
  assert.equal(state.rows[0].kind, 'skipped');
  assert.equal(state.rows[1].kind, 'compressing');
});

test('file_done_for_the_current_row_clears_it', () => {
  const running = apply(initialState(), threeFiles(), started(3), fileStarted(1));
  const state = reduce(running, done(1, 'logo.png'));
  assert.equal(state.current, null);
  assert.equal(state.rows[1].kind, 'done');
});

// ---------------------------------------------------------------------
// Fix round 1 (Task 11 review): what app.js decided, now pure and tested.
// ---------------------------------------------------------------------

const readyList = () => reduce(initialState(), threeFiles());
const onlySkipped = () => reduce(initialState(), listed([entry('notes.txt', null, SKIP)]));
const finishedRun = () =>
  apply(readyList(), started(1), fileStarted(0), done(0, 'holiday.jpg'), finished(1, 10, true));

// 1(a) the argument of start_run

test('start_run_args_are_the_exact_settings_for_lossy_40_and_for_lossless', () => {
  assert.deepEqual(startRunArgs(apply(readyList(), setMode('lossy'), setQuality('40'))), {
    settings: { mode: 'lossy', quality: 40 },
  });
  // Lossless still sends the quality (INTERFACE.md); the backend ignores it.
  assert.deepEqual(startRunArgs(readyList()), { settings: { mode: 'lossless', quality: 75 } });
  assert.deepEqual(startRunArgs(apply(readyList(), setMode('lossy'), setQuality('40'), setMode('lossless'))), {
    settings: { mode: 'lossless', quality: 40 },
  });
});

test('start_run_args_are_null_for_bad_quality_only_skipped_rows_running_or_finished', () => {
  for (const text of ['7.5', '101', '', 'abc']) {
    assert.equal(startRunArgs(apply(readyList(), setMode('lossy'), setQuality(text))), null, text);
  }
  assert.equal(startRunArgs(onlySkipped()), null);
  assert.equal(startRunArgs(reduce(readyList(), started(3))), null);
  assert.equal(startRunArgs(finishedRun()), null);
  assert.equal(startRunArgs(initialState()), null);
});

// 1(b) the settings lock

test('settings_view_locks_the_radios_while_running', () => {
  assert.equal(settingsView(reduce(readyList(), started(3))).radiosDisabled, true);
});

test('settings_view_leaves_the_radios_free_while_ready', () => {
  assert.equal(settingsView(readyList()).radiosDisabled, false);
  assert.equal(settingsView(reduce(readyList(), setMode('lossy'))).radiosDisabled, false);
});

test('settings_view_locks_quality_while_running_or_lossless', () => {
  assert.equal(settingsView(readyList()).qualityDisabled, true);
  const lossyRunning = apply(readyList(), setMode('lossy'), started(3, 'lossy', 75));
  assert.equal(settingsView(lossyRunning).qualityDisabled, true);
});

test('settings_view_leaves_quality_editable_while_lossy_and_ready', () => {
  assert.equal(settingsView(reduce(readyList(), setMode('lossy'))).qualityDisabled, false);
});

test('settings_view_is_invalid_for_lossy_with_bad_text', () => {
  for (const text of ['7.5', '101', '']) {
    assert.equal(settingsView(apply(readyList(), setMode('lossy'), setQuality(text))).invalid, true, text);
  }
});

test('settings_view_is_not_invalid_for_lossless_with_bad_text_or_lossy_with_good_text', () => {
  // A lossless state holding bad text (switching restores it, so build it).
  assert.equal(settingsView({ ...readyList(), mode: 'lossless', qualityText: '7.5' }).invalid, false);
  assert.equal(settingsView(apply(readyList(), setMode('lossy'), setQuality('40'))).invalid, false);
});

test('settings_view_shows_choose_output_with_and_without_a_folder_while_not_running', () => {
  assert.equal(settingsView(readyList()).showChooseOutput, true);
  assert.equal(settingsView(reduce(readyList(), outputChanged('exports'))).showChooseOutput, true);
});

test('settings_view_hides_choose_output_while_running', () => {
  assert.equal(settingsView(reduce(readyList(), started(3))).showChooseOutput, false);
  assert.equal(settingsView(apply(readyList(), outputChanged('exports'), started(3))).showChooseOutput, false);
});

test('settings_view_shows_clear_output_with_a_folder_while_not_running', () => {
  assert.equal(settingsView(reduce(readyList(), outputChanged('exports'))).showClearOutput, true);
  assert.equal(settingsView(reduce(readyList(), outputChanged(''))).showClearOutput, true);
});

test('settings_view_hides_clear_output_without_a_folder_or_while_running', () => {
  assert.equal(settingsView(readyList()).showClearOutput, false);
  assert.equal(settingsView(apply(readyList(), outputChanged('exports'), started(3))).showClearOutput, false);
});

test('settings_view_shows_quality_while_lossy_and_ready', () => {
  assert.equal(settingsView(reduce(readyList(), setMode('lossy'))).showQuality, true);
});

test('settings_view_shows_quality_while_lossy_and_running', () => {
  const lossyRunning = apply(readyList(), setMode('lossy'), started(3, 'lossy', 75));
  assert.equal(lossyRunning.phase, 'running');
  assert.equal(settingsView(lossyRunning).showQuality, true);
});

test('settings_view_hides_quality_while_lossless_and_ready', () => {
  assert.equal(readyList().mode, 'lossless');
  assert.equal(settingsView(readyList()).showQuality, false);
});

test('settings_view_hides_quality_while_lossless_and_running', () => {
  const losslessRunning = reduce(readyList(), started(3));
  assert.equal(losslessRunning.phase, 'running');
  assert.equal(settingsView(losslessRunning).showQuality, false);
});

test('settings_view_shows_quality_after_switching_to_lossy', () => {
  assert.equal(settingsView(readyList()).showQuality, false);
  assert.equal(settingsView(apply(readyList(), setMode('lossy'))).showQuality, true);
});

test('settings_view_hides_quality_again_after_switching_back_to_lossless', () => {
  const back = apply(readyList(), setMode('lossy'), setQuality('40'), setMode('lossless'));
  assert.equal(settingsView(back).showQuality, false);
  // The last valid quality is still what comes back with Lossy.
  assert.equal(apply(back, setMode('lossy')).qualityText, '40');
});

// the panel: which blocks show

test('panel_view_shows_the_panel_once_there_is_a_list_or_a_run', () => {
  for (const state of [readyList(), reduce(readyList(), started(3)), finishedRun()]) {
    assert.equal(panelView(state).showPanel, true, state.phase);
  }
});

test('panel_view_hides_the_panel_while_the_list_is_empty', () => {
  assert.equal(panelView(initialState()).showPanel, false);
  assert.equal(panelView(reduce(readyList(), listed([]))).showPanel, false);
});

test('panel_view_uses_the_list_heads_before_a_run', () => {
  assert.equal(panelView(readyList()).listing, true);
  assert.equal(panelView(initialState()).listing, true);
});

test('panel_view_uses_the_run_heads_while_running_and_finished', () => {
  assert.equal(panelView(reduce(readyList(), started(3))).listing, false);
  assert.equal(panelView(finishedRun()).listing, false);
});

test('panel_view_shows_the_table_when_it_has_rows', () => {
  assert.equal(panelView(readyList()).showTable, true);
  assert.equal(panelView(finishedRun()).showTable, true);
});

test('panel_view_hides_a_table_without_rows', () => {
  assert.equal(panelView(initialState()).showTable, false);
  assert.equal(panelView(apply(readyList(), started(0), finished(0, 0, false))).showTable, false);
});

test('panel_view_shows_the_summary_and_hides_the_settings_and_run_when_finished', () => {
  const view = panelView(finishedRun());
  assert.equal(view.showSummary, true);
  assert.equal(view.showSettings, false);
});

test('panel_view_shows_the_settings_and_run_and_no_summary_before_finishing', () => {
  for (const state of [readyList(), reduce(readyList(), started(3))]) {
    const view = panelView(state);
    assert.equal(view.showSummary, false, state.phase);
    assert.equal(view.showSettings, true, state.phase);
  }
});

test('panel_view_marks_the_drop_area_busy_while_running', () => {
  assert.equal(panelView(reduce(readyList(), started(3))).dropBusy, true);
});

test('panel_view_leaves_the_drop_area_free_when_not_running', () => {
  for (const state of [initialState(), readyList(), finishedRun()]) {
    assert.equal(panelView(state).dropBusy, false, state.phase);
  }
});

// what the page does with a rejected command

test('after_a_known_or_unknown_rejection_the_page_says_so_and_logs_only_the_unknown', () => {
  assert.deepEqual(afterRejection('output-missing'), {
    log: false,
    action: { type: 'command-rejected', payload: 'output-missing' },
  });
  assert.deepEqual(afterRejection('/home/x failed'), {
    log: true,
    action: { type: 'command-rejected', payload: '/home/x failed' },
  });
});

test('after_a_busy_rejection_the_page_says_nothing_more_and_logs_nothing', () => {
  assert.deepEqual(afterRejection('busy'), { log: false, action: null });
});

// Minor 1: a skipped row stays skipped when a run breaks off

test('a_waiting_row_without_a_skip_becomes_the_broken_off_failure', () => {
  const state = apply(
    initialState(),
    listed([entry('a.png'), entry('b.png')]),
    started(2),
    fileStarted(0),
    done(0, 'a.png'),
    finished(1, 1, true),
  );
  assert.deepEqual(rowView(state.rows[1]), {
    kind: 'failed',
    name: 'b.png',
    text: 'failed: The run ended before this file was done',
  });
});

test('a_waiting_row_with_a_skip_stays_skipped_with_its_own_reason_after_the_break', () => {
  const state = apply(
    initialState(),
    listed([entry('a.png'), entry('notes.txt', null, SKIP)]),
    started(2),
    fileStarted(0),
    done(0, 'a.png'),
    finished(1, 1, true),
  );
  assert.deepEqual(rowView(state.rows[1]), { kind: 'skipped', name: 'notes.txt', text: `skipped: ${SKIP_LOWER}` });
});

// Minor 3: the reason after "Will be skipped: " and "skipped: " starts in lower case

test('a_skip_reason_reads_in_lower_case_after_its_prefix', () => {
  const ready = reduce(
    initialState(),
    listed([entry('notes.txt', null, SKIP), entry('gone.png', null, 'Not a readable file')]),
  );
  assert.equal(rowView(ready.rows[0]).text, 'Will be skipped: not a PNG, JPG, SVG or WebP file');
  assert.equal(rowView(ready.rows[1]).text, 'Will be skipped: not a readable file');
  const running = reduce(ready, started(2));
  assert.equal(rowView(running.rows[0]).text, 'skipped: not a PNG, JPG, SVG or WebP file');
});

test('a_skip_reason_is_not_capitalised_and_a_lower_case_or_acronym_start_is_kept', () => {
  const ready = reduce(
    initialState(),
    listed([
      entry('notes.txt', null, SKIP),
      entry('a.png', null, 'already lower case'),
      entry('b.png', null, 'PNG decoder refused it'),
      entry('(over the limit)', null, '3 more files not optimised: over the 2000-file limit'),
    ]),
  );
  const texts = ready.rows.map((row) => rowView(row).text);
  assert.doesNotMatch(texts[0], /: Not /);
  assert.equal(texts[1], 'Will be skipped: already lower case');
  assert.equal(texts[2], 'Will be skipped: PNG decoder refused it');
  assert.equal(texts[3], 'Will be skipped: 3 more files not optimised: over the 2000-file limit');
  const running = reduce(ready, started(4));
  assert.doesNotMatch(rowView(running.rows[0]).text, /: Not /);
  assert.equal(rowView(running.rows[2]).text, 'skipped: PNG decoder refused it');
});

// Final review: the finished skipped row reads like the waiting one, and only
// a capitalised ordinary word is lowered.

test('a_finished_skipped_row_reads_like_the_waiting_row', () => {
  const running = apply(initialState(), listed([entry('notes.txt', null, SKIP)]), started(1));
  const waiting = rowView(running.rows[0]).text;
  const finishedRow = reduce(running, skipped(0, 'notes.txt', SKIP));
  const done = rowView(finishedRow.rows[0]);
  assert.equal(done.kind, 'skipped');
  assert.equal(done.text, 'skipped: not a PNG, JPG, SVG or WebP file');
  assert.equal(done.text, waiting);
});

test('a_skip_reason_keeps_an_acronym_a_single_letter_and_a_digit_start', () => {
  const kept = [
    'I/O error while reading the file',
    'PNG decoder refused it',
    'A file that vanished',
    '3 more files not optimised: over the 2000-file limit',
  ];
  const ready = reduce(
    initialState(),
    listed(kept.map((reason, index) => entry(`f${index}.png`, null, reason))),
  );
  ready.rows.forEach((row, index) => {
    assert.equal(rowView(row).text, `Will be skipped: ${kept[index]}`);
  });
  const running = reduce(ready, started(kept.length));
  running.rows.forEach((row, index) => {
    assert.equal(rowView(row).text, `skipped: ${kept[index]}`);
  });
  const finishedRows = kept.reduce(
    (state, reason, index) => reduce(state, skipped(index, `f${index}.png`, reason)),
    running,
  );
  finishedRows.rows.forEach((row, index) => {
    assert.equal(rowView(row).text, `skipped: ${kept[index]}`);
  });
});

// ---------------------------------------------------------------------
// Re-add: the button sits in the summary and `readd_list` brings the list
// back as a `list-changed` (Task 2). These pin the state the button and the
// reducer depend on.
// ---------------------------------------------------------------------

test('the_summary_with_readd_shows_only_when_finished', () => {
  // "Re-add" is inside the summary, so it shows exactly when the summary does.
  assert.equal(panelView(finishedRun()).showSummary, true);
  assert.equal(canChoose(finishedRun()), true);
  const others = [initialState(), readyList(), reduce(readyList(), started(3))];
  for (const state of others) {
    assert.equal(panelView(state).showSummary, false, state.phase);
  }
});

test('settings_survive_a_readd', () => {
  const lossyRun = apply(
    initialState(),
    threeFiles(),
    setMode('lossy'),
    setQuality('60'),
    started(3, 'lossy', 60),
    done(0, 'holiday.jpg'),
    done(1, 'logo.png'),
    done(2, 'banner.webp'),
    finished(3, 10, true),
  );
  assert.equal(lossyRun.phase, 'finished');
  const readded = reduce(lossyRun, threeFiles());
  assert.equal(readded.phase, 'ready');
  assert.equal(readded.mode, 'lossy');
  assert.equal(readded.quality, 60);
  assert.equal(readded.qualityText, '60');
  assert.deepEqual(startRunArgs(readded), { settings: { mode: 'lossy', quality: 60 } });
  assert.deepEqual(
    readded.rows.map((row) => row.name),
    ['holiday.jpg', 'logo.png', 'banner.webp'],
  );
  assert.equal(panelView(readded).showSummary, false);
  assert.equal(panelView(readded).showSettings, true);
});

test('a_list_changed_during_a_run_changes_nothing', () => {
  const running = reduce(readyList(), started(3, 'lossy', 60));
  assert.equal(running.phase, 'running');
  assert.equal(reduce(running, threeFiles()), running);
});

test('an_all_skipped_list_after_a_readd_is_ready_but_cannot_run', () => {
  const finishedState = finishedRun();
  const skippedOnly = reduce(finishedState, listed([entry('a.txt', null, SKIP), entry('b.txt', null, SKIP)]));
  assert.equal(skippedOnly.phase, 'ready');
  assert.equal(skippedOnly.rows.length, 2);
  assert.equal(canRun(skippedOnly), false);
  assert.equal(startRunArgs(skippedOnly), null);
  // One compressible row among them can run.
  const mixed = reduce(finishedState, listed([entry('a.txt', null, SKIP), entry('b.png')]));
  assert.equal(canRun(mixed), true);
});
