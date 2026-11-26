import { useEffect, useMemo, useState } from "react";
import { createFullscreenController, osFullscreenBackend } from "../lib/osFullscreen";

/**
 * True fullscreen of the app window while `enabled` (the live view is open). Leaving the view, or
 * the view going away altogether (another computer, a reconnect, a closed pop-out), always leaves
 * fullscreen too, so the app is never stranded in a mode the owner did not ask for. The rules live
 * in `createFullscreenController`.
 *
 * While fullscreen, `<html data-os-fullscreen="true">` lets the custom window controls step aside.
 */
export function useOsFullscreen(enabled: boolean): {
  fullscreen: boolean;
  toggle: () => void;
  exit: () => void;
} {
  const [fullscreen, setFullscreen] = useState(false);
  const controller = useMemo(() => createFullscreenController(osFullscreenBackend, setFullscreen), []);

  // The cleanup runs both when `enabled` turns false and when the component unmounts.
  useEffect(() => {
    if (!enabled) {
      return undefined;
    }
    controller.enable();
    return () => controller.disable();
  }, [enabled, controller]);

  useEffect(() => {
    if (!fullscreen) {
      return undefined;
    }
    const root = document.documentElement;
    root.dataset.osFullscreen = "true";
    return () => {
      delete root.dataset.osFullscreen;
    };
  }, [fullscreen]);

  return { fullscreen, toggle: controller.toggle, exit: controller.exit };
}
