import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { nameProblem } from '../../ui/names.js';

// The same fixture src-tauri/src/names.rs is tested against, so the two copies
// of the rules cannot drift apart. See the file's "_about" for the shape.

const MAX_TIMES = 10000;

/**
 * One name of the fixture: a string as it is, or a rule
 * { before, repeat, times, then } (`before` and `then` optional) for `before`,
 * then the text `times` times, then `then`. Anything else throws.
 */
function expand(entry) {
  if (typeof entry === 'string') {
    return entry;
  }
  if (entry === null || typeof entry !== 'object' || Array.isArray(entry)) {
    throw new Error(`not a string or a rule: ${JSON.stringify(entry)}`);
  }
  for (const key of Object.keys(entry)) {
    if (!['before', 'repeat', 'times', 'then'].includes(key)) {
      throw new Error(`unknown key ${key}`);
    }
  }
  if (typeof entry.repeat !== 'string') {
    throw new Error('"repeat" is not a string');
  }
  if (!Number.isInteger(entry.times) || entry.times < 0 || entry.times > MAX_TIMES) {
    throw new Error(`"times" is not a whole number from 0 to ${MAX_TIMES}`);
  }
  for (const key of ['before', 'then']) {
    if (key in entry && typeof entry[key] !== 'string') {
      throw new Error(`"${key}" is not a string`);
    }
  }
  return (entry.before ?? '') + entry.repeat.repeat(entry.times) + (entry.then ?? '');
}

// A number is a whole number written as digits only: `2`, not `2.0` or `2e0`.
// JSON.parse cannot tell them apart afterwards, so the reviver looks at the
// source text of each number (the Rust loader refuses the same spellings).
const PLAIN_INTEGER = /^(0|[1-9][0-9]*)$/;

function parseFixture(text) {
  return JSON.parse(text, (key, value, context) => {
    if (typeof value === 'number' && !PLAIN_INTEGER.test(context.source)) {
      throw new Error(`"${key}" is written as ${context.source}, not as a plain integer`);
    }
    return value;
  });
}

function fixtureText() {
  return readFileSync(new URL('../fixtures/save-names.json', import.meta.url), 'utf8');
}

function loadFixture() {
  const raw = parseFixture(fixtureText());
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

test('a_rule_may_start_with_a_before_text', () => {
  assert.equal(expand({ before: 'con.', repeat: 'x', times: 2, then: '!' }), 'con.xx!');
  assert.throws(() => expand({ before: 1, repeat: 'a', times: 2 }));
});

const rule = (written) => `{"repeat": "a", "times": ${written}}`;

test('a_times_written_as_a_plain_integer_is_accepted', () => {
  assert.equal(expand(parseFixture(rule('2'))), 'aa');
  assert.equal(expand(parseFixture(rule('0'))), '');
});

test('a_times_written_with_a_decimal_point_or_exponent_is_an_error', () => {
  // The Rust loader refuses these spellings too (names.rs).
  for (const written of ['2.0', '2e0', '1e1', '1.5', '-0', '-1']) {
    assert.throws(() => expand(parseFixture(rule(written))), written);
  }
});

// The characters of `text` that are not printable ASCII or a line break.
const notPrintableAscii = (text) => [...text].filter((c) => !/^[ -~\n]$/.test(c));

test('the_fixture_file_is_printable_ascii_only', () => {
  // Every other character is a \uXXXX escape, so an editor cannot change an
  // invisible or look-alike one unnoticed.
  assert.deepEqual(notPrintableAscii(fixtureText()), []);
});

test('a_character_outside_printable_ascii_in_a_fixture_text_is_found', () => {
  assert.deepEqual(notPrintableAscii('a\u00E9\u00A0\uFEFF\t"x"\n'), ['\u00E9', '\u00A0', '\uFEFF', '\t']);
  assert.deepEqual(notPrintableAscii('{"a": "\\u00E9"}\n'), []);
});

test('the_fixture_keeps_composed_and_decomposed_accents_apart', () => {
  const { accepted } = loadFixture();
  const composed = 'vakantie-caf\u00E9';
  const decomposed = 'vakantie-cafe\u0301';
  assert.notEqual(composed, decomposed);
  assert.ok(accepted.includes(composed));
  assert.ok(accepted.includes(decomposed));
});
