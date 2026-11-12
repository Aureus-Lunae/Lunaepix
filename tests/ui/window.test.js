// The window itself: ui/window.js's start function, driven through the fake
// page, a fake Tauri, strict timers, a storage and a ResizeObserver
// (fake-window.js). Each test fires the events a person or the backend would
// and asserts what the window sends and what the page shows.

import test from 'node:test';
import assert from 'node:assert/strict';

import { startWindow } from '../../ui/window.js';
import { LAYOUT_KEY } from '../../ui/prefs.js';
import { MESSAGES, initialState, reduce } from '../../ui/state.js';
import { pan, step, toActual, toFit, zoomAt, zoomLimits } from '../../ui/zoom.js';
import { fakePage, fakeResizeObservers, fakeStorage, fakeTauri, settle, strictTimers } from './fake-window.js';

const EVENTS = [
  'drag-state',
  'file-done',
  'file-started',
  'image-changed',
  'image-saved',
  'list-changed',
  'name-suggested',
  'output-changed',
  'preview-failed',
  'preview-ready',
  'preview-started',
  'run-busy',
  'run-cancelled',
  'run-finished',
  'run-started',
];

const AREA = { width: 800, height: 600 };
const RECT = { left: 10, top: 20, width: 800, height: 600 };
const BIG = { width: 4000, height: 3000 };
const LIMITS = zoomLimits(BIG, AREA);
const FIT = toFit(LIMITS);

/** Starts the window on a fresh fake page; every fake is returned for the test. */
function start({ stored = {}, storageThrows = false, refuse = {}, refuseListen = [] } = {}) {
  const doc = fakePage();
  const picture = doc.byId('picture');
  picture.clientWidth = AREA.width;
  picture.clientHeight = AREA.height;
  picture.rect = { ...RECT };
  const tauri = fakeTauri({ refuse, refuseListen });
  const timers = strictTimers();
  const storage = fakeStorage(stored, storageThrows);
  const observers = fakeResizeObservers();
  const logged = [];
  const { listening } = startWindow({
    doc,
    tauri,
    timers,
    storage,
    ResizeObserver: observers.ResizeObserver,
    log: (...args) => logged.push(args),
  });
  return { doc, el: doc.byId, tauri, timers, storage, observers, logged, listening };
}

const image = (key = '7', size = BIG) => ({
  name: 'holiday.jpg',
  size: 2400000,
  width: size.width,
  height: size.height,
  key,
  suggested: 'optimized-holiday',
  extension: '.jpg',
});

/** A new image from the backend; the preview it asks for is answered. */
async function load(w, key = '7', leftOut = 0, notice = null) {
  w.tauri.emit('image-changed', { image: image(key), left_out: leftOut, notice });
  await settle();
}

/** Image 7 loaded and its lossless version 7-1 shown. */
async function loadReady(w) {
  await load(w);
  w.tauri.emit('preview-started', { image: '7', version: 1 });
  w.tauri.emit('preview-ready', {
    image: '7',
    version: 1,
    key: '7-1',
    mode: 'lossless',
    quality: null,
    before: 2400000,
    after: 610000,
    note: null,
  });
  await settle();
}

async function pick(radio) {
  radio.checked = true;
  radio.fire('change');
  await settle();
}

const transformOf = (view) => `translate(${view.x}px, ${view.y}px) scale(${view.scale})`;

function shown(w) {
  const stages = [w.el('stageOriginal'), w.el('stageCompressed')];
  const [first, second] = stages.map((stage) => stage.style.transform);
  assert.equal(first, second, 'both stages show the same view');
  return first;
}

const previews = (w) => w.tauri.calls.filter(([command]) => command === 'preview_image');
const sent = (w, command) => w.tauri.calls.filter(([name]) => name === command);

// --- start ----------------------------------------------------------------------

test('start_shows_the_stored_layout', () => {
  const w = start({ stored: { [LAYOUT_KEY]: 'beside' } });
  assert.equal(w.el('singleBody').classList.contains('-beside'), true);
  assert.equal(w.el('singleBody').classList.contains('-under'), false);
  assert.equal(w.el('layoutBeside').checked, true);
  assert.equal(w.el('layoutUnder').checked, false);
});

