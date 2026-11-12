// Zoom and pan maths for the compare picture, as pure functions. No DOM here,
// so the node tests can load it; the window writes the result into the page.
//
// A view is `{ scale, x, y }`. `x` and `y` are how far the picture is moved
// from the centred position in the area, in pixels, so `{ scale: fit, x: 0,
// y: 0 }` is centred without knowing the area. The window shows it as
// `translate(x px, y px) scale(scale)` on a picture that sits centred in the
// area at its own size, with `transform-origin: center`.
//
// `image` is `{ width, height, svg? }`: the pixel size, or for an SVG the
// size it decoded to with `svg: true` (the backend reports no size for one).
// `area` is `{ width, height }` in pixels. `point` is a position inside the
// area, counted from its top-left corner.
//
// Every function returns a new view and never changes the one it is given.

const MAX_ZOOM = 8;
const STEP_FACTOR = 1.25;
const DIVIDER_START = 50;

function finiteOr(value, fallback) {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function isPositive(value) {
  return typeof value === 'number' && Number.isFinite(value) && value > 0;
}

/** A `{ width, height }` that can be divided by and shown at (also single-view.js's test for "something to fit"). */
export function isUsableSize(size) {
  return size !== null && typeof size === 'object' && isPositive(size.width) && isPositive(size.height);
}

function clamp(value, low, high) {
  return Math.min(Math.max(value, low), high);
}

/**
 * The scale range. A raster image fits the area but is never enlarged above
 * 100%, and zooms up to 800%. An SVG has no 100%: it fits the area (which may
 * enlarge it) and zooms up to 8 times that. A size that cannot be used
 * (zero, missing, not a number) gives the neutral range 1 to 8, so no NaN
 * reaches the page; the real range follows once both sizes are known.
 */
export function zoomLimits(image, area) {
  if (!isUsableSize(image) || !isUsableSize(area)) {
    return { fit: 1, max: MAX_ZOOM };
  }
  const toArea = Math.min(area.width / image.width, area.height / image.height);
  if (image.svg === true) {
    return { fit: toArea, max: MAX_ZOOM * toArea };
  }
  return { fit: Math.min(1, toArea), max: MAX_ZOOM };
}

/**
 * How far the picture may be moved from the centre on one axis: not past the
 * area's edge, and not at all when the picture is not bigger than the area.
 * The `+ 0` turns a `-0` into `0`.
 */
function limitOffset(offset, shown, room) {
  const half = Math.max(0, (shown - room) / 2);
  return clamp(offset, -half, half) + 0;
}

/** The view with its offsets limited to the area. Without usable sizes it is centred. */
function bound(scale, x, y, image, area) {
  if (!isUsableSize(image) || !isUsableSize(area)) {
    return { scale, x: 0, y: 0 };
  }
  return {
    scale,
    x: limitOffset(x, image.width * scale, area.width),
    y: limitOffset(y, image.height * scale, area.height),
  };
}

/**
 * `point` relative to the centre of the area. A point or an area that cannot
 * be used, or a coordinate that is not a number, counts as the centre.
 */
function fromCentre(point, area) {
  if (!isUsableSize(area) || point === null || typeof point !== 'object') {
    return { x: 0, y: 0 };
  }
  return {
    x: finiteOr(point.x - area.width / 2, 0),
    y: finiteOr(point.y - area.height / 2, 0),
  };
}

/**
 * Zoom by `factor` around `point`: the image point under it stays under it,
 * as far as the pan limit allows. The scale stays within `[fit, max]` and the
 * pan within the area. A factor that is not a positive number changes nothing.
 */
export function zoomAt(view, factor, point, limits, image, area) {
  if (!isPositive(factor)) {
    return view;
  }
  const scale = clamp(view.scale * factor, limits.fit, limits.max);
  const ratio = scale / view.scale;
  const at = fromCentre(point, area);
  return bound(scale, at.x - (at.x - view.x) * ratio, at.y - (at.y - view.y) * ratio, image, area);
}

function centreOf(area) {
  return isUsableSize(area) ? { x: area.width / 2, y: area.height / 2 } : null;
}

/**
 * One press of a zoom button: a factor of 1.25 in (`direction` above 0) or
 * out (below 0), about the centre of the area. Any other direction changes
 * nothing.
 */
export function step(view, direction, limits, image, area) {
  if (!(direction > 0 || direction < 0)) {
    return view;
  }
  const factor = direction > 0 ? STEP_FACTOR : 1 / STEP_FACTOR;
  return zoomAt(view, factor, centreOf(area), limits, image, area);
}

/**
 * The "100%" button: one image pixel per screen pixel, about the centre of
 * the area. An SVG has no 100%, so it goes to Fit.
 */
export function toActual(view, limits, image, area) {
  if (image !== null && typeof image === 'object' && image.svg === true) {
    return toFit(limits);
  }
  return zoomAt(view, 1 / view.scale, centreOf(area), limits, image, area);
}

/** The "Fit" button: the whole picture, centred. */
export function toFit(limits) {
  return { scale: limits.fit, x: 0, y: 0 };
}

/**
 * Move the picture by `dx`, `dy` pixels (a drag or an arrow key). The picture's
 * edge never passes the area's edge, and a picture smaller than the area stays
 * centred. A delta that is not a number counts as 0.
 */
export function pan(view, dx, dy, image, area) {
  return bound(view.scale, view.x + finiteOr(dx, 0), view.y + finiteOr(dy, 0), image, area);
}

/** The divider position in percent, within 0 to 100. A value that is not a number gives the middle. */
export function divider(value) {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return DIVIDER_START;
  }
  return clamp(value, 0, 100);
}
