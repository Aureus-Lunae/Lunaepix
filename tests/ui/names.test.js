import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { nameProblem } from '../../ui/names.js';

// The same fixture src-tauri/src/names.rs is tested against, so the two copies
// of the rules cannot drift apart. See the file's "_about" for the shape.

const MAX_TIMES = 10000;

/**
 * One name of the fixture: a string as it is, or a rule
 * { repeat, times, then } for the text `times` times and then `then`.
 * Anything else throws.
 */
function expand(entry) {
  if (typeof entry === 'string') {
    return entry;
  }
  if (entry === null || typeof entry !== 'object' || Array.isArray(entry)) {
    throw new Error(`not a string or a rule: ${JSON.stringify(entry)}`);
  }
  for (const key of Object.keys(entry)) {
    if (!['repeat', 'times', 'then'].includes(key)) {
      throw new Error(`unknown key ${key}`);
    }
  }
  if (typeof entry.repeat !== 'string') {
    throw new Error('"repeat" is not a string');
  }
  if (!Number.isInteger(entry.times) || entry.times < 0 || entry.times > MAX_TIMES) {
    throw new Error(`"times" is not a whole number from 0 to ${MAX_TIMES}`);
  }
  if ('then' in entry && typeof entry.then !== 'string') {
    throw new Error('"then" is not a string');
  }
  return entry.repeat.repeat(entry.times) + (entry.then ?? '');
}

function loadFixture() {
  const raw = JSON.parse(
    readFileSync(new URL('../fixtures/save-names.json', import.meta.url), 'utf8'),
  );
  return {
    extension: raw.extension,
    accepted: raw.accepted.map(expand),
    refused: Object.entries(raw.refused).map(([problem, list]) => [problem, list.map(expand)]),
  };
}

const TEXTS = {
  Empty: 'Enter a name.',
  ForbiddenCharacter: 'A name cannot contain / \\ : * ? " < > | or control characters.',
  TrailingSpaceOrDot: 'A name cannot end in a space or a dot.',
  Reserved: 'This name is reserved by Windows.',
  TooLong: 'This name is too long.',
};

const codePoints = (text) => [...text].length;
const bytes = (text) => new TextEncoder().encode(text).length;

test('every_accepted_fixture_name_passes', () => {
  const { accepted, extension } = loadFixture();
  for (const stem of accepted) {
    assert.equal(nameProblem(stem, extension), null, `accepted name ${JSON.stringify(stem)}`);
  }
});

test('every_refused_fixture_name_gives_its_problem', () => {
  const { refused, extension } = loadFixture();
  for (const [problem, stems] of refused) {
    assert.ok(problem in TEXTS, `the fixture names a problem that does not exist: ${problem}`);
    for (const stem of stems) {
      assert.equal(
        nameProblem(stem, extension),
        TEXTS[problem],
        `refused name ${JSON.stringify(stem)} under ${problem}`,
      );
    }
  }
});

test('a_reserved_word_inside_a_name_is_accepted', () => {
  for (const stem of ['console', 'com10', 'com0', 'icon', 'con-x']) {
    assert.equal(nameProblem(stem, '.jpg'), null, stem);
  }
});

test('each_problem_has_its_own_text', () => {
  const { refused, extension } = loadFixture();
  const seen = new Map();
  for (const [problem, stems] of refused) {
    const text = nameProblem(stems[0], extension);
    assert.equal(text, TEXTS[problem]);
    seen.set(problem, text);
  }
  assert.deepEqual([...seen.keys()].sort(), Object.keys(TEXTS).sort());
  assert.equal(new Set(seen.values()).size, 5);
});

test('a_good_name_has_no_problem', () => {
  assert.equal(nameProblem('optimized-holiday', '.jpg'), null);
});

test('the_fixture_covers_every_problem_and_both_caps', () => {
  const { accepted, refused, extension } = loadFixture();
  for (const problem of Object.keys(TEXTS)) {
    const entry = refused.find(([name]) => name === problem);
    assert.ok(entry && entry[1].length > 0, `${problem} has no names`);
  }
  const full = (stem) => stem + extension;
  const tooLong = refused.find(([name]) => name === 'TooLong')[1];
  // Only the byte cap refuses it: within 200 code points, over 255 bytes.
  assert.ok(tooLong.some((s) => codePoints(full(s)) <= 200 && bytes(full(s)) > 255));
  // Only the code point cap refuses it: over 200 code points, within 255 bytes.
  assert.ok(tooLong.some((s) => codePoints(full(s)) > 200 && bytes(full(s)) <= 255));
  // Names on the caps are accepted.
  assert.ok(accepted.some((s) => codePoints(full(s)) === 200));
  assert.ok(accepted.some((s) => bytes(full(s)) === 255));
});

test('a_repeat_rule_expands_to_the_text_repeated', () => {
  assert.equal(expand('plain'), 'plain');
  assert.equal(expand({ repeat: 'ab', times: 3 }), 'ababab');
  assert.equal(expand({ repeat: 'ab', times: 0 }), '');
  assert.equal(expand({ repeat: 'x', times: 2, then: 'abc' }), 'xxabc');
});

test('a_malformed_rule_is_an_error', () => {
  const bad = [
    { repeat: 'a' },
    { times: 2 },
    { repeat: 'a', times: -1 },
    { repeat: 'a', times: 1.5 },
    { repeat: 'a', times: 10001 },
    { repeat: 7, times: 2 },
    { repeat: 'a', times: 2, then: 1 },
    { repeat: 'a', times: 2, extra: 'x' },
    42,
    ['a'],
    null,
  ];
  for (const entry of bad) {
    assert.throws(() => expand(entry), JSON.stringify(entry));
  }
});
