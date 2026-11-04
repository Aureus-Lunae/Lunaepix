// The window's state as pure functions: `reduce` turns one backend event (or
// one rejected command) into the next state, and `rowView` turns a stored
// row into the text for the table. No DOM here, so the node tests can load it.
//
// Events are handled in the order they arrive; the backend guarantees runs
// never overlap (see src-tauri/INTERFACE.md).

import { formatPercent, formatSize, formatSummary, formatTime } from './format.js';

/** Messages for the live region. A rejection the page does not know gets `unknown`. */
export const MESSAGES = Object.freeze({
  busy: "Already optimising; try again when it's done.",
  noOutput: 'Nothing has been written yet.',
  openFailed: 'The folder could not be opened.',
  unknown: 'Something went wrong.',
});

const REJECTIONS = Object.freeze({
  busy: MESSAGES.busy,
  'no-output': MESSAGES.noOutput,
  'open-failed': MESSAGES.openFailed,
});

const NO_REASON = 'No reason was given';
const NO_NAME = '(no name)';

export function initialState() {
  return {
    running: false,
    total: 0,
    rows: [],
    summary: null,
    hasOutput: false,
    dragOver: false,
    // The live region text. `messageSeq` changes with every message, so the
    // same text twice is still announced twice.
    message: '',
    messageSeq: 0,
  };
}

function isObject(value) {
  return typeof value === 'object' && value !== null;
}

function count(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0;
}

function withMessage(state, message) {
  return { ...state, message, messageSeq: state.messageSeq + 1 };
}

function rowFrom(payload) {
  if (!isObject(payload)) {
    return null;
  }
  const name = typeof payload.name === 'string' ? payload.name : '';
  switch (payload.status) {
    case 'done':
      return {
        kind: 'done',
        name,
        before: count(payload.before),
        after: count(payload.after),
        millis: count(payload.millis),
      };
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

export function reduce(state, action) {
  const payload = action.payload;
  switch (action.type) {
    case 'run-started':
      return {
        ...withMessage(state, ''),
        running: true,
        total: Math.floor(isObject(payload) ? count(payload.total) : 0),
        rows: [],
        summary: null,
        hasOutput: false,
      };

    case 'file-done': {
      const row = rowFrom(payload);
      return row === null ? state : { ...state, rows: [...state.rows, row] };
    }

    case 'run-finished': {
      const finished = isObject(payload) ? payload : {};
      const summary = {
        optimised: count(finished.optimised),
        savedBytes: count(finished.saved_bytes),
      };
      return {
        ...withMessage(state, formatSummary(summary.optimised, summary.savedBytes)),
        running: false,
        summary,
        hasOutput: finished.has_output === true,
      };
    }

    case 'run-busy':
      return withMessage(state, MESSAGES.busy);

    case 'command-rejected':
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
 * What the table shows for one stored row. A done row has the formatted
 * numbers; a skipped or failed row has `text`, the status word and the
 * reason, in place of the numbers.
 */
export function rowView(row) {
  const name = row.name === '' ? NO_NAME : row.name;
  if (row.kind === 'done') {
    return {
      kind: 'done',
      name,
      before: formatSize(row.before),
      after: formatSize(row.after),
      saved: formatPercent(row.before, row.after),
      time: formatTime(row.millis),
    };
  }
  return { kind: row.kind, name, text: `${row.kind}: ${row.reason}` };
}
