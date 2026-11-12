// The window's state as pure functions: `reduce` turns one backend event, one
// rejected command or one local action into the next state, and `rowView`
// turns a stored row into the text for the table (and the key of its
// thumbnail). No DOM here, so the node tests can load it.
//
// `reduce` never changes the state it is given, with one exception:
// `file-started` and `file-done` write their row into the run's own row list
// instead of copying it (see `writeRow`).
//
// Events are handled in the order they arrive; the backend guarantees runs
// never overlap (see src-tauri/INTERFACE.md).
//
// The phases: 'empty' (no list, no panel), 'ready' (a list, the settings and
// Run), 'running' and 'finished' (the run's rows, the summary). They are
// Batch's. `view` says which mode shows; Single image mode's part of the
// state is `single`, and its actions and events are handled in single.js.
//
// This module and single.js import each other's functions. Neither uses the
// other's bindings while it loads, only inside functions, so either can be
// loaded first.

import { fileCount, formatPercent, formatSize, formatSummary, formatTime, formatTotal } from './format.js';
import { initialSingle, nameRejectionText, reduceSingle } from './single.js';

/** Messages for the live region. A rejection the page does not know gets `unknown`. */
export const MESSAGES = Object.freeze({
  busy: "Already optimising; try again when it's done.",
  noOutput: 'Nothing has been written yet.',
  openFailed: 'The folder could not be opened.',
  noFiles: 'No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)',
  invalidSettings: 'Quality must be a whole number from 0 to 100.',
  emptyList: 'No file in the list can be optimised.',
  outputMissing:
    'The output folder is gone or cannot be written to. Choose it again, or save next to the originals.',
  noImage: 'Load an image first.',
  notReady: 'The compressed version is not ready yet.',
  nothingToMove: 'There is nothing to move to Batch any more.',
  saveFailed: 'The file could not be saved.',
  // The window's engine could not decode a picture the backend served.
  pictureFailed: 'The picture could not be shown.',
  unknown: 'Something went wrong.',
});

// `output-missing` is also save_image's (the output folder, or with none the
// original's own folder); it keeps the same text.
const REJECTIONS = Object.freeze({
  busy: MESSAGES.busy,
  'no-output': MESSAGES.noOutput,
  'open-failed': MESSAGES.openFailed,
  'invalid-settings': MESSAGES.invalidSettings,
  'empty-list': MESSAGES.emptyList,
  'output-missing': MESSAGES.outputMissing,
  'no-image': MESSAGES.noImage,
  'not-ready': MESSAGES.notReady,
  'nothing-to-move': MESSAGES.nothingToMove,
  'save-failed': MESSAGES.saveFailed,
});

// save_image's rejections whose text depends on the name that was sent: they
// are known, and `reduceSingle` writes their message. (`invalid-view` is in
// neither list: the window never sends another view, so it is logged.)
const NAME_REJECTIONS = Object.freeze(['invalid-name', 'name-taken']);

export const MODES = Object.freeze(['lossless', 'lossy']);
const START_QUALITY = 75;
export const NO_REASON = 'No reason was given';
const NO_NAME = '(no name)';
const NEXT_TO_ORIGINALS = 'Next to the originals';
const NOT_FINISHED = 'The run ended before this file was done';

export function initialState() {
  return {
    phase: 'empty',
    // The list rows ('listed') while ready; the run's rows from Run on.
    rows: [],
    mode: 'lossless',
    quality: START_QUALITY,
    // What the number field holds; `quality` follows it while it is valid.
    qualityText: String(START_QUALITY),
    // The output folder's name, or null for next to the originals.
    folder: null,
    // The index of the row being compressed, or null.
    current: null,
    total: 0,
    // The settings of the run, as `run-started` named them.
    run: null,
    summary: null,
    // Whether Open folder has a folder to open: set by the last completed
    // run's `has_output`, and by a save (`image-saved`).
    hasOutput: false,
    dragOver: false,
    // The live region text. `messageSeq` changes with every message, so the
    // same text twice is still announced twice.
    message: '',
    messageSeq: 0,
    // The mode that shows: 'single' at every launch, or 'batch'.
    view: 'single',
    single: initialSingle(),
  };
}

export function isObject(value) {
  return typeof value === 'object' && value !== null;
}

function count(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0;
}

