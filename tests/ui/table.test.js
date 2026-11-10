import test from 'node:test';
import assert from 'node:assert/strict';

import { buildRow, setHeads } from '../../ui/table.js';

// A recording stand-in for `document`: it creates plain objects, records
// every element it creates, and records any write that would put markup into
// the page. Node has no DOM, so this is what lets the tests see which
// elements a row is made of.
function fakeDocument() {
  const created = [];
  const markupWrites = [];

  class FakeElement {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.classes = [];
      this.text = '';
      this.colSpan = 1;
      this.hidden = false;
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
      this.children = [];
    }

    append(...nodes) {
      this.children.push(...nodes);
    }

    replaceChildren(...nodes) {
      this.text = '';
      this.children = [...nodes];
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

  return {
    created,
    markupWrites,
    FakeElement,
    createElement(tagName) {
      const element = new FakeElement(tagName.toLowerCase());
      created.push(element);
      return element;
    },
  };
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
