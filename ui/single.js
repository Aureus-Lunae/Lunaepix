// Single image mode's part of the window state, as pure functions. No DOM
// here, so the node tests can load it. `reduce` in state.js hands this mode's
// actions and events to `reduceSingle`, which takes and returns the whole
// window state: the switch lock reads Batch's phase, and a save sets the
// `hasOutput` that Open folder reads. What the page shows is decided here;
// the page only writes it.
//
// This module and state.js import each other's functions. Neither uses the
// other's bindings while it loads, only inside functions, so either can be
// loaded first.
//
// The actions, besides the six backend events (src-tauri/INTERFACE.md):
// - `set-view` ('single' or 'batch'): dispatched once `set_view` resolved, or
//   with 'batch' once `move_drop_to_batch` resolved. A "busy" answer
//   dispatches nothing, so the view stays.
// - `set-name-text` (a string): the Save as box was typed in.
// - `preview-requested` ({ image, settings }): build it with
//   `previewRequestedAction` when `preview_image` is sent, and dispatch it
//   once `preview_image` resolved, i.e. the backend accepted it. A refused
//   request records nothing, so the next trigger asks again; one for an
//   image that is no longer the current one records nothing either.
// - `layout-changed` ('under' or 'beside').
// - `save-started` (the `saveArgs` sent) and `save-ended` (whatever the
//   answer): a save is going between the two. When `save_image` rejects,
//   dispatch the rejection (and call `afterRejection` with the state) BEFORE
//   `save-ended`: the name rejections name the stem recorded by
//   `save-started`, and without it they fall back to the box's text.
//
// The guard on preview outcomes (INTERFACE.md, "Rules for the window"): a
// `preview-ready` or `preview-failed` counts only when its `image` is the
// current image's key and its `version` is the last `preview-started`'s.

import { formatPercent, formatSize } from './format.js';
import { nameProblem } from './names.js';
import {
  MESSAGES,
  MODES,
  NO_REASON,
  canSwitchView,
  isObject,
  qualityOrNull,
  runSettings,
  sentenceOrNull,
  sizeOrNull,
  withMessage,
} from './state.js';

export { canSwitchView };

const VIEWS = Object.freeze(['single', 'batch']);
const LAYOUTS = Object.freeze(['under', 'beside']);
// An image key (`image-changed`) and a version key (`preview-ready`): the
// window's key pattern (`^\d{1,10}(-\d{1,10})?$`) split in its two forms.
const IMAGE_KEY = /^\d{1,10}$/;
const VERSION_KEY = /^\d{1,10}-\d{1,10}$/;

export function initialSingle() {
  return {
    // The loaded image, from `image-changed`, or null.
    image: null,
    // What the Save as box holds, and whether the user typed it (then a new
    // suggestion for the same image does not replace it).
    nameText: '',
    nameTyped: false,
    // The settings of the last request the backend accepted for this image.
    requested: null,
    // The last `preview-started` for this image: { image, version }.
    started: null,
    // A version is being made: from its `preview-started` to its outcome.
    compressing: false,
    // The version shown, from the last `preview-ready` that passed the guard.
    ready: null,
    // The reason of the last attempt when it failed, else null.
    failed: null,
    // The Switch to Batch offer: { notice } after a drop that left items out.
    offer: null,
    // The save going: { name } (the stem sent), or null.
    saving: null,
    // The last `name-taken`: { name, text }, shown while the box holds `name`.
    refused: null,
    layout: 'under',
  };
}

function textOrEmpty(value) {
  return typeof value === 'string' ? value : '';
}

function pixels(value) {
  return Number.isInteger(value) && value > 0 ? value : null;
}

function isVersion(value) {
  return Number.isInteger(value) && value >= 1;
}

