// Pure text formatting for the results table and the summary. No DOM here,
// so the node tests can load it.

const KIB = 1024;
const MIB = 1024 * 1024;

function isSize(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

/** "1023 B", "21.3 KiB", "1.2 MiB". A value that is not a size shows as "0 B". */
export function formatSize(bytes) {
  if (!isSize(bytes)) {
    return '0 B';
  }
  if (bytes < KIB) {
    return `${Math.round(bytes)} B`;
  }
  const kib = (bytes / KIB).toFixed(1);
  // 1048575 B would read "1024.0 KiB", a unit too small, so it moves to MiB.
  if (bytes < MIB && Number(kib) < KIB) {
    return `${kib} KiB`;
  }
  return `${(bytes / MIB).toFixed(1)} MiB`;
}

/**
 * The saving as a percentage of the size before, one decimal. A file that
 * grew shows a negative value. A zero or unusable size before gives "0.0%".
 */
export function formatPercent(before, after) {
  if (!isSize(before) || !isSize(after) || before === 0) {
    return '0.0%';
  }
  const text = (((before - after) / before) * 100).toFixed(1);
  // A tiny growth rounds to "-0.0"; show it as no change.
  return `${Number(text) === 0 ? '0.0' : text}%`;
}

/** "22 ms". A value that is not a time shows as "0 ms". */
export function formatTime(millis) {
  if (!isSize(millis)) {
    return '0 ms';
  }
  return `${Math.round(millis)} ms`;
}

/** "1 file", "2 files", "0 files". A value that is not a count is none. */
export function fileCount(count) {
  const n = typeof count === 'number' && Number.isInteger(count) && count > 0 ? count : 0;
  return n === 1 ? '1 file' : `${n} files`;
}

/** The announcement at the end of a run. */
export function formatSummary(optimised, savedBytes) {
  return `Optimised ${fileCount(optimised)}, saved ${formatSize(savedBytes)}`;
}

/** The text of the summary row under the table. */
export function formatTotal(optimised, savedBytes) {
  return `Total: ${fileCount(optimised)}, saved ${formatSize(savedBytes)}`;
}

/** The visible progress text. `done` files are finished; the next one is in work. */
export function formatProgress(done, total) {
  if (!(total > 0)) {
    return 'Optimising…';
  }
  const current = Math.min(done + 1, total);
  return `Optimising ${current} of ${total}…`;
}
