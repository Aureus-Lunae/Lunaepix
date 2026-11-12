// Single image mode in the page: what shows, the writes `render` makes, the
// zoom and pointer maths, and the commands with their triggers. The document,
// the elements, `invoke` and the timers are passed in, so the node tests can
// hand them recording stand-ins (tests/ui/fake-document.js). Every text goes
// in through textContent; names, reasons and notes are data, never markup.
//
// The picture is drawn as two layers (app.css, "The compare picture"): the
// original's stage, and over it an area-sized layer, clipped at the divider
// (`clip-path: inset(0 0 0 var(--split))`, never transformed), that holds the
// compressed version's stage. Both stages get the same transform, so both
// sides show the same spot at the same zoom, and the seam stays under the
// divider's handle whatever the zoom and pan.
//
// window.js looks up the elements and hands events to these functions; its
// own wiring is tested through its start function (tests/ui/window.test.js).
// Every choice about what is sent or shown is made here, in window.js or in
// the pure modules (state.js, single.js, zoom.js, prefs.js).

import { writeLayout } from './prefs.js';
import {
  canSwitchView,
  imageAlt,
  offerView,
  pixelSizeText,
  previewArgs,
  previewRequestedAction,
  saveArgs,
  viewResets,
} from './single.js';
import { afterRejection, panelView } from './state.js';
import { markInvalid, syncNumber } from './table.js';
import { divider, isUsableSize, pan, step, toActual, toFit, zoomAt, zoomLimits } from './zoom.js';

/** The window's key pattern for the `preview` scheme (INTERFACE.md). */
const PREVIEW_KEY = /^\d{1,10}(-\d{1,10})?$/;
const VIEWS = Object.freeze(['single', 'batch']);
const WHEEL_IN = 1.25;
const WHEEL_OUT = 1 / WHEEL_IN;
/** How far one arrow key moves the picture, in pixels. */
const ARROW_STEP = 40;
/** A scale this close to Fit's counts as Fit (zoom maths is floating point). */
const FIT_TOLERANCE = 1e-9;

/** How long the Quality number must stay unchanged before a version is asked for, in ms. */
export const QUALITY_SETTLE_MS = 400;

export const LOADING_TEXT = 'Loading picture…';
export const COMPRESSING_TEXT = 'Compressing…';

// --- addresses and what shows ---------------------------------------------------

/**
 * The address function for the compare picture: Tauri's `convertFileSrc`
 * asked for the `preview` scheme, for a key that matches the window's key
 * pattern only. Anything else gives null and never reaches `convertFileSrc`.
 */
export function previewSource(convertFileSrc) {
  return (key) => (typeof key === 'string' && PREVIEW_KEY.test(key) ? convertFileSrc(key, 'preview') : null);
}

/**
 * The two pictures' addresses (`src` is `previewSource`'s function) and the
 * compressed side's `alt`: the copy taken at load, and the version shown
 * (null until one is ready for this image).
 */
export function pictureView(state, src) {
  const { image, ready } = state.single;
  return {
    original: image === null ? null : src(image.key),
    compressed: image === null || ready === null ? null : src(ready.key),
    alt: imageAlt(state),
  };
}

/**
 * Which blocks of the window show, and their texts:
 * - the mode that shows (`view`, and `single` while it is Single image),
 *   Batch's drop area and panel only in
 *   Batch, and Batch's summary in place of the settings only in Batch (in
 *   Single image the one settings block is in Single image's slot);
 * - in Single image, the drop area until an image is loaded, then the file
 *   row, the tools, the picture, Save as, the size box and Save;
 * - which drop target files dragged over the window light up: Batch's drop
 *   area, Single image's, or the picture once an image is loaded;
 * - the Switch to Batch offer (shown while it lasts, usable while the switch is).
 */