export function sizeOrNull(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

export function sentenceOrNull(value) {
  return typeof value === 'string' && value !== '' ? value : null;
}

/** A quality as the backend sends it: a whole number 0–100, or null. */
export function qualityOrNull(value) {
  return Number.isInteger(value) && value >= 0 && value <= 100 ? value : null;
}

/** The quality a text stands for: a whole number 0–100 in plain digits, or null. */
function parseQuality(text) {
  if (typeof text !== 'string' || !/^\d{1,3}$/.test(text)) {
    return null;
  }
  const quality = Number(text);
  return quality <= 100 ? quality : null;
}

export function withMessage(state, message) {
  return { ...state, message, messageSeq: state.messageSeq + 1 };
}

/**
 * The key of a row's thumbnail (`<list>-<index>`, see INTERFACE.md): the
 * value when it is a string of one to ten digits, a dash and one to four
 * digits, otherwise null. The window builds an image address from it, so
 * nothing else gets through: no path, no scheme, no extra segment.
 */
export function thumbKey(value) {
  return typeof value === 'string' && /^\d{1,10}-\d{1,4}$/.test(value) ? value : null;
}

/** One row of a `list-changed` payload. A field of the wrong type reads as none. */
function listRowFrom(item) {
  const file = isObject(item) ? item : {};
  return {
    kind: 'listed',
    name: typeof file.name === 'string' ? file.name : '',
    size: sizeOrNull(file.size),
    skip: sentenceOrNull(file.skip),
    thumb: thumbKey(file.thumb),
  };
}

/** A row of a run that has not been handled yet, from the list row in its place. */
function waitingRow(row) {
  return { kind: 'waiting', name: row.name, size: row.size ?? null, skip: row.skip ?? null, thumb: row.thumb };
}

/**
 * A backend reason as it reads after a prefix such as "Will be skipped: ":
 * a first word that is a capitalised ordinary word (a capital followed by a
 * lower-case letter) gets its first letter in lower case ("not a PNG, JPG,
 * SVG or WebP file"). Anything else is kept: an acronym ("PNG …",
 * "I/O error …"), a single letter ("A …") or a digit ("3 more …").
 */
function afterPrefix(reason) {
  return reason.replace(/^\p{Lu}(?=\p{Ll})/u, (first) => first.toLowerCase());
}

/** The run's settings from a `run-started` payload: a lossless run has no quality. */
function runFrom(payload) {
  if (!MODES.includes(payload.mode)) {
    return null;
  }
  const quality = payload.mode === 'lossy' ? qualityOrNull(payload.quality) : null;
  return { mode: payload.mode, quality };
}

function rowFrom(payload) {
  if (!isObject(payload)) {
    return null;
  }
  const name = typeof payload.name === 'string' ? payload.name : '';
  switch (payload.status) {
    case 'done': {
      const row = {
        kind: 'done',
        name,
        before: count(payload.before),
        after: count(payload.after),
        millis: count(payload.millis),
      };
      const note = sentenceOrNull(payload.note);
      if (note !== null) {
        row.note = note;
      }
      return row;
    }
    case 'skipped':
    case 'failed':
      return {
        kind: payload.status,
        name,
        reason: typeof payload.reason === 'string' && payload.reason !== '' ? payload.reason : NO_REASON,
      };
    default:
      return null;
  }
}

function isRowIndex(state, index) {
  return Number.isInteger(index) && index >= 0 && index < state.rows.length;
}

/**
 * Writes `row` at `index` of the run's row list, in place: a run can have
 * thousands of rows and two events per row, and copying the list for each
 * would cost time that grows with the square of the count. The list belongs
 * to this run (`list-changed` and `run-started` make a new one), and `render`
 * compares row objects to see which rows changed. An index one past the end
 * appends, so a run with more rows than the list still keeps each one.
 */
function writeRow(state, index, row) {
  if (isRowIndex(state, index)) {
    state.rows[index] = row;
    return true;
  }
  if (index === state.rows.length) {
    state.rows.push(row);
    return true;
  }
  return false;
}

/**
 * A run row that was to be compressed but got no `file-done`. A waiting row
 * the list showed as skipped is not one: it keeps its own skip reason.
 */
function isUnfinished(row) {
  return row.kind === 'compressing' || (row.kind === 'waiting' && row.skip === null);
}

export function reduce(state, action) {
  const payload = action.payload;
  switch (action.type) {
    case 'list-changed': {
      // During a run the backend refuses adds (`run-busy`); a list arriving
      // anyway never replaces the run's rows.
      if (state.phase === 'running' || !isObject(payload) || !Array.isArray(payload.files)) {
        return state;
      }
      const rows = payload.files.map(listRowFrom);
      const next = {
        ...state,
        phase: rows.length > 0 ? 'ready' : 'empty',
        rows,
        current: null,
        total: 0,
        run: null,
        summary: null,
      };
      const notice = sentenceOrNull(payload.notice);
      return notice === null ? next : withMessage(next, notice);
    }

    case 'output-changed': {
      if (!isObject(payload) || !(typeof payload.folder === 'string' || payload.folder === null)) {
        return state;
      }
      return { ...state, folder: payload.folder };
    }

    case 'set-mode': {
      if (state.phase === 'running' || !MODES.includes(payload) || payload === state.mode) {
        return state;
      }
      // Lossless locks the quality fields; they show the last valid quality
      // then, so a half-typed value cannot block Run while it cannot be fixed.
      const qualityText =
        payload === 'lossless' && parseQuality(state.qualityText) === null ? String(state.quality) : state.qualityText;
      return { ...state, mode: payload, qualityText };
    }

    case 'set-quality-text': {
      if (state.phase === 'running' || state.mode !== 'lossy' || typeof payload !== 'string') {
        return state;
      }
      // The range follows the text only while the text is a valid quality.
      return { ...state, qualityText: payload, quality: parseQuality(payload) ?? state.quality };
    }

    case 'run-started': {
      const started = isObject(payload) ? payload : {};
      const total = Math.floor(count(started.total));
      // The run plans the list again in the same order, so its rows start
      // as the list's rows (see INTERFACE.md). Only a ready state holds the
      // list; after a run the rows are that run's results.
      const list = state.phase === 'ready' ? state.rows : [];
      return {
        ...withMessage(state, ''),
        phase: 'running',
        total,
        rows: list.slice(0, total).map(waitingRow),
        current: null,
        run: runFrom(started),
        summary: null,
        hasOutput: false,
      };
    }

    case 'file-started': {
      if (state.phase !== 'running' || !isObject(payload)) {
        return state;
      }
      const index = payload.index;
      if (!isRowIndex(state, index) || state.rows[index].kind !== 'waiting') {
        return state;
      }
      const row = state.rows[index];
      writeRow(state, index, { kind: 'compressing', name: row.name, size: row.size, thumb: row.thumb });
      const next = { ...state, current: index };
      return withMessage(next, headingLine(next));
    }

    case 'file-done': {
      if (state.phase !== 'running') {
        return state;
      }
      const row = rowFrom(payload);
      if (row === null) {
        return state;
      }
      // The result does not carry a key: the row keeps the one it had in the
      // list (a row past the end of the list has none).
      row.thumb = isRowIndex(state, payload.index) ? state.rows[payload.index].thumb : null;
      if (!writeRow(state, payload.index, row)) {
        return state;
      }
      return { ...state, current: state.current === payload.index ? null : state.current };
    }

    case 'run-finished': {
      const finished = isObject(payload) ? payload : {};
      const summary = {
        optimised: count(finished.optimised),
        savedBytes: count(finished.saved_bytes),
      };
      const message = isEmptyRun(state)
        ? MESSAGES.noFiles
        : formatSummary(summary.optimised, summary.savedBytes);
      // A run that broke off sends fewer `file-done` events; its rows that
      // never got one say so instead of waiting forever.
      const rows = state.rows.some(isUnfinished)
        ? state.rows.map((row) =>
            isUnfinished(row)
              ? { kind: 'failed', name: row.name, reason: NOT_FINISHED, thumb: row.thumb }
              : row,
          )
        : state.rows;
      return {
        ...withMessage(state, message),
        phase: 'finished',
        rows,
        current: null,
        summary,
        hasOutput: finished.has_output === true,
      };
    }

    case 'run-busy':
      return withMessage(state, MESSAGES.busy);

    // Single image mode's actions and events (see single.js).
    case 'set-view':
    case 'set-name-text':
    case 'preview-requested':
    case 'layout-changed':
    case 'save-started':
    case 'save-ended':
    case 'image-changed':
    case 'name-suggested':
    case 'preview-started':
    case 'preview-ready':
    case 'preview-failed':
    case 'image-saved':
    case 'picture-failed':
      return reduceSingle(state, action);

    case 'command-rejected':
      if (NAME_REJECTIONS.includes(payload)) {
        return reduceSingle(state, action);
      }
      return withMessage(
        state,
        typeof payload === 'string' && Object.hasOwn(REJECTIONS, payload)
          ? REJECTIONS[payload]
          : MESSAGES.unknown,
      );

    case 'drag-state': {
      const over = isObject(payload) && payload.over === true;
      return over === state.dragOver ? state : { ...state, dragOver: over };
    }

    // `run-cancelled` and anything unknown: nothing visible changes.
    default:
      return state;
  }
}

/**
 * The gates. The click handlers and the aria-disabled attributes both ask
 * these, so what a button looks like and what a click does cannot differ.
 * `canChoose` covers every button that changes the list or the output.
 */
export function canChoose(state) {
  return state.phase !== 'running';
}

/** Open folder: after a run that wrote something, or after a save; not while a run is going. */
export function canOpenFolder(state) {
  return state.phase !== 'running' && state.hasOutput === true;
}

/**
 * The Single image / Batch switch (and Switch to Batch): locked while a run
 * or a save is going, free again after. A locked switch does nothing.
 */
export function canSwitchView(state) {
  return state.phase !== 'running' && state.single.saving === null;
}

/** Run needs a list with a row that will be compressed, and valid settings. */
export function canRun(state) {
  return (
    state.phase === 'ready' && state.rows.some((row) => row.skip === null) && runSettings(state) !== null
  );
}

/**
 * The `settings` argument of `start_run` and `preview_image`: the mode and the
 * quality as a whole number. Lossless sends the last valid quality, which the
 * backend needs as a whole number 0–100 and then ignores, so the hidden
 * number field never blocks it. Lossy sends the number field's quality, or
 * gives null while the field holds anything but a whole number 0–100.
 */
export function runSettings(state) {
  if (state.mode === 'lossless') {
    return { mode: 'lossless', quality: state.quality };
  }
  const quality = parseQuality(state.qualityText);
  return quality === null ? null : { mode: state.mode, quality };
}

/**
 * The argument of `start_run` (INTERFACE.md): `{ settings: { mode, quality } }`
 * while Run can be used, or null, in which case nothing is sent.
 */
export function startRunArgs(state) {
  return canRun(state) ? { settings: runSettings(state) } : null;
}

/**
 * What the settings controls show: the radios lock while a run is going; the
 * whole Quality row shows only while Lossy is picked (also during a lossy
 * run) and its fields lock during a run; the number field is invalid only for
 * Lossy with text that is not a whole number 0–100; the output buttons are
 * gone while a run is going, and "Clear" shows only while a folder is chosen.
 */
export function settingsView(state) {
  const running = state.phase === 'running';
  return {
    radiosDisabled: running,
    showQuality: state.mode === 'lossy',
    qualityDisabled: running || state.mode === 'lossless',
    invalid: state.mode === 'lossy' && runSettings(state) === null,
    showChooseOutput: !running,
    showClearOutput: !running && state.folder !== null,
  };
}

/**
 * Which blocks of the window show: the panel once there is a list or a run;
 * the list heads before a run, the run heads from Run on; the table while it
 * has rows; the summary in place of the settings and Run once a run is
 * finished; the drop area marked busy while a run is going.
 */
export function panelView(state) {
  const running = state.phase === 'running';
  const finished = state.phase === 'finished';
  return {
    showPanel: state.phase !== 'empty',
    listing: !running && !finished,
    showTable: state.rows.length > 0,
    showSummary: finished,
    showSettings: !finished,
    dropBusy: running,
  };
}

/**
 * What the page does after a command was rejected: the action to dispatch
 * (none for "busy", which the backend's `run-busy` event already announces),
 * and whether to log the reason to the console: one it does not know, and,
 * when `state` is given, a name rejection the window cannot explain (an
 * `invalid-name` for a stem the window accepts, or either name rejection
 * with no image), which gets the generic message. Pass the state of the
 * moment the rejection arrived, before `save-ended`.
 */
export function afterRejection(reason, state) {
  const kind = classifyRejection(reason);
  const unexplained = state !== undefined && NAME_REJECTIONS.includes(reason) && nameRejectionText(state, reason) === null;
  return {
    log: kind === 'unknown' || unexplained,
    action: kind === 'busy' ? null : { type: 'command-rejected', payload: reason },
  };
}

/**
 * What the page does with a rejected command: "busy" is already announced by
 * the `run-busy` event the backend sends with it, so the page says nothing
 * more; "known" rejections get their message; anything else is "unknown" and
 * is logged to the console as well.
 */
export function classifyRejection(reason) {
  if (reason === 'busy') {
    return 'busy';
  }
  const known = typeof reason === 'string' && (Object.hasOwn(REJECTIONS, reason) || NAME_REJECTIONS.includes(reason));
  return known ? 'known' : 'unknown';
}

/** Whether the last run planned no file at all, so there is nothing to total. */
function isEmptyRun(state) {
  return state.total === 0 && state.rows.length === 0;
}

/**
 * The text of the summary row under the table: the total, or for a run that
 * planned no file at all, why the table is empty.
 */
export function summaryLine(state) {
  if (isEmptyRun(state)) {
    return MESSAGES.noFiles;
  }
  const summary = state.summary ?? { optimised: 0, savedBytes: 0 };
  return formatTotal(summary.optimised, summary.savedBytes);
}

/** "Lossy 75", "Lossy" (no usable quality) or "Lossless". */
function settingsText(run) {
  if (run.mode === 'lossless') {
    return 'Lossless';
  }
  return run.quality === null ? 'Lossy' : `Lossy ${run.quality}`;
}

function withSettings(text, run) {
  return run === null ? text : `${text} · ${settingsText(run)}`;
}

/** A run row that is, was or will be compressed (not a skip). */
function isCompressedRow(row) {
  return (
    row.kind === 'compressing' ||
    row.kind === 'done' ||
    row.kind === 'failed' ||
    (row.kind === 'waiting' && row.skip === null)
  );
}

/**
 * The panel's heading: "3 files ready", "Compressing 2 of 3 · Lossy 75",
 * "Done · Lossless". The counts are the files that are compressed, so a
 * skipped row does not count.
 */
export function headingLine(state) {
  switch (state.phase) {
    case 'ready':
      return `${fileCount(state.rows.filter((row) => row.skip === null).length)} ready`;
    case 'running': {
      const compressed = state.rows.filter(isCompressedRow);
      const total = compressed.length;
      const started = compressed.filter((row) => row.kind !== 'waiting').length;
      const progress =
        total > 0 ? `Compressing ${Math.min(Math.max(started, 1), total)} of ${total}` : 'Compressing';
      return withSettings(progress, state.run);
    }
    case 'finished':
      return withSettings('Done', state.run);
    default:
      return '';
  }
}

/** The "Save to" value: the folder's name, or next to the originals. */
export function saveToText(state) {
  if (state.folder === null) {
    return NEXT_TO_ORIGINALS;
  }
  return state.folder === '' ? NO_NAME : state.folder;
}

/** The drop area's title: "more" while there is a list to add to. */
export function dropTitleText(state) {
  return state.phase === 'ready' || state.phase === 'running'
    ? 'Drop more images or a folder here'
    : 'Drop images or a folder here';
}

/** The reasons of the rejected results of `Promise.allSettled`, in order. */
export function settledFailures(results) {
  return results.filter((result) => result.status === 'rejected').map((result) => result.reason);
}

function sizeText(size) {
  return size === null || size === undefined ? '' : formatSize(size);
}

/**
 * What the table shows for one stored row; every view also carries the row's
 * `thumb` key (or null) for the square before the name.
 * - listed: the size, and "Will be skipped: …" for a row that will be skipped;
 * - waiting / compressing: the size and the word, or for a row the list
 *   showed as skipped, its reason;
 * - done: the formatted numbers, and `note` only when the row has one;
 * - skipped / failed: `text`, the status word and the reason.
 */
export function rowView(row) {
  return { ...textView(row), thumb: row.thumb ?? null };
}

function textView(row) {
  const name = row.name === '' ? NO_NAME : row.name;
  switch (row.kind) {
    case 'done': {
      const view = {
        kind: 'done',
        name,
        before: formatSize(row.before),
        after: formatSize(row.after),
        saved: formatPercent(row.before, row.after),
        time: formatTime(row.millis),
      };
      if (row.note !== undefined) {
        view.note = row.note;
      }
      return view;
    }
    case 'listed':
      return {
        kind: 'listed',
        name,
        size: sizeText(row.size),
        text: row.skip === null ? '' : `Will be skipped: ${afterPrefix(row.skip)}`,
      };
    case 'waiting':
      return row.skip === null
        ? { kind: 'waiting', name, size: sizeText(row.size), text: 'Waiting' }
        : { kind: 'skipped', name, text: `skipped: ${afterPrefix(row.skip)}` };
    case 'compressing':
      return { kind: 'compressing', name, size: sizeText(row.size), text: 'Compressing…' };
    case 'skipped':
      // The same reason text as the waiting row showed for it.
      return { kind: 'skipped', name, text: `skipped: ${afterPrefix(row.reason)}` };
    default:
      return { kind: row.kind, name, text: `${row.kind}: ${row.reason}` };
  }
}
