import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauriRuntime } from "../overlay/events";

/** Events the Rust shell emits (tray menu, notification clicks). */
export const SHELL_EVENT_NOTIFICATION_ACTIVATED = "shell:notification_activated";
export const SHELL_EVENT_TRAY_OPEN_NEEDS = "shell:tray_open_needs";
export const SHELL_EVENT_TRAY_STOP_ALL = "shell:tray_stop_all";

export interface NativeNotification {
  title: string;
  body: string;
  /** Chat to open when the notification is clicked. */
  chatId?: string;
}

/** Whether the user is looking at the main window. */
export async function isAppFocused(): Promise<boolean> {
  if (isTauriRuntime()) {
    try {
      const win = getCurrentWindow();
      const [focused, minimized, visible] = await Promise.all([
        win.isFocused(),
        win.isMinimized(),
        win.isVisible()
      ]);
      return focused && !minimized && visible;
    } catch {
      return false;
    }
  }
  return typeof document !== "undefined" && document.hasFocus();
}

declare global {
  interface Window {
    /** Dev / browser fallback: notifications that would have been shown. */
    __praxiNotifications?: NativeNotification[];
  }
}

/** Show an OS notification (a native toast; a recorded stub in the browser). */
export async function showNotification(n: NativeNotification): Promise<void> {
  if (isTauriRuntime()) {
    await invoke("notify_show", {
      title: n.title,
      body: n.body,
      chatId: n.chatId ?? null
    });
    return;
  }
  (window.__praxiNotifications ??= []).push(n);
}

/** Tell the tray how many things wait for the user. */
export async function setNeedsCount(count: number): Promise<void> {
  if (isTauriRuntime()) {
    await invoke("set_needs_count", { count });
  }
}

/** Tray menu labels follow the UI language. */
export async function setTrayLabels(labels: {
  toggle: string;
  needs: string;
  stopAll: string;
  quit: string;
  tooltip: string;
  tooltipNeeds: string;
}): Promise<void> {
  if (isTauriRuntime()) {
    await invoke("set_tray_labels", { labels });
  }
}

/** Listen to a shell event; returns the unsubscribe. */
export function onShellEvent<T>(
  name: string,
  handler: (payload: T) => void
): () => void {
  if (!isTauriRuntime()) {
    return () => {};
  }
  let disposed = false;
  let unlisten: UnlistenFn | null = null;
  void listen<T>(name, (e) => handler(e.payload)).then((u) => {
    if (disposed) {
      u();
    } else {
      unlisten = u;
    }
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}
