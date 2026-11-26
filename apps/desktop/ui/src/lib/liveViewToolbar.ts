/** Pure helpers of the live view's toolbar: keyboard shortcuts, the remembered chat strip and the spoken status. */

export type ViewerShortcut =
  | "fit"
  | "takeover"
  | "fullscreen"
  | "zoomFit"
  | "zoomActual"
  | "zoomIn"
  | "zoomOut";

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
  { id: "fullscreen", keys: "F11" },
  { id: "zoomFit", keys: "0" },
  { id: "zoomActual", keys: "1" },
  { id: "zoomIn", keys: "+" },
  { id: "zoomOut", keys: "-" },
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
  if (key === "F11" && !ctrl && !event.altKey && !event.shiftKey) {
    return "fullscreen";
  }
  if (ctrl && event.shiftKey && !event.altKey && key === "d") {
    return "takeover";
  }
  if (ctrl || event.altKey) {
    return null;
  }
  if (key === "f" && !event.shiftKey) {
    return "fit";
  }
  // `key` is the character the keyboard produced, so Shift needs no guard of its own: it is how "+"
  // and "_" are typed on many layouts, and how "0" and "1" are typed on AZERTY.
  if (key === "0") return "zoomFit";
  if (key === "1") return "zoomActual";
  if (key === "+" || key === "=") return "zoomIn";
  if (key === "-" || key === "_") return "zoomOut";
  return null;
}

export type FitMode = "fit" | "fill";

export function toggleFit(mode: FitMode): FitMode {
  return mode === "fit" ? "fill" : "fit";
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
