import test from 'node:test';
import assert from 'node:assert/strict';

import { formatPercent, formatSize } from '../../ui/format.js';
import { nameProblem } from '../../ui/names.js';
import {
  canSave,
  canSwitchView,
  imageAlt,
  initialSingle,
  nameProblemText,
  nameText,
  offerView,
  pixelSizeText,
  previewArgs,
  reduceSingle,
  saveArgs,
  sizeView,
  viewResets,
} from '../../ui/single.js';
import { MESSAGES, afterRejection, canOpenFolder, initialState, reduce } from '../../ui/state.js';

function apply(state, ...actions) {
  return actions.reduce((current, action) => reduce(current, action), state);
}

const holiday = (key = '7', overrides = {}) => ({
  name: 'holiday.jpg',
  size: 2400000,
  width: 4032,
  height: 3024,
  key,
  suggested: 'optimized-holiday',
  extension: '.jpg',
  ...overrides,
});
const logo = (key = '9') => ({
  name: 'logo.svg',
  size: 4100,
  width: null,
  height: null,
  key,
  suggested: 'optimized-logo',
  extension: '.svg',
});
const imageChanged = (image, leftOut = 0, notice = null) => ({
  type: 'image-changed',
  payload: { image, left_out: leftOut, notice },
});
const loaded = (key = '7', overrides = {}) => imageChanged(holiday(key, overrides));
const setView = (view) => ({ type: 'set-view', payload: view });
const setMode = (mode) => ({ type: 'set-mode', payload: mode });
const setQuality = (text) => ({ type: 'set-quality-text', payload: text });
const typed = (text) => ({ type: 'set-name-text', payload: text });
const requested = (mode = 'lossless', quality = 75) => ({
  type: 'preview-requested',
  payload: { settings: { mode, quality } },
});
const started = (image, version) => ({ type: 'preview-started', payload: { image, version } });
const ready = (image, version, overrides = {}) => ({
  type: 'preview-ready',
  payload: {
    image,
    version,
    key: `${image}-${version}`,
    mode: 'lossy',
    quality: 75,
    before: 2400000,
    after: 610000,
    note: null,
    ...overrides,
  },
});
const failedPreview = (image, version, reason = 'the optimiser crashed on this file (it gave no valid result)') => ({
  type: 'preview-failed',
  payload: { image, version, reason },
});
const suggested = (image, name) => ({ type: 'name-suggested', payload: { image, suggested: name } });
const saved = (name = 'optimized-holiday.jpg', folder = null, next = 'optimized-holiday-1') => ({
  type: 'image-saved',
  payload: { name, folder, suggested: next },
});
const saveStarted = (name, preview) => ({ type: 'save-started', payload: { name, preview } });
const saveEnded = { type: 'save-ended', payload: null };
const rejected = (reason) => ({ type: 'command-rejected', payload: reason });
const runStarted = { type: 'run-started', payload: { total: 1, mode: 'lossless', quality: null } };
const runFinished = { type: 'run-finished', payload: { optimised: 1, saved_bytes: 10, has_output: true } };
const listed = { type: 'list-changed', payload: { files: [{ name: 'a.png', size: 10, skip: null, thumb: '1-0' }], notice: null } };

/** An image loaded in lossy 75, its version 1 asked for, started and ready. */
function withVersion(key = '7') {
  return apply(
    initialState(),
    setMode('lossy'),
    loaded(key),
    requested('lossy', 75),
    started(key, 1),
    ready(key, 1),
  );
}

// the switch

test('the_view_is_single_at_start', () => {
  const state = initialState();
  assert.equal(state.view, 'single');
  assert.notEqual(state.view, 'batch');
  assert.equal(canSwitchView(state), true);
  // The field does move: a set-view to batch changes it.
  assert.equal(reduce(state, setView('batch')).view, 'batch');
});

test('switching_keeps_each_views_content', () => {
  const single = apply(withVersion(), typed('my-holiday'), listed);
  const away = reduce(single, setView('batch'));
  assert.equal(away.view, 'batch');
  const back = reduce(away, setView('single'));
  assert.equal(back.view, 'single');
  assert.deepEqual(back.single, single.single);
  assert.deepEqual(back.rows, single.rows);
  assert.equal(back.mode, 'lossy');
  assert.equal(nameText(back), 'my-holiday');
});

test('set_view_with_a_bad_payload_changes_nothing', () => {
  const state = initialState();
  for (const payload of ['Single', null, { view: 'batch' }, 'list', undefined]) {
    assert.equal(reduce(state, setView(payload)), state, String(payload));
  }
  // Control: the exact spelling does switch.
  assert.equal(reduce(state, setView('batch')).view, 'batch');
});

test('the_switch_is_locked_during_a_run_and_a_save_and_free_after', () => {
  const batch = reduce(initialState(), setView('batch'));
  const running = reduce(batch, runStarted);
  assert.equal(canSwitchView(running), false);
  assert.equal(canSwitchView(reduce(running, runFinished)), true);

  const saving = reduce(withVersion(), saveStarted('optimized-holiday', '7-1'));
  assert.equal(canSwitchView(saving), false);
  assert.equal(canSwitchView(reduce(saving, saveEnded)), true);
});