/** The image of an `image-changed` payload, or null when it is not one (a key is required). */
function imageFrom(value) {
  if (!isObject(value) || typeof value.key !== 'string' || !IMAGE_KEY.test(value.key)) {
    return null;
  }
  return {
    name: textOrEmpty(value.name),
    size: sizeOrNull(value.size),
    width: pixels(value.width),
    height: pixels(value.height),
    key: value.key,
    suggested: textOrEmpty(value.suggested),
    extension: textOrEmpty(value.extension),
  };
}

/** `{ mode, quality }` as `preview_image` takes it, or null. */
function settingsFrom(value) {
  if (!isObject(value) || !MODES.includes(value.mode) || qualityOrNull(value.quality) === null) {
    return null;
  }
  return { mode: value.mode, quality: value.quality };
}

/** The version of a `preview-ready` payload, or null without a valid key and sizes. */
function readyFrom(payload) {
  const before = sizeOrNull(payload.before);
  const after = sizeOrNull(payload.after);
  if (typeof payload.key !== 'string' || !VERSION_KEY.test(payload.key) || before === null || after === null) {
    return null;
  }
  return {
    key: payload.key,
    mode: MODES.includes(payload.mode) ? payload.mode : null,
    quality: qualityOrNull(payload.quality),
    before,
    after,
    note: sentenceOrNull(payload.note),
  };
}

/** The window's guard: the outcome is for the current image and the last `preview-started`. */
function isLatest(single, payload) {
  return (
    isObject(payload) &&
    single.image !== null &&
    payload.image === single.image.key &&
    single.started !== null &&
    payload.version === single.started.version
  );
}

/**
 * Whether the version shown was made with `settings`: the same mode, and for
 * lossy the same quality (a lossless version's quality is null and does not
 * count). False when either is missing.
 */
function shownMatches(ready, settings) {
  return (
    ready !== null &&
    settings !== null &&
    ready.mode === settings.mode &&
    (ready.mode === 'lossless' || ready.quality === settings.quality)
  );
}

function withSingle(state, changes) {
  return { ...state, single: { ...state.single, ...changes } };
}

function imageChanged(state, payload) {
  if (!isObject(payload)) {
    return state;
  }
  const image = payload.image === null ? null : imageFrom(payload.image);
  if (payload.image !== null && image === null) {
    return state;
  }
  const single = state.single;
  const notice = sentenceOrNull(payload.notice);
  // Every `image-changed` is a load attempt, which ends an earlier offer.
  const leftOut = Number.isInteger(payload.left_out) && payload.left_out > 0;
  const offer = leftOut ? { notice: notice ?? '' } : null;
  const sameImage = (image?.key ?? null) === (single.image?.key ?? null);
  const next = sameImage
    ? { ...state, single: { ...single, offer } }
    : {
        ...state,
        // A new image starts over; the layout is the user's, not the image's.
        single: { ...initialSingle(), layout: single.layout, image, nameText: image?.suggested ?? '', offer },
      };
  return notice === null ? next : withMessage(next, notice);
}

/** The stem a name rejection is about: the one sent with the save going, else the box's text. */
function rejectedStem(single) {
  return single.saving === null ? single.nameText : single.saving.name;
}

/**
 * The text of `invalid-name` or `name-taken` for the stem that was sent, or
 * null when the window cannot explain it: no image is loaded, or
 * `invalid-name` came for a stem the window's name rules accept. The window
 * shows the generic message then, and `afterRejection` logs it.
 */
export function nameRejectionText(state, reason) {
  const single = state.single;
  if (single.image === null) {
    return null;
  }
  const stem = rejectedStem(single);
  if (reason === 'name-taken') {
    return `A file named ${stem}${single.image.extension} already exists there. Choose another name.`;
  }
  return nameProblem(stem, single.image.extension);
}

/** A name rejection: its message; `name-taken` is also tied to the box until its text changes. */
function nameRejected(state, reason) {
  const text = nameRejectionText(state, reason);
  if (text === null) {
    return withMessage(state, MESSAGES.unknown);
  }
  if (reason === 'name-taken') {
    return withMessage(withSingle(state, { refused: { name: rejectedStem(state.single), text } }), text);
  }
  return withMessage(state, text);
}

