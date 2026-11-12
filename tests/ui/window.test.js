// The window itself: ui/window.js's start function, driven through the fake
// page, a fake Tauri, strict timers and a ResizeObserver (fake-window.js).
// Each test fires the events a person or the backend would and asserts what
// the window sends and what the page shows. A storage is handed in too, so
// the tests can show the window never touches it.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { startWindow } from '../../ui/window.js';
import { MESSAGES, initialState, reduce } from '../../ui/state.js';
import { pan, step, toActual, toFit, zoomAt, zoomLimits, zoomText } from '../../ui/zoom.js';
import { fakePage, fakeResizeObservers, fakeStorage, fakeTauri, settle, strictTimers } from './fake-window.js';

/**
 * The fixture's `events` and `commands`, read strictly: exactly those two
 * keys, each a list of distinct, non-empty strings. Anything else throws, so
 * a broken fixture fails the tests instead of checking nothing.
 */
function contractNames(text) {
  const value = JSON.parse(text);
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('the fixture is not an object');
  }
  const keys = Object.keys(value).sort();
  if (keys.join(',') !== 'commands,events') {
    throw new Error(`the fixture's keys are ${keys.join(', ')}, not commands and events`);
  }
  for (const key of keys) {
    const list = value[key];
    if (!Array.isArray(list) || list.some((name) => typeof name !== 'string' || name === '')) {
      throw new Error(`${key} is not a list of names`);
    }
    if (new Set(list).size !== list.length) {
      throw new Error(`${key} has a name twice`);
    }
  }
  return value;
}

/**
 * The event and command names the window and the backend are both tested
 * against (tests/fixtures/contract-names.json; the Rust side reads it in
 * src-tauri/tests/common/contract_names.rs).
 */
function readContract() {
  return contractNames(readFileSync(new URL('../fixtures/contract-names.json', import.meta.url), 'utf8'));
}

/** What differs between the names expected and the names found: "missing x" and "extra y". */
function nameProblems(expected, found) {
  const have = new Set(found);
  const want = new Set(expected);
  return [
    ...expected.filter((name) => !have.has(name)).map((name) => `missing ${name}`),
    ...[...have].filter((name) => !want.has(name)).map((name) => `extra ${name}`),
  ];
}

const AREA = { width: 800, height: 600 };
const RECT = { left: 10, top: 20, width: 800, height: 600 };
const BIG = { width: 4000, height: 3000 };
const LIMITS = zoomLimits(BIG, AREA);
const FIT = toFit(LIMITS);

/** The key Task 9's layout switch stored its choice under; a launch may still find it. */
const OLD_LAYOUT_KEY = 'lunaepix.layout';

/**
 * Starts the window on a fresh fake page; every fake is returned for the
 * test. `resizeObserver: false` starts it without the ResizeObserver class.
 */
