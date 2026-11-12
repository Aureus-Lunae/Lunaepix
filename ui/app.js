// The Lunaepix window. The backend does the work and sends events (see
// src-tauri/INTERFACE.md); `state.js` and `single.js` turn each event or
// local action into the next state and decide what shows, `table.js` and
// `single-view.js` write it into the page and make the window's own choices
// (which trigger asks for a preview, what a drag or a key does), and
// `zoom.js` and `prefs.js` do the zoom maths and keep the layout choice. This
// file only looks up the elements and hands the events to those tested
// functions. File and folder names, reasons and notes are written with
// textContent only.

import { readLayout } from './prefs.js';
import {
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
import { canSave, nameProblemText, nameText, sizeView } from './single.js';
import {
  QUALITY_SETTLE_MS,
  applyDivider,
  applyRatio,
  applyView,
  askPreview,
  bindPreviewTriggers,
  bindRadios,
  busyText,
  chooseLayout,
  debounce,
  dragMove,
  dragStart,
  initialZoom,
  makeDispatch,
  moveSettings,
  moveToBatch,
  onRegionKey,
  pictureSize,
  pictureSlot,
  pictureView,
  picturesLoading,
  previewSource,
  rejectionHandler,
  routeDrag,
  saveImage,
  screenView,
  send,
  showText,
  switchView,
  syncModifier,
  syncName,
  syncPicture,
  syncRadios,
  syncSizes,
  wheelAction,
  zoomReduce,
} from './single-view.js';
import {
  bindCommand,
  hideKeepingFocus,
  markInvalid,
  setHeads,
  swapSummaryAndSettings,
  syncNumber,
  syncRows,
  syncStatus,
  thumbSource,
} from './table.js';
import { divider } from './zoom.js';

const { convertFileSrc, invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// The header and the status line.
const viewRadios = [document.getElementById('viewSingle'), document.getElementById('viewBatch')];
const status = document.getElementById('status');
const switchToBatch = document.getElementById('switchToBatch');

// Batch.
const drop = document.getElementById('drop');
const dropTitle = document.getElementById('dropTitle');
const chooseFiles = document.getElementById('chooseFiles');
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
const readdList = document.getElementById('readdList');
const openFolder = document.getElementById('openFolder');
const runRow = document.getElementById('runRow');
const run = document.getElementById('run');

// The settings, shared by both modes: one block, moved into the slot of the
// mode that shows.
const settings = document.getElementById('settings');
const settingsSlots = {
  single: document.getElementById('singleSettingsSlot'),
  batch: document.getElementById('batchSettingsSlot'),
};
const modeRadios = [document.getElementById('modeLossless'), document.getElementById('modeLossy')];
const qualityRow = document.getElementById('qualityRow');
const qualityRange = document.getElementById('qualityRange');
const qualityNumber = document.getElementById('qualityNumber');
const qualityError = document.getElementById('qualityError');
const saveTo = document.getElementById('saveTo');
const chooseOutput = document.getElementById('chooseOutput');
const clearOutput = document.getElementById('clearOutput');

// Single image.
const single = document.getElementById('single');
const singleFile = document.getElementById('singleFile');
const singleFileName = document.getElementById('singleFileName');
const singleFileSize = document.getElementById('singleFileSize');
const chooseOther = document.getElementById('chooseOther');
const singleTools = document.getElementById('singleTools');
const layoutRadios = [document.getElementById('layoutUnder'), document.getElementById('layoutBeside')];
const zoomOut = document.getElementById('zoomOut');
const zoomIn = document.getElementById('zoomIn');
const zoomActual = document.getElementById('zoomActual');
const zoomFit = document.getElementById('zoomFit');
const singleBody = document.getElementById('singleBody');
const singleDrop = document.getElementById('singleDrop');
const chooseImage = document.getElementById('chooseImage');
const picture = document.getElementById('picture');
const pictureParts = {
  picture,
  stages: [document.getElementById('stageOriginal'), document.getElementById('stageCompressed')],
};
const dividerInput = document.getElementById('divider');
const handle = document.getElementById('handle');
const pictureBusy = document.getElementById('pictureBusy');
const saveAs = document.getElementById('saveAs');
const saveName = document.getElementById('saveName');
const saveExtension = document.getElementById('saveExtension');
const nameError = document.getElementById('nameError');
const sizes = document.getElementById('sizes');
const sizeBox = {
  original: document.getElementById('sizeOriginal'),
  compressed: document.getElementById('sizeCompressed'),
  saves: document.getElementById('sizeSaves'),
  note: document.getElementById('sizeNote'),
};
const singleFoot = document.getElementById('singleFoot');
const singleOpenFolder = document.getElementById('singleOpenFolder');
const save = document.getElementById('save');

// The addresses of a row's thumbnail and of the compare pictures, from their keys.
const thumbSrc = thumbSource(convertFileSrc);
const previewSrc = previewSource(convertFileSrc);

// The layout choice is the one thing stored. Every access to localStorage
// happens inside prefs.js's try, so a storage that is missing or blocked
// gives the default and never breaks the window.
const layoutStorage = {
  getItem: (key) => window.localStorage.getItem(key),
  setItem: (key, value) => window.localStorage.setItem(key, value),
};

let state = initialState();
// What is already on screen: the table's rows (see `syncRows`) and the
// sequence number of the last message in the live region.
const shownRows = { rows: null, objects: [], elements: [] };
let shownMessageSeq = 0;
// The two compare pictures, and the zoom (both stages show the same view).
const originalSlot = pictureSlot(document.getElementById('imageOriginal'));
const compressedSlot = pictureSlot(document.getElementById('imageCompressed'));
let zoom = initialZoom();
let drag = null;

function setDisabled(button, disabled) {
  button.setAttribute('aria-disabled', String(disabled));
}

const getState = () => state;

const reject = rejectionHandler({
  getState,
  dispatch: (action) => dispatch(action),
  log: (command, reason) => console.error(command, reason),
});

const commandDeps = {
  getState,
  invoke,
  reject,
  dispatch: (action) => dispatch(action),
  render: () => render(),
  askPreview: () => askPreview(commandDeps),
};

const dispatch = makeDispatch({
  reduce,
  getState,
  setState: (next) => {
    state = next;
  },
  render: () => render(),
  onNewImage,
  askPreview: () => askPreview(commandDeps),
});

function call(command, args) {
  return send(commandDeps, command, args);
}

// --- the picture ----------------------------------------------------------------

function zoomImage() {
  return pictureSize(state, originalSlot);
}

function area() {
  return { width: picture.clientWidth, height: picture.clientHeight };
}

function zoomBy(action) {
  zoom = zoomReduce(zoom, action, zoomImage(), area());
  applyView(pictureParts, zoom.view);
}

function setDivider(value) {
  dividerInput.value = String(value);
  applyDivider(pictureParts, value);
}

// A new image: Fit, centred, the divider in the middle (`divider` gives its
// start for a value that is not a number).
function onNewImage() {
  zoomBy({ type: 'reset' });
  setDivider(divider(Number.NaN));
}

function onPictureFailed(error) {
  console.error('picture', error);
  dispatch({ type: 'picture-failed', payload: null });
}

// The original decoded: an SVG's size is known only now.
function onOriginalShown() {
  zoomBy({ type: 'refit' });
  render();
}

// --- render ---------------------------------------------------------------------

function renderSettings() {
  const view = settingsView(state);
  settings.classList.toggle('-busy', view.radiosDisabled);
  for (const radio of modeRadios) {
    radio.checked = radio.value === state.mode;
    radio.disabled = view.radiosDisabled;
  }
  // The row hides only while Lossless is picked, so focus (on a radio) is
  // never inside it; the checked Lossless radio is the fallback all the same.
  hideKeepingFocus(document, qualityRow, !view.showQuality, modeRadios[0]);
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

function renderBatch(screen) {
  const view = panelView(state);

  drop.hidden = !screen.showBatchDrop;
  drop.classList.toggle('-over', screen.dragBatchDrop);
  drop.classList.toggle('-busy', view.dropBusy);
  dropTitle.textContent = dropTitleText(state);
  setDisabled(chooseFiles, !canChoose(state));

  hideKeepingFocus(document, panel, !screen.showBatchPanel, chooseFiles);
  panelHeading.textContent = headingLine(state);
  setDisabled(clearList, !canChoose(state));

  setHeads(document, heads, view.listing);
  syncRows(document, resultsBody, shownRows, state.rows, thumbSrc);
  results.hidden = !view.showTable;

  // Finished: the summary (Re-add and Open folder) takes the place of the
  // settings and Run, and the other way round when Re-add brings the list
  // back. The block that comes up shows first, so focus has somewhere
  // visible to go (see `swapSummaryAndSettings`). In Single image the
  // settings always show.
  swapSummaryAndSettings(document, { summary, settings, runRow, run, openFolder }, screen.showSummary);
  summaryText.textContent = summaryLine(state);
  setDisabled(readdList, !canChoose(state));
  setDisabled(openFolder, !canOpenFolder(state));
  setDisabled(run, !canRun(state));
}

function renderSingle(screen) {
  single.hidden = !screen.single;

  // The loaded blocks show before the drop area goes, so focus on Choose
  // image has Choose other to go to.
  for (const block of [singleFile, singleTools, picture, saveAs, sizes, singleFoot]) {
    block.hidden = !screen.showLoaded;
  }
  hideKeepingFocus(document, singleDrop, !screen.showSingleDrop, chooseOther);
  singleDrop.classList.toggle('-over', screen.dragSingleDrop);
  picture.classList.toggle('-over', screen.dragPicture);
  setDisabled(chooseImage, !canChoose(state));
  setDisabled(chooseOther, !canChoose(state));

  singleFileName.textContent = screen.fileName;
  singleFileSize.textContent = screen.pixelSize;
  saveExtension.textContent = screen.extension;
  syncRadios(layoutRadios, screen.layout, false);
  syncModifier(singleBody, ['under', 'beside'], screen.layout);

  const pictures = pictureView(state, previewSrc);
  syncPicture(document, originalSlot, pictures.original, 'Original', onOriginalShown, onPictureFailed);
  syncPicture(document, compressedSlot, pictures.compressed, pictures.alt, () => render(), onPictureFailed);
  applyRatio(picture, zoomImage());

  const view = sizeView(state);
  showText(pictureBusy, busyText(view, picturesLoading([originalSlot, compressedSlot])));
  syncSizes(document, sizeBox, view);
  syncName(saveName, nameError, nameText(state), nameProblemText(state));
  setDisabled(save, !canSave(state));
  setDisabled(singleOpenFolder, !canOpenFolder(state));
}

function render() {
  const screen = screenView(state);

  syncRadios(viewRadios, screen.view, !screen.canSwitch);
  // The one settings block goes into the slot of the mode that shows.
  moveSettings(document, settings, settingsSlots[screen.settingsSlot]);

  renderBatch(screen);
  renderSingle(screen);
  renderSettings();

  hideKeepingFocus(document, switchToBatch, !screen.showMove, viewRadios[1]);
  setDisabled(switchToBatch, !screen.canMove);

  // The one live region: written only when there is a new message.
  shownMessageSeq = syncStatus(status, shownMessageSeq, state);
}

// --- commands -------------------------------------------------------------------

// A button that cannot be used right now stays focusable (aria-disabled), so
// its click asks the same gate and does nothing (see `bindCommand`).
function onClick(button, gate, command) {
  bindCommand(button, getState, gate, command, call);
}

onClick(chooseFiles, canChoose, 'choose_files');
onClick(clearList, canChoose, 'clear_list');
onClick(readdList, canChoose, 'readd_list');
onClick(chooseOutput, canChoose, 'choose_output_folder');
onClick(clearOutput, canChoose, 'clear_output_folder');
onClick(openFolder, canOpenFolder, 'open_output_folder');
onClick(chooseImage, canChoose, 'choose_image');
onClick(chooseOther, canChoose, 'choose_image');
onClick(singleOpenFolder, canOpenFolder, 'open_output_folder');

run.addEventListener('click', () => {
  const args = startRunArgs(state);
  if (args !== null) {
    call('start_run', args);
  }
});

save.addEventListener('click', () => saveImage(commandDeps));
switchToBatch.addEventListener('click', () => moveToBatch(commandDeps));
bindRadios(viewRadios, (view) => switchView(commandDeps, view));
bindRadios(layoutRadios, (layout) => chooseLayout({ storage: layoutStorage, dispatch }, layout));

// Mode, the Quality slider and the number field: the settings, and when a
// new compressed version is asked for (see `bindPreviewTriggers`).
bindPreviewTriggers(
  { modeRadios, range: qualityRange, number: qualityNumber },
  {
    dispatch,
    ask: () => askPreview(commandDeps),
    later: debounce({ setTimer: setTimeout, clearTimer: clearTimeout }, QUALITY_SETTLE_MS, () =>
      askPreview(commandDeps),
    ),
  },
);

saveName.addEventListener('input', () => {
  dispatch({ type: 'set-name-text', payload: saveName.value });
});

// --- zoom, pan and the divider ----------------------------------------------

zoomOut.addEventListener('click', () => zoomBy({ type: 'step', direction: -1 }));
zoomIn.addEventListener('click', () => zoomBy({ type: 'step', direction: 1 }));
zoomActual.addEventListener('click', () => zoomBy({ type: 'actual' }));
zoomFit.addEventListener('click', () => zoomBy({ type: 'fit' }));

// The wheel zooms the picture, not the page.
picture.addEventListener(
  'wheel',
  (event) => {
    event.preventDefault();
    zoomBy(wheelAction(event.deltaY, event.clientX, event.clientY, picture.getBoundingClientRect()));
  },
  { passive: false },
);

picture.addEventListener('keydown', (event) => onRegionKey(picture, event, zoomBy));

picture.addEventListener('pointerdown', (event) => {
  drag = dragStart(handle, event.target, event.clientX, event.clientY);
  picture.setPointerCapture(event.pointerId);
});

picture.addEventListener('pointermove', (event) => {
  const next = dragMove(drag, event.clientX, event.clientY, picture.getBoundingClientRect());
  drag = next.drag;
  routeDrag(next.result, { divider: setDivider, pan: zoomBy });
});

for (const type of ['pointerup', 'pointercancel']) {
  picture.addEventListener(type, () => {
    drag = null;
  });
}

dividerInput.addEventListener('input', () => {
  applyDivider(pictureParts, divider(dividerInput.valueAsNumber));
});

// The area changes with the window and when the picture first shows.
new ResizeObserver(() => zoomBy({ type: 'refit' })).observe(picture);

// --- events -----------------------------------------------------------------

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
  'image-changed',
  'name-suggested',
  'preview-started',
  'preview-ready',
  'preview-failed',
  'image-saved',
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

dispatch({ type: 'layout-changed', payload: readLayout(layoutStorage) });
