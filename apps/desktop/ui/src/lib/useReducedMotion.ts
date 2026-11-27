import { useEffect, useState } from "react";
import { isReducedMotionPreferred, subscribeReducedMotion } from "ui";

/** The system asks for less motion, or the app's own reduce-motion setting is on. */
function prefersStill(): boolean {
  return (
    isReducedMotionPreferred() ||
    (typeof document !== "undefined" && document.documentElement.hasAttribute("data-reduce-motion"))
  );
}

/** Whether animations should be skipped; follows the system preference live. */
export function useReducedMotion(): boolean {
  const [still, setStill] = useState(prefersStill);
  useEffect(() => subscribeReducedMotion(() => setStill(prefersStill())), []);
  return still;
}
