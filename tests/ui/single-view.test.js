import test from 'node:test';
import assert from 'node:assert/strict';

import {
  QUALITY_SETTLE_MS,
  applyDivider,
  applyRatio,
  applyView,
  askPreview,
  bindPreviewTriggers,
  bindRadios,
  busyText,
  chooseLayout,
  debounce,
  dividerFromPointer,
  dragMove,
  dragStart,
  initialZoom,
  makeDispatch,
  moveSettings,
  moveToBatch,
  onRegionKey,
  pictureSize,
  pictureSlot,
  pictureView,
  picturesLoading,
  previewSource,
  regionKey,
  rejectionHandler,
  routeDrag,
  saveImage,
  screenView,
  showText,
  swapPicture,
  switchView,
  syncModifier,
  syncName,
  syncPicture,
  syncRadios,
  syncSizes,
  wheelAction,
  wheelFactor,
  zoomImageOf,
  zoomReduce,
} from '../../ui/single-view.js';
import { MESSAGES, initialState, reduce } from '../../ui/state.js';
import { sizeView } from '../../ui/single.js';
import { LAYOUT_KEY } from '../../ui/prefs.js';
import { step, toActual, toFit, zoomAt, zoomLimits } from '../../ui/zoom.js';
import { fakeDocument } from './fake-document.js';

// --- shared helpers -----------------------------------------------------------

function apply(state, ...actions) {
  return actions.reduce((current, action) => reduce(current, action), state);
}