function start({ stored = {}, refuse = {}, refuseListen = [], hold = [], resizeObserver = true } = {}) {
  const doc = fakePage();
  const picture = doc.byId('picture');
  picture.clientWidth = AREA.width;
  picture.clientHeight = AREA.height;
  picture.rect = { ...RECT };
  const tauri = fakeTauri({ refuse, refuseListen, hold });
  const timers = strictTimers();
  const storage = fakeStorage(stored);
  const observers = fakeResizeObservers();
  const logged = [];
  const { listening } = startWindow({
    doc,
    tauri,
    timers,
    storage,
    ResizeObserver: resizeObserver ? observers.ResizeObserver : undefined,
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

/** The `preview-ready` of image 7's version `version`, lossless unless `fields` say otherwise. */
const readyOf = (version, fields = {}) => ({
  image: '7',
  version,
  key: `7-${version}`,
  mode: 'lossless',
  quality: null,
  before: 2400000,
  after: 610000,
  note: null,
  ...fields,
});

/** Version `version` of image 7 started and ready (its picture not decoded yet). */
async function versionReady(w, version, fields = {}) {
  w.tauri.emit('preview-started', { image: '7', version });
  w.tauri.emit('preview-ready', readyOf(version, fields));
  await settle();
}

/** Image 7 loaded and its lossless version 7-1 ready; its picture is still decoding. */
async function loadReady(w) {
  await load(w);
  await versionReady(w, 1);
}

/** `loadReady`, and the picture of 7-1 decoded: the compressed side shows it. */
async function loadShown(w) {
  await loadReady(w);
  decodeOf(w, 'preview://localhost/7-1').resolve();
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

/** Whether the column is shown, as every part of the page says it: the modifier, the toggle, the parts. */
function columnShown(w) {
  const folded = w.el('single').classList.contains('-folded');
  const expanded = w.el('columnToggle').getAttribute('aria-expanded');
  const text = w.el('columnToggle').textContent;
  const partsHidden = [w.el('columnBody').hidden, w.el('columnFoot').hidden];
  if (!folded && expanded === 'true' && text === 'Hide settings' && partsHidden.every((hidden) => !hidden)) {
    return true;
  }
  if (folded && expanded === 'false' && text === 'Show settings' && partsHidden.every((hidden) => hidden)) {
    return false;
  }
  throw new Error(`the column is half folded: ${JSON.stringify({ folded, expanded, text, partsHidden })}`);
}

test('a_new_launch_starts_with_the_column_shown', async () => {
  const first = start();
  first.el('columnToggle').click();
  assert.equal(columnShown(first), false);
  // A new launch: nothing of the folded column is carried over.
  const w = start({ stored: { [OLD_LAYOUT_KEY]: 'beside' } });
  assert.equal(columnShown(w), true);
});

test('the_stored_layout_value_is_not_read', async () => {
  const w = start({ stored: { [OLD_LAYOUT_KEY]: 'beside' } });
  await load(w);
  w.el('columnToggle').click();
  w.el('columnToggle').click();
  await settle();
  assert.deepEqual(w.storage.reads, []);
  assert.deepEqual(w.storage.writes, []);
});

test('start_listens_to_every_backend_event', async () => {
  const w = start();
  await w.listening;
  await settle();
  assert.deepEqual(nameProblems(readContract().events, [...w.tauri.handlers.keys()]), []);
  assert.equal(w.tauri.handlers.size, 15);
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
  for (const id of ['drop', 'panel', 'picture', 'singleFile', 'saveAs', 'sizes', 'singleFoot']) {
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
  await loadShown(w);
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
  // Shown, so only the name keeps Save from sending.
  await loadShown(w);
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

// --- the column: Hide settings, Show settings, what sits where ------------------------

test('hide_settings_folds_the_column_and_show_settings_brings_it_back', async () => {
  const w = start();
  await load(w);
  assert.equal(columnShown(w), true);
  w.el('columnToggle').click();
  assert.equal(columnShown(w), false);
  w.el('columnToggle').click();
  assert.equal(columnShown(w), true);
});

test('other_clicks_leave_the_column_as_it_is', async () => {
  const w = start();
  await loadShown(w);
  for (const id of ['zoomIn', 'zoomOut', 'zoomActual', 'zoomFit', 'chooseOther', 'save', 'singleOpenFolder']) {
    w.el(id).click();
    await settle();
    assert.equal(columnShown(w), true, id);
  }
  // Folded, the same clicks leave it folded.
  w.el('columnToggle').click();
  for (const id of ['zoomIn', 'zoomFit', 'chooseOther']) {
    w.el(id).click();
    await settle();
    assert.equal(columnShown(w), false, id);
  }
});

test('hiding_the_column_hides_its_contents_but_not_show_settings', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  assert.equal(w.el('columnBody').hidden, true);
  assert.equal(w.el('columnFoot').hidden, true);
  // Show settings itself stays, and nothing above it is hidden.
  for (let element = w.el('columnToggle'); element; element = element.parent) {
    assert.equal(element.hidden, false, element.id || element.tagName);
  }
});

test('showing_it_again_removes_hidden_from_every_part', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  w.el('columnToggle').click();
  assert.equal(w.el('columnBody').hidden, false);
  assert.equal(w.el('columnFoot').hidden, false);
  // The parts inside show as they did before folding.
  for (const id of ['singleFile', 'settings', 'saveAs', 'sizes', 'statusLine', 'singleFoot']) {
    assert.equal(w.el(id).hidden, false, id);
  }
});

test('hiding_the_column_while_focus_is_inside_moves_focus_to_show_settings', async () => {
  for (const id of ['saveName', 'save']) {
    const w = start();
    await loadShown(w);
    w.doc.strictFocus = true;
    w.el(id).focus();
    assert.equal(w.doc.activeElement, w.el(id), id);
    w.el('columnToggle').click();
    assert.equal(w.doc.activeElement, w.el('columnToggle'), id);
  }
});

test('hiding_it_with_focus_elsewhere_leaves_focus_alone', async () => {
  const w = start();
  await loadShown(w);
  w.doc.strictFocus = true;
  w.el('zoomIn').focus();
  w.el('columnToggle').click();
  assert.equal(columnShown(w), false);
  assert.equal(w.doc.activeElement, w.el('zoomIn'));
});

test('a_load_with_focus_on_choose_image_moves_focus_to_choose_other', async () => {
  const w = start();
  w.doc.strictFocus = true;
  w.el('chooseImage').focus();
  await load(w);
  assert.equal(w.el('singleDrop').hidden, true);
  assert.equal(w.doc.activeElement, w.el('chooseOther'));
});

test('a_load_with_the_column_folded_moves_focus_from_choose_image_to_the_picture', async () => {
  const w = start();
  w.el('columnToggle').click();
  w.doc.strictFocus = true;
  w.el('chooseImage').focus();
  await load(w);
  // Choose other is folded away with the column; the picture took the drop area's place.
  assert.deepEqual(w.doc.refusedFocus, []);
  assert.equal(w.doc.activeElement, w.el('picture'));
});

test('with_an_image_the_settings_status_and_save_sit_in_the_column', async () => {
  const w = start();
  await load(w);
  assert.equal(w.el('settings').parent, w.el('singleSettingsSlot'));
  assert.equal(w.el('statusLine').parent, w.el('singleStatusSlot'));
  for (const id of ['singleFile', 'settings', 'saveAs']) {
    assert.ok(w.el('columnBody').contains(w.el(id)), id);
  }
  for (const id of ['sizes', 'status', 'switchToBatch', 'save', 'singleOpenFolder']) {
    assert.ok(w.el('columnFoot').contains(w.el(id)), id);
  }
  // The size box, then the status line, then Open folder and Save.
  assert.deepEqual(w.el('columnFoot').children, [w.el('sizes'), w.el('singleStatusSlot'), w.el('singleFoot')]);
});

test('in_batch_the_settings_and_the_status_line_are_back_in_batchs_places', async () => {
  const w = start();
  await load(w);
  await pick(w.el('viewBatch'));
  assert.equal(w.el('settings').parent, w.el('batchSettingsSlot'));
  assert.equal(w.el('statusLine').parent, w.el('batchStatusSlot'));
  assert.equal(w.el('columnFoot').contains(w.el('status')), false);
  // One of each: the column holds no copy.
  assert.equal(w.doc.created.filter((element) => element.id === 'status').length, 1);
  // And back again.
  await pick(w.el('viewSingle'));
  assert.equal(w.el('statusLine').parent, w.el('singleStatusSlot'));
  assert.equal(w.el('settings').parent, w.el('singleSettingsSlot'));
});

test('in_the_empty_state_the_column_shows_only_the_settings', () => {
  const w = start();
  assert.equal(w.el('settings').hidden, false);
  assert.ok(w.el('columnBody').contains(w.el('settings')));
  for (const id of ['singleFile', 'saveAs', 'sizes', 'singleFoot']) {
    assert.equal(w.el(id).hidden, true, id);
  }
  // The drop area takes the picture's place.
  assert.equal(w.el('singleDrop').hidden, false);
  assert.equal(w.el('picture').hidden, true);
});

test('with_an_image_loaded_the_column_shows_the_file_save_as_sizes_and_save', async () => {
  const w = start();
  await load(w);
  for (const id of ['singleFile', 'settings', 'saveAs', 'sizes', 'singleFoot']) {
    assert.equal(w.el(id).hidden, false, id);
  }
  assert.equal(w.el('singleDrop').hidden, true);
  assert.equal(w.el('picture').hidden, false);
});

// --- the status line while the column is folded ("Folded note") -------------------

const FOLDER_NOTE = 'Folders go in Batch.';

/** Whether `element` and every element around it are not hidden. */
function nothingAboveHidden(element) {
  for (let current = element; current; current = current.parent) {
    if (current.hidden) {
      return false;
    }
  }
  return true;
}

test('folding_moves_the_status_line_over_the_picture', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  assert.equal(w.el('statusLine').parent, w.el('floatStatusSlot'));
  // Over the picture's place: beside the picture, not in the folded column.
  assert.equal(w.el('floatStatusSlot').parent, w.el('single'));
  assert.equal(w.el('columnFoot').contains(w.el('statusLine')), false);
  assert.equal(nothingAboveHidden(w.el('status')), true);
  // One status line: no copy.
  assert.equal(w.doc.created.filter((element) => element.id === 'status').length, 1);
});

test('unfolding_moves_it_back_to_the_column', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  w.el('columnToggle').click();
  assert.equal(w.el('statusLine').parent, w.el('singleStatusSlot'));
  assert.equal(w.el('floatStatusSlot').children.length, 0);
});

test('a_note_while_folded_shows_over_the_picture_and_is_not_hidden', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  await load(w, '7', 1, FOLDER_NOTE);
  assert.equal(columnShown(w), false);
  assert.equal(w.el('status').textContent, FOLDER_NOTE);
  assert.equal(w.el('statusLine').parent, w.el('floatStatusSlot'));
  assert.equal(nothingAboveHidden(w.el('status')), true);
  assert.equal(nothingAboveHidden(w.el('switchToBatch')), true);
});

test('with_the_column_shown_the_note_stays_in_the_column', async () => {
  const w = start();
  await load(w);
  await load(w, '7', 1, FOLDER_NOTE);
  assert.equal(w.el('status').textContent, FOLDER_NOTE);
  assert.equal(w.el('statusLine').parent, w.el('singleStatusSlot'));
  assert.equal(w.el('floatStatusSlot').children.length, 0);
  assert.equal(w.el('floatStatusSlot').classList.contains('-empty'), true);
});

test('in_batch_the_status_line_is_in_batchs_place_even_when_the_column_was_folded', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  await pick(w.el('viewBatch'));
  assert.equal(w.el('statusLine').parent, w.el('batchStatusSlot'));
  assert.equal(nothingAboveHidden(w.el('status')), true);
  // Back in Single image the column is still folded, so the line floats again.
  await pick(w.el('viewSingle'));
  assert.equal(columnShown(w), false);
  assert.equal(w.el('statusLine').parent, w.el('floatStatusSlot'));
});

test('folded_in_the_empty_state_the_status_line_floats_over_the_drop_area', async () => {
  const w = start();
  w.el('columnToggle').click();
  w.tauri.emit('image-changed', { image: null, left_out: 1, notice: FOLDER_NOTE });
  await settle();
  assert.equal(w.el('singleDrop').hidden, false);
  assert.equal(w.el('picture').hidden, true);
  assert.equal(w.el('statusLine').parent, w.el('floatStatusSlot'));
  // The drop area's neighbour, so the hidden picture does not hide it.
  assert.equal(w.el('floatStatusSlot').parent, w.el('singleDrop').parent);
  assert.equal(w.el('status').textContent, FOLDER_NOTE);
  assert.equal(nothingAboveHidden(w.el('switchToBatch')), true);
});

test('folded_with_an_image_the_status_line_floats_over_the_picture', async () => {
  const w = start();
  w.el('columnToggle').click();
  await load(w, '7', 2, LEFT_OUT_TWO);
  assert.equal(w.el('singleDrop').hidden, true);
  assert.equal(w.el('picture').hidden, false);
  assert.equal(w.el('statusLine').parent, w.el('floatStatusSlot'));
  assert.equal(w.el('floatStatusSlot').parent, w.el('picture').parent);
  assert.equal(nothingAboveHidden(w.el('switchToBatch')), true);
});

test('folding_with_focus_on_switch_to_batch_keeps_focus_on_it', async () => {
  const w = start();
  await load(w, '7', 2, LEFT_OUT_TWO);
  w.doc.strictFocus = true;
  w.el('switchToBatch').focus();
  w.el('columnToggle').click();
  assert.equal(w.el('statusLine').parent, w.el('floatStatusSlot'));
  assert.deepEqual(w.doc.refusedFocus, []);
  assert.equal(w.doc.activeElement, w.el('switchToBatch'));
});

test('folding_with_focus_in_the_column_body_moves_focus_to_show_settings', async () => {
  const w = start();
  await load(w, '7', 2, LEFT_OUT_TWO);
  w.doc.strictFocus = true;
  w.el('modeLossy').focus();
  w.el('columnToggle').click();
  assert.equal(w.doc.activeElement, w.el('columnToggle'));
});

test('the_floating_status_line_has_its_surface_with_a_note', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  await load(w, '7', 1, FOLDER_NOTE);
  assert.equal(w.el('floatStatusSlot').classList.contains('-empty'), false);
});

