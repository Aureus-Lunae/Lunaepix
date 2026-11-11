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
  thumbSource,
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

    prepend(...nodes) {
      for (const node of nodes) {
        node.parent = this;
      }
      this.children.unshift(...nodes);
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
  assert.equal(doneRow.children[0].children[1].textContent, '<b>kept</b>');
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
  // The square comes first, the note follows the name.
  assert.equal(name.children.length, 2);
  assert.deepEqual(name.children[0].classes, ['lunaepixResults__thumb']);
  assert.equal(name.children[1].tagName, 'span');
  assert.deepEqual(name.children[1].classes, ['lunaepixResults__note']);
  assert.equal(name.children[1].textContent, 'kept lossless: animated PNG');
  assert.equal(tr.children.length, 5);
});

test('a_row_without_a_note_builds_no_note_element', () => {
  const doc = fakeDocument();
  const tr = buildRow(doc, { kind: 'done', name: 'a.png', before: 1000, after: 800, millis: 3 });
  // Only the square: no note.
  assert.deepEqual(tr.children[0].children.map((child) => child.classes), [['lunaepixResults__thumb']]);
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

// ---------------------------------------------------------------------
// Task 7: the thumbnail square before each name.
// ---------------------------------------------------------------------

// A recording `thumbSrc`: what the table asks it, and what it answers.
function thumbDouble() {
  const calls = [];
  const thumbSrc = (key) => {
    calls.push(key);
    return `thumb://localhost/${key}`;
  };
  thumbSrc.calls = calls;
  return thumbSrc;
}

const ROW_KINDS_WITH_KEY = [
  { kind: 'listed', name: 'a.png', size: 1000, skip: null, thumb: '3-0' },
  { kind: 'waiting', name: 'a.png', size: 1000, skip: null, thumb: '3-0' },
  { kind: 'compressing', name: 'a.png', size: 1000, thumb: '3-0' },
  { kind: 'done', name: 'a.png', before: 1000, after: 800, millis: 3, thumb: '3-0' },
  { kind: 'skipped', name: 'a.png', reason: 'x', thumb: '3-0' },
  { kind: 'failed', name: 'a.png', reason: 'x', thumb: '3-0' },
];

const thumbSpanOf = (tr) => tr.children[0].children[0];
const imagesIn = (element) => tree(element).filter((node) => node.tagName === 'img');
const emptyShown = () => ({ rows: null, objects: [], elements: [] });

test('a_row_with_a_thumb_key_starts_its_name_cell_with_the_square_and_the_image', () => {
  const doc = fakeDocument();
  const thumbSrc = thumbDouble();
  for (const row of ROW_KINDS_WITH_KEY) {
    thumbSrc.calls.length = 0;
    const tr = buildRow(doc, row, thumbSrc);
    const name = tr.children[0];
    assert.equal(name.tagName, 'th', row.kind);
    assert.equal(name.scope, 'row', row.kind);
    const square = name.children[0];
    assert.equal(square.tagName, 'span', row.kind);
    assert.deepEqual(square.classes, ['lunaepixResults__thumb'], row.kind);
    assert.equal(square.children.length, 1, row.kind);
    const image = square.children[0];
    assert.equal(image.tagName, 'img', row.kind);
    assert.deepEqual(image.classes, ['lunaepixResults__thumbImage'], row.kind);
    assert.equal(image.src, 'thumb://localhost/3-0', row.kind);
    assert.equal(image.alt, '', row.kind);
    assert.equal(image.width, 32, row.kind);
    assert.equal(image.height, 32, row.kind);
    assert.equal(image.loading, 'lazy', row.kind);
    assert.equal(image.decoding, 'async', row.kind);
    assert.deepEqual(thumbSrc.calls, ['3-0'], row.kind);
  }
});

test('the_image_address_is_what_the_thumb_src_function_answers_for_the_rows_own_key', () => {
  const doc = fakeDocument();
  const addresses = [];
  const thumbSrc = (key) => {
    addresses.push(key);
    return `x:${key}:y`;
  };
  const first = buildRow(doc, { ...ROW_KINDS_WITH_KEY[0], thumb: '7-1' }, thumbSrc);
  const second = buildRow(doc, { ...ROW_KINDS_WITH_KEY[0], thumb: '7-2' }, thumbSrc);
  assert.equal(thumbSpanOf(first).children[0].src, 'x:7-1:y');
  assert.equal(thumbSpanOf(second).children[0].src, 'x:7-2:y');
  assert.deepEqual(addresses, ['7-1', '7-2']);
});

test('a_row_without_a_thumb_key_has_the_empty_square_and_never_asks_for_an_address', () => {
  const doc = fakeDocument();
  const thumbSrc = thumbDouble();
  for (const row of ROW_KINDS_WITH_KEY) {
    const noKey = { ...row, thumb: null };
    const tr = buildRow(doc, noKey, thumbSrc);
    const square = tr.children[0].children[0];
    assert.deepEqual(square.classes, ['lunaepixResults__thumb'], row.kind);
    assert.equal(square.children.length, 0, row.kind);
    assert.equal(imagesIn(tr).length, 0, row.kind);
    // A stored row from before the field existed reads the same.
    const { thumb, ...old } = row;
    const oldTr = buildRow(doc, old, thumbSrc);
    assert.deepEqual(oldTr.children[0].children[0].classes, ['lunaepixResults__thumb'], row.kind);
    assert.equal(imagesIn(oldTr).length, 0, `${row.kind} without the field`);
  }
  assert.deepEqual(thumbSrc.calls, []);
  assert.equal(doc.created.some((element) => element.tagName === 'img'), false);
});

test('the_name_text_stays_in_the_row_header_after_the_square_and_a_note_follows_the_name', () => {
  const doc = fakeDocument();
  const tr = buildRow(
    doc,
    {
      kind: 'done',
      name: MARKUP,
      before: 512000,
      after: 498000,
      millis: 380,
      note: 'kept lossless: animated PNG',
      thumb: '3-4',
    },
    thumbDouble(),
  );
  const name = tr.children[0];
  assert.equal(name.tagName, 'th');
  assert.equal(name.textContent, MARKUP);
  assert.deepEqual(
    name.children.map((child) => child.classes[0]),
    ['lunaepixResults__thumb', 'lunaepixResults__note'],
  );
  // The name is text, never markup: the only image is the thumbnail.
  assert.equal(imagesIn(tr).length, 1);
  assert.deepEqual(doc.markupWrites, []);
});

test('a_row_with_a_key_has_the_same_cells_as_one_without', () => {
  const doc = fakeDocument();
  const shape = (tr) => tr.children.map((cell) => [cell.tagName, cell.colSpan, cell.textContent]);
  for (const row of ROW_KINDS_WITH_KEY) {
    assert.deepEqual(
      shape(buildRow(doc, row, thumbDouble())),
      shape(buildRow(doc, { ...row, thumb: null }, thumbDouble())),
      row.kind,
    );
  }
});

// No flicker: the same image element stays through a status change.

const keyed = (kind, key, extra = {}) => ({ kind, name: 'a.png', size: 1000, skip: null, thumb: key, ...extra });

test('sync_rows_keeps_the_same_image_when_a_row_changes_status', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  const rows = [keyed('waiting', '3-0'), keyed('waiting', '3-1')];
  const shown = syncRows(doc, body, emptyShown(), rows, thumbSrc);
  const [firstImage, secondImage] = body.children.map((tr) => imagesIn(tr)[0]);
  const waitingRow = body.children[0];
  assert.deepEqual(thumbSrc.calls, ['3-0', '3-1']);
  rows[0] = keyed('compressing', '3-0');
  syncRows(doc, body, shown, rows, thumbSrc);
  rows[0] = { kind: 'done', name: 'a.png', before: 1000, after: 800, millis: 3, thumb: '3-0' };
  syncRows(doc, body, shown, rows, thumbSrc);
  const first = body.children[0];
  // The row itself was rebuilt: it is a new element showing the new status.
  assert.notEqual(first, waitingRow);
  assert.equal(first.children.length, 5);
  // The image in it is the old element, and it is the only one in the row.
  assert.equal(imagesIn(first).length, 1);
  assert.equal(thumbSpanOf(first).children[0], firstImage);
  assert.equal(thumbSpanOf(body.children[1]).children[0], secondImage);
  // The address was asked for once per key, not again for a kept image.
  assert.deepEqual(thumbSrc.calls, ['3-0', '3-1']);
});

test('sync_rows_gives_a_row_whose_key_changed_a_new_image', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  const rows = [keyed('waiting', '3-0')];
  const shown = syncRows(doc, body, emptyShown(), rows, thumbSrc);
  const oldImage = imagesIn(body.children[0])[0];
  rows[0] = keyed('compressing', '3-9');
  syncRows(doc, body, shown, rows, thumbSrc);
  const image = imagesIn(body.children[0])[0];
  assert.notEqual(image, oldImage);
  assert.equal(image.src, 'thumb://localhost/3-9');
  assert.deepEqual(thumbSrc.calls, ['3-0', '3-9']);
});

