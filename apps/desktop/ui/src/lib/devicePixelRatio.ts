/**
 * Follows the window's device pixel ratio. A ratio change (the window moves to a screen with another
 * scale, the OS scale or the browser zoom changes) does not always resize the page, so a size
 * observer alone would leave 1:1 and the pixel-exact math on the old ratio. `matchMedia` with the
 * current ratio fires once when it stops matching; each change re-arms it for the new ratio.
 */

interface MediaListLike {
  addEventListener?: (type: "change", listener: () => void) => void;
  removeEventListener?: (type: "change", listener: () => void) => void;
}

export interface RatioWindow {
  devicePixelRatio: number;
  matchMedia?: (query: string) => MediaListLike;
}

/** The ratio as the math wants it: finite and positive, else 1. */
export function currentRatio(win: Pick<RatioWindow, "devicePixelRatio">): number {
  const r = win.devicePixelRatio;
  return Number.isFinite(r) && r > 0 ? r : 1;
}

/** Calls `onChange(ratio)` every time the ratio changes; returns the function that stops it. */
export function watchDevicePixelRatio(win: RatioWindow, onChange: (ratio: number) => void): () => void {
  let stop = () => {};
  let stopped = false;
  const arm = () => {
    if (stopped || typeof win.matchMedia !== "function") {
      return;
    }
    const ratio = currentRatio(win);
    const media = win.matchMedia(`(resolution: ${ratio}dppx)`);
    const fired = () => {
      media.removeEventListener?.("change", fired);
      if (stopped) {
        return;
      }
      onChange(currentRatio(win));
      arm();
    };
    media.addEventListener?.("change", fired);
    stop = () => media.removeEventListener?.("change", fired);
  };
  arm();
  return () => {
    stopped = true;
    stop();
  };
}