test('the_floating_status_line_has_no_surface_while_it_is_empty', async () => {
  const w = start();
  await load(w);
  w.el('columnToggle').click();
  assert.equal(w.el('status').textContent, '');
  assert.equal(w.el('floatStatusSlot').classList.contains('-empty'), true);
  // Never hidden: the live region stays in the page for the next note.
  assert.equal(nothingAboveHidden(w.el('status')), true);
  // A note, then unfolding: the line leaves, and the empty slot has no surface.
  await load(w, '7', 1, FOLDER_NOTE);
  w.el('columnToggle').click();
  assert.equal(w.el('floatStatusSlot').classList.contains('-empty'), true);
});

test('single_image_marks_the_main_area_single', () => {
  const w = start();
  assert.equal(w.el('main').classList.contains('-single'), true);
  assert.equal(w.el('main').classList.contains('-batch'), false);
});

test('batch_marks_the_main_area_batch', async () => {
  const w = start();
  await pick(w.el('viewBatch'));
  assert.equal(w.el('main').classList.contains('-batch'), true);
  assert.equal(w.el('main').classList.contains('-single'), false);
});

// --- the header's height ----------------------------------------------------------

const headerHeight = (w) => w.doc.documentElement.style.getPropertyValue('--headerHeight');

test('start_observes_the_header_and_writes_its_height', () => {
  const w = start();
  const header = w.el('header');
  header.rect = { left: 0, top: 0, width: 760, height: 64 };
  w.observers.resize(header);
  assert.equal(headerHeight(w), '64px');
  // The header wraps on a narrow window: taller.
  header.rect = { left: 0, top: 0, width: 360, height: 96 };
  w.observers.resize(header);
  assert.equal(headerHeight(w), '96px');
  assert.deepEqual(w.doc.documentElement.style.writes, [
    ['--headerHeight', '64px'],
    ['--headerHeight', '96px'],
  ]);
});

