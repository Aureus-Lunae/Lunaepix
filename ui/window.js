// The Lunaepix window's wiring. The backend does the work and sends events
// (see src-tauri/INTERFACE.md); `state.js` and `single.js` turn each event or
// local action into the next state and decide what shows, `table.js` and
// `single-view.js` write it into the page and make the window's own choices
// (which trigger asks for a preview, what a drag or a key does), and
// `zoom.js` and `prefs.js` do the zoom maths and keep the layout choice.
//
// `startWindow` looks up the elements, draws the page and hands every event
// to those functions. It is given the page, the Tauri API, the timers, the
// storage, ResizeObserver and a log, so the node tests drive it with
// stand-ins (tests/ui/fake-window.js); app.js calls it with the real ones.
// File and folder names, reasons and notes are written with textContent only.

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

// Every event the backend sends that the window acts on.
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

/**
 * Starts the window on `env.doc` and draws it.
 * `env`:
 * - `doc`: the page;
 * - `tauri`: the Tauri API (`core.invoke`, `core.convertFileSrc`, `event.listen`);
 * - `timers`: `setTimer` and `clearTimer` (setTimeout and clearTimeout);
 * - `storage`: where the layout choice is kept (`getItem`, `setItem`);
 * - `ResizeObserver`: the class;
 * - `log(...values)`: where the unexpected goes.
 * Returns `{ listening }`, which settles once every event subscription has
 * been answered (a refused one is reported in the status line).
 */
