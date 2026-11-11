import test from 'node:test';
import assert from 'node:assert/strict';

import {
  bindCommand,
  buildRow,
  hideKeepingFocus,
  markInvalid,
  setHeads,
  swapSummaryAndSettings,
  syncNumber,
  syncRows,
  syncStatus,
} from '../../ui/table.js';
import { canChoose, canOpenFolder, initialState, reduce } from '../../ui/state.js';

// A recording stand-in for `document`: it creates plain objects, records
// every element it creates, and records any write that would put markup into
// the page. Node has no DOM, so this is what lets the tests see which
// elements a row is made of, which rows were rebuilt, and where focus went.
function fakeDocument() {
  const created = [];
  const markupWrites = [];
  // `strictFocus` makes `focus()` behave as a browser does: an element that is
  // hidden, or sits under a hidden element, cannot take focus. A refused
  // focus is recorded and leaves `activeElement` where it was.
  const doc = { created, markupWrites, activeElement: null, strictFocus: false, refusedFocus: [] };

  class FakeElement {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.classes = [];
      this.text = '';
      this.textWrites = 0;
      this.colSpan = 1;
      this.hidden = false;
      this.parent = null;
      this.attributes = new Map();
      this.listeners = [];
      this.valueText = '';
      this.valueWrites = 0;
      const classes = this.classes;
      this.classList = {
        add: (...names) => classes.push(...names),
        contains: (name) => classes.includes(name),
      };
    }

    // As in the DOM, writing the text replaces the children. Reading it gives
    // the element's own text only (the DOM would add its children's text).
    get textContent() {
      return this.text;
    }

    set textContent(value) {
      this.text = String(value);
      this.textWrites += 1;
      this.children = [];
    }

    get value() {
      return this.valueText;
    }

    set value(text) {
      this.valueText = String(text);
      this.valueWrites += 1;
    }

    append(...nodes) {
      for (const node of nodes) {
        node.parent = this;
      }
      this.children.push(...nodes);
    }

    replaceChildren(...nodes) {
      this.text = '';
      for (const node of nodes) {
        node.parent = this;
      }
      this.children = [...nodes];
    }

    replaceWith(node) {
      const index = this.parent.children.indexOf(this);
      this.parent.children[index] = node;
      node.parent = this.parent;
      this.parent = null;
    }

    contains(node) {
      for (let current = node; current; current = current.parent) {
        if (current === this) {
          return true;
        }
      }
      return false;
    }

    focus() {
      if (doc.strictFocus) {
        for (let current = this; current; current = current.parent) {
          if (current.hidden) {
            doc.refusedFocus.push(this);
            return;
          }
        }
      }
      doc.activeElement = this;
    }

    addEventListener(type, handler) {
      this.listeners.push([type, handler]);
    }

    // What a click on the element runs: the handlers added for 'click'.
    click() {
      for (const [type, handler] of this.listeners) {
        if (type === 'click') {
          handler();
        }
      }
    }

    setAttribute(name, value) {
      this.attributes.set(name, String(value));
    }

    removeAttribute(name) {
      this.attributes.delete(name);
    }

    getAttribute(name) {
      return this.attributes.has(name) ? this.attributes.get(name) : null;
    }

    set innerHTML(value) {
      markupWrites.push(['innerHTML', value]);
    }

    set outerHTML(value) {
      markupWrites.push(['outerHTML', value]);
    }

    insertAdjacentHTML(position, value) {
      markupWrites.push(['insertAdjacentHTML', value]);
    }
  }

  doc.FakeElement = FakeElement;
  doc.createElement = (tagName) => {
    const element = new FakeElement(tagName.toLowerCase());
    created.push(element);
    return element;
  };
  return doc;
}

/** Every element in the tree under `element`, itself included. */
function tree(element) {
  return [element, ...element.children.flatMap(tree)];
}

const MARKUP = '<img src=x onerror=alert(1)>.png';

// text only

test('a_name_with_img_markup_stays_text', () => {
  const doc = fakeDocument();
  const listedRow = buildRow(doc, { kind: 'listed', name: MARKUP, size: 1000, skip: null });
  assert.equal(listedRow.children[0].tagName, 'th');
  assert.equal(listedRow.children[0].textContent, MARKUP);
  const doneRow = buildRow(doc, {
    kind: 'done',
    name: MARKUP,
    before: 1000,
    after: 800,
    millis: 3,
    note: '<b>kept</b>',
  });
  assert.equal(doneRow.children[0].textContent, MARKUP);
  assert.equal(doneRow.children[0].children[0].textContent, '<b>kept</b>');
});

