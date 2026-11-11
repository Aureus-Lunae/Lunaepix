// The window's DOM helpers: the table's rows and column heads, and the
// small writes `render` makes. The document and the elements are passed in,
// so the node tests can hand them a recording stand-in. Every text goes in
// through textContent: names, reasons and notes are data, never markup.

import { rowView } from './state.js';

/** The modifier class for each row view; a row not listed here has none. */
const MODIFIERS = Object.freeze({
  waiting: '-waiting',
  compressing: '-busy',
  skipped: '-skipped',
  failed: '-failed',
});

function cell(doc, tag, text, ...modifiers) {
  const element = doc.createElement(tag);
  element.classList.add('lunaepixResults__cell', ...modifiers);
  element.textContent = text;
  return element;
}

function modifierOf(view) {
  // A listed row that will be skipped looks like a skipped row.
  if (view.kind === 'listed') {
    return view.text === '' ? null : '-skipped';
  }
  return MODIFIERS[view.kind] ?? null;
}

/**
 * The address function for the thumbnails: Tauri's `convertFileSrc` asked for
 * the `thumb` scheme, which is the scheme the backend answers (see
 * INTERFACE.md). The key is the row's `thumb` key; nothing else goes in.
 */
export function thumbSource(convertFileSrc) {
  return (key) => convertFileSrc(key, 'thumb');
}

/** The 32 px square before a name. It holds an image only for a row with a key. */
function thumbSquare(doc, key, thumbSrc, kept) {
  const square = doc.createElement('span');
  square.classList.add('lunaepixResults__thumb');
  if (key !== null) {
    square.append(kept ?? thumbImage(doc, thumbSrc(key)));
  }
  return square;
}

// The thumbnail is decoration: the name beside it says what the file is.
function thumbImage(doc, src) {
  const image = doc.createElement('img');
  image.classList.add('lunaepixResults__thumbImage');
  image.src = src;
  image.alt = '';
  image.width = 32;
  image.height = 32;
  image.loading = 'lazy';
  image.decoding = 'async';
  return image;
}

/** The thumbnail `<img>` a built row holds, or null (the square can be empty). */
function imageOf(tr) {
  return tr.children[0].children[0].children[0] ?? null;
}

/**
 * One `<tr>` for a stored row (see `rowView` for what each kind shows).
 * `thumbSrc(key)` gives the address of a row's thumbnail. `kept` is an `<img>`
 * from an earlier build of the same row, put in the square instead of making
 * a new one, so a row that changes status does not make its picture flicker.
 */
export function buildRow(doc, row, thumbSrc, kept = null) {
  const view = rowView(row);
  const tr = doc.createElement('tr');
  tr.classList.add('lunaepixResults__row');
  const modifier = modifierOf(view);
  if (modifier !== null) {
    tr.classList.add(modifier);
  }

  const name = cell(doc, 'th', view.name, '-name');
  name.scope = 'row';
  name.prepend(thumbSquare(doc, view.thumb, thumbSrc, kept));
  if (view.note !== undefined) {
    const note = doc.createElement('span');
    note.classList.add('lunaepixResults__note');
    note.textContent = view.note;
    name.append(note);
  }
  tr.append(name);

  switch (view.kind) {
    case 'done':
      tr.append(
        cell(doc, 'td', view.before, '-number'),
        cell(doc, 'td', view.after, '-number'),
        cell(doc, 'td', view.saved, '-number'),
        cell(doc, 'td', view.time, '-number'),
      );
      break;
    case 'listed':
    case 'waiting':
    case 'compressing': {
      // The size sits under "Size" (the list) or "Before" (the run); the word
      // or the skip reason takes the three columns after it.
      const text = cell(doc, 'td', view.text);
      text.colSpan = 3;
      tr.append(cell(doc, 'td', view.size, '-number'), text);
      break;
    }
    default: {
      // The status word and the reason take the place of the four numbers.
      const reason = cell(doc, 'td', view.text);
      reason.colSpan = 4;
      tr.append(reason);
    }
  }
  return tr;
}

/**
 * The column heads. The list has File, Size and a status column across the
 * last three (its head is for screen readers only); a run has File, Before,
 * After, Saved and Time. `heads` holds the caption and the four heads after
 * File.
 */
export function setHeads(doc, heads, listing) {
  heads.caption.textContent = listing ? 'Files to optimise' : 'Optimised files';
  heads.before.textContent = listing ? 'Size' : 'Before';
  if (listing) {
    const label = doc.createElement('span');
    label.classList.add('lunaepixResults__hiddenText');
    label.textContent = 'Status';
    heads.after.replaceChildren(label);
    heads.after.colSpan = 3;
  } else {
    heads.after.textContent = 'After';
    heads.after.colSpan = 1;
  }
  heads.saved.hidden = listing;
  heads.time.hidden = listing;
}

