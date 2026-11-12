import test from 'node:test';
import assert from 'node:assert/strict';

import { readLayout, writeLayout } from '../../ui/prefs.js';

const KEY = 'lunaepix.layout';

/** A storage that records every call and keeps values in a map. */
function fakeStorage(initial = {}) {
  const data = new Map(Object.entries(initial));
  const calls = [];
  return {
    calls,
    data,
    getItem(key) {
      calls.push(['get', key]);
      return data.has(key) ? data.get(key) : null;
    },
    setItem(key, value) {
      calls.push(['set', key, value]);
      data.set(key, value);
    },
  };
}

test('layout is under by default', () => {
  const storage = fakeStorage();
  assert.equal(readLayout(storage), 'under');
  // It asked for the right key.
  assert.deepEqual(storage.calls, [['get', KEY]]);
});

test('a stored beside is used', () => {
  assert.equal(readLayout(fakeStorage({ [KEY]: 'beside' })), 'beside');
  assert.equal(readLayout(fakeStorage({ [KEY]: 'under' })), 'under');
  // A value under another key is not the choice.
  assert.equal(readLayout(fakeStorage({ other: 'beside' })), 'under');
});

test('a throwing read a throwing write and a junk value give under without throwing', () => {
  const throwingRead = {
    getItem() {
      throw new Error('access denied');
    },
  };
  assert.equal(readLayout(throwingRead), 'under');
  assert.equal(readLayout(null), 'under');
  assert.equal(readLayout(undefined), 'under');
  for (const junk of ['Beside', 'sideways', '', 5]) {
    assert.equal(readLayout(fakeStorage({ [KEY]: junk })), 'under', `junk ${JSON.stringify(junk)}`);
  }
  // A valid value is still read.
  assert.equal(readLayout(fakeStorage({ [KEY]: 'beside' })), 'beside');

  const throwingWrite = {
    setItem() {
      throw new Error('quota exceeded');
    },
  };
  assert.doesNotThrow(() => writeLayout(throwingWrite, 'beside'));
  assert.doesNotThrow(() => writeLayout(null, 'beside'));
  assert.doesNotThrow(() => writeLayout(undefined, 'under'));
});

test('write stores only under or beside', () => {
  const storage = fakeStorage();
  writeLayout(storage, 'beside');
  writeLayout(storage, 'under');
  assert.deepEqual(storage.calls, [
    ['set', KEY, 'beside'],
    ['set', KEY, 'under'],
  ]);

  const refused = fakeStorage();
  for (const junk of ['sideways', 'BESIDE', undefined, null, 5, { toString: () => 'beside' }]) {
    writeLayout(refused, junk);
  }
  assert.deepEqual(refused.calls, []);
});