test('it_creates_no_element', () => {
  const doc = fakeDocument();
  for (const row of [
    { kind: 'listed', name: MARKUP, size: 1000, skip: null },
    { kind: 'waiting', name: MARKUP, size: 1000, skip: null },
    { kind: 'compressing', name: MARKUP, size: 1000 },
    { kind: 'skipped', name: MARKUP, reason: '<img src=y>' },
    { kind: 'done', name: MARKUP, before: 1000, after: 800, millis: 3, note: '<img src=z>' },
  ]) {
    const tr = buildRow(doc, row);
    const tags = tree(tr).map((element) => element.tagName);
    assert.equal(tags.includes('img'), false, row.kind);
    assert.equal(tags.every((tag) => ['tr', 'th', 'td', 'span'].includes(tag)), true, `${row.kind}: ${tags}`);
  }
  assert.deepEqual(doc.markupWrites, []);
  assert.equal(doc.created.some((element) => element.tagName === 'img'), false);
});

// row shapes

test('a_list_or_running_row_has_the_size_and_its_text_across_three_columns', () => {
  const doc = fakeDocument();
  const cases = [
    [{ kind: 'listed', name: 'a.png', size: 2048, skip: null }, ''],
    [{ kind: 'waiting', name: 'a.png', size: 2048, skip: null }, 'Waiting'],
    [{ kind: 'compressing', name: 'a.png', size: 2048 }, 'Compressing…'],
  ];
  for (const [row, text] of cases) {
    const tr = buildRow(doc, row);
    assert.deepEqual(tr.children.map((cell) => cell.tagName), ['th', 'td', 'td'], row.kind);
    assert.equal(tr.children[0].scope, 'row');
    assert.equal(tr.children[1].textContent, '2.0 KiB');
    assert.equal(tr.children[2].textContent, text);
    assert.equal(tr.children[2].colSpan, 3);
  }
});

test('a_skipped_or_failed_row_has_its_reason_across_four_columns', () => {
  const doc = fakeDocument();
  const tr = buildRow(doc, { kind: 'failed', name: 'b.png', reason: 'the optimiser crashed' });
  assert.deepEqual(tr.children.map((cell) => cell.tagName), ['th', 'td']);
  assert.equal(tr.children[1].textContent, 'failed: the optimiser crashed');
  assert.equal(tr.children[1].colSpan, 4);
});

test('a_running_row_carries_its_state_as_a_modifier_class', () => {
  const doc = fakeDocument();
  const modifiers = (row) => buildRow(doc, row).classes.filter((name) => name.startsWith('-'));
  assert.deepEqual(modifiers({ kind: 'waiting', name: 'a', size: 1, skip: null }), ['-waiting']);
  assert.deepEqual(modifiers({ kind: 'compressing', name: 'a', size: 1 }), ['-busy']);
  assert.deepEqual(modifiers({ kind: 'listed', name: 'a', size: null, skip: 'Not a PNG, JPG, SVG or WebP file' }), [
    '-skipped',
  ]);
  assert.deepEqual(modifiers({ kind: 'skipped', name: 'a', reason: 'x' }), ['-skipped']);
  assert.deepEqual(modifiers({ kind: 'failed', name: 'a', reason: 'x' }), ['-failed']);
});

test('a_row_that_will_be_compressed_or_is_done_has_no_modifier_class', () => {
  const doc = fakeDocument();
  for (const row of [
    { kind: 'listed', name: 'a', size: 1, skip: null },
    { kind: 'done', name: 'a', before: 2, after: 1, millis: 1 },
  ]) {
    const tr = buildRow(doc, row);
    assert.deepEqual(tr.classes, ['lunaepixResults__row'], row.kind);
  }
});

// notes

