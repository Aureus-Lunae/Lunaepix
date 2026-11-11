// The rules for the name typed in Save as. Pure, so the node tests can load it.
//
// The backend checks the same rules in src-tauri/src/names.rs and decides;
// this copy only refuses early. Both are tested against one fixture,
// tests/fixtures/save-names.json, so they cannot drift apart. The checks run
// in this order, and the first that fails is the one reported.

/** Most characters (code points) in a whole name, extension included. */
export const MAX_NAME_CHARS = 200;

/** Most bytes (UTF-8) in a whole name, extension included. */
export const MAX_NAME_BYTES = 255;

/** The text for each problem, in the order the checks run. */
export const NAME_PROBLEM_TEXTS = Object.freeze({
  empty: 'Enter a name.',
  forbiddenCharacter: 'A name cannot contain / \\ : * ? " < > | or control characters.',
  trailingSpaceOrDot: 'A name cannot end in a space or a dot.',
  reserved: 'This name is reserved by Windows.',
  tooLong: 'This name is too long.',
});

// White space (JavaScript's \s, which includes U+FEFF) plus U+0085, so it is
// the same set as Rust's char::is_whitespace plus U+FEFF in names.rs.
const BLANK = /^[\s\u0085]*$/;
const FORBIDDEN = /[/\\:*?"<>|\u0000-\u001F\u007F]/;
const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)/;

const encoder = new TextEncoder();

/**
 * Null when `stem` (what the user typed) saved with `extension` (the
 * original's, with its dot) is allowed, else the text of the first problem.
 * Forbidden characters and a trailing space or dot are looked for in the stem;
 * the device names and the length are checked on the whole name.
 */
export function nameProblem(stem, extension) {
  if (BLANK.test(stem)) {
    return NAME_PROBLEM_TEXTS.empty;
  }
  if (FORBIDDEN.test(stem)) {
    return NAME_PROBLEM_TEXTS.forbiddenCharacter;
  }
  if (stem.endsWith(' ') || stem.endsWith('.')) {
    return NAME_PROBLEM_TEXTS.trailingSpaceOrDot;
  }
  const full = stem + extension;
  if (RESERVED.test(full.toLowerCase())) {
    return NAME_PROBLEM_TEXTS.reserved;
  }
  if ([...full].length > MAX_NAME_CHARS || encoder.encode(full).length > MAX_NAME_BYTES) {
    return NAME_PROBLEM_TEXTS.tooLong;
  }
  return null;
}
