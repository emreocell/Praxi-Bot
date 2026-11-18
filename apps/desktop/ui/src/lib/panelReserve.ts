/**
 * Timing of the panel reserve as a small controller with injected timers, so the
 * behaviour behind `usePanelReserve` is unit-tested without a renderer: reserved immediately on
 * open, kept for `delayMs()` after closing starts, restored at once when there is no transition
 * (delay 0), a reopen cancels the pending release and unmounting clears the timer.
 */
export interface PanelReserveOptions<T> {
  initialOpen: boolean;
  /** Read at the moment closing starts (token and reduced motion are live values). */
  delayMs: () => number;
  setTimer: (fn: () => void, ms: number) => T;
  clearTimer: (timer: T) => void;
  onChange: (reserved: boolean) => void;
}

export interface PanelReserve {
  update(open: boolean): void;
  dispose(): void;
  readonly reserved: boolean;
}

export function createPanelReserve<T>(o: PanelReserveOptions<T>): PanelReserve {
  let reserved = o.initialOpen;
  let timer: T | undefined;
  const cancel = () => {
    if (timer !== undefined) o.clearTimer(timer);
    timer = undefined;
  };
  const set = (v: boolean) => {
    if (v === reserved) return;
    reserved = v;
    o.onChange(v);
  };
  return {
    get reserved() {
      return reserved;
    },
    update(open) {
      if (open) {
        cancel();
        set(true);
        return;
      }
      if (!reserved || timer !== undefined) return;
      const delay = o.delayMs();
      if (delay <= 0) {
        set(false);
        return;
      }
      timer = o.setTimer(() => {
        timer = undefined;
        set(false);
      }, delay);
    },
    dispose: cancel
  };
}
