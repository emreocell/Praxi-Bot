import { useEffect, useRef, useState } from "react";
import { createPanelReserve, type PanelReserve } from "../lib/panelReserve";
import { parseDurationMs, reserveDelayMs } from "../lib/shellLayout";

/** The panel's slide duration (the token the CSS transition uses) and the reduced-motion preference, read live. */
function currentDelayMs(): number {
  const slide = parseDurationMs(getComputedStyle(document.documentElement).getPropertyValue("--motion-duration-slow"));
  const reduced = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  return reserveDelayMs(slide, reduced);
}

/**
 * Whether the right panel still takes room from the rail's fit: true while it is open and for the
 * panel's slide after it started closing, so the rail does not grow back into the panel's space
 * mid-slide and squeeze the chat.  The timing lives in `createPanelReserve`.
 */
export function usePanelReserve(open: boolean): boolean {
  const [settling, setSettling] = useState(open);
  const ctl = useRef<PanelReserve | null>(null);
  if (ctl.current === null) {
    ctl.current = createPanelReserve({
      initialOpen: open,
      delayMs: currentDelayMs,
      setTimer: (fn, ms) => setTimeout(fn, ms),
      clearTimer: (t) => clearTimeout(t),
      onChange: setSettling
    });
  }
  useEffect(() => {
    ctl.current?.update(open);
  }, [open]);
  useEffect(() => () => ctl.current?.dispose(), []);
  return open || settling;
}
