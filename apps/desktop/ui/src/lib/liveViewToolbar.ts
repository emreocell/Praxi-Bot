/** Pure helpers of the live view's toolbar: keyboard shortcuts, the remembered chat strip and the spoken status. */

export type ViewerShortcut = "fit" | "takeover";

export interface ShortcutEvent {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  defaultPrevented: boolean;
  /** True when the key was typed into a text field, select or other editable element. */
  editable: boolean;
}

/** The shortcut listed in the "?" popover, in display order. `keys` are shown as typed. */
export const VIEWER_SHORTCUTS = [
  { id: "exit", keys: "Esc" },
  { id: "fit", keys: "F" },
  { id: "takeover", keys: "Ctrl+Shift+D" },
  { id: "release", keys: "Shift+Esc" }
] as const;

/**
 * What a key does in the viewer's toolbar layer. Nothing fires while the owner is driving the
 * remote screen (`interactive` or `stageFocused`): then every key belongs to that screen. Esc and
 * Shift+Esc keep their own handling elsewhere.
 */
export function viewerShortcut(
  event: ShortcutEvent,
  opts: { interactive: boolean; stageFocused: boolean }
): ViewerShortcut | null {
  if (event.defaultPrevented || event.editable || opts.interactive || opts.stageFocused) {
    return null;
  }
  const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
  const ctrl = event.ctrlKey || event.metaKey;
  if (ctrl && event.shiftKey && !event.altKey && key === "d") {
    return "takeover";
  }
  if (ctrl || event.altKey) {
    return null;
  }
  if (key === "f" && !event.shiftKey) {
    return "fit";
  }
  return null;
}

export type FitMode = "fit" | "fill";

export function toggleFit(mode: FitMode): FitMode {
  return mode === "fit" ? "fill" : "fit";
}

const CHAT_KEY = "praxi.liveView.chatOpen";

/** Whether the bottom-right chat strip was left open; a per-viewer convenience, collapsed by default. */
export function loadChatOpen(storage: Pick<Storage, "getItem"> | null = safeStorage()): boolean {
  try {
    return storage?.getItem(CHAT_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveChatOpen(open: boolean, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(CHAT_KEY, open ? "1" : "0");
  } catch {
    // Blocked storage: the choice just lasts for this session.
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** The line a screen reader announces about the picture, or null when all is well. */
export function viewerAnnouncementKey(s: {
  hasTarget: boolean;
  frameShown: boolean;
  disconnected: boolean;
  stalled: boolean;
}): "reconnecting" | "stalled" | "connecting" | null {
  if (s.disconnected) {
    return "reconnecting";
  }
  if (s.stalled) {
    return "stalled";
  }
  if (s.hasTarget && !s.frameShown) {
    return "connecting";
  }
  return null;
}