test('the_note_is_built_into_its_row', () => {
  const doc = fakeDocument();
  const tr = buildRow(doc, {
    kind: 'done',
    name: 'anim.png',
    before: 512000,
    after: 498000,
    millis: 380,
    note: 'kept lossless: animated PNG',
  });
  const name = tr.children[0];
  assert.equal(name.textContent, 'anim.png');
  assert.equal(name.children.length, 1);
  assert.equal(name.children[0].tagName, 'span');
  assert.deepEqual(name.children[0].classes, ['lunaepixResults__note']);
  assert.equal(name.children[0].textContent, 'kept lossless: animated PNG');
  assert.equal(tr.children.length, 5);
});

test('a_row_without_a_note_builds_no_note_element', () => {
  const doc = fakeDocument();
  const tr = buildRow(doc, { kind: 'done', name: 'a.png', before: 1000, after: 800, millis: 3 });
  assert.equal(tr.children[0].children.length, 0);
  assert.equal(doc.created.some((element) => element.classes.includes('lunaepixResults__note')), false);
});

// column heads

function heads(doc) {
  return {
    caption: new doc.FakeElement('caption'),
    before: new doc.FakeElement('th'),
    after: new doc.FakeElement('th'),
    saved: new doc.FakeElement('th'),
    time: new doc.FakeElement('th'),
  };
}

test('the_list_heads_read_file_size_and_a_hidden_status', () => {
  const doc = fakeDocument();
  const cells = heads(doc);
  setHeads(doc, cells, true);
  assert.equal(cells.caption.textContent, 'Files to optimise');
  assert.equal(cells.before.textContent, 'Size');
  assert.equal(cells.after.colSpan, 3);
  assert.equal(cells.after.children.length, 1);
  assert.equal(cells.after.children[0].textContent, 'Status');
  assert.deepEqual(cells.after.children[0].classes, ['lunaepixResults__hiddenText']);
  assert.equal(cells.saved.hidden, true);
  assert.equal(cells.time.hidden, true);
});

test('the_run_heads_come_back_after_the_list_heads', () => {
  const doc = fakeDocument();
  const cells = heads(doc);
  setHeads(doc, cells, true);
  setHeads(doc, cells, false);
  assert.equal(cells.caption.textContent, 'Optimised files');
  assert.equal(cells.before.textContent, 'Before');
  assert.equal(cells.after.textContent, 'After');
  assert.equal(cells.after.children.length, 0);
  assert.equal(cells.after.colSpan, 1);
  assert.equal(cells.saved.hidden, false);
  assert.equal(cells.time.hidden, false);
});

// ---------------------------------------------------------------------
// Fix round 1: what app.js used to decide on its own, now reachable here.
// ---------------------------------------------------------------------

const listedRow = (name) => ({ kind: 'listed', name, size: 1000, skip: null });
const rowsCreated = (doc) => doc.created.filter((element) => element.tagName === 'tr').length;
const nameOf = (tr) => tr.children[0].textContent;

function shownAfter(doc, body, rows) {
  return syncRows(doc, body, { rows: null, objects: [], elements: [] }, rows);
}

// syncRows

test('sync_rows_replaces_a_changed_row_object', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const rows = [listedRow('a.png'), listedRow('b.png'), listedRow('c.png')];
  const shown = shownAfter(doc, body, rows);
  const [first, second, third] = body.children;
  rows[1] = { kind: 'compressing', name: 'b.png', size: 1000 };
  syncRows(doc, body, shown, rows);
  assert.equal(rowsCreated(doc), 4);
  assert.equal(body.children.length, 3);
  assert.equal(body.children[0], first);
  assert.notEqual(body.children[1], second);
  assert.equal(body.children[1].children[2].textContent, 'Compressing…');
  assert.equal(body.children[2], third);
});

test('sync_rows_appends_a_row_at_a_new_index', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const rows = [listedRow('a.png')];
  const shown = shownAfter(doc, body, rows);
  rows.push({ kind: 'done', name: 'b.png', before: 2, after: 1, millis: 1 });
  syncRows(doc, body, shown, rows);
  assert.deepEqual(body.children.map(nameOf), ['a.png', 'b.png']);
  assert.equal(rowsCreated(doc), 2);
});

test('sync_rows_does_not_rebuild_an_unchanged_row', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const rows = [listedRow('a.png'), listedRow('b.png')];
  const shown = shownAfter(doc, body, rows);
  const before = [...body.children];
  const created = doc.created.length;
  syncRows(doc, body, shown, rows);
  assert.equal(doc.created.length, created);
  assert.equal(body.children.length, before.length);
  for (let index = 0; index < before.length; index += 1) {
    assert.equal(body.children[index], before[index]);
  }
});