test('a_locked_switch_does_nothing', () => {
  const running = apply(initialState(), setView('batch'), runStarted);
  assert.equal(reduce(running, setView('single')), running);
  const saving = reduce(withVersion(), saveStarted('optimized-holiday', '7-1'));
  assert.equal(reduce(saving, setView('batch')), saving);
  // Control: the same switch with nothing going does move the view.
  assert.equal(reduce(withVersion(), setView('batch')).view, 'batch');
});

test('a_busy_rejection_leaves_the_view', () => {
  // The window dispatches set-view only once set_view resolved; a "busy"
  // answer is all that arrives instead, and it does not move the view.
  assert.equal(afterRejection('busy').action, null);
  const state = reduce(initialState(), rejected('busy'));
  assert.equal(state.view, 'single');
});

test('leaving_for_batch_while_compressing_asks_again_on_return', () => {
  // A version is being made when the switch goes to Batch, which stops it.
  const compressing = apply(withVersion(), setQuality('60'), requested('lossy', 60), started('7', 2));
  assert.equal(compressing.single.compressing, true);
  const away = reduce(compressing, setView('batch'));
  assert.equal(away.single.compressing, false);
  assert.deepEqual(previewArgs(reduce(away, setView('single'))), { settings: { mode: 'lossy', quality: 60 } });

  // The request was accepted but its preview-started has not arrived yet:
  // the version shown is for older settings, so it is asked again too.
  const accepted = apply(withVersion(), setQuality('60'), requested('lossy', 60));
  assert.equal(accepted.single.compressing, false);
  const back = apply(accepted, setView('batch'), setView('single'));
  assert.deepEqual(previewArgs(back), { settings: { mode: 'lossy', quality: 60 } });
});

test('leaving_for_batch_after_the_version_is_ready_does_not_ask_again', () => {
  const back = apply(withVersion(), setView('batch'), setView('single'));
  assert.equal(previewArgs(back), null);
  assert.equal(back.single.ready.key, '7-1');

  // Lossless: the version's quality is null, the request's is 75; still the same.
  const lossless = apply(
    initialState(),
    loaded('7'),
    requested('lossless', 75),
    started('7', 1),
    ready('7', 1, { mode: 'lossless', quality: null }),
    setView('batch'),
    setView('single'),
  );
  assert.equal(previewArgs(lossless), null);
});

test('leaving_for_batch_after_a_failure_asks_again_on_return', () => {
  const failed = apply(
    initialState(),
    loaded('7'),
    requested('lossless', 75),
    started('7', 1),
    failedPreview('7', 1),
  );
  assert.equal(previewArgs(failed), null);
  const back = apply(failed, setView('batch'), setView('single'));
  assert.deepEqual(previewArgs(back), { settings: { mode: 'lossless', quality: 75 } });
});

test('a_preview_started_while_batch_shows_is_dropped', () => {
  const away = apply(initialState(), loaded('7'), requested(), setView('batch'));
  assert.equal(reduce(away, started('7', 1)), away);
  // Control: while Single image shows, the same event marks compressing.
  assert.equal(reduce(reduce(away, setView('single')), started('7', 1)).single.compressing, true);
});

test('an_outcome_sent_before_the_switch_to_batch_is_still_taken', () => {
  // Unlike preview-started, an outcome is not dropped because Batch shows:
  // one sent just before the switch stopped the request is the version the
  // backend keeps, so it passes on the image + version guard alone.
  const away = apply(
    withVersion('7'),
    setQuality('60'),
    requested('lossy', 60),
    started('7', 2),
    setView('batch'),
  );
  const readyLate = reduce(away, ready('7', 2, { quality: 60 }));
  assert.equal(readyLate.single.ready.key, '7-2');
  const failedLate = reduce(away, failedPreview('7', 2));
  assert.equal(failedLate.single.failed, 'the optimiser crashed on this file (it gave no valid result)');
  // The guard still applies while Batch shows: other versions are dropped.
  assert.equal(reduce(away, ready('7', 3)), away);
  assert.equal(reduce(away, failedPreview('7', 1)), away);
  assert.equal(reduce(away, ready('6', 2)), away);
});

// image events

test('a_new_key_sets_the_image_the_suggested_name_and_resets_typing_and_the_version', () => {
  const first = reduce(initialState(), loaded('7'));
  assert.deepEqual(first.single.image, holiday('7'));
  assert.equal(nameText(first), 'optimized-holiday');
  assert.equal(first.single.nameTyped, false);

  const before = apply(withVersion('7'), typed('mine'), { type: 'layout-changed', payload: 'beside' });
  assert.equal(before.single.nameTyped, true);
  const next = reduce(before, imageChanged(logo('8')));
  assert.equal(next.single.image.key, '8');
  assert.equal(nameText(next), 'optimized-logo');
  assert.equal(next.single.nameTyped, false);
  assert.equal(next.single.ready, null);
  assert.equal(next.single.requested, null);
  assert.equal(next.single.started, null);
  assert.equal(next.single.compressing, false);
  // The layout is the user's choice, not the image's.
  assert.equal(next.single.layout, 'beside');
});

test('the_same_key_with_a_notice_keeps_everything_and_only_sets_the_message', () => {
  const before = apply(withVersion('7'), typed('mine'));
  const notice = 'Not a PNG, JPG, SVG or WebP file.';
  const next = reduce(before, imageChanged(holiday('7'), 0, notice));
  assert.deepEqual(next.single, before.single);
  assert.equal(next.message, notice);
  assert.equal(next.messageSeq, before.messageSeq + 1);
});