test('start_with_a_storage_that_throws_shows_under', () => {
  const w = start({ storageThrows: true });
  assert.equal(w.el('singleBody').classList.contains('-under'), true);
  assert.equal(w.el('singleBody').classList.contains('-beside'), false);
  assert.equal(w.el('layoutUnder').checked, true);
});

test('start_listens_to_every_backend_event', async () => {
  const w = start();
  await w.listening;
  await settle();
  assert.deepEqual([...w.tauri.handlers.keys()].sort(), EVENTS);
  assert.equal(w.el('status').textContent, '');
  assert.deepEqual(w.logged, []);
});

test('a_refused_subscription_says_so_once_and_logs', async () => {
  const w = start({ refuseListen: ['file-done', 'run-busy'] });
  await w.listening;
  await settle();
  const expected = reduce(initialState(), { type: 'command-rejected', payload: 'listen-failed' }).message;
  assert.notEqual(expected, '');
  assert.equal(w.el('status').textContent, expected);
  assert.equal(w.el('status').textWrites, 1);
  assert.equal(w.logged.length, 1);
  assert.equal(w.logged[0][0], 'listen failed');
  assert.equal(w.logged[0][1].length, 2);
});

test('start_shows_single_image_empty', () => {
  const w = start();
  assert.equal(w.el('single').hidden, false);
  assert.equal(w.el('singleDrop').hidden, false);
  assert.equal(w.el('viewSingle').checked, true);
  assert.equal(w.el('settings').parent, w.el('singleSettingsSlot'));
  assert.equal(w.el('chooseImage').getAttribute('aria-disabled'), 'false');
});

test('start_does_not_show_batch_or_the_picture', () => {
  const w = start();
  for (const id of ['drop', 'panel', 'picture', 'singleFile', 'singleTools', 'saveAs', 'sizes', 'singleFoot']) {
    assert.equal(w.el(id).hidden, true, id);
  }
  assert.equal(w.el('switchToBatch').hidden, true);
  // The page's two compare pictures can never start the browser's image drag.
  assert.equal(w.el('imageOriginal').draggable, false);
  assert.equal(w.el('imageCompressed').draggable, false);
  assert.deepEqual(w.tauri.calls, []);
});

// --- backend events ---------------------------------------------------------------

const listed = (count) => ({
  files: Array.from({ length: count }, (_, at) => ({ name: `a${at}.png`, size: 10, skip: null, thumb: null })),
  notice: null,
});

test('a_backend_event_is_dispatched_and_drawn', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  w.tauri.emit('list-changed', listed(1));
  assert.equal(w.el('panel').hidden, false);
  assert.equal(w.el('resultsBody').children.length, 1);
  assert.equal(w.el('run').getAttribute('aria-disabled'), 'false');
});

test('an_event_while_running_leaves_the_list', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  w.tauri.emit('list-changed', listed(1));
  w.tauri.emit('run-started', { total: 1, mode: 'lossless', quality: null });
  w.tauri.emit('list-changed', listed(3));
  assert.equal(w.el('resultsBody').children.length, 1);
  assert.equal(w.el('run').getAttribute('aria-disabled'), 'true');
});

// --- buttons that send a command --------------------------------------------------

test('choose_image_and_choose_other_send_choose_image', () => {
  const w = start();
  w.el('chooseImage').click();
  assert.deepEqual(w.tauri.commands(), ['choose_image']);
  w.el('chooseOther').click();
  assert.deepEqual(w.tauri.commands(), ['choose_image', 'choose_image']);
});

test('choose_image_and_choose_other_send_nothing_during_a_run', () => {
  const w = start();
  w.tauri.emit('run-started', { total: 1, mode: 'lossless', quality: null });
  w.el('chooseImage').click();
  w.el('chooseOther').click();
  assert.deepEqual(w.tauri.calls, []);
});

test('a_refused_command_shows_its_message_and_logs_an_unknown_one', async () => {
  const w = start({ refuse: { choose_image: 'something-odd' } });
  w.el('chooseImage').click();
  await settle();
  assert.equal(w.el('status').textContent, MESSAGES.unknown);
  assert.deepEqual(w.logged, [['choose_image', 'something-odd']]);
});