test('sync_rows_rebuilds_every_row_for_a_new_rows_array', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const rows = [listedRow('a.png'), listedRow('b.png')];
  const shown = shownAfter(doc, body, rows);
  const before = [...body.children];
  // The same row objects in a new array: a new list or a new run.
  syncRows(doc, body, shown, [...rows]);
  assert.equal(rowsCreated(doc), 4);
  assert.equal(body.children.length, 2);
  for (let index = 0; index < before.length; index += 1) {
    assert.notEqual(body.children[index], before[index]);
  }
  // A shorter new list drops the rows it no longer has.
  syncRows(doc, body, shown, [listedRow('z.png')]);
  assert.deepEqual(body.children.map(nameOf), ['z.png']);
});

// hideKeepingFocus

test('hiding_a_block_that_holds_focus_moves_focus_to_the_fallback_first', () => {
  const doc = fakeDocument();
  const block = new doc.FakeElement('div');
  const button = new doc.FakeElement('button');
  const fallback = new doc.FakeElement('button');
  block.append(button);
  button.focus();
  hideKeepingFocus(doc, block, true, fallback);
  assert.equal(doc.activeElement, fallback);
  assert.equal(block.hidden, true);
});

test('hiding_or_showing_a_block_without_focus_leaves_focus_where_it_is', () => {
  const doc = fakeDocument();
  const block = new doc.FakeElement('div');
  const inside = new doc.FakeElement('button');
  const outside = new doc.FakeElement('button');
  const fallback = new doc.FakeElement('button');
  block.append(inside);
  outside.focus();
  hideKeepingFocus(doc, block, true, fallback);
  assert.equal(doc.activeElement, outside);
  assert.equal(block.hidden, true);
  // Showing never moves focus, even when it is inside.
  inside.focus();
  hideKeepingFocus(doc, block, false, fallback);
  assert.equal(doc.activeElement, inside);
  assert.equal(block.hidden, false);
  // A block that is already hidden cannot hold visible focus; nothing moves.
  block.hidden = true;
  hideKeepingFocus(doc, block, true, fallback);
  assert.equal(doc.activeElement, inside);
});

// syncStatus

test('sync_status_writes_a_new_message_even_when_its_text_repeats', () => {
  const doc = fakeDocument();
  const status = new doc.FakeElement('p');
  const shown = syncStatus(status, 0, { message: 'Busy', messageSeq: 1 });
  assert.equal(shown, 1);
  assert.equal(status.textContent, 'Busy');
  syncStatus(status, shown, { message: 'Busy', messageSeq: 2 });
  assert.equal(status.textWrites, 2);
});

test('sync_status_does_not_write_the_message_already_shown', () => {
  const doc = fakeDocument();
  const status = new doc.FakeElement('p');
  const shown = syncStatus(status, 0, { message: 'Busy', messageSeq: 1 });
  assert.equal(syncStatus(status, shown, { message: 'Busy', messageSeq: 1 }), 1);
  assert.equal(status.textWrites, 1);
});

// syncNumber

test('sync_number_writes_a_text_that_differs', () => {
  const doc = fakeDocument();
  const input = new doc.FakeElement('input');
  input.value = '7.5';
  syncNumber(input, '60');
  assert.equal(input.value, '60');
  assert.equal(input.valueWrites, 2);
});

test('sync_number_does_not_write_the_text_the_field_holds', () => {
  const doc = fakeDocument();
  const input = new doc.FakeElement('input');
  input.value = '7.5';
  syncNumber(input, '7.5');
  assert.equal(input.valueWrites, 1);
});

// markInvalid

test('mark_invalid_flags_the_field_and_ties_the_error_to_it', () => {
  const doc = fakeDocument();
  const input = new doc.FakeElement('input');
  const error = new doc.FakeElement('p');
  error.id = 'qualityError';
  error.hidden = true;
  markInvalid(input, error, true);
  assert.equal(input.getAttribute('aria-invalid'), 'true');
  assert.equal(input.getAttribute('aria-describedby'), 'qualityError');
  assert.equal(error.hidden, false);
});