test('an_image_changed_with_no_notice_leaves_the_message', () => {
  const before = apply(initialState(), rejected('no-image'));
  const next = reduce(before, loaded('7'));
  assert.equal(next.single.image.key, '7');
  assert.equal(next.message, before.message);
  assert.equal(next.messageSeq, before.messageSeq);
});

test('a_folder_with_no_image_loaded_sets_the_notice_and_no_image', () => {
  const next = reduce(initialState(), imageChanged(null, 1, 'Folders go in Batch.'));
  assert.equal(next.single.image, null);
  assert.equal(next.message, 'Folders go in Batch.');
  assert.deepEqual(offerView(next), { notice: 'Folders go in Batch.', canMove: true });
});

test('image_changed_with_a_bad_payload_changes_nothing', () => {
  const state = reduce(initialState(), loaded('7'));
  const bad = [
    { type: 'image-changed', payload: null },
    { type: 'image-changed', payload: 'x' },
    imageChanged(holiday('7a')),
    imageChanged(holiday('../7')),
    imageChanged(holiday(7)),
    imageChanged(holiday('12345678901')),
    imageChanged(holiday('')),
    imageChanged([holiday('8')]),
    imageChanged('8'),
  ];
  for (const action of bad) {
    assert.equal(reduce(state, action), state, JSON.stringify(action.payload));
  }
  // Control: a well-formed new key is taken.
  assert.equal(reduce(state, loaded('8')).single.image.key, '8');
});

test('image_changed_reads_bad_fields_as_none', () => {
  const next = reduce(
    initialState(),
    loaded('8', { name: 7, size: '2400000', width: -1, height: 3.5, suggested: null, extension: ['.jpg'] }),
  );
  assert.deepEqual(next.single.image, {
    name: '',
    size: null,
    width: null,
    height: null,
    key: '8',
    suggested: '',
    extension: '',
  });
  assert.equal(sizeView(next).original, '');
});

test('left_out_above_0_offers_switch_to_batch', () => {
  const notice = 'Single image mode takes one image; 3 others were left out.';
  const next = reduce(initialState(), imageChanged(holiday('8'), 3, notice));
  assert.deepEqual(offerView(next), { notice, canMove: true });
  assert.equal(next.message, notice);
  // Without a notice the offer still stands, with no text of its own.
  assert.deepEqual(offerView(reduce(initialState(), imageChanged(holiday('8'), 2, null))), { notice: '', canMove: true });
});

test('left_out_0_or_not_a_count_offers_nothing', () => {
  for (const leftOut of [0, '3', -1, null, 1.5]) {
    const next = reduce(initialState(), imageChanged(holiday('8'), leftOut, 'Folders go in Batch.'));
    assert.deepEqual(offerView(next), { notice: '', canMove: false }, String(leftOut));
  }
});

test('a_later_image_changed_removes_the_offer', () => {
  const offered = reduce(initialState(), imageChanged(holiday('8'), 2, 'Single image mode takes one image; 2 others were left out.'));
  assert.equal(offerView(offered).canMove, true);
  assert.equal(offerView(reduce(offered, imageChanged(holiday('8')))).canMove, false);
  assert.equal(offerView(reduce(offered, imageChanged(holiday('9')))).canMove, false);
  assert.equal(offerView(reduce(offered, imageChanged(holiday('8'), 0, 'Folders go in Batch.'))).canMove, false);
});

test('a_set_view_removes_the_offer', () => {
  const offered = reduce(initialState(), imageChanged(holiday('8'), 2, 'Single image mode takes one image; 2 others were left out.'));
  assert.deepEqual(offerView(reduce(offered, setView('batch'))), { notice: '', canMove: false });
  // Also a set-view to the view that already shows: the backend ends the offer at every set_view.
  assert.deepEqual(offerView(reduce(offered, setView('single'))), { notice: '', canMove: false });
});

test('the_offer_cannot_be_used_while_a_save_is_going', () => {
  const notice = 'Single image mode takes one image; 1 other was left out.';
  const offered = apply(
    initialState(),
    setMode('lossy'),
    imageChanged(holiday('8'), 1, notice),
    requested('lossy', 75),
    started('8', 1),
    ready('8', 1),
  );
  assert.deepEqual(offerView(offered), { notice, canMove: true });
  const saving = reduce(offered, saveStarted('optimized-holiday', '8-1'));
  assert.deepEqual(offerView(saving), { notice, canMove: false });
});

test('the_offer_cannot_be_used_while_a_run_is_going', () => {
  // Run lives in Batch, so a run does not start while Single image shows;
  // if a run-started arrived there anyway, the switch lock still holds.
  const notice = 'Single image mode takes one image; 1 other was left out.';
  const offered = reduce(initialState(), imageChanged(holiday('8'), 1, notice));
  assert.deepEqual(offerView(offered), { notice, canMove: true });
  const running = reduce(offered, runStarted);
  assert.deepEqual(offerView(running), { notice, canMove: false });
  assert.deepEqual(offerView(reduce(running, runFinished)), { notice, canMove: true });
});

// preview requests

test('one_request_after_a_new_image', () => {
  assert.deepEqual(previewArgs(reduce(initialState(), loaded('7'))), {
    settings: { mode: 'lossless', quality: 75 },
  });
});

test('no_request_without_an_image', () => {
  assert.equal(previewArgs(initialState()), null);
  assert.equal(previewArgs(reduce(initialState(), imageChanged(null, 1, 'Folders go in Batch.'))), null);
});

