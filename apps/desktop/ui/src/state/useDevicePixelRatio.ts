import { useEffect, useState } from "react";
import { currentRatio, watchDevicePixelRatio } from "../lib/devicePixelRatio";

/** The window's device pixel ratio, kept current while `enabled` (the live view is open). */
export function useDevicePixelRatio(enabled: boolean): number {
  const [ratio, setRatio] = useState(() => currentRatio(window));
  useEffect(() => {
    if (!enabled) {
      return undefined;
    }
    setRatio(currentRatio(window));
    return watchDevicePixelRatio(window, setRatio);
  }, [enabled]);
  return ratio;
}