export function screenView(state) {
  const single = state.view === 'single';
  const image = state.single.image;
  const panel = panelView(state);
  return {
    view: state.view,
    single,
    showBatchDrop: !single,
    showBatchPanel: !single && panel.showPanel,
    showSummary: !single && panel.showSummary,
    showSingleDrop: image === null,
    showLoaded: image !== null,
    settingsSlot: single ? 'single' : 'batch',
    layout: state.single.layout,
    fileName: image === null ? '' : image.name,
    pixelSize: pixelSizeText(image),
    extension: image === null ? '' : image.extension,
    dragBatchDrop: !single && state.dragOver,
    dragSingleDrop: single && image === null && state.dragOver,
    dragPicture: single && image !== null && state.dragOver,
    canSwitch: canSwitchView(state),
    showMove: state.single.offer !== null,
    canMove: offerView(state).canMove,
  };
}

/**
 * The text over the picture: a picture decoding (`loading`) wins over a
 * version being made (`sizes`, from `sizeView`); '' for neither.
 */
export function busyText(sizes, loading) {
  if (loading) {
    return LOADING_TEXT;
  }
  return sizes.busy ? COMPRESSING_TEXT : '';
}

// --- page writes ------------------------------------------------------------

/**
 * A picture's record: the `<img>` shown, its address, and the swap going or
 * failed. The `<img>` is made not draggable: a pointer down on the picture
 * pans, and the browser's own image drag would cancel that pointer.
 */
export function pictureSlot(image) {
  image.draggable = false;
  return { image, url: null, pending: null, failed: null, seq: 0 };
}

/**
 * Shows `url` in `slot` without a blank moment: a new `<img>` (with the old
 * one's classes and alt, and not draggable, as `pictureSlot` makes the
 * first one) is decoded off the main thread, and only then takes
 * the old one's place. The old one stays until then. A newer swap makes an
 * older one stale: whichever finishes first, a stale one changes nothing and
 * reports nothing. A failed decode keeps the old picture, remembers `url` as
 * failed and calls `onFail(error)`.
 */
export function swapPicture(doc, slot, url, onReady, onFail) {
  slot.seq += 1;
  const seq = slot.seq;
  slot.pending = url;
  const image = doc.createElement('img');
  image.classList.add(...slot.image.classList);
  image.alt = slot.image.alt;
  image.draggable = false;
  image.decoding = 'async';
  image.src = url;
  return image.decode().then(
    () => {
      if (slot.seq !== seq) {
        return false;
      }
      slot.pending = null;
      slot.image.replaceWith(image);
      slot.image = image;
      slot.url = url;
      onReady(image);
      return true;
    },
    (error) => {
      if (slot.seq !== seq) {
        return false;
      }
      slot.pending = null;
      slot.failed = url;
      onFail(error);
      return false;
    },
  );
}

/**
 * Brings `slot` to `url`, called on every render: a new address is swapped
 * in (`swapPicture`) and gets `alt` once shown; the address shown, the one
 * decoding and the one that failed are not asked for again. No address
 * hides the picture and drops a swap still going.
 */
export function syncPicture(doc, slot, url, alt, onDone, onFail) {
  if (url === null) {
    if (slot.url !== null || slot.pending !== null) {
      slot.seq += 1;
      slot.pending = null;
      slot.url = null;
      slot.image.hidden = true;
    }
    return;
  }
  if (url === slot.url || url === slot.pending || url === slot.failed) {
    return;
  }
  swapPicture(
    doc,
    slot,
    url,
    (image) => {
      image.alt = alt;
      onDone(image);
    },
    onFail,
  );
}

/**
 * Puts the one settings block first in `slot` (before Save as in Single
 * image's slot). A block already there is not moved. Focus inside the block
 * stays on the same control: a browser drops the focus of an element that
 * moves, so it is given back.
 */
export function moveSettings(doc, settings, slot) {
  if (slot.contains(settings)) {
    return false;
  }
  const active = doc.activeElement;
  const inside = active !== null && active !== undefined && settings.contains(active);
  slot.prepend(settings);
  if (inside) {
    active.focus();
  }
  return true;
}

