import test from 'node:test';
import assert from 'node:assert/strict';

import { MESSAGES, initialState, reduce, rowView } from '../state.js';

function apply(state, ...actions) {
  return actions.reduce((current, action) => reduce(current, action), state);
}

const started = (total) => ({ type: 'run-started', payload: { total } });
const done = (index, name = 'a.png', before = 1000, after = 800, millis = 12) => ({
  type: 'file-done',
  payload: { index, name, status: 'done', before, after, millis },
});
const skipped = (index, name = 'notes.txt', reason = 'Not a PNG, JPG or SVG file') => ({
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
  assert.equal(state.running, false);
  assert.deepEqual(state.rows, []);
  assert.equal(state.summary, null);
  assert.equal(state.hasOutput, false);
  assert.equal(state.dragOver, false);
  assert.equal(state.message, '');
});

// run-started

test('run-started begins a run with the announced total', () => {
  const state = reduce(initialState(), started(3));
  assert.equal(state.running, true);
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
    { kind: 'skipped', name: 'notes.txt', reason: 'Not a PNG, JPG or SVG file' },
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
  assert.equal(state.running, false);
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

test('run-finished for an empty run announces zero files', () => {
  const state = apply(initialState(), started(0), finished(0, 0, false));
  assert.equal(state.message, 'Optimised 0 files, saved 0 B');
  assert.deepEqual(state.rows, []);
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
  assert.equal(state.running, true);
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

test('reduce never changes the state it was given', () => {
  const before = apply(initialState(), started(1));
  const snapshot = JSON.stringify(before);
  reduce(before, done(0));
  reduce(before, finished(1, 5, true));
  assert.equal(JSON.stringify(before), snapshot);
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
  const view = rowView({ kind: 'skipped', name: 'notes.txt', reason: 'Not a PNG, JPG or SVG file' });
  assert.deepEqual(view, {
    kind: 'skipped',
    name: 'notes.txt',
    text: 'skipped: Not a PNG, JPG or SVG file',
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