/**
 * Brings the table body up to `rows`. `shown` remembers what is on screen
 * (`{ rows, objects, elements }`: the row list, its row objects and their
 * `<tr>`s) and is returned updated. A new row list (a new list or a new run)
 * rebuilds every row. The same list rebuilds only the rows whose object
 * changed and appends the rows past the end: `state.js` writes a run's rows
 * in place, one new object per change. A rebuilt row keeps the thumbnail
 * `<img>` it had when its key is the same, so the picture does not flicker
 * when a row goes from waiting to compressing to done, or from the list to a
 * run (a key always names the same file, see INTERFACE.md).
 */
export function syncRows(doc, body, shown, rows, thumbSrc) {
  if (rows !== shown.rows) {
    const images = new Map();
    shown.objects.forEach((row, index) => {
      if ((row.thumb ?? null) !== null) {
        images.set(row.thumb, imageOf(shown.elements[index]));
      }
    });
    shown.elements = rows.map((row) => {
      const key = row.thumb ?? null;
      const kept = key === null ? null : (images.get(key) ?? null);
      // One element belongs to one row only.
      images.delete(key);
      return buildRow(doc, row, thumbSrc, kept);
    });
    body.replaceChildren(...shown.elements);
    shown.rows = rows;
    shown.objects = rows.slice();
    return shown;
  }
  for (let index = 0; index < rows.length; index += 1) {
    if (rows[index] === shown.objects[index]) {
      continue;
    }
    const exists = index < shown.elements.length;
    const key = rows[index].thumb ?? null;
    const kept = exists && key !== null && key === shown.objects[index].thumb ? imageOf(shown.elements[index]) : null;
    const tr = buildRow(doc, rows[index], thumbSrc, kept);
    if (exists) {
      shown.elements[index].replaceWith(tr);
    } else {
      body.append(tr);
    }
    shown.elements[index] = tr;
    shown.objects[index] = rows[index];
  }
  return shown;
}

/**
 * Hides or shows `element`. When it is about to be hidden while focus is
 * inside it, focus moves to `fallback` first, so it is never lost.
 */
export function hideKeepingFocus(doc, element, hidden, fallback) {
  if (hidden && !element.hidden && element.contains(doc.activeElement)) {
    fallback.focus();
  }
  element.hidden = hidden;
}

/**
 * Writes the live region only for a new message (`messageSeq` changed), so
 * the same text twice is still announced twice and a re-render is never
 * announced. Returns the sequence number now shown.
 */
export function syncStatus(element, shownSeq, state) {
  if (state.messageSeq !== shownSeq) {
    element.textContent = state.message;
  }
  return state.messageSeq;
}

/** Sets the number field's text only when it differs, so typing is never interrupted. */
export function syncNumber(input, text) {
  if (input.value !== text) {
    input.value = text;
  }
}

/** Flags `input` as invalid or not, and ties `error` to it only while it is. */
export function markInvalid(input, error, invalid) {
  input.setAttribute('aria-invalid', String(invalid));
  if (invalid) {
    input.setAttribute('aria-describedby', error.id);
  } else {
    input.removeAttribute('aria-describedby');
  }
  error.hidden = !invalid;
}

/**
 * The summary (with Re-add and Open folder) and the settings with the Run row
 * take each other's place. `parts` holds `summary`, `settings`, `runRow`,
 * `run` and `openFolder`. Whichever block comes up is shown first, so the
 * fallback that catches the focus of the block that goes is visible when it
 * is focused (a hidden element cannot take focus): back to ready, focus on
 * Re-add or Open folder moves to Run; finished, focus on Run or in the
 * settings moves to Open folder. Focus anywhere else stays.
 */
export function swapSummaryAndSettings(doc, parts, showSummary) {
  if (showSummary) {
    hideKeepingFocus(doc, parts.summary, false, parts.run);
    hideKeepingFocus(doc, parts.settings, true, parts.openFolder);
    hideKeepingFocus(doc, parts.runRow, true, parts.openFolder);
  } else {
    hideKeepingFocus(doc, parts.settings, false, parts.openFolder);
    hideKeepingFocus(doc, parts.runRow, false, parts.openFolder);
    hideKeepingFocus(doc, parts.summary, true, parts.run);
  }
}

/**
 * Sends `command` through `send` when `button` is clicked and `gate(state)`
 * allows it. The gate is asked at the time of the click, with the state as it
 * is then (`getState`), so a button that cannot be used right now (it stays
 * focusable, with aria-disabled) does nothing when clicked.
 */
export function bindCommand(button, getState, gate, command, send) {
  button.addEventListener('click', () => {
    if (gate(getState())) {
      send(command);
    }
  });
}
