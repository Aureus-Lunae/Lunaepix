// The Lunaepix window. The backend does the work and sends events (see
// src-tauri/INTERFACE.md); `state.js` turns each event or local action into
// the next state and `render` shows it. File and folder names, reasons and
// notes are written with textContent only.

import {
  canChoose,
  canOpenFolder,
  canRun,
  classifyRejection,
  dropTitleText,
  headingLine,
  initialState,
  reduce,
  runSettings,
  saveToText,
  settledFailures,
  summaryLine,
} from './state.js';
import { buildRow, setHeads } from './table.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const drop = document.getElementById('drop');
const dropTitle = document.getElementById('dropTitle');
const chooseFiles = document.getElementById('chooseFiles');
const status = document.getElementById('status');
const panel = document.getElementById('panel');
const panelHeading = document.getElementById('panelHeading');
const clearList = document.getElementById('clearList');
const results = document.getElementById('results');
const resultsBody = document.getElementById('resultsBody');
const heads = {
  caption: document.getElementById('resultsCaption'),
  before: document.getElementById('headBefore'),
  after: document.getElementById('headAfter'),
  saved: document.getElementById('headSaved'),
  time: document.getElementById('headTime'),
};
const summary = document.getElementById('summary');
const summaryText = document.getElementById('summaryText');
const openFolder = document.getElementById('openFolder');
const settings = document.getElementById('settings');
const modeLossless = document.getElementById('modeLossless');
const modeLossy = document.getElementById('modeLossy');
const qualityRange = document.getElementById('qualityRange');
const qualityNumber = document.getElementById('qualityNumber');
const qualityError = document.getElementById('qualityError');
const saveTo = document.getElementById('saveTo');
const chooseOutput = document.getElementById('chooseOutput');
const clearOutput = document.getElementById('clearOutput');
const runRow = document.getElementById('runRow');
const run = document.getElementById('run');

let state = initialState();
// What is already on screen: the row list shown, its row objects and their
// <tr> elements, whether the heads are the list's, and the last message.
let shownRows = null;
let shownRowObjects = [];
let rowElements = [];
let shownListing = true;
let renderedMessageSeq = 0;

function setDisabled(button, disabled) {
  button.setAttribute('aria-disabled', String(disabled));
}

/** Hides `element`; if focus is inside it, moves focus to `fallback` first. */
function hide(element, hidden, fallback) {
  if (hidden && !element.hidden && element.contains(document.activeElement)) {
    fallback.focus();
  }
  element.hidden = hidden;
}

function renderRows() {
  // A new list or a new run: build every row.
  if (state.rows !== shownRows) {
    rowElements = state.rows.map((row) => buildRow(document, row));
    resultsBody.replaceChildren(...rowElements);
    shownRows = state.rows;
    shownRowObjects = state.rows.slice();
    return;
  }
  // The same run: rebuild only the rows that changed.
  for (let index = 0; index < state.rows.length; index += 1) {
    if (state.rows[index] === shownRowObjects[index]) {
      continue;
    }
    const tr = buildRow(document, state.rows[index]);
    if (index < rowElements.length) {
      rowElements[index].replaceWith(tr);
    } else {
      resultsBody.append(tr);
    }
    rowElements[index] = tr;
    shownRowObjects[index] = state.rows[index];
  }
}

function renderSettings(running) {
  settings.classList.toggle('-busy', running);
  modeLossless.checked = state.mode === 'lossless';
  modeLossy.checked = state.mode === 'lossy';
  modeLossless.disabled = running;
  modeLossy.disabled = running;

  const qualityLocked = running || state.mode === 'lossless';
  qualityRange.value = String(state.quality);
  qualityRange.disabled = qualityLocked;
  // Written only when it differs, so typing is never interrupted.
  if (qualityNumber.value !== state.qualityText) {
    qualityNumber.value = state.qualityText;
  }
  qualityNumber.disabled = qualityLocked;

  const invalid = state.mode === 'lossy' && runSettings(state) === null;
  qualityNumber.setAttribute('aria-invalid', String(invalid));
  if (invalid) {
    qualityNumber.setAttribute('aria-describedby', 'qualityError');
  } else {
    qualityNumber.removeAttribute('aria-describedby');
  }
  qualityError.hidden = !invalid;

  saveTo.textContent = saveToText(state);
  hide(clearOutput, running || state.folder === null, running ? run : chooseOutput);
  chooseOutput.hidden = running;
}