/**
 * The size box (`box`: `original`, `compressed`, `saves`, `note`), from
 * `sizeView`. While a version is being made the note says "Compressing…" and
 * the numbers before stay; an empty note hides.
 */
export function syncSizes(doc, box, view) {
  box.original.textContent = view.original;
  box.compressed.textContent = view.compressed;
  box.saves.textContent = view.saves;
  showText(box.note, view.busy ? COMPRESSING_TEXT : (view.note ?? ''));
}

/**
 * The Save as box: its text (only written when it differs, so typing is never
 * interrupted), and the problem with it, tied to the box with
 * aria-describedby while there is one.
 */
export function syncName(input, error, text, problem) {
  syncNumber(input, text);
  error.textContent = problem ?? '';
  markInvalid(input, error, problem !== null);
}

/** Writes `text` into `element` when it differs, and hides the element while the text is empty. */
export function showText(element, text) {
  if (element.textContent !== text) {
    element.textContent = text;
  }
  element.hidden = text === '';
}

/** Checks the radio whose value is `value`, and marks every radio aria-disabled or not. */
export function syncRadios(radios, value, disabled) {
  for (const radio of radios) {
    radio.checked = radio.value === value;
    radio.setAttribute('aria-disabled', String(disabled));
  }
}

/** Sets the modifier class `-<chosen>` on `element` and removes the other `modifiers`. */
export function syncModifier(element, modifiers, chosen) {
  for (const modifier of modifiers) {
    element.classList.toggle(`-${modifier}`, modifier === chosen);
  }
}

/**
 * The zoom: the same transform on both stages (`parts.stages`), and nothing
 * else. The clip layer and the picture area are never transformed.
 */
export function applyView(parts, view) {
  const transform = `translate(${view.x}px, ${view.y}px) scale(${view.scale})`;
  for (const stage of parts.stages) {
    stage.style.transform = transform;
  }
}

/** The divider: the custom property `--split` on the picture area, and nothing else. */
export function applyDivider(parts, value) {
  parts.picture.style.setProperty('--split', `${value}%`);
}

function isPositive(value) {
  return typeof value === 'number' && Number.isFinite(value) && value > 0;
}

/** The picture area's proportions (`--ratio`), from a usable `{ width, height }`; none otherwise. */
export function applyRatio(picture, size) {
  if (size !== null && typeof size === 'object' && isPositive(size.width) && isPositive(size.height)) {
    picture.style.setProperty('--ratio', `${size.width} / ${size.height}`);
  } else {
    picture.style.removeProperty('--ratio');
  }
}

// --- zoom and pointer maths ---------------------------------------------------

/**
 * The size zoom.js works with: a raster's pixel size from `image-changed`;
 * for an SVG (no pixel size) the size `decoded` (the original's `<img>`)
 * decoded to, marked `svg`; null while neither is known.
 */
export function zoomImageOf(image, decoded) {
  if (image === null || image === undefined) {
    return null;
  }
  if (image.width !== null && image.height !== null) {
    return { width: image.width, height: image.height };
  }
  const width = decoded?.naturalWidth;
  const height = decoded?.naturalHeight;
  return isPositive(width) && isPositive(height) ? { width, height, svg: true } : null;
}

/** `zoomImageOf` for the image loaded in `state` and the picture the original's slot shows. */
export function pictureSize(state, slot) {
  return zoomImageOf(state.single.image, slot.image);
}

/** Whether either picture is still decoding (`slots`: the slot records). */
export function picturesLoading(slots) {
  return slots.some((slot) => slot.pending !== null);
}

/** The start: centred at scale 1, at Fit (the real Fit follows once the sizes are known). */
export function initialZoom() {
  return { view: { scale: 1, x: 0, y: 0 }, atFit: true };
}

function isAtFit(view, limits) {
  return Math.abs(view.scale - limits.fit) < FIT_TOLERANCE && view.x === 0 && view.y === 0;
}