test('a_header_resize_to_zero_writes_nothing', () => {
  const w = start();
  const header = w.el('header');
  header.rect = { left: 0, top: 0, width: 0, height: 0 };
  w.observers.resize(header);
  assert.deepEqual(w.doc.documentElement.style.writes, []);
});

test('start_without_a_ResizeObserver_writes_no_height_and_does_not_throw', async () => {
  const w = start({ resizeObserver: false });
  assert.deepEqual(w.observers.observers, []);
  assert.deepEqual(w.doc.documentElement.style.writes, []);
  // The window still works.
  await load(w);
  assert.equal(shown(w), transformOf(FIT));
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

test('the_zoom_percentage_follows_the_buttons_and_the_wheel', async () => {
  const w = start();
  await load(w);
  const value = () => w.el('zoomValue').textContent;
  // Fit for 4000 × 3000 in 800 × 600 is 20%.
  assert.equal(value(), '20%');
  assert.equal(value(), zoomText(FIT));
  const in1 = step(FIT, 1, LIMITS, BIG, AREA);
  w.el('zoomIn').click();
  assert.equal(value(), zoomText(in1));
  assert.equal(value(), '25%');
  w.el('zoomOut').click();
  assert.equal(value(), '20%');
  w.el('zoomActual').click();
  assert.equal(value(), '100%');
  w.el('zoomFit').click();
  assert.equal(value(), '20%');
  w.el('picture').fire('wheel', { deltaY: -100, clientX: RECT.left + 200, clientY: RECT.top + 150 });
  assert.equal(value(), zoomText(zoomAt(FIT, 1.25, { x: 200, y: 150 }, LIMITS, BIG, AREA)));
  assert.notEqual(value(), '20%');
  assert.equal(w.el('zoomValue').hidden, false);
});

test('a_zoom_that_does_not_change_the_view_keeps_the_text', async () => {
  const w = start();
  await load(w);
  const writes = w.el('zoomValue').textWrites;
  assert.equal(w.el('zoomValue').textContent, '20%');
  // At Fit: Zoom out and Fit change nothing, and nothing is written again.
  w.el('zoomOut').click();
  w.el('zoomFit').click();
  w.el('picture').fire('wheel', { deltaY: 0, clientX: 200, clientY: 150 });
  assert.equal(w.el('zoomValue').textContent, '20%');
  assert.equal(w.el('zoomValue').textWrites, writes);
});

test('a_press_on_the_zoom_controls_starts_no_drag', async () => {
  const w = start();
  const view = await zoomedIn(w);
  const picture = w.el('picture');
  for (const target of [w.el('zoomIn'), w.el('zoomValue'), w.el('zoomControls')]) {
    picture.fire('pointerdown', pointer({ target }));
    picture.fire('pointermove', pointer({ clientX: 250 }));
    picture.fire('pointerup', pointer({ clientX: 250 }));
  }
  assert.deepEqual(picture.captured, []);
  assert.equal(shown(w), transformOf(view));
  // Control: a press on the picture still drags, and one on the handle moves the divider.
  picture.fire('pointerdown', pointer({ target: w.el('stageOriginal').children[0] }));
  picture.fire('pointermove', pointer({ clientX: 290 }));
  assert.deepEqual(picture.captured, [5]);
  assert.equal(shown(w), transformOf(pan(view, -10, 0, BIG, AREA)));
  picture.fire('pointerup', pointer({}));
  picture.fire('pointerdown', pointer({ target: w.el('handle') }));
  picture.fire('pointermove', pointer({ clientX: RECT.left + 200 }));
  assert.equal(w.el('divider').value, '25');
});

test('the_wheel_over_the_zoom_controls_does_not_zoom', async () => {
  const w = start();
  await load(w);
  for (const target of [w.el('zoomFit'), w.el('zoomControls')]) {
    const event = w.el('picture').fire('wheel', { deltaY: -100, clientX: 400, clientY: 580, target });
    assert.equal(event.defaultPrevented, false);
  }
  assert.equal(shown(w), transformOf(FIT));
  // Control: the wheel on the picture zooms.
  const onPicture = w.el('picture').fire('wheel', { deltaY: -100, clientX: 400, clientY: 300 });
  assert.equal(onPicture.defaultPrevented, true);
  assert.notEqual(shown(w), transformOf(FIT));
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

// --- one preview at a time, busy retried ---------------------------------------------

/** A key press on the Quality slider: the browser fires `input` and `change` for each. */
function arrowTo(w, value) {
  w.el('qualityRange').value = value;
  w.el('qualityRange').fire('input');
  w.el('qualityRange').fire('change');
}

/** Answers held call `index` (resolve) and lets it be handled. */
async function answer(w, index) {
  w.tauri.held[index].resolve(null);
  await settle();
}

/** Refuses held call `index` with `reason`, the backend's `run-busy` first for busy. */
async function refuseHeld(w, index, reason) {
  if (reason === 'busy') {
    w.tauri.emit('run-busy', null);
  }
  w.tauri.held[index].reject(reason);
  await settle();
}

test('holding_the_quality_arrow_key_during_a_flight_sends_one_more_preview_with_the_last_quality', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  await answer(w, 0);
  await pick(w.el('modeLossy'));
  for (const value of ['74', '73', '72']) {
    arrowTo(w, value);
  }
  await settle();
  assert.equal(previews(w).length, 2, 'nothing more while lossy 75 is in flight');
  await answer(w, 1);
  assert.deepEqual(previews(w).slice(2), [['preview_image', { settings: { mode: 'lossy', quality: 72 } }]]);
  await answer(w, 2);
  assert.equal(previews(w).length, 3);
});

test('a_mode_click_during_a_flight_is_sent_when_the_flight_settles', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  await pick(w.el('modeLossy'));
  assert.equal(previews(w).length, 1);
  await answer(w, 0);
  assert.deepEqual(previews(w), [
    ['preview_image', { settings: { mode: 'lossless', quality: 75 } }],
    ['preview_image', { settings: { mode: 'lossy', quality: 75 } }],
  ]);
  // With no flight going, a change is sent at once.
  await answer(w, 1);
  arrowTo(w, '60');
  assert.equal(previews(w).length, 3);
});

test('a_busy_preview_is_retried_and_the_status_line_stays_quiet', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  await refuseHeld(w, 0, 'busy');
  assert.equal(w.el('status').textContent, '');
  w.timers.advance(200);
  await settle();
  assert.equal(previews(w).length, 2);
  await answer(w, 1);
  assert.equal(w.el('status').textContent, '');
  assert.deepEqual(w.logged, []);
});

test('a_preview_that_stays_busy_stops_after_three_retries_and_says_busy', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  for (let index = 0; index < 4; index += 1) {
    await refuseHeld(w, index, 'busy');
    if (index < 3) {
      assert.equal(w.el('status').textContent, '', `retry ${index + 1}`);
    }
    w.timers.advance(200);
    await settle();
  }
  assert.equal(previews(w).length, 4);
  assert.equal(w.el('status').textContent, MESSAGES.busy);
  w.timers.advance(10000);
  await settle();
  assert.equal(previews(w).length, 4);
});

