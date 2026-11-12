// The window's one stored choice: where the settings sit in Single image mode.
// Pure functions that take the storage as an argument (the window passes a
// wrapper whose methods reach `window.localStorage` only when called, so even
// that access happens inside the try below), so the node tests can use a fake
// one. No DOM here.
//
// Storage can be missing, full or blocked. Nothing here throws because of it:
// a read that fails gives the default, a write that fails is dropped.

export const LAYOUT_KEY = 'lunaepix.layout';

const LAYOUTS = Object.freeze(['under', 'beside']);

/**
 * One of `LAYOUTS`, "under" (the default) or "beside", exactly as stored. A
 * missing, junk or unreadable value gives "under".
 */
export function readLayout(storage) {
  try {
    const value = storage.getItem(LAYOUT_KEY);
    return LAYOUTS.includes(value) ? value : 'under';
  } catch {
    return 'under';
  }
}

/** Store "under" or "beside". Any other value is not stored; a storage that fails is ignored. */
export function writeLayout(storage, value) {
  if (!LAYOUTS.includes(value)) {
    return;
  }
  try {
    storage.setItem(LAYOUT_KEY, value);
  } catch {
    // Nothing to do: the choice just is not remembered for the next start.
  }
}