function moved(view, limits) {
  return { view, atFit: isAtFit(view, limits) };
}

/**
 * The next zoom (`{ view, atFit }`) for an action:
 * - `reset` (a new image) and `fit` (the Fit button): the whole picture;
 * - `refit` (the area or the decoded size changed): Fit again while at Fit,
 *   otherwise the same scale, kept within the new limits and pan bounds.
 *   Without a usable area (the picture is hidden) or picture size, nothing
 *   changes: the view stays as it was until there is something to fit;
 * - `wheel` ({ deltaY, point }): zoom about the pointer;
 * - `step` ({ direction }), `actual`: the buttons, about the centre;
 * - `pan` ({ dx, dy }): a drag or an arrow key.
 * `atFit` says whether the view is Fit, so a resize keeps a fitted picture
 * fitted. Anything else changes nothing.
 */
export function zoomReduce(zoom, action, image, area) {
  const limits = zoomLimits(image, area);
  switch (action.type) {
    case 'reset':
    case 'fit':
      return { view: toFit(limits), atFit: true };
    case 'refit':
      if (!isUsableSize(image) || !isUsableSize(area)) {
        return zoom;
      }
      return zoom.atFit
        ? { view: toFit(limits), atFit: true }
        : moved(zoomAt(zoom.view, 1, null, limits, image, area), limits);
    case 'wheel': {
      const factor = wheelFactor(action.deltaY);
      return factor === null ? zoom : moved(zoomAt(zoom.view, factor, action.point, limits, image, area), limits);
    }
    case 'step':
      return moved(step(zoom.view, action.direction, limits, image, area), limits);
    case 'actual':
      return moved(toActual(zoom.view, limits, image, area), limits);
    case 'pan':
      return moved(pan(zoom.view, action.dx, action.dy, image, area), limits);
    default:
      return zoom;
  }
}

/** The wheel's zoom factor: in for a wheel turned up (negative `deltaY`), out for down, null for none. */
export function wheelFactor(deltaY) {
  if (typeof deltaY !== 'number' || !Number.isFinite(deltaY) || deltaY === 0) {
    return null;
  }
  return deltaY < 0 ? WHEEL_IN : WHEEL_OUT;
}

/**
 * The zoom action for a wheel turn at the pointer (`clientX`, `clientY`),
 * with the point counted from the area's top-left corner (`rect`). Whether
 * the turn zooms at all is `zoomReduce`'s call (`wheelFactor`).
 */
export function wheelAction(deltaY, clientX, clientY, rect) {
  return { type: 'wheel', deltaY, point: { x: clientX - rect.left, y: clientY - rect.top } };
}

/**
 * An arrow key pressed while the picture region itself has focus: how far
 * to move the picture, so the arrow shows more of that side. Null for any
 * other key, or a key meant for a control inside the region (the divider).
 */
export function regionKey(region, target, key) {
  if (target !== region) {
    return null;
  }
  switch (key) {
    case 'ArrowLeft':
      return { dx: ARROW_STEP, dy: 0 };
    case 'ArrowRight':
      return { dx: -ARROW_STEP, dy: 0 };
    case 'ArrowUp':
      return { dx: 0, dy: ARROW_STEP };
    case 'ArrowDown':
      return { dx: 0, dy: -ARROW_STEP };
    default:
      return null;
  }
}

/**
 * A key on the picture region: an arrow key on the region itself pans
 * (`pan` gets the zoom action) and does not scroll the page; any other key,
 * or one meant for the divider inside the region, is left alone. True when
 * it panned.
 */
export function onRegionKey(region, event, panBy) {
  const move = regionKey(region, event.target, event.key);
  if (move === null) {
    return false;
  }
  event.preventDefault();
  panBy({ type: 'pan', ...move });
  return true;
}

/** The divider's whole percent under the pointer, within 0 to 100; null without a usable width or position. */
export function dividerFromPointer(clientX, rect) {
  if (!isPositive(rect.width) || typeof clientX !== 'number' || !Number.isFinite(clientX)) {
    return null;
  }
  return Math.round(divider(((clientX - rect.left) / rect.width) * 100));
}