test('an_invalid_settings_refusal_is_not_retried', async () => {
  const w = start({ refuse: { preview_image: 'invalid-settings' } });
  await load(w);
  assert.equal(w.el('status').textContent, MESSAGES.invalidSettings);
  w.timers.advance(10000);
  await settle();
  assert.equal(previews(w).length, 1);
});

test('a_retry_after_a_new_image_sends_one_request', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  await refuseHeld(w, 0, 'busy');
  // Image 8 loads while the retry waits: its own ask adds nothing.
  await load(w, '8');
  assert.equal(previews(w).length, 1);
  w.timers.advance(200);
  await settle();
  assert.equal(previews(w).length, 2);
  await answer(w, 1);
  w.timers.advance(10000);
  await settle();
  assert.equal(previews(w).length, 2);
});

test('a_preview_accepted_after_a_switch_to_batch_is_asked_again_on_return', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  await pick(w.el('viewBatch'));
  // The switch stopped it; its acceptance arrives late.
  await answer(w, 0);
  await pick(w.el('viewSingle'));
  assert.deepEqual(previews(w), [
    ['preview_image', { settings: { mode: 'lossless', quality: 75 } }],
    ['preview_image', { settings: { mode: 'lossless', quality: 75 } }],
  ]);
});

test('a_preview_whose_version_is_ready_is_not_asked_again_after_batch_and_back', async () => {
  const w = start({ hold: ['preview_image'] });
  await load(w);
  await answer(w, 0);
  await versionReady(w, 1);
  await pick(w.el('viewBatch'));
  await pick(w.el('viewSingle'));
  assert.equal(previews(w).length, 1);
});