test('one_request_after_a_mode_change', () => {
  const asked = apply(initialState(), loaded('7'), requested('lossless', 75));
  assert.equal(previewArgs(asked), null);
  const lossy = reduce(asked, setMode('lossy'));
  assert.deepEqual(previewArgs(lossy), { settings: { mode: 'lossy', quality: 75 } });
  const backToLossless = apply(lossy, requested('lossy', 75), setMode('lossless'));
  assert.deepEqual(previewArgs(backToLossless), { settings: { mode: 'lossless', quality: 75 } });
});

test('none_for_the_same_settings_twice', () => {
  const asked = apply(initialState(), loaded('7'), requested('lossless', 75));
  assert.deepEqual(asked.single.requested, { mode: 'lossless', quality: 75 });
  assert.equal(previewArgs(asked), null);
  // Lossy 60 typed, sent, then typed again: nothing new to send.
  const sixty = apply(asked, setMode('lossy'), setQuality('60'), requested('lossy', 60), setQuality('60'));
  assert.equal(previewArgs(sixty), null);
});

test('the_same_settings_are_asked_again_for_a_new_image', () => {
  const asked = apply(initialState(), loaded('7'), requested('lossless', 75));
  assert.deepEqual(previewArgs(reduce(asked, loaded('8'))), { settings: { mode: 'lossless', quality: 75 } });
});

test('none_for_an_invalid_quality_text', () => {
  const lossy = apply(initialState(), loaded('7'), setMode('lossy'));
  for (const text of ['101', 'abc', '']) {
    assert.equal(previewArgs(reduce(lossy, setQuality(text))), null, text);
  }
});

test('quality_0_and_100_are_sent_as_such', () => {
  const lossy = apply(initialState(), loaded('7'), setMode('lossy'));
  assert.deepEqual(previewArgs(reduce(lossy, setQuality('0'))), { settings: { mode: 'lossy', quality: 0 } });
  assert.deepEqual(previewArgs(reduce(lossy, setQuality('100'))), { settings: { mode: 'lossy', quality: 100 } });
});

test('no_request_while_batch_shows', () => {
  const away = apply(initialState(), loaded('7'), setView('batch'));
  assert.equal(previewArgs(away), null);
  assert.equal(previewArgs(reduce(away, setMode('lossy'))), null);
  // The same state in Single image does ask.
  assert.deepEqual(previewArgs(apply(away, setMode('lossy'), setView('single'))), {
    settings: { mode: 'lossy', quality: 75 },
  });
});

test('a_bad_preview_requested_records_nothing', () => {
  const state = reduce(initialState(), loaded('7'));
  const bad = [
    null,
    {},
    { settings: null },
    { settings: { mode: 'Lossy', quality: 75 } },
    { settings: { mode: 'lossy', quality: '75' } },
    { settings: { mode: 'lossy', quality: 75.5 } },
    { settings: { mode: 'lossy', quality: 101 } },
    { settings: { mode: 'lossy', quality: -1 } },
  ];
  for (const payload of bad) {
    assert.equal(reduce(state, { type: 'preview-requested', payload }), state, JSON.stringify(payload));
  }
  const none = initialState();
  assert.equal(reduce(none, requested()), none);
  // Control: well-formed settings with an image are recorded.
  assert.notEqual(reduce(state, requested()).single.requested, null);
});

// preview-started and stale outcomes

test('preview_started_for_the_current_image_marks_compressing', () => {
  const next = apply(initialState(), loaded('7'), requested(), started('7', 4));
  assert.deepEqual(next.single.started, { image: '7', version: 4 });
  assert.equal(next.single.compressing, true);
  assert.equal(sizeView(next).busy, true);
});

test('a_stale_preview_started_changes_nothing', () => {
  const current = apply(initialState(), loaded('7'), requested(), started('7', 4));
  assert.equal(reduce(current, started('6', 5)), current, 'another image');
  assert.equal(reduce(current, started('7', 3)), current, 'a lower version');
  assert.equal(reduce(current, started('7', 4)), current, 'the same version again');
  assert.equal(reduce(current, started('7', 0)), current, 'version 0');
  assert.equal(reduce(current, started('7', '5')), current, 'a version as text');
  assert.equal(reduce(current, { type: 'preview-started', payload: null }), current, 'no payload');
  const none = initialState();
  assert.equal(reduce(none, started('7', 1)), none, 'before any image');
  // Control: a newer version of the current image is taken.
  assert.deepEqual(reduce(current, started('7', 5)).single.started, { image: '7', version: 5 });
});

test('a_preview_ready_for_another_image_changes_nothing', () => {
  const state = apply(initialState(), loaded('7'), requested(), started('7', 4));
  assert.equal(reduce(state, ready('6', 4)), state);
  assert.equal(state.single.ready, null);
});

test('a_preview_ready_for_a_lower_version_changes_nothing', () => {
  const state = apply(initialState(), loaded('7'), requested(), started('7', 3), started('7', 4));
  assert.equal(reduce(state, ready('7', 3)), state);
  assert.equal(state.single.ready, null);
});

test('a_preview_ready_for_a_version_with_no_preview_started_changes_nothing', () => {
  const noneStarted = apply(initialState(), loaded('7'), requested());
  assert.equal(reduce(noneStarted, ready('7', 4)), noneStarted);
  const fourStarted = reduce(noneStarted, started('7', 4));
  assert.equal(reduce(fourStarted, ready('7', 5)), fourStarted);
  // Control: the started version is taken.
  assert.equal(reduce(fourStarted, ready('7', 4)).single.ready.key, '7-4');
});