/**
 * A pointer drag starting at (x, y) on `target` with `button`: on the handle
 * it moves the divider, anywhere else it pans. Only the primary button (0)
 * drags; any other button gives null (no drag).
 */
export function dragStart(handle, target, x, y, button) {
  if (button !== 0) {
    return null;
  }
  return { mode: handle.contains(target) ? 'divider' : 'pan', x, y };
}

/**
 * A pointer move during `drag`: the divider position under the pointer, or
 * how far to pan since the last move. Returns the drag as it is now and the
 * result; with no drag going, nothing.
 */
export function dragMove(drag, x, y, rect) {
  if (drag === null) {
    return { drag: null, result: null };
  }
  if (drag.mode === 'divider') {
    const value = dividerFromPointer(x, rect);
    return { drag, result: value === null ? null : { divider: value } };
  }
  return { drag: { ...drag, x, y }, result: { dx: x - drag.x, dy: y - drag.y } };
}

/**
 * Hands a `dragMove` result on: a divider position to `handlers.divider`, a
 * pan to `handlers.pan` as a zoom action; no result, nothing.
 */
export function routeDrag(result, handlers) {
  if (result === null) {
    return;
  }
  if (Object.hasOwn(result, 'divider')) {
    handlers.divider(result.divider);
  } else {
    handlers.pan({ type: 'pan', dx: result.dx, dy: result.dy });
  }
}

// --- commands and triggers ----------------------------------------------------

/**
 * What the page does with a rejected command (`afterRejection`, with the
 * state of the moment it arrived): dispatch its message, and log the ones
 * the page does not know. "busy" dispatches nothing (`run-busy` says it).
 * `deps`: `getState`, `dispatch`, `log(command, reason)`.
 */
export function rejectionHandler(deps) {
  return (command, reason) => {
    const after = afterRejection(reason, deps.getState());
    if (after.log) {
      deps.log(command, reason);
    }
    if (after.action !== null) {
      deps.dispatch(after.action);
    }
  };
}

/** Sends a command; true when it resolved, false (after `reject`) when it was refused. */
export async function send(deps, command, args) {
  try {
    await deps.invoke(command, args);
    return true;
  } catch (reason) {
    deps.reject(command, reason);
    return false;
  }
}

/**
 * Asks for a compressed version when there is something new to make
 * (`previewArgs`). The `preview-requested` action is built when the request
 * is sent (so it names that image) and dispatched only once the backend
 * accepted it: a refused request records nothing, so the next trigger asks
 * again. `deps`: `getState`, `invoke`, `dispatch`, `reject`.
 */
export async function askPreview(deps) {
  const state = deps.getState();
  const args = previewArgs(state);
  if (args === null) {
    return false;
  }
  const action = previewRequestedAction(state, args);
  if (!(await send(deps, 'preview_image', args))) {
    return false;
  }
  deps.dispatch(action);
  return true;
}

/** The view to send for a picked switch value, or null: an unknown value, the view that shows, or a locked switch. */
function viewChoice(state, value) {
  return VIEWS.includes(value) && value !== state.view && canSwitchView(state) ? value : null;
}

/**
 * The Single image / Batch switch: `set_view`, then the view changes in the
 * page, and back in Single image a version is asked for again. A switch
 * that cannot be used, or a refused `set_view`, changes nothing; the page is
 * drawn again so the radios show the view that shows.
 * `deps`: `getState`, `invoke`, `dispatch`, `reject`, `render`, `askPreview`.
 */
export async function switchView(deps, value) {
  const view = viewChoice(deps.getState(), value);
  if (view === null) {
    deps.render();
    return false;
  }
  if (!(await send(deps, 'set_view', { view }))) {
    deps.render();
    return false;
  }
  deps.dispatch({ type: 'set-view', payload: view });
  if (view === 'single') {
    await deps.askPreview();
  }
  return true;
}

