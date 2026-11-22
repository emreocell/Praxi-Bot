/**
 * The Tab trap of a dialog (review of PR #95, finding 10).  Pure over any element type so the
 * rules are testable without a DOM.
 */

/** What decides whether an element is a stop in the Tab order. */
export interface TabStopInfo {
  /** `el.tabIndex`: -1 for `tabindex="-1"` (an inactive tab button), 0 for a plain button. */
  tabIndex: number;
  /** `el.getClientRects().length`: 0 for hidden controls (a `display: none` file input). */
  rects: number;
  /** Inside an `inert` subtree (the body of a dialog that is saving): the browser cannot focus it. */
  inert?: boolean;
}

export function isTabStop(info: TabStopInfo): boolean {
  return info.tabIndex >= 0 && info.rects > 0 && !info.inert;
}

/**
 * Where focus must go on Tab / Shift+Tab, or `null` when the browser's own move stays inside the
 * dialog.  `items` are the Tab stops in order; `active` may be a non-stop (an inactive tab that was
 * clicked) or outside them.
 */
export function trapTarget<T>(
  items: readonly T[],
  active: T | null,
  node: T,
  shift: boolean
): T | null | "none" {
  const first = items[0];
  const last = items[items.length - 1];
  // Nothing usable (everything inert or hidden): the caller keeps focus on the dialog itself.
  if (first === undefined || last === undefined) return "none";
  const inCycle = active !== null && items.includes(active);
  if (shift) {
    return active === first || active === node || !inCycle ? last : null;
  }
  return active === last || !inCycle ? first : null;
}

/**
 * Element to focus when a dialog or palette closes: the trigger that was active when it opened
 * if still connected to the document, or `fallback` when the original trigger unmounted (e.g. a
 * menu item inside a popover that closed as the dialog opened).
 */
export function returnFocusTarget<T>(
  previous: T | null,
  isConnected: (el: T) => boolean,
  fallback: T | null = null
): T | null {
  if (previous !== null && isConnected(previous)) {
    return previous;
  }
  if (fallback !== null && isConnected(fallback)) {
    return fallback;
  }
  return null;
}