test('a_preview_ready_before_any_image_changes_nothing', () => {
  const none = initialState();
  assert.equal(reduce(none, ready('7', 1)), none);
  assert.equal(none.single.ready, null);
});

test('the_latest_preview_ready_sets_the_sizes_and_the_key', () => {
  const state = apply(initialState(), setMode('lossy'), loaded('7'), requested('lossy', 75), started('7', 4), ready('7', 4));
  assert.deepEqual(state.single.ready, {
    key: '7-4',
    mode: 'lossy',
    quality: 75,
    before: 2400000,
    after: 610000,
    note: null,
  });
  assert.equal(state.single.compressing, false);
  assert.equal(sizeView(state).compressed, formatSize(610000));
});

test('a_preview_ready_with_a_bad_key_or_sizes_changes_nothing', () => {
  const state = apply(initialState(), loaded('7'), requested(), started('7', 4));
  const bad = [
    ready('7', 4, { key: '7-x' }),
    ready('7', 4, { key: '7' }),
    ready('7', 4, { key: '../7-4' }),
    ready('7', 4, { key: 74 }),
    ready('7', 4, { before: -1 }),
    ready('7', 4, { after: '610000' }),
    ready('7', 4, { before: undefined }),
    { type: 'preview-ready', payload: null },
  ];
  for (const action of bad) {
    assert.equal(reduce(state, action), state, JSON.stringify(action.payload));
  }
  // Control: the well-formed event is taken.
  assert.equal(reduce(state, ready('7', 4)).single.ready.key, '7-4');
});

test('a_preview_ready_reads_a_bad_mode_quality_or_note_as_none', () => {
  const base = apply(initialState(), loaded('7'), requested(), started('7', 4));
  const mode = reduce(base, ready('7', 4, { mode: 'LOSSY' }));
  assert.equal(mode.single.ready.mode, null);
  assert.equal(imageAlt(mode), 'Compressed');
  assert.equal(reduce(base, ready('7', 4, { quality: 101 })).single.ready.quality, null);
  assert.equal(reduce(base, ready('7', 4, { quality: 75.5 })).single.ready.quality, null);
  assert.equal(reduce(base, ready('7', 4, { note: '' })).single.ready.note, null);
  assert.equal(reduce(base, ready('7', 4, { note: 3 })).single.ready.note, null);
});

test('a_stale_preview_failed_changes_nothing', () => {
  const state = apply(initialState(), loaded('7'), requested(), started('7', 3), started('7', 4));
  assert.equal(reduce(state, failedPreview('6', 4)), state, 'another image');
  assert.equal(reduce(state, failedPreview('7', 3)), state, 'a lower version');
  assert.equal(reduce(state, failedPreview('7', 5)), state, 'a version never started');
  assert.equal(reduce(state, { type: 'preview-failed', payload: null }), state, 'no payload');
  const none = initialState();
  assert.equal(reduce(none, failedPreview('7', 1)), none, 'before any image');
  // Control: the latest version's failure is taken.
  assert.notEqual(reduce(state, failedPreview('7', 4)).single.failed, null);
});

test('the_latest_preview_failed_shows_its_reason_and_keeps_the_version', () => {
  const state = apply(withVersion('7'), setQuality('60'), requested('lossy', 60), started('7', 2), failedPreview('7', 2));
  assert.equal(state.single.failed, 'the optimiser crashed on this file (it gave no valid result)');
  assert.equal(state.single.compressing, false);
  assert.equal(state.single.ready.key, '7-1');
  const sizes = sizeView(state);
  assert.equal(sizes.note, 'the optimiser crashed on this file (it gave no valid result)');
  assert.equal(sizes.compressed, formatSize(610000));
  assert.equal(sizes.busy, false);
});

test('a_preview_failed_with_no_reason_says_so_in_words', () => {
  const state = apply(initialState(), loaded('7'), requested(), started('7', 1));
  const missing = reduce(state, { type: 'preview-failed', payload: { image: '7', version: 1 } });
  assert.equal(missing.single.failed, 'No reason was given');
  const empty = reduce(state, failedPreview('7', 1, ''));
  assert.equal(empty.single.failed, 'No reason was given');
});

// the save gate

test('save_is_allowed_with_a_ready_key_and_a_valid_name', () => {
  const state = withVersion('7');
  assert.equal(canSave(state), true);
  assert.deepEqual(saveArgs(state), { name: 'optimized-holiday', preview: '7-1' });
  const typedName = reduce(state, typed('vakantie-café'));
  assert.deepEqual(saveArgs(typedName), { name: 'vakantie-café', preview: '7-1' });
});

test('save_is_refused_while_compressing', () => {
  const state = apply(withVersion('7'), setQuality('60'), requested('lossy', 60), started('7', 2));
  assert.equal(canSave(state), false);
  assert.equal(saveArgs(state), null);
});

test('save_is_refused_after_preview_failed_until_a_later_version_works', () => {
  const failed = apply(withVersion('7'), setQuality('60'), requested('lossy', 60), started('7', 2), failedPreview('7', 2));
  assert.equal(canSave(failed), false);
  assert.equal(saveArgs(failed), null);
  const later = apply(failed, setQuality('50'), requested('lossy', 50), started('7', 3), ready('7', 3, { quality: 50 }));
  assert.equal(canSave(later), true);
  assert.deepEqual(saveArgs(later), { name: 'optimized-holiday', preview: '7-3' });
});

