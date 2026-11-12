// Stand-ins for what the window's start function (ui/window.js) is given:
// the page, the Tauri API, the timers, the storage and ResizeObserver. They
// record what the window does with them, and are as strict as the real ones
// where a mistake would only show in the app: a timer called with the wrong
// `this` throws, as setTimeout does in a webview; asking the page for an id it
// does not have throws instead of giving null.

import { fakeDocument } from './fake-document.js';

// Every element of ui/index.html that has an id, in its nesting, plus the
// compressed layer (no id in the page, so `_layer`, which `getElementById`
// refuses): [id, tag, parent id or null, fields to set]. Inputs carry their
// type and value, so `valueAsNumber` and the radios read as in the page.
const PAGE = [
  ['viewSingle', 'input', null, { type: 'radio', value: 'single', checked: true }],
  ['viewBatch', 'input', null, { type: 'radio', value: 'batch' }],
  ['drop', 'section', null, { hidden: true }],
  ['dropTitle', 'h2', 'drop'],
  ['chooseFiles', 'button', 'drop'],
  ['status', 'p', null],
  ['switchToBatch', 'button', null, { hidden: true }],
  ['single', 'section', null],
  ['singleFile', 'div', 'single'],
  ['singleFileName', 'h2', 'singleFile'],
  ['singleFileSize', 'span', 'singleFile'],
  ['chooseOther', 'button', 'singleFile'],
  ['singleTools', 'div', 'single'],
  ['layoutUnder', 'input', 'singleTools', { type: 'radio', value: 'under', checked: true }],
  ['layoutBeside', 'input', 'singleTools', { type: 'radio', value: 'beside' }],
  ['zoomOut', 'button', 'singleTools'],
  ['zoomIn', 'button', 'singleTools'],
  ['zoomActual', 'button', 'singleTools'],
  ['zoomFit', 'button', 'singleTools'],
  ['singleBody', 'div', 'single', { classes: ['lunaepixSingle__body', '-under'] }],
  ['singleDrop', 'section', 'singleBody'],
  ['singleDropHeading', 'h2', 'singleDrop'],
  ['chooseImage', 'button', 'singleDrop'],
  ['picture', 'div', 'singleBody', { hidden: true }],
  ['stageOriginal', 'div', 'picture'],
  ['imageOriginal', 'img', 'stageOriginal', { alt: 'Original', hidden: true, classes: ['lunaepixSingle__image'] }],
  ['_layer', 'div', 'picture', { classes: ['lunaepixSingle__layer', '-compressed'] }],
  ['stageCompressed', 'div', '_layer'],
  [
    'imageCompressed',
    'img',
    'stageCompressed',
    { alt: 'Compressed', hidden: true, classes: ['lunaepixSingle__image'] },
  ],
  ['divider', 'input', 'picture', { type: 'range', value: '50' }],
  ['handle', 'div', 'picture'],
  ['pictureBusy', 'p', 'picture'],
  ['singleSettingsSlot', 'div', 'singleBody'],
  ['settings', 'div', 'singleSettingsSlot'],
  ['modeLossless', 'input', 'settings', { type: 'radio', value: 'lossless', checked: true }],
  ['modeLossy', 'input', 'settings', { type: 'radio', value: 'lossy' }],
  ['qualityRow', 'div', 'settings'],
  ['qualityRange', 'input', 'qualityRow', { type: 'range', value: '75' }],
  ['qualityNumber', 'input', 'qualityRow', { type: 'number', value: '75' }],
  ['qualityError', 'p', 'qualityRow'],
  ['saveToLabel', 'span', 'settings'],
  ['saveTo', 'span', 'settings'],
  ['chooseOutput', 'button', 'settings'],
  ['clearOutput', 'button', 'settings'],
  ['saveAs', 'div', 'singleSettingsSlot'],
  ['saveName', 'input', 'saveAs', { type: 'text' }],
  ['saveExtension', 'span', 'saveAs'],
  ['nameError', 'p', 'saveAs'],
  ['sizes', 'div', 'singleBody'],
  ['sizeOriginal', 'dd', 'sizes'],
  ['sizeCompressed', 'dd', 'sizes'],
  ['sizeSaves', 'dd', 'sizes'],
  ['sizeNote', 'p', 'sizes'],
  ['singleFoot', 'div', 'singleBody'],
  ['singleOpenFolder', 'button', 'singleFoot'],
  ['save', 'button', 'singleFoot'],
  ['panel', 'section', null, { hidden: true }],
  ['panelHeading', 'h2', 'panel'],
  ['clearList', 'button', 'panel'],
  ['results', 'table', 'panel'],
  ['resultsCaption', 'caption', 'results'],
  ['headBefore', 'th', 'results'],
  ['headAfter', 'th', 'results'],
  ['headSaved', 'th', 'results'],
  ['headTime', 'th', 'results'],
  ['resultsBody', 'tbody', 'results'],
  ['summary', 'div', 'panel', { hidden: true }],
  ['summaryText', 'p', 'summary'],
  ['readdList', 'button', 'summary'],
  ['openFolder', 'button', 'summary'],
  ['batchSettingsSlot', 'div', 'panel'],
  ['runRow', 'div', 'panel'],
  ['run', 'button', 'runRow'],
];

