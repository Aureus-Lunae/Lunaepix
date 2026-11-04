import test from 'node:test';
import assert from 'node:assert/strict';

import {
  fileCount,
  formatPercent,
  formatProgress,
  formatSize,
  formatSummary,
  formatTime,
  formatTotal,
} from '../../ui/format.js';

const KIB = 1024;
const MIB = 1024 * 1024;

// formatSize

test('formatSize shows 1023 bytes as bytes', () => {
  assert.equal(formatSize(1023), '1023 B');
});

test('formatSize shows 1024 bytes as KiB, not bytes', () => {
  assert.equal(formatSize(1024), '1.0 KiB');
  assert.notEqual(formatSize(1024), '1024 B');
});

test('formatSize shows zero as 0 B', () => {
  assert.equal(formatSize(0), '0 B');
});

test('formatSize shows KiB with one decimal', () => {
  assert.equal(formatSize(21828), '21.3 KiB');
  assert.equal(formatSize(9631), '9.4 KiB');
});

test('formatSize shows exactly 1 MiB as MiB, not KiB', () => {
  assert.equal(formatSize(MIB), '1.0 MiB');
  assert.notEqual(formatSize(MIB), '1024.0 KiB');
});

test('formatSize keeps the largest KiB value that still rounds below 1024', () => {
  assert.equal(formatSize(MIB - 52), '1023.9 KiB');
});

test('formatSize switches to MiB when KiB would round up to 1024.0', () => {
  // 1048575 B is 1023.999 KiB; "1024.0 KiB" would be a unit too small.
  assert.equal(formatSize(MIB - 1), '1.0 MiB');
});

test('formatSize shows MiB with one decimal', () => {
  assert.equal(formatSize(1.2 * MIB), '1.2 MiB');
  assert.equal(formatSize(5 * MIB + 0.26 * MIB), '5.3 MiB');
});

test('formatSize rounds the decimal, it does not truncate', () => {
  assert.equal(formatSize(KIB + 0.06 * KIB), '1.1 KiB');
  assert.equal(formatSize(KIB + 0.04 * KIB), '1.0 KiB');
});

test('formatSize does not stop at MiB for large files', () => {
  assert.equal(formatSize(1500 * MIB), '1500.0 MiB');
});

test('formatSize shows 0 B for a value that is not a size', () => {
  assert.equal(formatSize(-5), '0 B');
  assert.equal(formatSize(Number.NaN), '0 B');
  assert.equal(formatSize(undefined), '0 B');
  assert.equal(formatSize('1024'), '0 B');
});

// formatPercent

test('formatPercent computes the saving with one decimal', () => {
  assert.equal(formatPercent(21828, 9631), '55.9%');
  assert.equal(formatPercent(200, 150), '25.0%');
});

test('formatPercent shows 0.0% when the file did not change size', () => {
  assert.equal(formatPercent(500, 500), '0.0%');
});

test('formatPercent shows 0.0% for a before size of 0, not NaN or Infinity', () => {
  assert.equal(formatPercent(0, 0), '0.0%');
  assert.equal(formatPercent(0, 10), '0.0%');
});

test('formatPercent shows a grown file as a negative saving, not clamped', () => {
  assert.equal(formatPercent(1000, 1100), '-10.0%');
  assert.notEqual(formatPercent(1000, 1100), '0.0%');
});

test('formatPercent rounds to one decimal', () => {
  assert.equal(formatPercent(1000, 666), '33.4%');
  assert.equal(formatPercent(3, 2), '33.3%');
});

test('formatPercent shows a tiny growth as 0.0%, never as -0.0%', () => {
  assert.equal(formatPercent(100000, 100001), '0.0%');
});

test('formatPercent shows 0.0% for values that are not sizes', () => {
  assert.equal(formatPercent(undefined, 5), '0.0%');
  assert.equal(formatPercent(10, Number.NaN), '0.0%');
});

// formatTime

test('formatTime shows milliseconds', () => {
  assert.equal(formatTime(22), '22 ms');
  assert.equal(formatTime(0), '0 ms');
});

test('formatTime rounds a fractional value', () => {
  assert.equal(formatTime(21.6), '22 ms');
  assert.equal(formatTime(21.4), '21 ms');
});

test('formatTime shows 0 ms for a value that is not a time', () => {
  assert.equal(formatTime(-3), '0 ms');
  assert.equal(formatTime(Number.NaN), '0 ms');
  assert.equal(formatTime(null), '0 ms');
});

// fileCount and the summary

test('fileCount says "1 file" for one', () => {
  assert.equal(fileCount(1), '1 file');
  assert.notEqual(fileCount(1), '1 files');
});

test('fileCount says "N files" for two or more and for none', () => {
  assert.equal(fileCount(2), '2 files');
  assert.equal(fileCount(10), '10 files');
  assert.equal(fileCount(0), '0 files');
});

test('fileCount treats a value that is not a count as none', () => {
  assert.equal(fileCount(-1), '0 files');
  assert.equal(fileCount(Number.NaN), '0 files');
  assert.equal(fileCount(undefined), '0 files');
});

test('formatSummary announces the count and the saving', () => {
  assert.equal(formatSummary(2, 341210), 'Optimised 2 files, saved 333.2 KiB');
});

test('formatSummary uses the singular for one file', () => {
  assert.equal(formatSummary(1, 2048), 'Optimised 1 file, saved 2.0 KiB');
  assert.doesNotMatch(formatSummary(1, 2048), /1 files/);
});

test('formatSummary handles no files', () => {
  assert.equal(formatSummary(0, 0), 'Optimised 0 files, saved 0 B');
});

test('formatTotal is the summary row text', () => {
  assert.equal(formatTotal(2, 341210), 'Total: 2 files, saved 333.2 KiB');
  assert.equal(formatTotal(1, 10), 'Total: 1 file, saved 10 B');
  assert.equal(formatTotal(0, 0), 'Total: 0 files, saved 0 B');
});

// formatProgress

test('formatProgress names the file being worked on', () => {
  assert.equal(formatProgress(2, 10), 'Optimising 3 of 10…');
  assert.equal(formatProgress(0, 10), 'Optimising 1 of 10…');
});

test('formatProgress never goes past the total', () => {
  assert.equal(formatProgress(10, 10), 'Optimising 10 of 10…');
  assert.doesNotMatch(formatProgress(10, 10), /11/);
});

test('formatProgress has no count for an empty run', () => {
  assert.equal(formatProgress(0, 0), 'Optimising…');
});
