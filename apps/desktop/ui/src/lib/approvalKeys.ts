/**
 * Keyboard shortcuts of the approval card, kept pure so they run without a browser. They never
 * work from "somewhere on the page": the focus must be on the card the person chose to reach.
 *  - Enter allows once only while the card ITSELF has the focus (not a field, not a button, not a
 *    code block inside it, and never the composer or the page). A focused button keeps its own Enter.
 *  - Escape denies from the card or anything non-editable inside it.
 * Nothing here moves the focus: the card is never focused for the person, and neither is Allow.
 */

const EDITABLE = "input, textarea, select, [contenteditable=''], [contenteditable='true'], [contenteditable='plaintext-only']";
const INTERACTIVE =
  "button, a[href], summary, [role='button'], [role='menuitem'], [role='tab'], [role='option'], [role='checkbox'], [role='switch'], [role='radio']";
const CARD = "[data-approval-card]";
const DIALOG = "[role='dialog'], [aria-modal='true'], dialog";

/** The slice of a DOM element these rules need. */
export interface KeyTargetLike {
  closest(selector: string): unknown;
  value?: unknown;
  tagName?: string;
}

export interface ApprovalKeyEvent {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  defaultPrevented?: boolean;
  target: unknown;
}

function isTargetLike(t: unknown): t is KeyTargetLike {
  return typeof t === "object" && t !== null && typeof (t as KeyTargetLike).closest === "function";
}

/**
 * `armed`: the card has been on screen long enough that an Enter held down before it appeared cannot
 * allow it. `canAllow`: this card may be allowed by a key (`enterMayAllowCall`: only a look-only
 * call; never a change, delete, shell or program start, payload to review, send or mail). Escape
 * denies whatever the card is.
 */
export function approvalKeyAction(
  e: ApprovalKeyEvent,
  state: { armed: boolean; canAllow: boolean }
): "allow" | "deny" | null {
  if (e.key !== "Enter" && e.key !== "Escape") return null;
  if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return null;
  if (e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return null;
  const target = e.target;
  if (!isTargetLike(target)) return null;
  const card = target.closest(CARD);
  if (card === null || target.closest(DIALOG)) return null;
  // A field of the card (the reason line) keeps its own Enter / Escape.
  if (target.closest(EDITABLE)) return null;
  if (e.key === "Enter" && (card !== target || target.closest(INTERACTIVE))) return null;
  if (e.key === "Escape") return "deny";
  return state.armed && state.canAllow ? "allow" : null;
}
