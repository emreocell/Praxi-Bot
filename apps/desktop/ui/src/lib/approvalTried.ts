/**
 * Whether this person has decided an approval card (Allow or Deny) in this app. The daemon lists
 * decided approvals only per run, so the "Try an approval" step of the checklist reads this small
 * local mark instead: set the moment a decision goes through, never on a guess.
 */
const KEY = "praxi.approvalDecided";
const EVENT = "praxi:approval-decided";

export function approvalDecided(): boolean {
  try {
    return window.localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

export function markApprovalDecided(): void {
  try {
    window.localStorage.setItem(KEY, "1");
  } catch {
    // blocked storage: the checklist step simply stays open
  }
  window.dispatchEvent(new Event(EVENT));
}

/** Calls `fn` when a decision is made later; returns the unsubscribe function. */
export function onApprovalDecided(fn: () => void): () => void {
  window.addEventListener(EVENT, fn);
  return () => window.removeEventListener(EVENT, fn);
}