// --- what the page shows: Save, "Loading picture…", the alt -------------------------

test('save_waits_for_the_ready_picture_to_show', async () => {
  const w = start();
  await loadReady(w);
  assert.equal(w.el('save').getAttribute('aria-disabled'), 'true');
  w.el('save').click();
  await settle();
  assert.deepEqual(sent(w, 'save_image'), []);
  decodeOf(w, 'preview://localhost/7-1').resolve();
  await settle();
  assert.equal(w.el('save').getAttribute('aria-disabled'), 'false');
  w.el('save').click();
  await settle();
  assert.deepEqual(sent(w, 'save_image'), [['save_image', { name: 'optimized-holiday', preview: '7-1' }]]);
});

test('save_stays_off_after_the_ready_picture_failed_until_a_later_version_shows', async () => {
  const w = start();
  await loadReady(w);
  decodeOf(w, 'preview://localhost/7-1').reject(new Error('EncodingError'));
  await settle();
  assert.equal(w.el('status').textContent, MESSAGES.pictureFailed);
  assert.equal(w.el('save').getAttribute('aria-disabled'), 'true');
  w.el('save').click();
  await settle();
  assert.deepEqual(sent(w, 'save_image'), []);
  await versionReady(w, 2);
  assert.equal(w.el('save').getAttribute('aria-disabled'), 'true');
  decodeOf(w, 'preview://localhost/7-2').resolve();
  await settle();
  assert.equal(w.el('save').getAttribute('aria-disabled'), 'false');
});

