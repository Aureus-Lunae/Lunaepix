// The Lunaepix window. The backend does the work and sends events (see
// src-tauri/INTERFACE.md); `state.js` turns each event or local action into
// the next state and decides what shows, and `table.js` writes it into the
// page. This file only looks up the elements, applies those decisions and
// wires the events: every choice about what is sent or shown is made in a
// tested function. File and folder names, reasons and notes are written with
// textContent only.

import {
  afterRejection,
  canChoose,
  canOpenFolder,
  canRun,
  dropTitleText,
  headingLine,
  initialState,
  panelView,
  reduce,
  saveToText,
  settingsView,
  settledFailures,
  startRunArgs,
  summaryLine,
} from './state.js';
import { hideKeepingFocus, markInvalid, setHeads, syncNumber, syncRows, syncStatus } from './table.js';

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
const modeRadios = [document.getElementById('modeLossless'), document.getElementById('modeLossy')];
const qualityRange = document.getElementById('qualityRange');
const qualityNumber = document.getElementById('qualityNumber');
const qualityError = document.getElementById('qualityError');
const saveTo = document.getElementById('saveTo');
const chooseOutput = document.getElementById('chooseOutput');
const clearOutput = document.getElementById('clearOutput');
const runRow = document.getElementById('runRow');
const run = document.getElementById('run');

let state = initialState();
// What is already on screen: the table's rows (see `syncRows`) and the
// sequence number of the last message in the live region.
const shownRows = { rows: null, objects: [], elements: [] };
let shownMessageSeq = 0;

function setDisabled(button, disabled) {
  button.setAttribute('aria-disabled', String(disabled));
}

function renderSettings() {
  const view = settingsView(state);
  settings.classList.toggle('-busy', view.radiosDisabled);
  for (const radio of modeRadios) {
    radio.checked = radio.value === state.mode;
    radio.disabled = view.radiosDisabled;
  }
  qualityRange.value = String(state.quality);
  qualityRange.disabled = view.qualityDisabled;
  syncNumber(qualityNumber, state.qualityText);
  qualityNumber.disabled = view.qualityDisabled;
  markInvalid(qualityNumber, qualityError, view.invalid);

  saveTo.textContent = saveToText(state);
  // Focus on a button that goes moves to the next one that stays.
  hideKeepingFocus(document, clearOutput, !view.showClearOutput, chooseOutput);
  hideKeepingFocus(document, chooseOutput, !view.showChooseOutput, run);
}

function render() {
  const view = panelView(state);

  drop.classList.toggle('-over', state.dragOver);
  drop.classList.toggle('-busy', view.dropBusy);
  dropTitle.textContent = dropTitleText(state);
  setDisabled(chooseFiles, !canChoose(state));

  hideKeepingFocus(document, panel, !view.showPanel, chooseFiles);
  panelHeading.textContent = headingLine(state);
  setDisabled(clearList, !canChoose(state));

  setHeads(document, heads, view.listing);
  syncRows(document, resultsBody, shownRows, state.rows);
  results.hidden = !view.showTable;

  // Finished: the summary and Open folder take the place of the settings and
  // Run. The summary shows first, so focus on Run can move to Open folder.
  hideKeepingFocus(document, summary, !view.showSummary, chooseFiles);
  summaryText.textContent = summaryLine(state);
  setDisabled(openFolder, !canOpenFolder(state));
  hideKeepingFocus(document, settings, !view.showSettings, openFolder);
  hideKeepingFocus(document, runRow, !view.showSettings, openFolder);
  renderSettings();
  setDisabled(run, !canRun(state));

  // The one live region: written only when there is a new message.
  shownMessageSeq = syncStatus(status, shownMessageSeq, state);
}

function dispatch(action) {
  state = reduce(state, action);
  render();
}

async function call(command, args) {
  try {
    await invoke(command, args);
  } catch (reason) {
    const after = afterRejection(reason);
    if (after.log) {
      console.error(command, reason);
    }
    if (after.action !== null) {
      dispatch(after.action);
    }
  }
}

// A button that cannot be used right now stays focusable (aria-disabled), so
// its click asks the same gate and does nothing.
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
  const args = startRunArgs(state);
  if (args !== null) {
    call('start_run', args);
  }
});

for (const radio of modeRadios) {
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