/** Switch to Batch: moves the last drop's items to Batch, then shows Batch. Only while the offer can be used. */
export async function moveToBatch(deps) {
  if (!offerView(deps.getState()).canMove) {
    return false;
  }
  if (!(await send(deps, 'move_drop_to_batch'))) {
    return false;
  }
  deps.dispatch({ type: 'set-view', payload: 'batch' });
  return true;
}

/**
 * Save: `save-started` with the arguments sent, `save_image`, and
 * `save-ended` whatever the answer. A rejection is handled before
 * `save-ended`, so a name rejection names the stem that was sent.
 */
export async function saveImage(deps) {
  const args = saveArgs(deps.getState());
  if (args === null) {
    return false;
  }
  deps.dispatch({ type: 'save-started', payload: args });
  try {
    await deps.invoke('save_image', args);
    return true;
  } catch (reason) {
    deps.reject('save_image', reason);
    return false;
  } finally {
    deps.dispatch({ type: 'save-ended', payload: null });
  }
}

/**
 * The page's dispatch: the next state, then the page. A new image (a new
 * key, `viewResets`) first resets the zoom and the divider (`onNewImage`)
 * and, once the page is drawn, asks for its compressed version.
 * `deps`: `reduce`, `getState`, `setState`, `render`, `onNewImage`, `askPreview`.
 */
export function makeDispatch(deps) {
  return (action) => {
    const prev = deps.getState();
    deps.setState(deps.reduce(prev, action));
    const fresh = viewResets(prev, deps.getState());
    if (fresh) {
      deps.onNewImage();
    }
    deps.render();
    if (fresh) {
      deps.askPreview();
    }
  };
}

/**
 * Runs `fn` once, `delay` ms after the last call of the returned function:
 * each call clears the timer the call before set. `timers`: `setTimer`,
 * `clearTimer` (the window's setTimeout and clearTimeout).
 */
export function debounce(timers, delay, fn) {
  // Called on their own, never as `timers.setTimer(…)`: a webview's
  // setTimeout and clearTimeout throw when `this` is another object.
  const { setTimer, clearTimer } = timers;
  let timer = null;
  return () => {
    if (timer !== null) {
      clearTimer(timer);
    }
    timer = setTimer(() => {
      timer = null;
      fn();
    }, delay);
  };
}

/** Calls `onPick(value)` when one of `radios` becomes checked. */
export function bindRadios(radios, onPick) {
  for (const radio of radios) {
    radio.addEventListener('change', () => {
      if (radio.checked) {
        onPick(radio.value);
      }
    });
  }
}

/**
 * The settings' events and the preview triggers: a mode picked sets it and
 * asks at once; the Quality slider sets the quality while it is dragged
 * (`input`) and asks when it is let go (`change`); the number field sets it
 * on each `input` and asks a moment after the last one (`later`, a
 * `debounce`). `parts`: `modeRadios`, `range`, `number`; `handlers`:
 * `dispatch`, `ask`, `later`. Whether there is anything to ask for is
 * `previewArgs`'s call (an invalid number asks nothing).
 */
export function bindPreviewTriggers(parts, handlers) {
  bindRadios(parts.modeRadios, (mode) => {
    handlers.dispatch({ type: 'set-mode', payload: mode });
    handlers.ask();
  });
  parts.range.addEventListener('input', () => {
    handlers.dispatch({ type: 'set-quality-text', payload: parts.range.value });
  });
  parts.range.addEventListener('change', () => {
    handlers.ask();
  });
  parts.number.addEventListener('input', () => {
    handlers.dispatch({ type: 'set-quality-text', payload: parts.number.value });
    handlers.later();
  });
}

/** The layout switch: stores the choice (a storage that fails is ignored) and shows it. */
export function chooseLayout(deps, layout) {
  writeLayout(deps.storage, layout);
  deps.dispatch({ type: 'layout-changed', payload: layout });
}