test('mark_invalid_false_clears_the_flag_and_the_tie', () => {
  const doc = fakeDocument();
  const input = new doc.FakeElement('input');
  const error = new doc.FakeElement('p');
  error.id = 'qualityError';
  markInvalid(input, error, true);
  markInvalid(input, error, false);
  assert.equal(input.getAttribute('aria-invalid'), 'false');
  assert.equal(input.getAttribute('aria-describedby'), null);
  assert.equal(error.hidden, true);
});

// swapSummaryAndSettings (R3): the summary and the settings + Run row take
// each other's place, in an order that never focuses a hidden element.

/**
 * The panel as `index.html` builds it: the head's Clear, the summary (Re-add
 * and Open folder), the settings (a radio) and the Run row (Run). `finished`
 * says which of the two blocks shows. Focus is strict: a hidden target
 * refuses it.
 */
function panelLayout(doc, finished) {
  doc.strictFocus = true;
  const make = (tag) => new doc.FakeElement(tag);
  const parts = {
    panel: make('section'),
    clearList: make('button'),
    summary: make('div'),
    readdList: make('button'),
    openFolder: make('button'),
    settings: make('div'),
    radio: make('input'),
    runRow: make('div'),
    run: make('button'),
  };
  parts.summary.append(parts.readdList, parts.openFolder);
  parts.settings.append(parts.radio);
  parts.runRow.append(parts.run);
  parts.panel.append(parts.clearList, parts.summary, parts.settings, parts.runRow);
  parts.summary.hidden = !finished;
  parts.settings.hidden = finished;
  parts.runRow.hidden = finished;
  return parts;
}

test('focus_on_readd_moves_to_a_run_that_is_visible_at_that_moment', () => {
  for (const focused of ['readdList', 'openFolder']) {
    const doc = fakeDocument();
    const parts = panelLayout(doc, true);
    parts[focused].focus();
    assert.equal(doc.activeElement, parts[focused]);
    swapSummaryAndSettings(doc, parts, false);
    // Run took the focus, and the double never refused it (it was visible).
    assert.equal(doc.activeElement, parts.run, focused);
    assert.deepEqual(doc.refusedFocus, [], focused);
    assert.equal(parts.summary.hidden, true);
    assert.equal(parts.settings.hidden, false);
    assert.equal(parts.runRow.hidden, false);
  }
});

test('the_old_order_loses_focus_on_a_strict_document', () => {
  // The order before R3: the summary hides first, while Run is still hidden.
  const doc = fakeDocument();
  const parts = panelLayout(doc, true);
  parts.readdList.focus();
  hideKeepingFocus(doc, parts.summary, true, parts.run);
  hideKeepingFocus(doc, parts.settings, false, parts.openFolder);
  hideKeepingFocus(doc, parts.runRow, false, parts.openFolder);
  // The double refused the hidden Run: focus is still on the hidden Re-add.
  assert.deepEqual(doc.refusedFocus, [parts.run]);
  assert.equal(doc.activeElement, parts.readdList);
  assert.notEqual(doc.activeElement, parts.run);
});

test('focus_on_run_moves_to_open_folder_when_the_summary_shows', () => {
  const doc = fakeDocument();
  const parts = panelLayout(doc, false);
  parts.run.focus();
  swapSummaryAndSettings(doc, parts, true);
  assert.equal(doc.activeElement, parts.openFolder);
  assert.deepEqual(doc.refusedFocus, []);
  assert.equal(parts.summary.hidden, false);
  assert.equal(parts.settings.hidden, true);
  assert.equal(parts.runRow.hidden, true);
});

test('focus_in_the_settings_moves_to_open_folder_when_the_summary_shows', () => {
  const doc = fakeDocument();
  const parts = panelLayout(doc, false);
  parts.radio.focus();
  swapSummaryAndSettings(doc, parts, true);
  assert.equal(doc.activeElement, parts.openFolder);
  assert.deepEqual(doc.refusedFocus, []);
});