test('a_busy_refusal_shows_nothing_more_and_logs_nothing', async () => {
  const w = start({ refuse: { choose_image: 'busy' } });
  w.el('chooseImage').click();
  await settle();
  assert.equal(w.el('status').textContent, '');
  assert.deepEqual(w.logged, []);
});

const finishedWithOutput = { optimised: 1, saved_bytes: 10, has_output: true };

test('single_open_folder_opens_after_output', () => {
  const w = start();
  w.tauri.emit('run-finished', finishedWithOutput);
  w.el('singleOpenFolder').click();
  assert.deepEqual(w.tauri.commands(), ['open_output_folder']);
});

test('single_open_folder_sends_nothing_without_output', () => {
  const w = start();
  w.el('singleOpenFolder').click();
  w.tauri.emit('run-finished', { ...finishedWithOutput, has_output: false });
  w.el('singleOpenFolder').click();
  assert.deepEqual(w.tauri.calls, []);
});

const BATCH_BUTTONS = [
  ['chooseFiles', 'choose_files'],
  ['clearList', 'clear_list'],
  ['readdList', 'readd_list'],
  ['chooseOutput', 'choose_output_folder'],
  ['clearOutput', 'clear_output_folder'],
];

test('every_batch_choose_button_sends_its_own_command', async () => {
  for (const [id, command] of BATCH_BUTTONS) {
    const w = start();
    await pick(w.el('viewBatch'));
    const before = w.tauri.calls.length;
    w.el(id).click();
    assert.deepEqual(w.tauri.commands().slice(before), [command], id);
  }
});

test('every_batch_choose_button_sends_nothing_during_a_run', async () => {
  for (const [id] of BATCH_BUTTONS) {
    const w = start();
    await pick(w.el('viewBatch'));
    w.tauri.emit('run-started', { total: 1, mode: 'lossless', quality: null });
    const before = w.tauri.calls.length;
    w.el(id).click();
    assert.equal(w.tauri.calls.length, before, id);
  }
});

test('batch_open_folder_opens_after_output', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  w.tauri.emit('run-finished', finishedWithOutput);
  w.el('openFolder').click();
  assert.deepEqual(w.tauri.commands().slice(-1), ['open_output_folder']);
});

test('batch_open_folder_sends_nothing_without_output', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  const before = w.tauri.calls.length;
  w.el('openFolder').click();
  assert.equal(w.tauri.calls.length, before);
});

test('run_starts_with_the_settings', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  w.tauri.emit('list-changed', listed(2));
  w.el('run').click();
  assert.deepEqual(w.tauri.calls.slice(-1), [['start_run', { settings: { mode: 'lossless', quality: 75 } }]]);
});

test('run_sends_nothing_without_a_list', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  const before = w.tauri.calls.length;
  w.el('run').click();
  w.tauri.emit('list-changed', listed(0));
  w.el('run').click();
  assert.equal(w.tauri.calls.length, before);
});

// --- Save and the name --------------------------------------------------------------

test('save_sends_the_typed_name_and_the_shown_version', async () => {
  const w = start();
  await loadReady(w);
  w.el('saveName').value = 'holiday-small';
  w.el('saveName').fire('input');
  w.el('save').click();
  await settle();
  assert.deepEqual(sent(w, 'save_image'), [['save_image', { name: 'holiday-small', preview: '7-1' }]]);
});

test('save_sends_nothing_without_a_version', async () => {
  const w = start();
  w.el('save').click();
  await load(w);
  w.el('save').click();
  await settle();
  assert.deepEqual(sent(w, 'save_image'), []);
});

test('an_invalid_name_shows_its_problem_and_save_sends_nothing', async () => {
  const w = start();
  await loadReady(w);
  w.el('saveName').value = 'a/b';
  w.el('saveName').fire('input');
  assert.equal(w.el('nameError').hidden, false);
  assert.notEqual(w.el('nameError').textContent, '');
  assert.equal(w.el('saveName').getAttribute('aria-invalid'), 'true');
  w.el('save').click();
  await settle();
  assert.deepEqual(sent(w, 'save_image'), []);
});

// --- the switches -------------------------------------------------------------------

const LEFT_OUT_TWO = 'Single image mode takes one image; 2 others were left out.';