export function reduceSingle(state, action) {
  const single = state.single;
  const payload = action.payload;
  switch (action.type) {
    case 'set-view': {
      if (!canSwitchView(state) || !VIEWS.includes(payload)) {
        return state;
      }
      // Every set_view ends the Switch to Batch offer.
      const changes = { offer: null };
      if (payload === 'batch') {
        // Batch stops a version being made and nothing more arrives for it,
        // so a request whose version is not the one shown is asked again on
        // return to Single image.
        changes.compressing = false;
        if (!shownMatches(single.ready, single.requested)) {
          changes.requested = null;
        }
      }
      return { ...withSingle(state, changes), view: payload };
    }

    case 'set-name-text':
      return typeof payload === 'string' ? withSingle(state, { nameText: payload, nameTyped: true }) : state;

    case 'preview-requested': {
      // Only for the image it was sent for, if that is still the current one:
      // a late answer for an image replaced since must not count for the new one.
      const settings = isObject(payload) ? settingsFrom(payload.settings) : null;
      const current = single.image !== null && isObject(payload) && payload.image === single.image.key;
      return current && settings !== null ? withSingle(state, { requested: settings }) : state;
    }

    case 'layout-changed':
      return LAYOUTS.includes(payload) ? withSingle(state, { layout: payload }) : state;

    case 'save-started':
      return isObject(payload) && typeof payload.name === 'string'
        ? withSingle(state, { saving: { name: payload.name } })
        : state;

    case 'save-ended':
      return single.saving === null ? state : withSingle(state, { saving: null });

    case 'image-changed':
      return imageChanged(state, payload);

    case 'name-suggested': {
      if (
        !isObject(payload) ||
        single.image === null ||
        payload.image !== single.image.key ||
        typeof payload.suggested !== 'string'
      ) {
        return state;
      }
      return withSingle(state, {
        image: { ...single.image, suggested: payload.suggested },
        nameText: single.nameTyped ? single.nameText : payload.suggested,
      });
    }

    case 'preview-started': {
      // Requests are made only while Single image shows; one that starts
      // while Batch shows was stopped by the switch.
      if (
        state.view !== 'single' ||
        !isObject(payload) ||
        single.image === null ||
        payload.image !== single.image.key ||
        !isVersion(payload.version) ||
        (single.started !== null && payload.version <= single.started.version)
      ) {
        return state;
      }
      return withSingle(state, { started: { image: payload.image, version: payload.version }, compressing: true });
    }

    case 'preview-ready': {
      const ready = isLatest(single, payload) ? readyFrom(payload) : null;
      return ready === null ? state : withSingle(state, { ready, failed: null, compressing: false });
    }

    case 'preview-failed':
      // The version shown before stays: the backend still keeps and serves it.
      return isLatest(single, payload)
        ? withSingle(state, { failed: sentenceOrNull(payload.reason) ?? NO_REASON, compressing: false })
        : state;

    case 'image-saved': {
      const saved = isObject(payload) ? payload : {};
      const next = {
        ...withSingle(state, {
          nameText: typeof saved.suggested === 'string' ? saved.suggested : single.nameText,
          refused: null,
        }),
        hasOutput: true,
      };
      const name = sentenceOrNull(saved.name);
      return name === null ? next : withMessage(next, `Saved as ${name}`);
    }

    case 'command-rejected':
      return payload === 'invalid-name' || payload === 'name-taken' ? nameRejected(state, payload) : state;

    default:
      return state;
  }
}

/**
 * The `preview_image` argument when there is something new to make: an image
 * shows in Single image, the settings are valid (`runSettings`: Lossless
 * always, Lossy with a valid quality text), and they differ from the last
 * request the backend accepted for this image. Otherwise null.
 */
export function previewArgs(state) {
  const { image, requested } = state.single;
  if (state.view !== 'single' || image === null) {
    return null;
  }
  const settings = runSettings(state);
  if (settings === null) {
    return null;
  }
  const same = requested !== null && requested.mode === settings.mode && requested.quality === settings.quality;
  return same ? null : { settings };
}