test('save_is_refused_with_an_invalid_name', () => {
  for (const name of ['', '   ', 'a/b', 'holiday.', 'CON']) {
    const state = reduce(withVersion('7'), typed(name));
    assert.equal(canSave(state), false, name);
    assert.equal(saveArgs(state), null, name);
  }
});

test('save_is_refused_with_no_image_or_no_version', () => {
  assert.equal(canSave(initialState()), false);
  assert.equal(saveArgs(initialState()), null);
  const noVersion = apply(initialState(), loaded('7'), requested());
  assert.equal(canSave(noVersion), false);
  assert.equal(saveArgs(noVersion), null);
});

test('save_is_refused_while_a_save_is_going', () => {
  const saving = reduce(withVersion('7'), saveStarted('optimized-holiday', '7-1'));
  assert.equal(canSave(saving), false);
  assert.equal(saveArgs(saving), null);
  assert.equal(canSave(reduce(saving, saveEnded)), true);
});

test('a_bad_save_started_records_nothing', () => {
  const state = withVersion('7');
  assert.equal(reduce(state, { type: 'save-started', payload: null }), state);
  assert.equal(reduce(state, { type: 'save-started', payload: { name: 7, preview: '7-1' } }), state);
  assert.equal(reduce(state, saveEnded), state);
  // Control: a well-formed save-started is recorded.
  assert.deepEqual(reduce(state, saveStarted('optimized-holiday', '7-1')).single.saving, { name: 'optimized-holiday' });
});

// names

test('the_suggestion_fills_the_box_until_the_user_types', () => {
  const state = reduce(initialState(), loaded('7'));
  assert.equal(nameText(state), 'optimized-holiday');
  const moved = reduce(state, suggested('7', 'holiday'));
  assert.equal(nameText(moved), 'holiday');
  assert.equal(moved.single.image.suggested, 'holiday');
});

test('name_suggested_does_not_overwrite_typed_text', () => {
  const mine = apply(initialState(), loaded('7'), typed('mine'));
  const moved = reduce(mine, suggested('7', 'holiday'));
  assert.equal(nameText(moved), 'mine');
  assert.equal(moved.single.image.suggested, 'holiday');
  // Typing the box empty is still typing.
  const emptied = apply(initialState(), loaded('7'), typed(''), suggested('7', 'holiday'));
  assert.equal(nameText(emptied), '');
});

test('name_suggested_for_another_image_or_with_a_bad_payload_changes_nothing', () => {
  const state = reduce(initialState(), loaded('7'));
  assert.equal(reduce(state, suggested('6', 'holiday')), state);
  assert.equal(reduce(state, suggested('7', null)), state);
  assert.equal(reduce(state, { type: 'name-suggested', payload: null }), state);
  const none = initialState();
  assert.equal(reduce(none, suggested('7', 'holiday')), none);
  // Control: the current image's suggestion is taken.
  assert.equal(nameText(reduce(state, suggested('7', 'holiday'))), 'holiday');
});

test('set_name_text_with_a_non_string_changes_nothing', () => {
  const state = reduce(initialState(), loaded('7'));
  assert.equal(reduce(state, typed(7)), state);
  assert.equal(reduce(state, typed(null)), state);
  // Control: text is taken.
  assert.equal(nameText(reduce(state, typed('x'))), 'x');
});

test('image_saved_sets_the_next_free_name_the_status_and_open_folder', () => {
  const state = reduce(withVersion('7'), saved());
  assert.equal(nameText(state), 'optimized-holiday-1');
  assert.equal(state.message, 'Saved as optimized-holiday.jpg');
  assert.equal(state.hasOutput, true);
  // Also over a typed name, which stays the user's: a later suggestion keeps it.
  const typedName = apply(withVersion('7'), typed('beach'), saved('beach.jpg', null, 'beach-1'));
  assert.equal(nameText(typedName), 'beach-1');
  assert.equal(nameText(reduce(typedName, suggested('7', 'holiday'))), 'beach-1');
});

test('image_saved_with_a_bad_payload_still_allows_open_folder_and_keeps_the_box', () => {
  const before = withVersion('7');
  const nothing = reduce(before, { type: 'image-saved', payload: null });
  assert.equal(nothing.hasOutput, true);
  assert.equal(nameText(nothing), 'optimized-holiday');
  assert.equal(nothing.message, before.message);
  const badFields = reduce(before, { type: 'image-saved', payload: { name: 7, folder: null, suggested: 8 } });
  assert.equal(badFields.hasOutput, true);
  assert.equal(nameText(badFields), 'optimized-holiday');
  assert.equal(badFields.messageSeq, before.messageSeq);
  const emptyName = reduce(before, saved('', null, 'x'));
  assert.equal(emptyName.messageSeq, before.messageSeq);
  assert.equal(nameText(emptyName), 'x');
});

test('the_name_problem_comes_from_nameProblem', () => {
  const state = reduce(initialState(), loaded('7'));
  const long = 'a'.repeat(197);
  for (const name of ['', 'a/b', 'holiday.', 'CON', long, 'a'.repeat(196)]) {
    assert.equal(nameProblemText(reduce(state, typed(name))), nameProblem(name, '.jpg'), name);
  }
  // The extension counts: 197 + ".jpg" is 201 characters.
  assert.equal(nameProblemText(reduce(state, typed(long))), 'This name is too long.');
  assert.equal(nameProblemText(reduce(state, typed('CON'))), 'This name is reserved by Windows.');
  assert.equal(nameProblemText(state), null);
  assert.equal(nameProblemText(initialState()), null);
});