export function startWindow(env) {
  const { doc, tauri, timers, storage, log } = env;
  const { convertFileSrc, invoke } = tauri.core;
  const { listen } = tauri.event;

  // The header and the status line.
  const viewRadios = [doc.getElementById('viewSingle'), doc.getElementById('viewBatch')];
  const status = doc.getElementById('status');
  const switchToBatch = doc.getElementById('switchToBatch');

  // Batch.
  const drop = doc.getElementById('drop');
  const dropTitle = doc.getElementById('dropTitle');
  const chooseFiles = doc.getElementById('chooseFiles');
  const panel = doc.getElementById('panel');
  const panelHeading = doc.getElementById('panelHeading');
  const clearList = doc.getElementById('clearList');
  const results = doc.getElementById('results');
  const resultsBody = doc.getElementById('resultsBody');
  const heads = {
    caption: doc.getElementById('resultsCaption'),
    before: doc.getElementById('headBefore'),
    after: doc.getElementById('headAfter'),
    saved: doc.getElementById('headSaved'),
    time: doc.getElementById('headTime'),
  };
  const summary = doc.getElementById('summary');
  const summaryText = doc.getElementById('summaryText');
  const readdList = doc.getElementById('readdList');
  const openFolder = doc.getElementById('openFolder');
  const runRow = doc.getElementById('runRow');
  const run = doc.getElementById('run');

  // The settings, shared by both modes: one block, moved into the slot of the
  // mode that shows.
  const settings = doc.getElementById('settings');
  const settingsSlots = {
    single: doc.getElementById('singleSettingsSlot'),
    batch: doc.getElementById('batchSettingsSlot'),
  };
  const modeRadios = [doc.getElementById('modeLossless'), doc.getElementById('modeLossy')];
  const qualityRow = doc.getElementById('qualityRow');
  const qualityRange = doc.getElementById('qualityRange');
  const qualityNumber = doc.getElementById('qualityNumber');
  const qualityError = doc.getElementById('qualityError');
  const saveTo = doc.getElementById('saveTo');
  const chooseOutput = doc.getElementById('chooseOutput');
  const clearOutput = doc.getElementById('clearOutput');

  // Single image.
  const single = doc.getElementById('single');
  const singleFile = doc.getElementById('singleFile');
  const singleFileName = doc.getElementById('singleFileName');
  const singleFileSize = doc.getElementById('singleFileSize');
  const chooseOther = doc.getElementById('chooseOther');
  const singleTools = doc.getElementById('singleTools');
  const layoutRadios = [doc.getElementById('layoutUnder'), doc.getElementById('layoutBeside')];
  const zoomOut = doc.getElementById('zoomOut');
  const zoomIn = doc.getElementById('zoomIn');
  const zoomActual = doc.getElementById('zoomActual');
  const zoomFit = doc.getElementById('zoomFit');
  const singleBody = doc.getElementById('singleBody');
  const singleDrop = doc.getElementById('singleDrop');
  const chooseImage = doc.getElementById('chooseImage');
  const picture = doc.getElementById('picture');
  const pictureParts = {
    picture,
    stages: [doc.getElementById('stageOriginal'), doc.getElementById('stageCompressed')],
  };
  const dividerInput = doc.getElementById('divider');
  const handle = doc.getElementById('handle');
  const pictureBusy = doc.getElementById('pictureBusy');
  const saveAs = doc.getElementById('saveAs');
  const saveName = doc.getElementById('saveName');
  const saveExtension = doc.getElementById('saveExtension');
  const nameError = doc.getElementById('nameError');
  const sizes = doc.getElementById('sizes');
  const sizeBox = {
    original: doc.getElementById('sizeOriginal'),
    compressed: doc.getElementById('sizeCompressed'),
    saves: doc.getElementById('sizeSaves'),
    note: doc.getElementById('sizeNote'),
  };
  const singleFoot = doc.getElementById('singleFoot');
  const singleOpenFolder = doc.getElementById('singleOpenFolder');
  const save = doc.getElementById('save');

  // The addresses of a row's thumbnail and of the compare pictures, from their keys.
  const thumbSrc = thumbSource(convertFileSrc);
  const previewSrc = previewSource(convertFileSrc);

  let state = initialState();
  // What is already on screen: the table's rows (see `syncRows`) and the
  // sequence number of the last message in the live region.
  const shownRows = { rows: null, objects: [], elements: [] };
  let shownMessageSeq = 0;
  // The two compare pictures, and the zoom (both stages show the same view).
  const originalSlot = pictureSlot(doc.getElementById('imageOriginal'));
  const compressedSlot = pictureSlot(doc.getElementById('imageCompressed'));
  let zoom = initialZoom();
  let drag = null;

  function setDisabled(button, disabled) {
    button.setAttribute('aria-disabled', String(disabled));
  }

  const getState = () => state;

  const reject = rejectionHandler({
    getState,
    dispatch: (action) => dispatch(action),
    log: (command, reason) => log(command, reason),
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

  // --- the picture --------------------------------------------------------------

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
    log('picture', error);
    dispatch({ type: 'picture-failed', payload: null });
  }

  // The original decoded: an SVG's size is known only now.
  function onOriginalShown() {
    zoomBy({ type: 'refit' });
    render();
  }

  // --- render -------------------------------------------------------------------

  function renderSettings() {
    const view = settingsView(state);
    settings.classList.toggle('-busy', view.radiosDisabled);
    for (const radio of modeRadios) {
      radio.checked = radio.value === state.mode;
      radio.disabled = view.radiosDisabled;
    }
    // The row hides only while Lossless is picked, so focus (on a radio) is
    // never inside it; the checked Lossless radio is the fallback all the same.
    hideKeepingFocus(doc, qualityRow, !view.showQuality, modeRadios[0]);
    qualityRange.value = String(state.quality);
    qualityRange.disabled = view.qualityDisabled;
    syncNumber(qualityNumber, state.qualityText);
    qualityNumber.disabled = view.qualityDisabled;
    markInvalid(qualityNumber, qualityError, view.invalid);

    saveTo.textContent = saveToText(state);
    // Focus on a button that goes moves to the next one that stays.
    hideKeepingFocus(doc, clearOutput, !view.showClearOutput, chooseOutput);
    hideKeepingFocus(doc, chooseOutput, !view.showChooseOutput, run);
  }

  function renderBatch(screen) {
    const view = panelView(state);

    drop.hidden = !screen.showBatchDrop;
    drop.classList.toggle('-over', screen.dragBatchDrop);
    drop.classList.toggle('-busy', view.dropBusy);
    dropTitle.textContent = dropTitleText(state);
    setDisabled(chooseFiles, !canChoose(state));

    hideKeepingFocus(doc, panel, !screen.showBatchPanel, chooseFiles);
    panelHeading.textContent = headingLine(state);
    setDisabled(clearList, !canChoose(state));

    setHeads(doc, heads, view.listing);
    syncRows(doc, resultsBody, shownRows, state.rows, thumbSrc);
    results.hidden = !view.showTable;

    // Finished: the summary (Re-add and Open folder) takes the place of the
    // settings and Run, and the other way round when Re-add brings the list
    // back. The block that comes up shows first, so focus has somewhere
    // visible to go (see `swapSummaryAndSettings`). In Single image the
    // settings always show.
    swapSummaryAndSettings(doc, { summary, settings, runRow, run, openFolder }, screen.showSummary);
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
    hideKeepingFocus(doc, singleDrop, !screen.showSingleDrop, chooseOther);
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
    syncPicture(doc, originalSlot, pictures.original, 'Original', onOriginalShown, onPictureFailed);
    syncPicture(doc, compressedSlot, pictures.compressed, pictures.alt, () => render(), onPictureFailed);
    applyRatio(picture, zoomImage());

    const view = sizeView(state);
    showText(pictureBusy, busyText(view, picturesLoading([originalSlot, compressedSlot])));
    syncSizes(doc, sizeBox, view);
    syncName(saveName, nameError, nameText(state), nameProblemText(state));
    setDisabled(save, !canSave(state));
    setDisabled(singleOpenFolder, !canOpenFolder(state));
  }

  function render() {
    const screen = screenView(state);

    syncRadios(viewRadios, screen.view, !screen.canSwitch);
    // The one settings block goes into the slot of the mode that shows.
    moveSettings(doc, settings, settingsSlots[screen.settingsSlot]);

    renderBatch(screen);
    renderSingle(screen);
    renderSettings();

    hideKeepingFocus(doc, switchToBatch, !screen.showMove, viewRadios[1]);
    setDisabled(switchToBatch, !screen.canMove);

    // The one live region: written only when there is a new message.
    shownMessageSeq = syncStatus(status, shownMessageSeq, state);
  }

  // --- commands -----------------------------------------------------------------

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
  bindRadios(layoutRadios, (layout) => chooseLayout({ storage, dispatch }, layout));

  // Mode, the Quality slider and the number field: the settings, and when a
  // new compressed version is asked for (see `bindPreviewTriggers`).
  bindPreviewTriggers(
    { modeRadios, range: qualityRange, number: qualityNumber },
    {
      dispatch,
      ask: () => askPreview(commandDeps),
      later: debounce(timers, QUALITY_SETTLE_MS, () => askPreview(commandDeps)),
    },
  );

  saveName.addEventListener('input', () => {
    dispatch({ type: 'set-name-text', payload: saveName.value });
  });

  // --- zoom, pan and the divider ------------------------------------------------

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

  // Only a drag that starts (the primary button, see `dragStart`) takes the pointer.
  picture.addEventListener('pointerdown', (event) => {
    drag = dragStart(handle, event.target, event.clientX, event.clientY, event.button);
    if (drag !== null) {
      picture.setPointerCapture(event.pointerId);
    }
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
  new env.ResizeObserver(() => zoomBy({ type: 'refit' })).observe(picture);

  // --- events -------------------------------------------------------------------

  // If listening fails, say so once, not once per event.
  const listening = Promise.allSettled(
    EVENTS.map((type) => listen(type, (event) => dispatch({ type, payload: event.payload }))),
  ).then((settled) => {
    const failures = settledFailures(settled);
    if (failures.length > 0) {
      log('listen failed', failures);
      dispatch({ type: 'command-rejected', payload: 'listen-failed' });
    }
  });

  dispatch({ type: 'layout-changed', payload: readLayout(storage) });

  return { listening };
}
