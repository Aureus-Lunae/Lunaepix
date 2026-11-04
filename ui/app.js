// The Lunaepix window. The backend does the work and sends events (see
// src-tauri/INTERFACE.md); `state.js` turns each event into the next state and
// `render` shows it. File names and reasons are written with textContent only.

import { formatProgress, formatTotal } from './format.js';
import { initialState, reduce, rowView } from './state.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const drop = document.getElementById('drop');
const chooseFiles = document.getElementById('chooseFiles');
const chooseFolder = document.getElementById('chooseFolder');
const progress = document.getElementById('progress');
const status = document.getElementById('status');
const results = document.getElementById('results');
const resultsBody = document.getElementById('resultsBody');
const summary = document.getElementById('summary');
const summaryText = document.getElementById('summaryText');
const openFolder = document.getElementById('openFolder');

let state = initialState();
// How much of the state is already on screen.
let renderedRows = 0;
let renderedMessageSeq = 0;

function cell(tag, text, ...modifiers) {
  const element = document.createElement(tag);
  element.classList.add('lunaepixResults__cell', ...modifiers);
  element.textContent = text;
  return element;
}

function buildRow(row) {
  const view = rowView(row);
  const tr = document.createElement('tr');
  tr.classList.add('lunaepixResults__row');
  if (view.kind !== 'done') {
    tr.classList.add(`-${view.kind}`);
  }

  const name = cell('th', view.name, '-name');
  name.scope = 'row';
  tr.append(name);

  if (view.kind === 'done') {
    tr.append(
      cell('td', view.before, '-number'),
      cell('td', view.after, '-number'),
      cell('td', view.saved, '-number'),
      cell('td', view.time, '-number'),
    );
  } else {
    // The status word and the reason take the place of the four numbers.
    const reason = cell('td', view.text);
    reason.colSpan = 4;
    tr.append(reason);
  }
  return tr;
}

function setDisabled(button, disabled) {
  button.setAttribute('aria-disabled', String(disabled));
}

function render() {
  drop.classList.toggle('-over', state.dragOver);

  progress.hidden = !state.running;
  if (state.running) {
    progress.textContent = formatProgress(state.rows.length, state.total);
  }
  setDisabled(chooseFiles, state.running);
  setDisabled(chooseFolder, state.running);

  if (state.rows.length < renderedRows) {
    resultsBody.replaceChildren();
    renderedRows = 0;
  }
  resultsBody.append(...state.rows.slice(renderedRows).map(buildRow));
  renderedRows = state.rows.length;
  results.hidden = state.rows.length === 0;

  summary.hidden = state.summary === null;
  if (state.summary !== null) {
    summaryText.textContent = formatTotal(state.summary.optimised, state.summary.savedBytes);
  }
  setDisabled(openFolder, !state.hasOutput);

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

async function call(command) {
  try {
    await invoke(command);
  } catch (reason) {
    dispatch({ type: 'command-rejected', payload: reason });
  }
}

// A button that cannot be used right now stays focusable (aria-disabled), so
// its click does nothing.
chooseFiles.addEventListener('click', () => {
  if (!state.running) {
    call('choose_files');
  }
});

chooseFolder.addEventListener('click', () => {
  if (!state.running) {
    call('choose_folder');
  }
});

openFolder.addEventListener('click', () => {
  if (state.hasOutput) {
    call('open_output_folder');
  }
});

for (const type of ['run-started', 'file-done', 'run-finished', 'run-busy', 'run-cancelled', 'drag-state']) {
  listen(type, (event) => dispatch({ type, payload: event.payload })).catch((reason) => {
    dispatch({ type: 'command-rejected', payload: reason });
  });
}

render();
