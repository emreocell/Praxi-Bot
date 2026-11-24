import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Connection } from "../api/connection";

// The only native commands the Quick ask window may call (capabilities/quick-ask.json).
// Keep every native call of this window in this file.

/** Event the shell sends each time the window is shown. */
export const QUICK_ASK_SHOWN = "quick-ask:shown";

/** The daemon address and token (never the owner key). */
export function quickAskConnect(): Promise<Connection> {
  return invoke<Connection>("quick_ask_connect");
}

/** Hide this window. */
export function quickAskHide(): Promise<void> {
  return invoke("quick_ask_hide");
}

/** Show the main window on this chat and hide this window. */
export function quickAskOpenInApp(chatId: string | null): Promise<void> {
  return invoke("quick_ask_open_in_app", { chatId });
}

/** Run `handler` every time the shell shows the window; returns the unsubscribe. */
export function onQuickAskShown(handler: () => void): () => void {
  let off: (() => void) | null = null;
  let disposed = false;
  void listen(QUICK_ASK_SHOWN, handler).then((u) => {
    if (disposed) u();
    else off = u;
  });
  return () => {
    disposed = true;
    off?.();
  };
}