test('switch_to_batch_moves_the_drop_and_shows_batch', async () => {
  const w = start();
  await load(w, '7', 2, LEFT_OUT_TWO);
  assert.equal(w.el('switchToBatch').hidden, false);
  w.el('switchToBatch').click();
  await settle();
  assert.deepEqual(sent(w, 'move_drop_to_batch'), [['move_drop_to_batch', undefined]]);
  assert.equal(w.el('single').hidden, true);
  assert.equal(w.el('drop').hidden, false);
});

test('switch_to_batch_sends_nothing_without_an_offer', async () => {
  const w = start();
  await load(w);
  w.el('switchToBatch').click();
  await settle();
  assert.deepEqual(sent(w, 'move_drop_to_batch'), []);
  assert.equal(w.el('single').hidden, false);
});

test('picking_batch_sends_set_view_and_shows_batch', async () => {
  const w = start();
  w.el('viewSingle').checked = false;
  await pick(w.el('viewBatch'));
  assert.deepEqual(sent(w, 'set_view'), [['set_view', { view: 'batch' }]]);
  assert.equal(w.el('single').hidden, true);
  assert.equal(w.el('drop').hidden, false);
  assert.equal(w.el('settings').parent, w.el('batchSettingsSlot'));
});

test('an_unchecked_radio_or_a_locked_switch_sends_nothing', async () => {
  const w = start();
  // A change that unchecks a radio picks nothing.
  w.el('viewSingle').checked = false;
  w.el('viewSingle').fire('change');
  await settle();
  // During a run the switch is locked: Batch picked, nothing sent, Single image shown again.
  w.tauri.emit('run-started', { total: 1, mode: 'lossless', quality: null });
  await pick(w.el('viewBatch'));
  assert.deepEqual(sent(w, 'set_view'), []);
  assert.equal(w.el('viewSingle').checked, true);
  assert.equal(w.el('viewBatch').checked, false);
  assert.equal(w.el('viewBatch').getAttribute('aria-disabled'), 'true');
  assert.equal(w.el('single').hidden, false);
});

test('picking_beside_stores_it_and_moves_the_settings_beside', async () => {
  const w = start();
  await pick(w.el('layoutBeside'));
  assert.deepEqual(w.storage.writes, [[LAYOUT_KEY, 'beside']]);
  assert.equal(w.el('singleBody').classList.contains('-beside'), true);
  assert.equal(w.el('singleBody').classList.contains('-under'), false);
});

test('an_unchecked_layout_radio_changes_nothing', () => {
  const w = start();
  w.el('layoutUnder').checked = false;
  w.el('layoutUnder').fire('change');
  assert.deepEqual(w.storage.writes, []);
  assert.equal(w.el('singleBody').classList.contains('-under'), true);
});

// --- the settings and the preview triggers -------------------------------------------

test('picking_lossy_asks_for_a_lossy_preview', async () => {
  const w = start();
  await load(w);
  await pick(w.el('modeLossy'));
  assert.deepEqual(previews(w), [
    ['preview_image', { settings: { mode: 'lossless', quality: 75 } }],
    ['preview_image', { settings: { mode: 'lossy', quality: 75 } }],
  ]);
  assert.equal(w.el('qualityRow').hidden, false);
});

test('picking_lossy_in_batch_asks_for_no_preview', async () => {
  const w = start();
  await load(w);
  await pick(w.el('viewBatch'));
  const before = previews(w).length;
  await pick(w.el('modeLossy'));
  assert.equal(previews(w).length, before);
  // Batch as before: Lossy shows the Quality row.
  assert.equal(w.el('qualityRow').hidden, false);
});

async function loadLossy(w) {
  await load(w);
  await pick(w.el('modeLossy'));
  return previews(w).length;
}

test('releasing_the_slider_asks_for_one_preview', async () => {
  const w = start();
  const before = await loadLossy(w);
  w.el('qualityRange').value = '40';
  w.el('qualityRange').fire('input');
  assert.equal(w.el('qualityNumber').value, '40');
  assert.equal(previews(w).length, before);
  w.el('qualityRange').fire('change');
  await settle();
  assert.deepEqual(previews(w).slice(before), [['preview_image', { settings: { mode: 'lossy', quality: 40 } }]]);
});

