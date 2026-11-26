import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauriRuntime } from "../overlay/events";

/**
 * True fullscreen of the app window (F11 style), for the live view's toolbar. The desktop shell asks
 * its own window; a plain browser falls back to the Fullscreen API. A failure (a refused request,
 * a missing grant) leaves the window as it is and returns the state that is true afterwards.
 */
export async function isOsFullscreen(): Promise<boolean> {
  try {
    if (isTauriRuntime()) {
      return await getCurrentWindow().isFullscreen();
    }
  } catch {
    return false;
  }
  return typeof document !== "undefined" && document.fullscreenElement !== null;
}

export async function setOsFullscreen(on: boolean): Promise<boolean> {
  try {
    if (isTauriRuntime()) {
      await getCurrentWindow().setFullscreen(on);
    } else if (on) {
      await document.documentElement.requestFullscreen();
    } else if (document.fullscreenElement) {
      await document.exitFullscreen();
    }
  } catch {
    // Refused (no user gesture, no permission): keep the current state.
  }
  return isOsFullscreen();
}

/** Calls `onChange` when the window enters or leaves fullscreen by any means (F11, the OS, Esc). */
export function watchOsFullscreen(onChange: (on: boolean) => void): () => void {
  if (typeof window === "undefined") {
    return () => {};
  }
  const refresh = () => void isOsFullscreen().then(onChange);
  window.addEventListener("resize", refresh);
  document.addEventListener("fullscreenchange", refresh);
  return () => {
    window.removeEventListener("resize", refresh);
    document.removeEventListener("fullscreenchange", refresh);
  };
}