const holiday = (key = '7') => ({
  name: 'holiday.jpg',
  size: 2400000,
  width: 4032,
  height: 3024,
  key,
  suggested: 'optimized-holiday',
  extension: '.jpg',
});
const loaded = (key = '7', leftOut = 0, notice = null) => ({
  type: 'image-changed',
  payload: { image: holiday(key), left_out: leftOut, notice },
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
const lossy = { type: 'set-mode', payload: 'lossy' };
const runStarted = { type: 'run-started', payload: { total: 1, mode: 'lossless', quality: null } };
const runFinished = { type: 'run-finished', payload: { optimised: 1, saved_bytes: 10, has_output: true } };
const listed = {
  type: 'list-changed',
  payload: { files: [{ name: 'a.png', size: 10, skip: null, thumb: '1-0' }], notice: null },
};

/** A state with image 7 loaded and lossy 75 shown as version 7-3, ready to save. */
function readyToSave() {
  return apply(initialState(), loaded('7'), lossy, started('7', 3), ready('7', 3));
}

/** An invoke double: records every call; a command whose answer is `{ reject: reason }` rejects, any other resolves. */
function fakeInvoke(answers = {}) {
  const calls = [];
  const invoke = (command, args) => {
    calls.push([command, args]);
    const answer = answers[command];
    if (answer !== undefined && answer !== null && Object.hasOwn(answer, 'reject')) {
      return Promise.reject(answer.reject);
    }
    return Promise.resolve(null);
  };
  return { calls, invoke };
}

/** The deps the wiring functions take, with every effect recorded in `log`. */
function harness(state, answers = {}) {
  const log = [];
  const box = { state };
  const { calls, invoke } = fakeInvoke(answers);
  const deps = {
    getState: () => box.state,
    invoke: (command, args) => {
      log.push(['invoke', command]);
      return invoke(command, args);
    },
    dispatch: (action) => {
      log.push(['dispatch', action.type]);
      box.state = reduce(box.state, action);
    },
    reject: (command, reason) => log.push(['reject', command, reason]),
    render: () => log.push(['render']),
    askPreview: () => {
      log.push(['askPreview']);
      return Promise.resolve(false);
    },
  };
  return { box, calls, deps, log };
}

// --- previewSource ------------------------------------------------------------

test('previewSource_accepts_only_window_keys', () => {
  const asked = [];
  const src = previewSource((key, scheme) => {
    asked.push([key, scheme]);
    return `${scheme}://localhost/${key}`;
  });
  assert.equal(src('7'), 'preview://localhost/7');
  assert.equal(src('7-3'), 'preview://localhost/7-3');
  assert.equal(src('1234567890-1234567890'), 'preview://localhost/1234567890-1234567890');
  const bad = ['07x', '7-', '-3', '7-3-1', '12345678901', '7?x', '../7', ' 7', '7 ', '', 7, null, undefined, ['7']];
  for (const key of bad) {
    assert.equal(src(key), null, String(key));
  }
  assert.deepEqual(asked, [
    ['7', 'preview'],
    ['7-3', 'preview'],
    ['1234567890-1234567890', 'preview'],
  ]);
});

// --- swapPicture / syncPicture -----------------------------------------------

/** A stage holding one `<img>` and its slot record. */
function stageWithImage(doc, classes = ['lunaepixSingle__image', '-compressed']) {
  const stage = doc.createElement('div');
  const image = doc.createElement('img');
  image.classList.add(...classes);
  image.alt = 'Compressed';
  stage.append(image);
  return { stage, slot: pictureSlot(image) };
}

const settle = () => new Promise((resolve) => setImmediate(resolve));

test('swapPicture_keeps_the_old_image_until_the_new_one_decodes', async () => {
  const doc = fakeDocument();
  const { stage, slot } = stageWithImage(doc);
  const old = slot.image;
  const ready = [];
  swapPicture(doc, slot, 'preview://localhost/7-3', (img) => ready.push(img), () => assert.fail('no failure'));
  // Asked for, not decoded yet: the old picture is still the one shown.
  assert.equal(doc.decodes.length, 1);
  assert.deepEqual(stage.children, [old]);
  assert.equal(slot.pending, 'preview://localhost/7-3');
  assert.deepEqual(ready, []);

  doc.decodes[0].resolve();
  await settle();
  const fresh = doc.decodes[0].element;
  assert.deepEqual(stage.children, [fresh]);
  assert.notEqual(fresh, old);
  assert.equal(fresh.src, 'preview://localhost/7-3');
  assert.deepEqual([...fresh.classList], ['lunaepixSingle__image', '-compressed']);
  assert.equal(slot.image, fresh);
  assert.equal(slot.url, 'preview://localhost/7-3');
  assert.equal(slot.pending, null);
  assert.deepEqual(ready, [fresh]);
  assert.deepEqual(doc.markupWrites, []);
});

test('a_failed_decode_keeps_the_old_image_and_reports', async () => {
  const doc = fakeDocument();
  const { stage, slot } = stageWithImage(doc);
  const old = slot.image;
  const failures = [];
  swapPicture(doc, slot, 'preview://localhost/7-4', () => assert.fail('not ready'), (error) => failures.push(error));
  doc.decodes[0].reject(new Error('EncodingError'));
  await settle();
  assert.deepEqual(stage.children, [old]);
  assert.equal(slot.image, old);
  assert.equal(slot.url, null);
  assert.equal(slot.pending, null);
  assert.equal(slot.failed, 'preview://localhost/7-4');
  assert.equal(failures.length, 1);
  assert.equal(failures[0].message, 'EncodingError');
});

test('an_older_swap_that_finishes_late_is_dropped', async () => {
  for (const order of [
    [1, 0],
    [0, 1],
  ]) {
    const doc = fakeDocument();
    const { stage, slot } = stageWithImage(doc);
    const ready = [];
    const failed = [];
    swapPicture(doc, slot, 'u/7-3', (img) => ready.push(img.src), () => failed.push('7-3'));
    swapPicture(doc, slot, 'u/7-4', (img) => ready.push(img.src), () => failed.push('7-4'));
    for (const index of order) {
      doc.decodes[index].resolve();
      await settle();
    }
    // Whichever finishes first, only the newer request is shown.
    assert.deepEqual(ready, ['u/7-4'], `order ${order}`);
    assert.equal(stage.children.length, 1);
    assert.equal(stage.children[0].src, 'u/7-4');
    assert.equal(slot.url, 'u/7-4');
    assert.deepEqual(failed, []);
  }
  // A stale failure reports nothing either.
  const doc = fakeDocument();
  const { slot } = stageWithImage(doc);
  const failed = [];
  swapPicture(doc, slot, 'u/7-3', () => {}, () => failed.push('old'));
  swapPicture(doc, slot, 'u/7-4', () => {}, () => failed.push('new'));
  doc.decodes[0].reject(new Error('gone'));
  await settle();
  assert.deepEqual(failed, []);
  assert.equal(slot.failed, null);
});

test('syncPicture_swaps_in_a_new_url_and_sets_its_alt', async () => {
  const doc = fakeDocument();
  const { stage, slot } = stageWithImage(doc);
  const done = [];
  syncPicture(doc, slot, 'u/7-3', 'Compressed, lossy quality 75', (img) => done.push(img), () => {});
  doc.decodes[0].resolve();
  await settle();
  assert.equal(stage.children[0].alt, 'Compressed, lossy quality 75');
  assert.equal(done.length, 1);
});

test('syncPicture_does_not_swap_the_url_shown_pending_or_failed', async () => {
  const doc = fakeDocument();
  const { slot } = stageWithImage(doc);
  syncPicture(doc, slot, 'u/a', 'x', () => {}, () => {});
  // Pending: asked again by the next render, nothing new is made.
  syncPicture(doc, slot, 'u/a', 'x', () => {}, () => {});
  assert.equal(doc.decodes.length, 1);
  doc.decodes[0].resolve();
  await settle();
  // Shown.
  syncPicture(doc, slot, 'u/a', 'x', () => {}, () => {});
  assert.equal(doc.decodes.length, 1);
  // Failed: not tried again on every render.
  syncPicture(doc, slot, 'u/b', 'x', () => {}, () => {});
  doc.decodes[1].reject(new Error('404'));
  await settle();
  syncPicture(doc, slot, 'u/b', 'x', () => {}, () => {});
  assert.equal(doc.decodes.length, 2);
  // A new address is asked for.
  syncPicture(doc, slot, 'u/c', 'x', () => {}, () => {});
  assert.equal(doc.decodes.length, 3);
});

test('syncPicture_with_no_url_hides_the_image_and_cancels_a_pending_swap', async () => {
  const doc = fakeDocument();
  const { stage, slot } = stageWithImage(doc);
  // Nothing shown, nothing pending: nothing to do.
  syncPicture(doc, slot, null, 'x', () => assert.fail('no swap'), () => {});
  assert.equal(slot.image.hidden, false);
  assert.equal(doc.decodes.length, 0);

  syncPicture(doc, slot, 'u/a', 'x', () => {}, () => {});
  doc.decodes[0].resolve();
  await settle();
  const shown = stage.children[0];
  syncPicture(doc, slot, 'u/b', 'x', () => assert.fail('cancelled'), () => {});
  syncPicture(doc, slot, null, 'x', () => {}, () => {});
  assert.equal(shown.hidden, true);
  assert.equal(slot.url, null);
  assert.equal(slot.pending, null);
  doc.decodes[1].resolve();
  await settle();
  assert.deepEqual(stage.children, [shown]);
});

// --- moveSettings -----------------------------------------------------------

function settingsLayout(doc) {
  const single = doc.createElement('div');
  const batch = doc.createElement('div');
  const settings = doc.createElement('div');
  const radio = doc.createElement('input');
  const saveAs = doc.createElement('div');
  settings.append(radio);
  batch.append(settings);
  single.append(saveAs);
  return { single, batch, settings, radio, saveAs };
}

test('moveSettings_moves_the_one_block_and_keeps_focus_inside_it', () => {
  const doc = fakeDocument();
  const { single, batch, settings, radio, saveAs } = settingsLayout(doc);
  radio.focus();
  // A browser blurs a focused element that moves; the radio must be focused again.
  const focusCalls = [];
  const realFocus = radio.focus.bind(radio);
  radio.focus = () => {
    focusCalls.push('radio');
    realFocus();
  };
  assert.equal(moveSettings(doc, settings, single), true);
  // The one block, first in the slot (before Save as), gone from Batch's slot.
  assert.deepEqual(single.children, [settings, saveAs]);
  assert.deepEqual(batch.children, []);
  assert.equal(doc.created.filter((element) => element.children.includes(radio)).length, 1);
  assert.deepEqual(focusCalls, ['radio']);
  assert.equal(doc.activeElement, radio);
});

test('moveSettings_leaves_focus_outside_alone_and_does_not_move_a_block_already_in_its_slot', () => {
  const doc = fakeDocument();
  const { single, batch, settings } = settingsLayout(doc);
  const outside = doc.createElement('button');
  outside.focus();
  assert.equal(moveSettings(doc, settings, single), true);
  assert.equal(doc.activeElement, outside);
  // Already there: nothing moves.
  const before = single.children.slice();
  assert.equal(moveSettings(doc, settings, single), false);
  assert.deepEqual(single.children, before);
  assert.deepEqual(batch.children, []);
});

// --- syncSizes / syncName / showText ------------------------------------------

function sizeBox(doc) {
  return {
    original: doc.createElement('dd'),
    compressed: doc.createElement('dd'),
    saves: doc.createElement('dd'),
    note: doc.createElement('p'),
  };
}

test('syncSizes_writes_text_only', () => {
  const doc = fakeDocument();
  const box = sizeBox(doc);
  const view = sizeView(apply(readyToSave(), ready('7', 3, { note: '<b>kept</b>' })));
  syncSizes(doc, box, view);
  assert.equal(box.original.textContent, view.original);
  assert.equal(box.compressed.textContent, view.compressed);
  assert.equal(box.saves.textContent, view.saves);
  assert.equal(box.note.textContent, '<b>kept</b>');
  assert.equal(box.note.hidden, false);
  assert.deepEqual(doc.markupWrites, []);
  assert.equal(doc.created.length, 4);
});

test('syncSizes_shows_compressing_while_busy_and_hides_an_empty_note', () => {
  const doc = fakeDocument();
  const box = sizeBox(doc);
  syncSizes(doc, box, { original: '2.4 MB', compressed: '610 KB', saves: '75% · 1.8 MB', note: 'old', busy: true });
  assert.equal(box.note.textContent, 'Compressing…');
  assert.equal(box.note.hidden, false);
  assert.equal(box.compressed.textContent, '610 KB');
  syncSizes(doc, box, { original: '2.4 MB', compressed: '', saves: '', note: null, busy: false });
  assert.equal(box.note.textContent, '');
  assert.equal(box.note.hidden, true);
  assert.equal(box.compressed.textContent, '');
});

test('syncName_ties_the_error_with_aria_describedby_and_clears_it', () => {
  const doc = fakeDocument();
  const input = doc.createElement('input');
  const error = doc.createElement('p');
  error.id = 'nameError';
  syncName(input, error, 'CON', 'This name is reserved by Windows.');
  assert.equal(input.value, 'CON');
  assert.equal(input.getAttribute('aria-invalid'), 'true');
  assert.equal(input.getAttribute('aria-describedby'), 'nameError');
  assert.equal(error.textContent, 'This name is reserved by Windows.');
  assert.equal(error.hidden, false);
  syncName(input, error, 'holiday', null);
  assert.equal(input.getAttribute('aria-invalid'), 'false');
  assert.equal(input.getAttribute('aria-describedby'), null);
  assert.equal(error.hidden, true);
  assert.equal(error.textContent, '');
});

test('syncName_does_not_rewrite_the_text_the_box_holds', () => {
  const doc = fakeDocument();
  const input = doc.createElement('input');
  const error = doc.createElement('p');
  input.value = 'vakantie-café';
  const writes = input.valueWrites;
  syncName(input, error, 'vakantie-café', null);
  assert.equal(input.valueWrites, writes);
  syncName(input, error, 'optimized-holiday-1', null);
  assert.equal(input.valueWrites, writes + 1);
  assert.equal(input.value, 'optimized-holiday-1');
});

test('showText_writes_and_shows_a_text', () => {
  const doc = fakeDocument();
  const element = doc.createElement('p');
  element.hidden = true;
  showText(element, 'Loading picture…');
  assert.equal(element.textContent, 'Loading picture…');
  assert.equal(element.hidden, false);
});

test('showText_hides_an_empty_text_and_does_not_rewrite_the_same_text', () => {
  const doc = fakeDocument();
  const element = doc.createElement('p');
  showText(element, 'Compressing…');
  const writes = element.textWrites;
  showText(element, 'Compressing…');
  assert.equal(element.textWrites, writes);
  showText(element, '');
  assert.equal(element.hidden, true);
});

// --- applyView / applyDivider / applyRatio ------------------------------------

function pictureParts(doc) {
  const picture = doc.createElement('div');
  const layer = doc.createElement('div');
  const original = doc.createElement('div');
  const compressed = doc.createElement('div');
  layer.append(compressed);
  picture.append(original, layer);
  return { picture, layer, stages: [original, compressed] };
}

test('applyView_sets_the_same_transform_on_both_stages_and_leaves_the_layer_alone', () => {
  const doc = fakeDocument();
  const parts = pictureParts(doc);
  applyView(parts, { scale: 0.25, x: -12.5, y: 30 });
  for (const stage of parts.stages) {
    assert.deepEqual(stage.style.writes, [['transform', 'translate(-12.5px, 30px) scale(0.25)']]);
  }
  assert.equal(parts.stages[0].style.transform, parts.stages[1].style.transform);
  assert.deepEqual(parts.layer.style.writes, []);
  assert.deepEqual(parts.picture.style.writes, []);
});

test('applyDivider_sets_only_the_custom_property', () => {
  const doc = fakeDocument();
  const parts = pictureParts(doc);
  applyDivider(parts, 30);
  assert.deepEqual(parts.picture.style.writes, [['--split', '30%']]);
  for (const element of [parts.layer, ...parts.stages]) {
    assert.deepEqual(element.style.writes, []);
  }
});

test('applyRatio_sets_the_ratio_of_a_usable_size', () => {
  const doc = fakeDocument();
  const picture = doc.createElement('div');
  applyRatio(picture, { width: 4032, height: 3024 });
  applyRatio(picture, { width: 120, height: 40, svg: true });
  assert.deepEqual(picture.style.writes, [
    ['--ratio', '4032 / 3024'],
    ['--ratio', '120 / 40'],
  ]);
});

test('applyRatio_removes_the_ratio_for_an_unusable_size', () => {
  const doc = fakeDocument();
  const picture = doc.createElement('div');
  for (const size of [null, { width: 0, height: 10 }, { width: 10, height: Number.NaN }]) {
    applyRatio(picture, size);
  }
  assert.deepEqual(picture.style.writes, [
    ['--ratio', null],
    ['--ratio', null],
    ['--ratio', null],
  ]);
});

// --- screenView -------------------------------------------------------------

test('screenView_in_single_with_an_image_shows_the_loaded_blocks', () => {
  const view = screenView(apply(initialState(), loaded('7')));
  assert.equal(view.view, 'single');
  assert.equal(view.single, true);
  assert.equal(view.showLoaded, true);
  assert.equal(view.showSingleDrop, false);
  assert.equal(view.showBatchDrop, false);
  assert.equal(view.showBatchPanel, false);
  assert.equal(view.settingsSlot, 'single');
  assert.equal(view.layout, 'under');
  assert.equal(view.fileName, 'holiday.jpg');
  assert.equal(view.pixelSize, '4032 × 3024');
  assert.equal(view.extension, '.jpg');
});

test('screenView_in_single_without_an_image_shows_the_drop_area_only', () => {
  const view = screenView(initialState());
  assert.equal(view.single, true);
  assert.equal(view.showLoaded, false);
  assert.equal(view.showSingleDrop, true);
  assert.equal(view.fileName, '');
  assert.equal(view.pixelSize, '');
  assert.equal(view.extension, '');
  assert.equal(view.settingsSlot, 'single');
  // A finished batch's summary does not take the settings away in Single image.
  const finished = apply(initialState(), { type: 'set-view', payload: 'batch' }, listed, runStarted, runFinished, {
    type: 'set-view',
    payload: 'single',
  });
  assert.equal(screenView(finished).showSummary, false);
});

test('screenView_in_batch_shows_batch_and_puts_the_settings_in_batchs_slot', () => {
  const batch = apply(initialState(), loaded('7'), { type: 'set-view', payload: 'batch' });
  const view = screenView(batch);
  assert.equal(view.view, 'batch');
  assert.equal(view.single, false);
  assert.equal(view.showBatchDrop, true);
  assert.equal(view.showBatchPanel, false);
  assert.equal(view.settingsSlot, 'batch');
  assert.equal(screenView(apply(batch, listed)).showBatchPanel, true);
  const finished = apply(batch, listed, runStarted, runFinished);
  assert.equal(screenView(finished).showSummary, true);
  assert.equal(screenView(apply(batch, listed)).showSummary, false);
});

test('the_drag_highlight_goes_to_the_drop_area_that_shows_or_the_picture', () => {
  const over = { type: 'drag-state', payload: { over: true } };
  const pick = (state) => {
    const view = screenView(state);
    return [view.dragBatchDrop, view.dragSingleDrop, view.dragPicture];
  };
  assert.deepEqual(pick(apply(initialState(), over)), [false, true, false]);
  assert.deepEqual(pick(apply(initialState(), loaded('7'), over)), [false, false, true]);
  assert.deepEqual(pick(apply(initialState(), { type: 'set-view', payload: 'batch' }, over)), [true, false, false]);
  // Not dragging: nothing is highlighted.
  assert.deepEqual(pick(initialState()), [false, false, false]);
  assert.deepEqual(pick(apply(initialState(), loaded('7'))), [false, false, false]);
});

test('screenView_offers_switch_to_batch_while_the_offer_lasts', () => {
  const offered = apply(initialState(), loaded('7', 3, 'Single image mode takes one image; 3 others were left out.'));
  assert.equal(screenView(offered).showMove, true);
  assert.equal(screenView(offered).canMove, true);
  assert.equal(screenView(initialState()).showMove, false);
  assert.equal(screenView(initialState()).canMove, false);
  // During a save the offer shows but cannot be used, nor can the switch.
  const saving = apply(offered, lossy, started('7', 3), ready('7', 3), {
    type: 'save-started',
    payload: { name: 'optimized-holiday', preview: '7-3' },
  });
  assert.equal(screenView(saving).showMove, true);
  assert.equal(screenView(saving).canMove, false);
  assert.equal(screenView(saving).canSwitch, false);
  assert.equal(screenView(offered).canSwitch, true);
});

// --- busyText / pictureView / zoomImageOf -------------------------------------

test('busyText_says_loading_while_a_picture_decodes_else_compressing', () => {
  const compressing = sizeView(apply(readyToSave(), started('7', 4)));
  const idle = sizeView(readyToSave());
  assert.equal(compressing.busy, true);
  assert.equal(busyText(idle, true), 'Loading picture…');
  assert.equal(busyText(compressing, true), 'Loading picture…');
  assert.equal(busyText(compressing, false), 'Compressing…');
  assert.equal(busyText(idle, false), '');
});

test('the_number_field_settles_after_400_ms', () => {
  assert.equal(QUALITY_SETTLE_MS, 400);
});

test('pictureView_gives_the_original_and_the_shown_versions_addresses', () => {
  const src = (key) => `p/${key}`;
  const view = pictureView(readyToSave(), src);
  assert.deepEqual(view, { original: 'p/7', compressed: 'p/7-3', alt: 'Compressed, lossy quality 75' });
});

test('pictureView_gives_no_address_without_an_image_or_a_ready_version', () => {
  const asked = [];
  const src = (key) => {
    asked.push(key);
    return `p/${key}`;
  };
  assert.deepEqual(pictureView(initialState(), src), { original: null, compressed: null, alt: 'Compressed' });
  assert.deepEqual(pictureView(apply(initialState(), loaded('8')), src), {
    original: 'p/8',
    compressed: null,
    alt: 'Compressed',
  });
  assert.deepEqual(asked, ['8']);
});

test('zoomImageOf_uses_the_pixel_size_or_the_decoded_svg_size', () => {
  assert.deepEqual(zoomImageOf(holiday(), null), { width: 4032, height: 3024 });
  // A raster's reported size wins over what the element decoded to.
  assert.deepEqual(zoomImageOf(holiday(), { naturalWidth: 10, naturalHeight: 10 }), { width: 4032, height: 3024 });
  const svg = { ...holiday('9'), width: null, height: null };
  assert.deepEqual(zoomImageOf(svg, { naturalWidth: 300, naturalHeight: 150 }), { width: 300, height: 150, svg: true });
});

test('zoomImageOf_gives_null_without_a_size', () => {
  const svg = { ...holiday('9'), width: null, height: null };
  assert.equal(zoomImageOf(null, { naturalWidth: 300, naturalHeight: 150 }), null);
  assert.equal(zoomImageOf(svg, null), null);
  assert.equal(zoomImageOf(svg, { naturalWidth: 0, naturalHeight: 0 }), null);
});

// --- zoomReduce and the input maths -------------------------------------------

const AREA = { width: 800, height: 600 };
const BIG = { width: 4000, height: 3000 };
const LIMITS = zoomLimits(BIG, AREA);

test('zoomReduce_fits_on_reset_and_fit_and_marks_it', () => {
  const zoomed = { view: { scale: 2, x: 100, y: 50 }, atFit: false };
  for (const type of ['reset', 'fit']) {
    assert.deepEqual(zoomReduce(zoomed, { type }, BIG, AREA), { view: toFit(LIMITS), atFit: true });
  }
  assert.deepEqual(initialZoom(), { view: { scale: 1, x: 0, y: 0 }, atFit: true });
});

test('zoomReduce_wheel_steps_actual_and_pan_move_and_clear_the_fit_mark', () => {
  const fit = zoomReduce(initialZoom(), { type: 'reset' }, BIG, AREA);
  const point = { x: 100, y: 100 };
  const wheeled = zoomReduce(fit, { type: 'wheel', deltaY: -3, point }, BIG, AREA);
  assert.deepEqual(wheeled, { view: zoomAt(fit.view, 1.25, point, LIMITS, BIG, AREA), atFit: false });
  const out = zoomReduce(wheeled, { type: 'wheel', deltaY: 3, point }, BIG, AREA);
  assert.deepEqual(out.view, zoomAt(wheeled.view, 0.8, point, LIMITS, BIG, AREA));
  const stepped = zoomReduce(fit, { type: 'step', direction: 1 }, BIG, AREA);
  assert.deepEqual(stepped, { view: step(fit.view, 1, LIMITS, BIG, AREA), atFit: false });
  const actual = zoomReduce(fit, { type: 'actual' }, BIG, AREA);
  assert.deepEqual(actual, { view: toActual(fit.view, LIMITS, BIG, AREA), atFit: false });
  const panned = zoomReduce(actual, { type: 'pan', dx: 40, dy: -40 }, BIG, AREA);
  assert.deepEqual(panned.view, { scale: actual.view.scale, x: actual.view.x + 40, y: actual.view.y - 40 });
  // Zooming out back to the bottom of the range is at Fit again.
  const back = zoomReduce(stepped, { type: 'step', direction: -1 }, BIG, AREA);
  assert.equal(back.atFit, true);
});

test('zoomReduce_refit_follows_fit_only_while_at_fit', () => {
  const fit = zoomReduce(initialZoom(), { type: 'reset' }, BIG, { width: 100, height: 100 });
  const refit = zoomReduce(fit, { type: 'refit' }, BIG, AREA);
  assert.deepEqual(refit, { view: toFit(LIMITS), atFit: true });
  // Zoomed in: a new area keeps the scale (within the limits) and the pan within bounds.
  const zoomed = { view: { scale: 2, x: 5000, y: 0 }, atFit: false };
  const kept = zoomReduce(zoomed, { type: 'refit' }, BIG, AREA);
  assert.equal(kept.atFit, false);
  assert.equal(kept.view.scale, 2);
  assert.equal(kept.view.x, (4000 * 2 - 800) / 2);
});

test('zoomReduce_ignores_an_unknown_action_and_a_zero_wheel', () => {
  const zoomed = { view: { scale: 2, x: 10, y: 0 }, atFit: false };
  assert.equal(zoomReduce(zoomed, { type: 'spin' }, BIG, AREA), zoomed);
  assert.equal(zoomReduce(zoomed, { type: 'wheel', deltaY: 0, point: null }, BIG, AREA), zoomed);
});

test('wheelFactor_zooms_in_on_up_and_out_on_down', () => {
  assert.equal(wheelFactor(-1), 1.25);
  assert.equal(wheelFactor(-120), 1.25);
  assert.equal(wheelFactor(1), 0.8);
});

test('wheelFactor_gives_null_for_zero_or_not_a_number', () => {
  for (const delta of [0, -0, Number.NaN, undefined, '3']) {
    assert.equal(wheelFactor(delta), null, String(delta));
  }
});

test('regionKey_pans_on_arrow_keys_on_the_region_itself', () => {
  const region = {};
  assert.deepEqual(regionKey(region, region, 'ArrowLeft'), { dx: 40, dy: 0 });
  assert.deepEqual(regionKey(region, region, 'ArrowRight'), { dx: -40, dy: 0 });
  assert.deepEqual(regionKey(region, region, 'ArrowUp'), { dx: 0, dy: 40 });
  assert.deepEqual(regionKey(region, region, 'ArrowDown'), { dx: 0, dy: -40 });
});

test('regionKey_ignores_other_keys_and_keys_on_the_divider', () => {
  const region = {};
  const dividerInput = {};
  assert.equal(regionKey(region, region, 'Enter'), null);
  assert.equal(regionKey(region, region, 'a'), null);
  // The divider is inside the region: its arrow keys move the divider, not the picture.
  assert.equal(regionKey(region, dividerInput, 'ArrowLeft'), null);
});

test('dividerFromPointer_gives_the_percent_under_the_pointer', () => {
  const rect = { left: 100, width: 400 };
  assert.equal(dividerFromPointer(200, rect), 25);
  assert.equal(dividerFromPointer(301, rect), 50);
  assert.equal(dividerFromPointer(100, rect), 0);
});

test('dividerFromPointer_clamps_and_refuses_a_zero_width', () => {
  const rect = { left: 100, width: 400 };
  assert.equal(dividerFromPointer(0, rect), 0);
  assert.equal(dividerFromPointer(900, rect), 100);
  assert.equal(dividerFromPointer(200, { left: 0, width: 0 }), null);
  assert.equal(dividerFromPointer(Number.NaN, rect), null);
});

test('a_drag_that_starts_on_the_handle_moves_the_divider_and_elsewhere_pans', () => {
  const doc = fakeDocument();
  const picture = doc.createElement('div');
  const handle = doc.createElement('div');
  const knob = doc.createElement('span');
  handle.append(knob);
  picture.append(handle);
  const rect = { left: 0, width: 200 };

  const onHandle = dragStart(handle, knob, 50, 10);
  assert.equal(onHandle.mode, 'divider');
  assert.deepEqual(dragMove(onHandle, 150, 20, rect).result, { divider: 75 });

  const elsewhere = dragStart(handle, picture, 50, 10);
  assert.equal(elsewhere.mode, 'pan');
  const moved = dragMove(elsewhere, 60, 4, rect);
  assert.deepEqual(moved.result, { dx: 10, dy: -6 });
  // The next move is counted from where the last one ended.
  assert.deepEqual(dragMove(moved.drag, 61, 4, rect).result, { dx: 1, dy: 0 });
  // No drag going: a move does nothing.
  assert.deepEqual(dragMove(null, 61, 4, rect), { drag: null, result: null });
});

test('wheelAction_zooms_about_the_pointer_inside_the_area', () => {
  const rect = { left: 100, top: 50 };
  assert.deepEqual(wheelAction(-3, 150, 80, rect), { type: 'wheel', deltaY: -3, point: { x: 50, y: 30 } });
});

test('wheelAction_hands_any_delta_on_for_zoomReduce_to_refuse', () => {
  // A zero delta still reaches zoomReduce, which changes nothing for it.
  const action = wheelAction(0, 0, 0, { left: 0, top: 0 });
  assert.deepEqual(action, { type: 'wheel', deltaY: 0, point: { x: 0, y: 0 } });
  const zoomed = { view: { scale: 2, x: 0, y: 0 }, atFit: false };
  assert.equal(zoomReduce(zoomed, action, BIG, AREA), zoomed);
});

test('routeDrag_sends_a_divider_move_to_the_divider_and_a_pan_to_the_zoom', () => {
  const calls = [];
  const handlers = { divider: (value) => calls.push(['divider', value]), pan: (action) => calls.push(['pan', action]) };
  routeDrag({ divider: 30 }, handlers);
  routeDrag({ dx: 5, dy: -2 }, handlers);
  assert.deepEqual(calls, [
    ['divider', 30],
    ['pan', { type: 'pan', dx: 5, dy: -2 }],
  ]);
});

test('routeDrag_does_nothing_without_a_result', () => {
  const calls = [];
  routeDrag(null, { divider: () => calls.push('divider'), pan: () => calls.push('pan') });
  assert.deepEqual(calls, []);
});

test('onRegionKey_pans_and_keeps_the_arrow_from_scrolling_the_page', () => {
  const region = {};
  const panned = [];
  let prevented = 0;
  const event = { target: region, key: 'ArrowDown', preventDefault: () => (prevented += 1) };
  assert.equal(
    onRegionKey(region, event, (action) => panned.push(action)),
    true,
  );
  assert.deepEqual(panned, [{ type: 'pan', dx: 0, dy: -40 }]);
  assert.equal(prevented, 1);
});

test('onRegionKey_leaves_other_keys_and_the_dividers_keys_alone', () => {
  const region = {};
  const panned = [];
  let prevented = 0;
  const preventDefault = () => (prevented += 1);
  assert.equal(onRegionKey(region, { target: region, key: 'Tab', preventDefault }, (a) => panned.push(a)), false);
  assert.equal(onRegionKey(region, { target: {}, key: 'ArrowLeft', preventDefault }, (a) => panned.push(a)), false);
  assert.deepEqual(panned, []);
  assert.equal(prevented, 0);
});

test('picturesLoading_is_true_while_either_picture_decodes', () => {
  const idle = { pending: null };
  const busy = { pending: 'u/7' };
  assert.equal(picturesLoading([busy, idle]), true);
  assert.equal(picturesLoading([idle, busy]), true);
});

test('picturesLoading_is_false_when_neither_decodes', () => {
  assert.equal(picturesLoading([{ pending: null }, { pending: null }]), false);
  assert.equal(picturesLoading([]), false);
});

test('pictureSize_reads_the_loaded_image_and_the_original_slots_picture', () => {
  const svg = { ...holiday('9'), width: null, height: null };
  const state = apply(initialState(), { type: 'image-changed', payload: { image: svg, left_out: 0, notice: null } });
  const slot = { image: { naturalWidth: 300, naturalHeight: 150 } };
  assert.deepEqual(pictureSize(state, slot), { width: 300, height: 150, svg: true });
  assert.deepEqual(pictureSize(apply(initialState(), loaded('7')), slot), { width: 4032, height: 3024 });
});

test('pictureSize_is_null_without_an_image', () => {
  assert.equal(pictureSize(initialState(), { image: { naturalWidth: 300, naturalHeight: 150 } }), null);
});

// --- wiring: rejections, preview, view switch, Switch to Batch, save ----------

test('a_known_rejection_dispatches_its_message', () => {
  const dispatched = [];
  const logged = [];
  const reject = rejectionHandler({
    getState: () => initialState(),
    dispatch: (action) => dispatched.push(action),
    log: (...args) => logged.push(args),
  });
  reject('move_drop_to_batch', 'nothing-to-move');
  assert.deepEqual(dispatched, [{ type: 'command-rejected', payload: 'nothing-to-move' }]);
  assert.deepEqual(logged, []);
});

test('busy_dispatches_nothing_and_an_unknown_one_is_logged', () => {
  const dispatched = [];
  const logged = [];
  const reject = rejectionHandler({
    getState: () => initialState(),
    dispatch: (action) => dispatched.push(action),
    log: (...args) => logged.push(args),
  });
  reject('set_view', 'busy');
  assert.deepEqual(dispatched, []);
  reject('set_view', 'invalid-view');
  assert.deepEqual(logged, [['set_view', 'invalid-view']]);
  // A name rejection with no image cannot be explained: logged too.
  reject('save_image', 'name-taken');
  assert.deepEqual(logged[1], ['save_image', 'name-taken']);
  assert.equal(dispatched.length, 2);
});

test('askPreview_sends_preview_args_and_records_them_only_once_resolved', async () => {
  const { box, calls, deps, log } = harness(apply(initialState(), loaded('7')));
  const sent = askPreview(deps);
  // Sent, not recorded yet.
  assert.deepEqual(calls, [['preview_image', { settings: { mode: 'lossless', quality: 75 } }]]);
  assert.equal(box.state.single.requested, null);
  assert.equal(await sent, true);
  assert.deepEqual(log, [
    ['invoke', 'preview_image'],
    ['dispatch', 'preview-requested'],
  ]);
  assert.deepEqual(box.state.single.requested, { mode: 'lossless', quality: 75 });
  // The same settings again: nothing new to make.
  assert.equal(await askPreview(deps), false);
  assert.equal(calls.length, 1);
});

test('askPreview_sends_nothing_without_preview_args', async () => {
  const { calls, deps, log } = harness(initialState());
  assert.equal(await askPreview(deps), false);
  assert.deepEqual(calls, []);
  assert.deepEqual(log, []);
});

test('a_refused_preview_records_nothing_and_reports_the_rejection', async () => {
  const { box, deps, log } = harness(apply(initialState(), loaded('7')), { preview_image: { reject: 'busy' } });
  assert.equal(await askPreview(deps), false);
  assert.deepEqual(log, [
    ['invoke', 'preview_image'],
    ['reject', 'preview_image', 'busy'],
  ]);
  assert.equal(box.state.single.requested, null);
});

test('the_preview_requested_action_names_the_image_it_was_sent_for', async () => {
  const { box, deps } = harness(apply(initialState(), loaded('7')));
  const sent = askPreview(deps);
  // A new image arrives while the request is on its way.
  box.state = reduce(box.state, loaded('8'));
  await sent;
  assert.equal(box.state.single.image.key, '8');
  assert.equal(box.state.single.requested, null);
});

test('switching_view_sends_set_view_then_dispatches_and_asks_for_a_preview_in_single', async () => {
  const { box, calls, deps, log } = harness(apply(initialState(), { type: 'set-view', payload: 'batch' }));
  assert.equal(await switchView(deps, 'single'), true);
  assert.deepEqual(calls, [['set_view', { view: 'single' }]]);
  assert.deepEqual(log, [['invoke', 'set_view'], ['dispatch', 'set-view'], ['askPreview']]);
  assert.equal(box.state.view, 'single');
});

test('switching_to_batch_asks_for_no_preview', async () => {
  const { box, deps, log } = harness(initialState());
  assert.equal(await switchView(deps, 'batch'), true);
  assert.deepEqual(log, [['invoke', 'set_view'], ['dispatch', 'set-view']]);
  assert.equal(box.state.view, 'batch');
});

test('a_locked_or_same_view_sends_nothing_and_renders', async () => {
  const running = apply(initialState(), { type: 'set-view', payload: 'batch' }, listed, runStarted);
  const saving = apply(readyToSave(), { type: 'save-started', payload: { name: 'a', preview: '7-3' } });
  for (const [state, value] of [
    [running, 'single'],
    [saving, 'batch'],
    [initialState(), 'single'],
    [initialState(), 'Single'],
  ]) {
    const { calls, deps, log } = harness(state);
    assert.equal(await switchView(deps, value), false);
    assert.deepEqual(calls, []);
    assert.deepEqual(log, [['render']]);
  }
});

test('a_refused_set_view_dispatches_nothing_and_renders', async () => {
  const { box, deps, log } = harness(initialState(), { set_view: { reject: 'busy' } });
  assert.equal(await switchView(deps, 'batch'), false);
  assert.deepEqual(log, [['invoke', 'set_view'], ['reject', 'set_view', 'busy'], ['render']]);
  assert.equal(box.state.view, 'single');
});

const LEFT_OUT_TWO = 'Single image mode takes one image; 2 others were left out.';
const offered = () => apply(initialState(), loaded('7', 2, LEFT_OUT_TWO));

test('switch_to_batch_sends_the_move_then_shows_batch', async () => {
  const { box, calls, deps, log } = harness(offered());
  assert.equal(await moveToBatch(deps), true);
  assert.deepEqual(calls, [['move_drop_to_batch', undefined]]);
  assert.deepEqual(log, [['invoke', 'move_drop_to_batch'], ['dispatch', 'set-view']]);
  assert.equal(box.state.view, 'batch');
  assert.equal(box.state.single.offer, null);
});

test('switch_to_batch_does_nothing_without_a_movable_offer', async () => {
  const saving = apply(offered(), lossy, started('7', 3), ready('7', 3), {
    type: 'save-started',
    payload: { name: 'a', preview: '7-3' },
  });
  for (const state of [initialState(), saving]) {
    const { calls, deps } = harness(state);
    assert.equal(await moveToBatch(deps), false);
    assert.deepEqual(calls, []);
  }
});

test('a_refused_move_keeps_the_view', async () => {
  const { box, deps, log } = harness(offered(), { move_drop_to_batch: { reject: 'nothing-to-move' } });
  assert.equal(await moveToBatch(deps), false);
  assert.deepEqual(log, [
    ['invoke', 'move_drop_to_batch'],
    ['reject', 'move_drop_to_batch', 'nothing-to-move'],
  ]);
  assert.equal(box.state.view, 'single');
});

test('save_dispatches_started_sends_the_args_then_ended', async () => {
  const { box, calls, deps, log } = harness(readyToSave());
  const saving = saveImage(deps);
  assert.equal(box.state.single.saving.name, 'optimized-holiday');
  assert.equal(await saving, true);
  assert.deepEqual(calls, [['save_image', { name: 'optimized-holiday', preview: '7-3' }]]);
  assert.deepEqual(log, [['dispatch', 'save-started'], ['invoke', 'save_image'], ['dispatch', 'save-ended']]);
  assert.equal(box.state.single.saving, null);
});

test('save_sends_nothing_when_it_cannot_save', async () => {
  const compressing = apply(readyToSave(), started('7', 4));
  for (const state of [initialState(), compressing]) {
    const { calls, deps, log } = harness(state);
    assert.equal(await saveImage(deps), false);
    assert.deepEqual(calls, []);
    assert.deepEqual(log, []);
  }
});

test('a_refused_save_dispatches_the_rejection_before_save_ended', async () => {
  const { box, deps, log } = harness(readyToSave(), { save_image: { reject: 'name-taken' } });
  const seen = [];
  deps.reject = (command, reason) => {
    // The state of that moment still has the save going, so the message names its stem.
    seen.push(box.state.single.saving?.name);
    log.push(['reject', command, reason]);
  };
  assert.equal(await saveImage(deps), false);
  assert.deepEqual(log, [
    ['dispatch', 'save-started'],
    ['invoke', 'save_image'],
    ['reject', 'save_image', 'name-taken'],
    ['dispatch', 'save-ended'],
  ]);
  assert.deepEqual(seen, ['optimized-holiday']);
});

// --- makeDispatch / debounce / triggers ---------------------------------------

function dispatchHarness(state) {
  const log = [];
  const box = { state };
  const dispatch = makeDispatch({
    reduce,
    getState: () => box.state,
    setState: (next) => {
      box.state = next;
    },
    render: () => log.push('render'),
    onNewImage: () => log.push('onNewImage'),
    askPreview: () => log.push('askPreview'),
  });
  return { box, dispatch, log };
}

test('a_new_image_key_resets_the_view_renders_and_asks_for_a_preview', () => {
  const { box, dispatch, log } = dispatchHarness(initialState());
  dispatch(loaded('7'));
  assert.deepEqual(log, ['onNewImage', 'render', 'askPreview']);
  assert.equal(box.state.single.image.key, '7');
  dispatch(loaded('8'));
  assert.deepEqual(log.slice(3), ['onNewImage', 'render', 'askPreview']);
});

test('the_same_image_key_only_renders', () => {
  const { dispatch, log } = dispatchHarness(apply(initialState(), loaded('7')));
  dispatch(loaded('7', 1, 'Single image mode takes one image; 1 other was left out.'));
  dispatch({ type: 'name-suggested', payload: { image: '7', suggested: 'holiday' } });
  // A folder dropped onto a loaded image: the same image stays.
  dispatch({ type: 'image-changed', payload: { image: holiday('7'), left_out: 1, notice: 'Folders go in Batch.' } });
  assert.deepEqual(log, ['render', 'render', 'render']);
});

function fakeTimers() {
  const set = [];
  const cleared = [];
  return {
    set,
    cleared,
    setTimer: (fn, delay) => {
      set.push({ fn, delay });
      return set.length;
    },
    clearTimer: (id) => cleared.push(id),
  };
}

test('debounce_runs_once_after_the_last_trigger', () => {
  const timers = fakeTimers();
  let runs = 0;
  const later = debounce(timers, 400, () => {
    runs += 1;
  });
  later();
  assert.equal(timers.set.length, 1);
  assert.equal(timers.set[0].delay, 400);
  assert.equal(runs, 0);
  timers.set[0].fn();
  assert.equal(runs, 1);
  // Once fired, the next trigger has no timer to clear.
  later();
  assert.deepEqual(timers.cleared, []);
});

test('debounce_clears_the_earlier_timer_on_each_trigger', () => {
  const timers = fakeTimers();
  let runs = 0;
  const later = debounce(timers, 400, () => {
    runs += 1;
  });
  later();
  later();
  later();
  assert.deepEqual(timers.cleared, [1, 2]);
  timers.set[2].fn();
  assert.equal(runs, 1);
});

function triggerParts(doc) {
  const lossless = doc.createElement('input');
  lossless.value = 'lossless';
  const lossyRadio = doc.createElement('input');
  lossyRadio.value = 'lossy';
  const range = doc.createElement('input');
  const number = doc.createElement('input');
  return { modeRadios: [lossless, lossyRadio], range, number };
}

function triggerHarness() {
  const doc = fakeDocument();
  const parts = triggerParts(doc);
  const log = [];
  bindPreviewTriggers(parts, {
    dispatch: (action) => log.push(['dispatch', action.type, action.payload]),
    ask: () => log.push(['ask']),
    later: () => log.push(['later']),
  });
  return { parts, log };
}

test('the_mode_change_the_slider_release_and_the_settled_number_ask_for_a_preview', () => {
  const { parts, log } = triggerHarness();
  parts.modeRadios[1].checked = true;
  parts.modeRadios[1].fire('change');
  assert.deepEqual(log, [['dispatch', 'set-mode', 'lossy'], ['ask']]);
  parts.range.fire('change');
  assert.deepEqual(log.slice(2), [['ask']]);
  parts.number.value = '60';
  parts.number.fire('input');
  assert.deepEqual(log.slice(3), [['dispatch', 'set-quality-text', '60'], ['later']]);
});

test('dragging_the_slider_and_typing_in_the_number_field_do_not_ask_at_once', () => {
  const { parts, log } = triggerHarness();
  parts.range.value = '40';
  parts.range.fire('input');
  parts.number.value = '4';
  parts.number.fire('input');
  assert.deepEqual(log, [['dispatch', 'set-quality-text', '40'], ['dispatch', 'set-quality-text', '4'], ['later']]);
  // A radio that was unchecked by the change sends nothing.
  parts.modeRadios[0].checked = false;
  parts.modeRadios[0].fire('change');
  assert.equal(log.length, 3);
  assert.equal(log.some(([kind]) => kind === 'ask'), false);
});

// --- radios, layout, modifiers --------------------------------------------------

test('syncRadios_checks_the_radio_with_the_value_and_sets_aria_disabled', () => {
  const doc = fakeDocument();
  const single = doc.createElement('input');
  single.value = 'single';
  const batch = doc.createElement('input');
  batch.value = 'batch';
  syncRadios([single, batch], 'batch', true);
  assert.equal(single.checked, false);
  assert.equal(batch.checked, true);
  assert.equal(single.getAttribute('aria-disabled'), 'true');
  syncRadios([single, batch], 'single', false);
  assert.equal(single.checked, true);
  assert.equal(batch.checked, false);
  assert.equal(batch.getAttribute('aria-disabled'), 'false');
});

test('bindRadios_calls_back_only_for_the_radio_that_became_checked', () => {
  const doc = fakeDocument();
  const under = doc.createElement('input');
  under.value = 'under';
  const beside = doc.createElement('input');
  beside.value = 'beside';
  const picked = [];
  bindRadios([under, beside], (value) => picked.push(value));
  beside.checked = true;
  beside.fire('change');
  under.checked = false;
  under.fire('change');
  assert.deepEqual(picked, ['beside']);
});

test('chooseLayout_stores_and_dispatches_the_layout', () => {
  const stored = [];
  const dispatched = [];
  const storage = { setItem: (key, value) => stored.push([key, value]) };
  chooseLayout({ storage, dispatch: (action) => dispatched.push(action) }, 'beside');
  assert.deepEqual(stored, [[LAYOUT_KEY, 'beside']]);
  assert.deepEqual(dispatched, [{ type: 'layout-changed', payload: 'beside' }]);
});

test('chooseLayout_with_a_storage_that_throws_still_dispatches', () => {
  const dispatched = [];
  const storage = {
    setItem: () => {
      throw new Error('QuotaExceededError');
    },
  };
  chooseLayout({ storage, dispatch: (action) => dispatched.push(action) }, 'under');
  assert.deepEqual(dispatched, [{ type: 'layout-changed', payload: 'under' }]);
});

test('syncModifier_sets_exactly_the_chosen_modifier', () => {
  const doc = fakeDocument();
  const body = doc.createElement('div');
  body.classList.add('lunaepixSingle__body', '-under');
  syncModifier(body, ['under', 'beside'], 'beside');
  assert.deepEqual([...body.classList], ['lunaepixSingle__body', '-beside']);
  syncModifier(body, ['under', 'beside'], 'under');
  assert.deepEqual([...body.classList], ['lunaepixSingle__body', '-under']);
});

// --- the decode failure in the status line --------------------------------------

test('a_picture_failure_says_so_in_the_status_line', () => {
  const state = reduce(initialState(), { type: 'picture-failed', payload: null });
  assert.equal(state.message, MESSAGES.pictureFailed);
  assert.equal(MESSAGES.pictureFailed, 'The picture could not be shown.');
  assert.equal(state.messageSeq, 1);
});

test('other_actions_do_not_announce_a_picture_failure', () => {
  const state = apply(initialState(), loaded('7'), { type: 'picture-ok', payload: null });
  assert.notEqual(state.message, 'The picture could not be shown.');
  assert.equal(state.messageSeq, 0);
});