test('dragging_the_slider_asks_for_nothing', async () => {
  const w = start();
  const before = await loadLossy(w);
  for (const value of ['50', '45', '40']) {
    w.el('qualityRange').value = value;
    w.el('qualityRange').fire('input');
  }
  w.timers.advance(1000);
  await settle();
  assert.equal(previews(w).length, before);
});

test('the_number_field_asks_once_400_ms_after_its_last_input', async () => {
  const w = start();
  const before = await loadLossy(w);
  w.el('qualityNumber').value = '6';
  w.el('qualityNumber').fire('input');
  w.timers.advance(200);
  w.el('qualityNumber').value = '60';
  w.el('qualityNumber').fire('input');
  w.timers.advance(399);
  await settle();
  assert.equal(previews(w).length, before);
  w.timers.advance(1);
  await settle();
  assert.deepEqual(previews(w).slice(before), [['preview_image', { settings: { mode: 'lossy', quality: 60 } }]]);
  w.timers.advance(1000);
  await settle();
  assert.equal(previews(w).length, before + 1);
});

test('an_invalid_number_or_less_than_400_ms_asks_for_nothing', async () => {
  const w = start();
  const before = await loadLossy(w);
  w.el('qualityNumber').value = '30';
  w.el('qualityNumber').fire('input');
  w.timers.advance(399);
  await settle();
  assert.equal(previews(w).length, before);
  w.el('qualityNumber').value = 'abc';
  w.el('qualityNumber').fire('input');
  w.timers.advance(400);
  await settle();
  assert.equal(previews(w).length, before);
  assert.equal(w.el('qualityError').hidden, false);
});

// --- zoom, pan, the divider ---------------------------------------------------------

test('the_zoom_buttons_step_in_out_actual_and_fit', async () => {
  const w = start();
  await load(w);
  assert.equal(shown(w), transformOf(FIT));
  const in1 = step(FIT, 1, LIMITS, BIG, AREA);
  const in2 = step(in1, 1, LIMITS, BIG, AREA);
  const out = step(in2, -1, LIMITS, BIG, AREA);
  const actual = toActual(out, LIMITS, BIG, AREA);
  // Fit, one and two steps in, and 100% all differ, so a swapped pair of
  // buttons fails: In wired as Out would stay at Fit, Actual wired as Fit would
  // give Fit. Out from two steps in is one step in again.
  assert.equal(new Set([FIT, in1, in2, actual].map(transformOf)).size, 4);
  assert.equal(transformOf(out), transformOf(in1));

  w.el('zoomIn').click();
  assert.equal(shown(w), transformOf(in1));
  w.el('zoomIn').click();
  assert.equal(shown(w), transformOf(in2));
  w.el('zoomOut').click();
  assert.equal(shown(w), transformOf(out));
  w.el('zoomActual').click();
  assert.equal(shown(w), transformOf(actual));
  w.el('zoomFit').click();
  assert.equal(shown(w), transformOf(FIT));
});

test('zoom_out_at_fit_stays_at_fit', async () => {
  const w = start();
  await load(w);
  w.el('zoomOut').click();
  assert.equal(shown(w), transformOf(FIT));
  w.el('zoomFit').click();
  assert.equal(shown(w), transformOf(FIT));
  assert.deepEqual(w.tauri.commands(), ['preview_image']);
});

test('the_wheel_zooms_about_the_pointer_and_keeps_the_page_still', async () => {
  const w = start();
  await load(w);
  const event = w.el('picture').fire('wheel', { deltaY: -100, clientX: RECT.left + 200, clientY: RECT.top + 150 });
  assert.equal(event.defaultPrevented, true);
  const expected = zoomAt(FIT, 1.25, { x: 200, y: 150 }, LIMITS, BIG, AREA);
  assert.notEqual(transformOf(expected), transformOf(FIT));
  assert.equal(shown(w), transformOf(expected));
});

test('a_wheel_turn_without_a_vertical_delta_keeps_the_view', async () => {
  const w = start();
  await load(w);
  const event = w.el('picture').fire('wheel', { deltaY: 0, clientX: 200, clientY: 150 });
  assert.equal(event.defaultPrevented, true);
  assert.equal(shown(w), transformOf(FIT));
});