function render() {
  const running = state.phase === 'running';
  const finished = state.phase === 'finished';

  drop.classList.toggle('-over', state.dragOver);
  drop.classList.toggle('-busy', running);
  dropTitle.textContent = dropTitleText(state);
  setDisabled(chooseFiles, !canChoose(state));

  hide(panel, state.phase === 'empty', chooseFiles);
  panelHeading.textContent = headingLine(state);
  setDisabled(clearList, !canChoose(state));

  const listing = !running && !finished;
  if (listing !== shownListing) {
    setHeads(document, heads, listing);
    shownListing = listing;
  }
  renderRows();
  results.hidden = state.rows.length === 0;

  // Finished: the summary and Open folder take the place of the settings and
  // Run. The summary shows first, so focus on Run can move to Open folder.
  hide(summary, !finished, chooseFiles);
  if (finished) {
    summaryText.textContent = summaryLine(state);
  }
  setDisabled(openFolder, !canOpenFolder(state));
  hide(settings, finished, openFolder);
  hide(runRow, finished, openFolder);
  if (!finished) {
    renderSettings(running);
  }
  setDisabled(run, !canRun(state));

  // The one live region: written only when there is a new message.
  if (state.messageSeq !== renderedMessageSeq) {
    status.textContent = state.message;
    renderedMessageSeq = state.messageSeq;
  }
}

function dispatch(action) {
  state = reduce(state, action);
  render();
}

async function call(command, args) {
  try {
    await invoke(command, args);
  } catch (reason) {
    const kind = classifyRejection(reason);
    // "busy" is announced by the run-busy event that comes with it.
    if (kind === 'busy') {
      return;
    }
    if (kind === 'unknown') {
      console.error(command, reason);
    }
    dispatch({ type: 'command-rejected', payload: reason });
  }
}

// A button that cannot be used right now stays focusable (aria-disabled), so
// its click does nothing.
function onClick(button, gate, command) {
  button.addEventListener('click', () => {
    if (gate(state)) {
      call(command);
    }
  });
}

onClick(chooseFiles, canChoose, 'choose_files');
onClick(clearList, canChoose, 'clear_list');
onClick(chooseOutput, canChoose, 'choose_output_folder');
onClick(clearOutput, canChoose, 'clear_output_folder');
onClick(openFolder, canOpenFolder, 'open_output_folder');

run.addEventListener('click', () => {
  const runWith = runSettings(state);
  // Only valid settings are ever sent; the backend checks them again.
  if (canRun(state) && runWith !== null) {
    call('start_run', { settings: runWith });
  }
});

for (const radio of [modeLossless, modeLossy]) {
  radio.addEventListener('change', () => {
    if (radio.checked) {
      dispatch({ type: 'set-mode', payload: radio.value });
    }
  });
}

// The range and the number field send the same action, so each follows the
// other through the state.
for (const input of [qualityRange, qualityNumber]) {
  input.addEventListener('input', () => {
    dispatch({ type: 'set-quality-text', payload: input.value });
  });
}

const EVENTS = [
  'list-changed',
  'output-changed',
  'run-started',
  'file-started',
  'file-done',
  'run-finished',
  'run-busy',
  'run-cancelled',
  'drag-state',
];

// If listening fails, say so once, not once per event.
Promise.allSettled(
  EVENTS.map((type) => listen(type, (event) => dispatch({ type, payload: event.payload }))),
).then((settled) => {
  const failures = settledFailures(settled);
  if (failures.length > 0) {
    console.error('listen failed', failures);
    dispatch({ type: 'command-rejected', payload: 'listen-failed' });
  }
});

render();