test('focus_on_the_block_that_stays_does_not_move', () => {
  // The summary stays: focus on its buttons stays.
  const finishedDoc = fakeDocument();
  const finishedParts = panelLayout(finishedDoc, true);
  for (const kept of ['readdList', 'openFolder']) {
    finishedParts[kept].focus();
    swapSummaryAndSettings(finishedDoc, finishedParts, true);
    assert.equal(finishedDoc.activeElement, finishedParts[kept], kept);
  }
  // The settings and Run stay: focus on Run and on the radio stays.
  const readyDoc = fakeDocument();
  const readyParts = panelLayout(readyDoc, false);
  for (const kept of ['run', 'radio']) {
    readyParts[kept].focus();
    swapSummaryAndSettings(readyDoc, readyParts, false);
    assert.equal(readyDoc.activeElement, readyParts[kept], kept);
  }
});

test('focus_elsewhere_does_not_move_in_either_direction', () => {
  const doc = fakeDocument();
  const parts = panelLayout(doc, true);
  parts.clearList.focus();
  swapSummaryAndSettings(doc, parts, false);
  assert.equal(doc.activeElement, parts.clearList);
  swapSummaryAndSettings(doc, parts, true);
  assert.equal(doc.activeElement, parts.clearList);
  assert.deepEqual(doc.refusedFocus, []);
  // And with focus nowhere at all.
  const none = fakeDocument();
  const noneParts = panelLayout(none, true);
  swapSummaryAndSettings(none, noneParts, false);
  assert.equal(none.activeElement, null);
});

// bindCommand: the click decision of every button

const FILES = { type: 'list-changed', payload: { files: [{ name: 'a.png', size: 10, skip: null }], notice: null } };
const RUN_STARTED = { type: 'run-started', payload: { total: 1, mode: 'lossless', quality: null } };
const FILE_DONE = {
  type: 'file-done',
  payload: { index: 0, name: 'a.png', status: 'done', before: 10, after: 5, millis: 3 },
};
const RUN_FINISHED = { type: 'run-finished', payload: { optimised: 1, saved_bytes: 5, has_output: true } };
const stateAfter = (...actions) => actions.reduce(reduce, initialState());

test('a_click_on_a_bound_button_sends_its_command_when_the_gate_allows_it', () => {
  const doc = fakeDocument();
  const button = new doc.FakeElement('button');
  let state = stateAfter(FILES, RUN_STARTED, FILE_DONE, RUN_FINISHED);
  assert.equal(state.phase, 'finished');
  const sent = [];
  bindCommand(button, () => state, canChoose, 'readd_list', (command) => sent.push(command));
  button.click();
  assert.deepEqual(sent, ['readd_list']);
  // The gate is asked at the time of the click, with the state at that time.
  state = stateAfter(FILES);
  button.click();
  assert.deepEqual(sent, ['readd_list', 'readd_list']);
});

test('a_click_on_a_bound_button_sends_nothing_when_the_gate_refuses', () => {
  const doc = fakeDocument();
  const button = new doc.FakeElement('button');
  let state = stateAfter(FILES, RUN_STARTED);
  assert.equal(state.phase, 'running');
  const sent = [];
  bindCommand(button, () => state, canChoose, 'readd_list', (command) => sent.push(command));
  button.click();
  assert.deepEqual(sent, []);
  // The same button sends as soon as the state allows it.
  state = stateAfter(FILES, RUN_STARTED, FILE_DONE, RUN_FINISHED);
  button.click();
  assert.deepEqual(sent, ['readd_list']);
  // Another gate: Open folder with no output written yet sends nothing.
  const open = new doc.FakeElement('button');
  bindCommand(open, () => stateAfter(FILES), canOpenFolder, 'open_output_folder', (command) => sent.push(command));
  open.click();
  assert.deepEqual(sent, ['readd_list']);
});

test('a_click_runs_only_the_handler_of_the_button_it_is_bound_to', () => {
  const doc = fakeDocument();
  const readdList = new doc.FakeElement('button');
  const openFolder = new doc.FakeElement('button');
  const state = stateAfter(FILES, RUN_STARTED, FILE_DONE, RUN_FINISHED);
  const sent = [];
  const send = (command) => sent.push(command);
  bindCommand(readdList, () => state, canChoose, 'readd_list', send);
  bindCommand(openFolder, () => state, canOpenFolder, 'open_output_folder', send);
  openFolder.click();
  assert.deepEqual(sent, ['open_output_folder']);
  readdList.click();
  assert.deepEqual(sent, ['open_output_folder', 'readd_list']);
});