// sizes

test('sizes_are_formatted_as_batch_does', () => {
  const sizes = sizeView(withVersion('7'));
  assert.deepEqual(sizes, {
    original: formatSize(2400000),
    compressed: formatSize(610000),
    saves: `${formatPercent(2400000, 610000)} · ${formatSize(1790000)}`,
    note: null,
    busy: false,
  });
  assert.equal(sizes.original, '2.3 MiB');
  assert.equal(sizes.saves, '74.6% · 1.7 MiB');
  const small = apply(
    initialState(),
    imageChanged(logo('9')),
    requested(),
    started('9', 1),
    ready('9', 1, { mode: 'lossless', quality: null, before: 4100, after: 3000 }),
  );
  assert.equal(sizeView(small).compressed, '2.9 KiB');
  assert.equal(sizeView(small).saves, '26.8% · 1.1 KiB');
});

test('no_image_shows_no_sizes', () => {
  assert.deepEqual(sizeView(initialState()), { original: '', compressed: '', saves: '', note: null, busy: false });
  const noVersion = reduce(initialState(), loaded('7'));
  assert.deepEqual(sizeView(noVersion), { original: '2.3 MiB', compressed: '', saves: '', note: null, busy: false });
});

test('a_file_that_grew_shows_a_negative_saving', () => {
  const grew = apply(
    initialState(),
    loaded('7', { size: 1000 }),
    requested(),
    started('7', 1),
    ready('7', 1, { mode: 'lossless', quality: null, before: 1000, after: 1027 }),
  );
  assert.equal(sizeView(grew).saves, '-2.7% · -27 B');
});

test('a_zero_byte_original_saves_nothing', () => {
  const empty = apply(
    initialState(),
    loaded('7', { size: 0 }),
    requested(),
    started('7', 1),
    ready('7', 1, { mode: 'lossless', quality: null, before: 0, after: 0 }),
  );
  assert.equal(sizeView(empty).saves, '0.0% · 0 B');
});

test('a_note_shows_when_present_and_not_otherwise', () => {
  const base = apply(initialState(), loaded('7'), requested(), started('7', 1));
  const noted = reduce(base, ready('7', 1, { note: 'original kept: lossy was not smaller' }));
  assert.equal(sizeView(noted).note, 'original kept: lossy was not smaller');
  assert.equal(sizeView(reduce(base, ready('7', 1))).note, null);
});

test('compressing_keeps_the_previous_numbers', () => {
  const before = withVersion('7');
  const busy = apply(before, setQuality('60'), requested('lossy', 60), started('7', 2));
  const sizes = sizeView(busy);
  assert.equal(sizes.busy, true);
  assert.equal(sizes.compressed, sizeView(before).compressed);
  assert.equal(sizes.saves, sizeView(before).saves);
  assert.equal(sizeView(before).busy, false);
});

// rejections tied to the name

test('name_taken_names_the_full_file_name_and_ties_it_to_the_box', () => {
  // During a save: the stem sent, even when the box was typed in since.
  const saving = apply(withVersion('7'), typed('beach'), saveStarted('beach', '7-1'), typed('beach'));
  const taken = reduce(saving, rejected('name-taken'));
  const text = 'A file named beach.jpg already exists there. Choose another name.';
  assert.equal(taken.message, text);
  assert.equal(nameProblemText(taken), text);
  const sentOther = apply(withVersion('7'), saveStarted('optimized-holiday', '7-1'), typed('later'), rejected('name-taken'));
  assert.equal(sentOther.message, 'A file named optimized-holiday.jpg already exists there. Choose another name.');
  // No save recorded: the box's text.
  const plain = apply(withVersion('7'), typed('sea'), rejected('name-taken'));
  assert.equal(plain.message, 'A file named sea.jpg already exists there. Choose another name.');
  assert.equal(afterRejection('name-taken').log, false);
});

test('a_changed_name_drops_the_name_taken_problem', () => {
  const taken = apply(withVersion('7'), typed('beach'), rejected('name-taken'));
  assert.notEqual(nameProblemText(taken), null);
  assert.equal(nameProblemText(reduce(taken, typed('beach-2'))), null);
  assert.equal(nameProblemText(reduce(taken, saved('beach-2.jpg', null, 'beach-3'))), null);
});

test('invalid_name_gives_the_name_problem_text', () => {
  const state = apply(withVersion('7'), saveStarted('a:b', '7-1'), rejected('invalid-name'));
  assert.equal(state.message, 'A name cannot contain / \\ : * ? " < > | or control characters.');
  assert.equal(afterRejection('invalid-name').log, false);
});

test('invalid_name_for_a_name_the_window_accepts_gives_the_generic_message', () => {
  const state = apply(withVersion('7'), rejected('invalid-name'));
  assert.equal(state.message, MESSAGES.unknown);
  // Control: the string itself is known (not logged); only its text falls back.
  assert.equal(afterRejection('invalid-name').log, false);
  // With no image there is no file name to speak of: both get the generic message.
  assert.equal(reduce(initialState(), rejected('invalid-name')).message, MESSAGES.unknown);
  assert.equal(reduce(initialState(), rejected('name-taken')).message, MESSAGES.unknown);
  assert.equal(reduce(initialState(), rejected('name-taken')).single.refused, null);
});