test('sync_rows_builds_an_image_for_a_row_that_gains_a_key_and_none_for_one_that_loses_it', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  const rows = [keyed('waiting', null), keyed('waiting', '3-1')];
  const shown = syncRows(doc, body, emptyShown(), rows, thumbSrc);
  assert.deepEqual(thumbSrc.calls, ['3-1']);
  rows[0] = keyed('compressing', '3-0');
  rows[1] = keyed('compressing', null);
  syncRows(doc, body, shown, rows, thumbSrc);
  assert.equal(imagesIn(body.children[0]).length, 1);
  assert.equal(imagesIn(body.children[0])[0].src, 'thumb://localhost/3-0');
  assert.equal(imagesIn(body.children[1]).length, 0);
  assert.deepEqual(thumbSrc.calls, ['3-1', '3-0']);
});

test('sync_rows_builds_an_image_when_the_old_row_shows_none_although_it_had_a_key', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  // A shown row whose element holds no image (hand-built): nothing to keep.
  const bare = new doc.FakeElement('tr');
  const bareName = new doc.FakeElement('th');
  bareName.append(new doc.FakeElement('span'));
  bare.append(bareName);
  body.append(bare);
  const rows = [keyed('compressing', '3-0')];
  const shown = { rows, objects: [keyed('waiting', '3-0')], elements: [bare] };
  syncRows(doc, body, shown, rows, thumbSrc);
  assert.equal(imagesIn(body.children[0]).length, 1);
  assert.deepEqual(thumbSrc.calls, ['3-0']);
});

