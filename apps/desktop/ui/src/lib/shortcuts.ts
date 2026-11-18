/**
 * Registry behind the keyboard shortcuts dialog (HelpModal). Every entry names the file that
 * implements it; `shortcuts.test.ts` checks that file still exists and that every source file
 * with key handlers is either listed here or explicitly exempted there.
 */
export type ShortcutGroup = "global" | "composer" | "dialogs";

/** `@send`, `@newline` and `@stop` are resolved at render time from settings. */
export type ShortcutKeys = string;

export interface Shortcut {
  id: string;
  group: ShortcutGroup;
  /** i18n key under `help.` (resolved with `t`). */
  labelKey: string;
  keys: ShortcutKeys;
  /** Path under `src/` of the handler this entry documents. */
  source: string;
}

export const SHORTCUT_GROUPS: readonly ShortcutGroup[] = ["global", "composer", "dialogs"];

export const SHORTCUTS: readonly Shortcut[] = [
  { id: "palette", group: "global", labelKey: "help.palette", keys: "Ctrl/Cmd+K", source: "state/PaletteHost.tsx" },
  { id: "settings", group: "global", labelKey: "help.settings", keys: "Ctrl/Cmd+,", source: "App.tsx" },
  { id: "stop", group: "global", labelKey: "help.stop", keys: "@stop", source: "App.tsx" },
  { id: "closePanel", group: "global", labelKey: "help.closePanel", keys: "Esc", source: "App.tsx" },
  { id: "exitLiveFocus", group: "global", labelKey: "help.exitLiveFocus", keys: "Shift+Esc", source: "lib/liveViewStage.ts" },
  { id: "send", group: "composer", labelKey: "help.send", keys: "@send", source: "lib/settingsPrefs.ts" },
  { id: "newline", group: "composer", labelKey: "help.newline", keys: "@newline", source: "lib/settingsPrefs.ts" },
  { id: "recall", group: "composer", labelKey: "help.recall", keys: "Up / Down", source: "lib/composerHistory.ts" },
  { id: "menuMove", group: "composer", labelKey: "help.menuMove", keys: "Up / Down", source: "views/ChatView.tsx" },
  { id: "menuAccept", group: "composer", labelKey: "help.menuAccept", keys: "Enter / Tab", source: "views/ChatView.tsx" },
  { id: "menuDismiss", group: "composer", labelKey: "help.menuDismiss", keys: "Esc", source: "views/ChatView.tsx" },
  { id: "dialogClose", group: "dialogs", labelKey: "help.dialogClose", keys: "Esc", source: "components/Modal.tsx" },
  { id: "dialogFocus", group: "dialogs", labelKey: "help.dialogFocus", keys: "Tab / Shift+Tab", source: "components/Modal.tsx" },
  { id: "paletteMove", group: "dialogs", labelKey: "help.paletteMove", keys: "Up / Down, Enter", source: "components/CommandPalette.tsx" },
  { id: "menuNav", group: "dialogs", labelKey: "help.menuNav", keys: "Up / Down, Home / End", source: "components/Popover.tsx" },
  { id: "popoverClose", group: "dialogs", labelKey: "help.popoverClose", keys: "Esc", source: "components/Popover.tsx" }
];

/** Resolve the `@...` placeholders to the keys the user actually has configured. */
export function resolveKeys(
  keys: ShortcutKeys,
  ctx: { stopHotkey: string; sendWith: "enter" | "ctrl-enter" }
): string {
  if (keys === "@stop") return ctx.stopHotkey;
  if (keys === "@send") return ctx.sendWith === "enter" ? "Enter" : "Ctrl/Cmd+Enter";
  if (keys === "@newline") return ctx.sendWith === "enter" ? "Shift+Enter" : "Enter";
  return keys;
}
