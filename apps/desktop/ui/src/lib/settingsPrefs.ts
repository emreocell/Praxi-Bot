/** Which key chord sends a chat message. */
export type SendWith = "enter" | "ctrl-enter";

/** Where the bot's hands (computer / browser) run. */
export type ComputerTarget = "this-pc" | "browser" | "remote" | "off";

export const SEND_WITH_VALUES: readonly SendWith[] = ["enter", "ctrl-enter"];
export const COMPUTER_TARGETS: readonly ComputerTarget[] = [
  "this-pc",
  "browser",
  "remote",
  "off"
];

/** Alternate targets have no daemon API wiring yet. */
export function isComputerTargetAvailable(target: ComputerTarget): boolean {
  return target === "this-pc";
}

interface KeyLike {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  isComposing?: boolean;
}

/**
 * Whether a keydown in the composer should send the message.
 * `enter`: Enter sends, Shift+Enter breaks the line.
 * `ctrl-enter`: Ctrl/Cmd+Enter sends, Enter breaks the line.
 */
export function shouldSend(e: KeyLike, sendWith: SendWith): boolean {
  if (e.key !== "Enter" || e.isComposing) {
    return false;
  }
  if (sendWith === "ctrl-enter") {
    return e.ctrlKey || e.metaKey;
  }
  return !e.shiftKey && !e.ctrlKey && !e.metaKey;
}

/** Narrow a stored value to a known `SendWith` (defaults to `enter`). */
export function parseSendWith(v: unknown): SendWith {
  return v === "ctrl-enter" ? "ctrl-enter" : "enter";
}

/** Discard preview-only targets so old settings cannot imply an unenforced restriction. */
export function parseComputerTarget(v: unknown): ComputerTarget {
  return COMPUTER_TARGETS.find((t) => t === v && isComputerTargetAvailable(t)) ?? "this-pc";
}