async function zoomedIn(w) {
  await load(w);
  w.el('zoomIn').click();
  w.el('zoomIn').click();
  w.el('zoomIn').click();
  return step(step(step(FIT, 1, LIMITS, BIG, AREA), 1, LIMITS, BIG, AREA), 1, LIMITS, BIG, AREA);
}

test('an_arrow_on_the_picture_pans_and_does_not_scroll', async () => {
  const w = start();
  const view = await zoomedIn(w);
  const event = w.el('picture').fire('keydown', { key: 'ArrowLeft' });
  assert.equal(event.defaultPrevented, true);
  const expected = pan(view, 40, 0, BIG, AREA);
  assert.notEqual(transformOf(expected), transformOf(view));
  assert.equal(shown(w), transformOf(expected));
});

test('other_keys_and_keys_on_the_divider_do_not_pan', async () => {
  const w = start();
  const view = await zoomedIn(w);
  const letter = w.el('picture').fire('keydown', { key: 'a' });
  const onDivider = w.el('picture').fire('keydown', { key: 'ArrowLeft', target: w.el('divider') });
  assert.equal(letter.defaultPrevented, false);
  assert.equal(onDivider.defaultPrevented, false);
  assert.equal(shown(w), transformOf(view));
});

const pointer = (fields) => ({ pointerId: 5, button: 0, clientX: 300, clientY: 300, ...fields });

test('a_primary_drag_on_the_picture_captures_and_pans', async () => {
  const w = start();
  const view = await zoomedIn(w);
  const picture = w.el('picture');
  picture.fire('pointerdown', pointer({ target: w.el('stageOriginal').children[0] }));
  assert.deepEqual(picture.captured, [5]);
  picture.fire('pointermove', pointer({ clientX: 290 }));
  const expected = pan(view, -10, 0, BIG, AREA);
  assert.notEqual(transformOf(expected), transformOf(view));
  assert.equal(shown(w), transformOf(expected));
});

test('a_primary_drag_on_the_handle_moves_the_divider', async () => {
  const w = start();
  await load(w);
  const picture = w.el('picture');
  picture.fire('pointerdown', pointer({ target: w.el('handle'), clientX: RECT.left + 400 }));
  picture.fire('pointermove', pointer({ clientX: RECT.left + 200 }));
  assert.equal(w.el('divider').value, '25');
  assert.equal(picture.style.getPropertyValue('--split'), '25%');
  assert.equal(shown(w), transformOf(FIT));
});

test('a_right_button_drag_captures_nothing_and_pans_nothing', async () => {
  const w = start();
  const view = await zoomedIn(w);
  const picture = w.el('picture');
  for (const button of [1, 2]) {
    picture.fire('pointerdown', pointer({ button }));
    picture.fire('pointermove', pointer({ button, clientX: 250 }));
  }
  picture.fire('pointerdown', pointer({ button: 2, target: w.el('handle') }));
  picture.fire('pointermove', pointer({ button: 2, clientX: RECT.left + 200 }));
  assert.deepEqual(picture.captured, []);
  assert.equal(shown(w), transformOf(view));
  assert.equal(w.el('divider').value, '50');
});

test('the_drag_ends_on_pointerup_and_on_pointercancel', async () => {
  for (const end of ['pointerup', 'pointercancel']) {
    const w = start();
    const view = await zoomedIn(w);
    const picture = w.el('picture');
    picture.fire('pointerdown', pointer({}));
    picture.fire('pointermove', pointer({ clientX: 290 }));
    const panned = pan(view, -10, 0, BIG, AREA);
    assert.equal(shown(w), transformOf(panned), end);
    picture.fire(end, pointer({ clientX: 290 }));
    picture.fire('pointermove', pointer({ clientX: 250 }));
    assert.equal(shown(w), transformOf(panned), end);
  }
});

test('the_divider_input_moves_the_seam', async () => {
  const w = start();
  await load(w);
  const writes = w.el('stageOriginal').style.writes.length;
  w.el('divider').value = '30';
  w.el('divider').fire('input');
  assert.equal(w.el('picture').style.getPropertyValue('--split'), '30%');
  assert.equal(w.el('stageOriginal').style.writes.length, writes);
});

test('an_empty_divider_value_goes_to_the_middle', async () => {
  const w = start();
  await load(w);
  w.el('divider').value = '30';
  w.el('divider').fire('input');
  w.el('divider').value = '';
  w.el('divider').fire('input');
  assert.equal(w.el('picture').style.getPropertyValue('--split'), '50%');
});