test('loading_picture_shows_until_the_ready_picture_decodes', async () => {
  const w = start();
  await load(w);
  assert.equal(w.el('pictureBusy').textContent, 'Loading picture…');
  decodeOf(w, 'preview://localhost/7').resolve();
  await settle();
  assert.equal(w.el('pictureBusy').hidden, true);
  await versionReady(w, 1);
  assert.equal(w.el('pictureBusy').textContent, 'Loading picture…');
  assert.equal(w.el('pictureBusy').hidden, false);
  decodeOf(w, 'preview://localhost/7-1').resolve();
  await settle();
  assert.equal(w.el('pictureBusy').hidden, true);
  // A version whose picture cannot be decoded is not loading any more.
  await versionReady(w, 2);
  decodeOf(w, 'preview://localhost/7-2').reject(new Error('EncodingError'));
  await settle();
  assert.equal(w.el('pictureBusy').hidden, true);
});

test('the_alt_stays_the_shown_versions_while_a_newer_one_decodes', async () => {
  const w = start();
  await loadShown(w);
  const shownImage = () => w.el('stageCompressed').children[0];
  assert.equal(shownImage().alt, 'Compressed, lossless');
  await versionReady(w, 2, { mode: 'lossy', quality: 60 });
  assert.equal(shownImage().alt, 'Compressed, lossless');
  decodeOf(w, 'preview://localhost/7-2').resolve();
  await settle();
  assert.equal(shownImage().alt, 'Compressed, lossy quality 60');
});