test('sync_rows_builds_an_appended_row_with_its_own_image', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  const rows = [keyed('waiting', '3-0')];
  const shown = syncRows(doc, body, emptyShown(), rows, thumbSrc);
  rows.push(keyed('waiting', '3-1'));
  syncRows(doc, body, shown, rows, thumbSrc);
  assert.equal(body.children.length, 2);
  assert.equal(imagesIn(body.children[1])[0].src, 'thumb://localhost/3-1');
  assert.deepEqual(thumbSrc.calls, ['3-0', '3-1']);
});

test('a_new_row_list_keeps_the_images_of_the_keys_it_shares_with_the_old_one', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  // The list becomes a run: a new array, the same files.
  const list = [keyed('listed', '3-0'), keyed('listed', '3-1')];
  const shown = syncRows(doc, body, emptyShown(), list, thumbSrc);
  const [firstImage, secondImage] = body.children.map((tr) => imagesIn(tr)[0]);
  syncRows(doc, body, shown, [keyed('waiting', '3-0'), keyed('waiting', '3-1')], thumbSrc);
  assert.equal(imagesIn(body.children[0])[0], firstImage);
  assert.equal(imagesIn(body.children[1])[0], secondImage);
  assert.deepEqual(thumbSrc.calls, ['3-0', '3-1']);
});

test('a_new_row_list_builds_new_images_for_keys_the_old_one_did_not_have', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  const list = [keyed('listed', '3-0'), keyed('listed', '3-1')];
  const shown = syncRows(doc, body, emptyShown(), list, thumbSrc);
  const [firstImage, secondImage] = body.children.map((tr) => imagesIn(tr)[0]);
  // A new list: another list number, so no key is shared.
  syncRows(doc, body, shown, [keyed('listed', '4-0'), keyed('listed', '4-1'), keyed('listed', null)], thumbSrc);
  const images = body.children.map((tr) => imagesIn(tr)[0]);
  assert.notEqual(images[0], firstImage);
  assert.notEqual(images[1], secondImage);
  assert.equal(images[0].src, 'thumb://localhost/4-0');
  assert.equal(images[1].src, 'thumb://localhost/4-1');
  assert.equal(images[2], undefined);
  assert.deepEqual(thumbSrc.calls, ['3-0', '3-1', '4-0', '4-1']);
});

test('a_kept_image_is_used_by_one_row_only', () => {
  const doc = fakeDocument();
  const body = new doc.FakeElement('tbody');
  const thumbSrc = thumbDouble();
  const shown = syncRows(doc, body, emptyShown(), [keyed('listed', '3-0')], thumbSrc);
  // Two rows with one key cannot come from the backend; the window must
  // still not put one element in two rows.
  syncRows(doc, body, shown, [keyed('waiting', '3-0'), keyed('waiting', '3-0')], thumbSrc);
  const images = body.children.map((tr) => imagesIn(tr)[0]);
  assert.notEqual(images[0], images[1]);
});

// The address of a thumbnail.

test('thumb_source_asks_the_tauri_function_for_the_thumb_scheme', () => {
  const asked = [];
  const convert = (key, scheme) => {
    asked.push([key, scheme]);
    return `converted:${scheme}:${key}`;
  };
  const thumbSrc = thumbSource(convert);
  assert.deepEqual(asked, []);
  assert.equal(thumbSrc('3-0'), 'converted:thumb:3-0');
  assert.equal(thumbSrc('4-12'), 'converted:thumb:4-12');
  assert.deepEqual(asked, [
    ['3-0', 'thumb'],
    ['4-12', 'thumb'],
  ]);
});

test('thumb_source_never_asks_for_the_file_scheme_or_none', () => {
  const schemes = [];
  const thumbSrc = thumbSource((key, scheme) => {
    schemes.push(scheme);
    return key;
  });
  thumbSrc('3-0');
  assert.deepEqual(schemes, ['thumb']);
  assert.equal(schemes.includes('asset'), false);
  assert.equal(schemes.includes(undefined), false);
});
