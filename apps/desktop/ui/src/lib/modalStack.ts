/**
 * Which open dialog is topmost.  Each dialog takes a sequence number when it
 * is *opened* (during render), so the order follows opening order even when
 * two dialogs mount in the same commit and their effects run in tree order.
 */
export interface ModalStack {
  /** A new, strictly increasing sequence number. */
  next(): number;
  add(seq: number): void;
  remove(seq: number): void;
  /** The most recently opened dialog still open. */
  isTop(seq: number): boolean;
}

export function createModalStack(): ModalStack {
  let counter = 0;
  const open = new Set<number>();
  return {
    next: () => ++counter,
    add: (seq) => void open.add(seq),
    remove: (seq) => void open.delete(seq),
    isTop: (seq) => open.size > 0 && seq === Math.max(...open)
  };
}

/**
 * The app-wide layer stack: dialogs (`Modal`) and popovers (`Popover`) all take a number here,
 * so only the topmost layer reacts to Esc / Tab (a popover opened from inside a dialog closes
 * first; the dialog stays).
 */
export const layerStack: ModalStack = createModalStack();
