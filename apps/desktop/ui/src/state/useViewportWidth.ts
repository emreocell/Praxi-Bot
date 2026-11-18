import { useEffect, useState } from "react";

/** How long after the last resize event the panel's width transition stays switched off. */
const RESIZE_QUIET_MS = 150;

/**
 * Window width.  While the window is being resized the panel's width transition is disabled
 * (`data-window-resizing`), so the panel and the fitted rail jump together instead of the rail
 * following the new target while the panel still slides from its old width.
 */
export function useViewportWidth(): number {
  const [width, setWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const root = document.documentElement;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onResize = () => {
      root.dataset.windowResizing = "true";
      setWidth(window.innerWidth);
      clearTimeout(timer);
      timer = setTimeout(() => delete root.dataset.windowResizing, RESIZE_QUIET_MS);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      clearTimeout(timer);
      delete root.dataset.windowResizing;
    };
  }, []);
  return width;
}