// viewResets

test('view_resets_for_a_new_key', () => {
  const first = reduce(initialState(), loaded('7'));
  assert.equal(viewResets(initialState(), first), true);
  assert.equal(viewResets(first, reduce(first, loaded('8'))), true);
});

test('view_does_not_reset_for_a_name_a_setting_a_version_or_the_same_key', () => {
  const base = apply(initialState(), loaded('7'), requested(), started('7', 1));
  const cases = {
    'name-suggested': suggested('7', 'holiday'),
    'set-mode': setMode('lossy'),
    'set-quality-text': setQuality('60'),
    'preview-ready': ready('7', 1),
    'same key': imageChanged(holiday('7'), 0, 'Folders go in Batch.'),
    'typing': typed('x'),
  };
  for (const [name, action] of Object.entries(cases)) {
    assert.equal(viewResets(base, reduce(base, action)), false, name);
  }
  assert.equal(viewResets(initialState(), initialState()), false);
});

// imageAlt and pixelSizeText

test('image_alt_names_lossless_and_lossy_quality', () => {
  assert.equal(imageAlt(withVersion('7')), 'Compressed, lossy quality 75');
  const base = apply(initialState(), loaded('7'), requested(), started('7', 1));
  assert.equal(imageAlt(reduce(base, ready('7', 1, { mode: 'lossless', quality: null }))), 'Compressed, lossless');
  assert.equal(imageAlt(reduce(base, ready('7', 1, { mode: 'lossy', quality: null }))), 'Compressed, lossy');
});

test('image_alt_before_any_version_is_plain_compressed', () => {
  assert.equal(imageAlt(initialState()), 'Compressed');
  assert.equal(imageAlt(reduce(initialState(), loaded('7'))), 'Compressed');
});

test('pixel_size_text_of_a_raster_image', () => {
  assert.equal(pixelSizeText(holiday('7')), '4032 × 3024');
});

test('pixel_size_text_of_an_svg_or_no_image_is_empty', () => {
  assert.equal(pixelSizeText(logo('9')), '');
  assert.equal(pixelSizeText(null), '');
  assert.equal(pixelSizeText({ ...holiday('7'), width: null }), '');
  assert.equal(pixelSizeText({ ...holiday('7'), height: null }), '');
});

// open folder

test('open_folder_is_usable_after_image_saved', () => {
  assert.equal(canOpenFolder(reduce(withVersion('7'), saved())), true);
});

test('open_folder_is_not_usable_before_a_save_or_a_run', () => {
  assert.equal(canOpenFolder(initialState()), false);
  assert.equal(canOpenFolder(withVersion('7')), false);
  const savedFirst = apply(withVersion('7'), saved(), setView('batch'));
  // Control: usable after the save; then a run going locks it.
  assert.equal(canOpenFolder(savedFirst), true);
  assert.equal(canOpenFolder(reduce(savedFirst, runStarted)), false);
});

// layout

test('layout_is_under_at_start_and_layout_changed_sets_it', () => {
  assert.equal(initialSingle().layout, 'under');
  assert.equal(initialState().single.layout, 'under');
  const beside = reduce(initialState(), { type: 'layout-changed', payload: 'beside' });
  assert.equal(beside.single.layout, 'beside');
  assert.equal(reduce(beside, { type: 'layout-changed', payload: 'under' }).single.layout, 'under');
});

test('layout_changed_with_another_value_changes_nothing', () => {
  const state = initialState();
  for (const payload of ['Beside', null, 'left', undefined]) {
    assert.equal(reduce(state, { type: 'layout-changed', payload }), state, String(payload));
  }
  // Control: the exact spelling is taken.
  assert.equal(reduce(state, { type: 'layout-changed', payload: 'beside' }).single.layout, 'beside');
});

// purity

test('reduce_single_with_an_unknown_action_returns_the_state', () => {
  const state = withVersion('7');
  assert.equal(reduceSingle(state, { type: 'nothing-like-this', payload: null }), state);
  // A rejection that is not about the name is not this module's.
  assert.equal(reduceSingle(state, rejected('busy')), state);
});

test('reduce_never_changes_the_state_it_was_given_for_the_single_actions', () => {
  const base = apply(withVersion('7'), imageChanged(holiday('7'), 2, 'Single image mode takes one image; 2 others were left out.'));
  const busyBase = apply(base, setQuality('60'), requested('lossy', 60), started('7', 2));
  const actions = [
    [base, setView('batch')],
    [base, typed('x')],
    [base, requested('lossy', 50)],
    [base, { type: 'layout-changed', payload: 'beside' }],
    [base, loaded('8')],
    [base, suggested('7', 'holiday')],
    [base, started('7', 2)],
    [busyBase, ready('7', 2, { quality: 60 })],
    [busyBase, failedPreview('7', 2)],
    [base, saved()],
    [base, saveStarted('optimized-holiday', '7-1')],
    [reduce(base, saveStarted('optimized-holiday', '7-1')), saveEnded],
    [base, rejected('name-taken')],
  ];
  for (const [state, action] of actions) {
    const copy = structuredClone(state);
    const next = reduce(state, action);
    assert.notEqual(next, state, action.type);
    assert.deepEqual(state, copy, action.type);
  }
});