/**
 * The page: the shared fake document with every element above, in its place.
 * `byId(id)` gives an element to the test; `getElementById` is what the
 * window calls, and it throws for an id the page does not have.
 */
export function fakePage() {
  const doc = fakeDocument();
  const elements = new Map();
  const body = doc.createElement('body');
  for (const [id, tag, parentId, fields = {}] of PAGE) {
    const element = doc.createElement(tag);
    // A name starting with `_` is for the test only: that element has no id in the page.
    if (!id.startsWith('_')) {
      element.id = id;
      element.setAttribute('id', id);
    }
    const { classes = [], ...rest } = fields;
    element.classList.add(...classes);
    Object.assign(element, rest);
    (parentId === null ? body : elements.get(parentId)).append(element);
    elements.set(id, element);
  }
  doc.body = body;
  doc.byId = (id) => elements.get(id);
  doc.getElementById = (id) => {
    if (!elements.has(id) || id.startsWith('_')) {
      throw new Error(`the page has no element with the id ${id}`);
    }
    return elements.get(id);
  };
  return doc;
}

/**
 * The Tauri API as `window.__TAURI__` has it. `invoke` records each call and
 * resolves, or rejects with `refuse[command]`; `listen` records each
 * subscription, or rejects for a name in `refuseListen`; `emit(type,
 * payload)` runs the handler subscribed to `type`, as the backend's event
 * would, and throws when nobody listens.
 */
export function fakeTauri({ refuse = {}, refuseListen = [] } = {}) {
  const calls = [];
  const handlers = new Map();
  return {
    calls,
    handlers,
    core: {
      invoke(command, args) {
        calls.push([command, args]);
        return Object.hasOwn(refuse, command) ? Promise.reject(refuse[command]) : Promise.resolve(null);
      },
      convertFileSrc(key, scheme) {
        return `${scheme}://localhost/${key}`;
      },
    },
    event: {
      listen(type, handler) {
        if (refuseListen.includes(type)) {
          return Promise.reject(new Error(`no ${type}`));
        }
        handlers.set(type, handler);
        return Promise.resolve(() => handlers.delete(type));
      },
    },
    emit(type, payload) {
      const handler = handlers.get(type);
      if (handler === undefined) {
        throw new Error(`nobody listens to ${type}`);
      }
      handler({ event: type, id: 1, payload });
    },
    commands() {
      return calls.map(([command]) => command);
    },
  };
}

function refuseThis(self) {
  // A webview's setTimeout and clearTimeout throw when called as a method of
  // another object.
  if (self !== undefined && self !== globalThis) {
    throw new TypeError('Illegal invocation');
  }
}

/**
 * setTimeout and clearTimeout on a clock the test moves (`advance(ms)` runs
 * every timer that is due, in order). Strict as a webview: calling either
 * with a `this` other than undefined or globalThis throws.
 * `set`: every timer set, as { id, fn, delay }; `cleared`: the ids cleared.
 */
export function strictTimers() {
  let now = 0;
  const set = [];
  const cleared = [];
  const due = new Map();
  function setTimer(fn, delay) {
    refuseThis(this);
    const id = set.length + 1;
    set.push({ id, fn, delay });
    due.set(id, { fn, at: now + delay });
    return id;
  }
  function clearTimer(id) {
    refuseThis(this);
    cleared.push(id);
    due.delete(id);
  }
  function advance(ms) {
    now += ms;
    const ready = [...due].filter(([, timer]) => timer.at <= now).sort((a, b) => a[1].at - b[1].at);
    for (const [id, timer] of ready) {
      due.delete(id);
      timer.fn();
    }
  }
  return { set, cleared, setTimer, clearTimer, advance };
}

/** A storage holding `stored` (key → text); `throws` makes every access throw, as a blocked storage does. */
export function fakeStorage(stored = {}, throws = false) {
  const items = new Map(Object.entries(stored));
  const writes = [];
  const guard = () => {
    if (throws) {
      throw new Error('SecurityError');
    }
  };
  return {
    writes,
    getItem(key) {
      guard();
      return items.has(key) ? items.get(key) : null;
    },
    setItem(key, value) {
      guard();
      writes.push([key, String(value)]);
      items.set(key, String(value));
    },
  };
}

/** A ResizeObserver class; `resize(element)` runs the callback of every observer watching it. */
export function fakeResizeObservers() {
  const observers = [];
  class FakeResizeObserver {
    constructor(callback) {
      this.callback = callback;
      this.targets = [];
      observers.push(this);
    }

    observe(target) {
      this.targets.push(target);
    }
  }
  return {
    observers,
    ResizeObserver: FakeResizeObserver,
    resize(element) {
      for (const observer of observers) {
        if (observer.targets.includes(element)) {
          observer.callback([{ target: element }], observer);
        }
      }
    },
  };
}

/** Lets every promise that is already settled run its handlers. */
export const settle = () => new Promise((resolve) => setImmediate(resolve));