/**
 * The `preview-requested` action to dispatch once `preview_image` with `args`
 * resolved: built when the request is sent, so it names the image it was sent
 * for. Null when `args` is null or no image is loaded.
 */
export function previewRequestedAction(state, args) {
  const image = state.single.image;
  if (args === null || image === null) {
    return null;
  }
  return { type: 'preview-requested', payload: { image: image.key, settings: args.settings } };
}

/**
 * Save needs a version shown that was made with the settings shown now (the
 * mode, and for lossy the quality; so not between a settings change and its
 * version), none being made, no failed last attempt, no save going, and a
 * name that passes the name rules. A name refused as taken may be tried
 * again (the file may have gone since).
 */
export function canSave(state) {
  const { image, ready, compressing, failed, saving, nameText: text } = state.single;
  return (
    image !== null &&
    shownMatches(ready, runSettings(state)) &&
    !compressing &&
    failed === null &&
    saving === null &&
    nameProblem(text, image.extension) === null
  );
}

/** The `save_image` argument: the typed stem and the key of the version shown, or null. */
export function saveArgs(state) {
  return canSave(state) ? { name: state.single.nameText, preview: state.single.ready.key } : null;
}

function savesText(before, after) {
  const bytes = after > before ? `-${formatSize(after - before)}` : formatSize(before - after);
  return `${formatPercent(before, after)} · ${bytes}`;
}

/**
 * The size box: the sizes as Batch formats them ('' where there is none), the
 * saving as a percentage and bytes (negative for a file that grew), the note
 * and whether a version is being made. While one is, the previous numbers
 * stay. The note is the last attempt's failure reason, except while a new
 * version is being made ("Compressing…" wins); otherwise the shown version's
 * note, or null.
 */
export function sizeView(state) {
  const { image, ready, failed, compressing } = state.single;
  const versionNote = ready === null ? null : ready.note;
  return {
    original: image === null || image.size === null ? '' : formatSize(image.size),
    compressed: ready === null ? '' : formatSize(ready.after),
    saves: ready === null ? '' : savesText(ready.before, ready.after),
    note: failed !== null && !compressing ? failed : versionNote,
    busy: compressing,
  };
}

/** The `alt` of the compressed side, from the version shown. */
export function imageAlt(state) {
  const ready = state.single.ready;
  if (ready === null || ready.mode === null) {
    return 'Compressed';
  }
  if (ready.mode === 'lossless') {
    return 'Compressed, lossless';
  }
  return ready.quality === null ? 'Compressed, lossy' : `Compressed, lossy quality ${ready.quality}`;
}

/** "4032 × 3024", or '' for an image without a pixel size (an SVG) or no image. */
export function pixelSizeText(image) {
  if (!isObject(image) || image.width === null || image.height === null) {
    return '';
  }
  return `${image.width} × ${image.height}`;
}

/** The Switch to Batch offer: its notice, and whether the button can be used now. */
export function offerView(state) {
  const offer = state.single.offer;
  return offer === null ? { notice: '', canMove: false } : { notice: offer.notice, canMove: canSwitchView(state) };
}

/** Whether zoom, position and divider go back to the start: only when a new image shows. */
export function viewResets(prev, next) {
  const key = next.single.image?.key ?? null;
  return key !== null && key !== (prev.single.image?.key ?? null);
}

/** The text of the Save as box. */
export function nameText(state) {
  return state.single.nameText;
}

/**
 * The problem shown with the Save as box, or null: the first name rule the
 * text breaks (`nameProblem`), else a `name-taken` for exactly this text.
 */
export function nameProblemText(state) {
  const { image, nameText: text, refused } = state.single;
  if (image === null) {
    return null;
  }
  const problem = nameProblem(text, image.extension);
  if (problem !== null) {
    return problem;
  }
  return refused !== null && refused.name === text ? refused.text : null;
}