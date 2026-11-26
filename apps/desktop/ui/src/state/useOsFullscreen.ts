import { useCallback, useEffect, useRef, useState } from "react";
import { isOsFullscreen, setOsFullscreen, watchOsFullscreen } from "../lib/osFullscreen";

/**
 * True fullscreen of the app window while `enabled` (the live view is open). Leaving the view always
 * leaves fullscreen too, so closing the screen never strands the app in a mode the owner did not ask for.
 */
export function useOsFullscreen(enabled: boolean): {
  fullscreen: boolean;
  toggle: () => void;
  exit: () => void;
} {
  const [fullscreen, setFullscreen] = useState(false);
  const fullscreenRef = useRef(false);
  fullscreenRef.current = fullscreen;

  useEffect(() => {
    if (!enabled) {
      return undefined;
    }
    let cancelled = false;
    void isOsFullscreen().then((on) => !cancelled && setFullscreen(on));
    const stop = watchOsFullscreen((on) => !cancelled && setFullscreen(on));
    return () => {
      cancelled = true;
      stop();
    };
  }, [enabled]);

  // Only a fullscreen this hook entered is undone on close; a window the owner already had in
  // fullscreen before opening the view stays that way.
  const enteredHere = useRef(false);
  useEffect(() => {
    if (enabled) {
      return undefined;
    }
    if (enteredHere.current) {
      enteredHere.current = false;
      void setOsFullscreen(false).then(setFullscreen);
    }
    return undefined;
  }, [enabled]);

  const toggle = useCallback(() => {
    const next = !fullscreenRef.current;
    if (next) {
      enteredHere.current = true;
    }
    void setOsFullscreen(next).then(setFullscreen);
  }, []);
  const exit = useCallback(() => {
    void setOsFullscreen(false).then(setFullscreen);
  }, []);
  return { fullscreen, toggle, exit };
}