test('a_new_image_puts_the_divider_at_50_and_fits', async () => {
  const w = start();
  await zoomedIn(w);
  w.el('divider').value = '30';
  w.el('divider').fire('input');
  await load(w, '8');
  assert.equal(w.el('divider').value, '50');
  assert.equal(w.el('picture').style.getPropertyValue('--split'), '50%');
  assert.equal(shown(w), transformOf(FIT));
});

test('the_same_image_again_keeps_the_divider', async () => {
  const w = start();
  const view = await zoomedIn(w);
  w.el('divider').value = '30';
  w.el('divider').fire('input');
  await load(w, '7', 1, 'Folders go in Batch.');
  assert.equal(w.el('divider').value, '30');
  assert.equal(w.el('picture').style.getPropertyValue('--split'), '30%');
  assert.equal(shown(w), transformOf(view));
});

// --- the pictures -------------------------------------------------------------------

function decodeOf(w, url) {
  const found = w.doc.decodes.find((entry) => entry.element.src === url);
  assert.ok(found, `a decode of ${url}`);
  return found;
}

test('the_original_shows_with_alt_original_and_cannot_be_dragged', async () => {
  const w = start();
  await load(w);
  const decode = decodeOf(w, 'preview://localhost/7');
  decode.resolve();
  await settle();
  const [shownImage] = w.el('stageOriginal').children;
  assert.equal(shownImage, decode.element);
  assert.equal(shownImage.alt, 'Original');
  assert.equal(shownImage.draggable, false);
});

test('a_picture_that_cannot_be_decoded_says_so_and_keeps_the_old_one', async () => {
  const w = start();
  await load(w);
  const old = w.el('stageOriginal').children[0];
  const reason = new Error('EncodingError');
  decodeOf(w, 'preview://localhost/7').reject(reason);
  await settle();
  assert.equal(w.el('status').textContent, 'The picture could not be shown.');
  assert.deepEqual(w.el('stageOriginal').children, [old]);
  assert.deepEqual(w.logged, [['picture', reason]]);
});

test('the_compressed_picture_shows_with_its_alt', async () => {
  const w = start();
  await loadReady(w);
  const decode = decodeOf(w, 'preview://localhost/7-1');
  decode.resolve();
  await settle();
  const [shownImage] = w.el('stageCompressed').children;
  assert.equal(shownImage, decode.element);
  assert.equal(shownImage.alt, 'Compressed, lossless');
  assert.equal(shownImage.draggable, false);
  assert.equal(w.el('status').textContent, '');
});

// --- resizing -----------------------------------------------------------------------

test('a_resize_at_fit_fits_again', async () => {
  const w = start();
  await load(w);
  const picture = w.el('picture');
  picture.clientWidth = 1600;
  picture.clientHeight = 1200;
  w.observers.resize(picture);
  assert.equal(shown(w), transformOf(toFit(zoomLimits(BIG, { width: 1600, height: 1200 }))));
  assert.notEqual(shown(w), transformOf(FIT));
});

test('a_resize_to_a_hidden_area_keeps_the_view', async () => {
  const w = start();
  const view = await zoomedIn(w);
  const picture = w.el('picture');
  picture.clientWidth = 0;
  picture.clientHeight = 0;
  w.observers.resize(picture);
  assert.equal(shown(w), transformOf(view));
});

// --- the drag highlight -------------------------------------------------------------

test('the_drag_highlight_goes_to_the_drop_area_or_the_picture', async () => {
  const w = start();
  w.tauri.emit('drag-state', { over: true });
  assert.equal(w.el('singleDrop').classList.contains('-over'), true);
  await load(w);
  w.tauri.emit('drag-state', { over: true });
  assert.equal(w.el('picture').classList.contains('-over'), true);
  assert.equal(w.el('singleDrop').classList.contains('-over'), false);
});

test('no_drag_no_highlight', async () => {
  const w = start();
  w.tauri.emit('drag-state', { over: true });
  w.tauri.emit('drag-state', { over: false });
  for (const id of ['drop', 'singleDrop', 'picture']) {
    assert.equal(w.el(id).classList.contains('-over'), false, id);
  }
});
