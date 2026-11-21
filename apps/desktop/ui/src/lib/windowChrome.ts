/** Window-chrome logic for the integrated title bar (no React, so it is testable). */

/** The slice of Tauri's Window the title bar uses. */
export interface ChromeWindow {
  isMaximized(): Promise<boolean>;
  onResized(handler: () => void): Promise<() => void>;
}

/** Windows draws the controls itself; elsewhere the native frame stays the default. */
export function defaultWindowFrame(userAgent: string): boolean {
  return !/Windows/i.test(userAgent);
}

/** i18n key for the maximize/restore button in the given state. */
export function maximizeLabelKey(maximized: boolean): "window.maximize" | "window.restore" {
  return maximized ? "window.restore" : "window.maximize";
}

/**
 * Follows the maximized state of a window. `onChange` fires with the current
 * state once at start and after every resize (maximize, restore, snap).
 * Returns a stop function that is safe to call before the listener is ready.
 */
export function trackMaximized(win: ChromeWindow, onChange: (maximized: boolean) => void): () => void {
  let stopped = false;
  let unlisten: (() => void) | undefined;
  let last: boolean | undefined;
  const refresh = () => {
    win.isMaximized().then((value) => {
      if (stopped || value === last) return;
      last = value;
      onChange(value);
    }).catch(() => {});
  };
  refresh();
  win.onResized(refresh).then((fn) => {
    if (stopped) fn();
    else unlisten = fn;
  }).catch(() => {});
  return () => {
    stopped = true;
    unlisten?.();
  };
}
