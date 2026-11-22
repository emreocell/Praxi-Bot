import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * Hides the app window from screen capture and recording (`setContentProtected`) while a
 * password form is open: Windows and macOS honour it, Linux ignores it. Reference counted so two
 * open cards keep it on until the last one closes. Outside the desktop app (browser preview,
 * tests) it does nothing, and a failure never blocks the form.
 */
let open = 0;

function apply(on: boolean): void {
  try {
    void getCurrentWindow()
      .setContentProtected(on)
      .catch(() => {});
  } catch {
    // not running inside the desktop shell
  }
}

/** Start hiding the window from capture; call the returned function when the form closes. */
export function shieldFromCapture(): () => void {
  open += 1;
  if (open === 1) apply(true);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    open -= 1;
    if (open === 0) apply(false);
  };
}