test('an_older_pictures_failure_after_a_newer_version_says_nothing', async () => {
  const w = start();
  await loadReady(w);
  await versionReady(w, 2);
  decodeOf(w, 'preview://localhost/7-1').reject(new Error('NotFound'));
  await settle();
  assert.equal(w.el('status').textContent, '');
  // Control: the current version's failure does say so.
  decodeOf(w, 'preview://localhost/7-2').reject(new Error('EncodingError'));
  await settle();
  assert.equal(w.el('status').textContent, MESSAGES.pictureFailed);
});

// --- the save text for the original's folder ----------------------------------------

test('a_save_with_no_folder_that_cannot_write_says_choose_a_folder', async () => {
  const w = start({ refuse: { save_image: 'output-missing' } });
  await loadShown(w);
  w.el('save').click();
  await settle();
  assert.equal(w.el('status').textContent, "The original's folder cannot be written to. Choose a folder in Save to.");
});

test('a_save_to_a_chosen_folder_that_is_gone_keeps_todays_text', async () => {
  const w = start({ refuse: { save_image: 'output-missing' } });
  w.tauri.emit('output-changed', { folder: 'Pictures' });
  await loadShown(w);
  w.el('save').click();
  await settle();
  assert.equal(w.el('status').textContent, MESSAGES.outputMissing);
  // Batch's run with no folder chosen keeps it too.
  const b = start({ refuse: { start_run: 'output-missing' } });
  await pick(b.el('viewBatch'));
  b.tauri.emit('list-changed', listed(1));
  b.el('run').click();
  await settle();
  assert.equal(b.el('status').textContent, MESSAGES.outputMissing);
});

// --- the names both sides share -------------------------------------------------------

test('the_window_listens_to_exactly_the_fixtures_events', async () => {
  const w = start();
  await w.listening;
  assert.deepEqual(nameProblems(readContract().events, [...w.tauri.handlers.keys()]), []);
});

test('the_window_sends_exactly_the_fixtures_commands', async () => {
  const w = start();
  w.el('chooseImage').click();
  await load(w, '7', 2, LEFT_OUT_TWO);
  await versionReady(w, 1);
  decodeOf(w, 'preview://localhost/7-1').resolve();
  await settle();
  w.el('save').click();
  await settle();
  w.el('switchToBatch').click();
  await settle();
  w.el('chooseFiles').click();
  w.tauri.emit('list-changed', listed(1));
  w.el('chooseOutput').click();
  w.tauri.emit('output-changed', { folder: 'small' });
  w.el('clearOutput').click();
  w.el('run').click();
  w.tauri.emit('run-started', { total: 1, mode: 'lossless', quality: null });
  w.tauri.emit('run-finished', finishedWithOutput);
  w.el('readdList').click();
  w.el('openFolder').click();
  w.el('clearList').click();
  await pick(w.el('viewSingle'));
  await settle();
  assert.deepEqual(nameProblems(readContract().commands, w.tauri.commands()), []);
  assert.equal(new Set(w.tauri.commands()).size, 12);
});

test('a_names_list_missing_or_adding_a_name_is_flagged', () => {
  const { events, commands } = readContract();
  assert.deepEqual(nameProblems(events, events.slice(1)), [`missing ${events[0]}`]);
  assert.deepEqual(nameProblems(commands, [...commands, 'delete_everything']), ['extra delete_everything']);
  assert.deepEqual(nameProblems(['a', 'b'], ['b', 'c']), ['missing a', 'extra c']);
  // Control: the same names in another order and twice are no problem.
  assert.deepEqual(nameProblems(commands, [...commands].reverse().concat(commands[0])), []);
});

test('the_contract_fixture_is_read_strictly', () => {
  const real = readContract();
  assert.equal(real.events.length, 15);
  assert.equal(real.commands.length, 12);
  for (const bad of [
    'not json',
    '[]',
    'null',
    '{"events": [], "commands": [], "extra": []}',
    '{"events": []}',
    '{"events": ["a", 7], "commands": []}',
    '{"events": ["a", ""], "commands": []}',
    '{"events": ["a", "a"], "commands": []}',
    '{"events": "a", "commands": []}',
  ]) {
    assert.throws(() => contractNames(bad), undefined, bad);
  }
  // Control: a well-formed text is read.
  assert.deepEqual(contractNames('{"commands": ["x"], "events": ["y"]}'), { commands: ['x'], events: ['y'] });
});
