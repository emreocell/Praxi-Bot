/**
 * The composer's round button (design v7): an arrow to send, a rounded-square STOP while the bot
 * works and the box is empty. With text in the box it is the arrow again (the message is queued).
 */
export type SendButtonMode = "send" | "stop";

export function sendButtonMode(input: { runActive: boolean; hasText: boolean }): SendButtonMode {
  return input.runActive && !input.hasText ? "stop" : "send";
}

/** The button cannot be pressed: nothing to send, a send in flight, or a stop already under way. */
export function sendButtonDisabled(input: {
  mode: SendButtonMode;
  sending: boolean;
  stopping: boolean;
}): boolean {
  return input.mode === "stop" ? input.stopping : input.sending;
}

/**
 * Which run the composer may still stop. A run the event stream has already
 * finished is not live, even when the last run list still says it is.
 */
export function activeRunAfterCancel(input: {
  listedLiveId: string | null;
  finishedByEvent: ReadonlySet<string>;
  eventLiveId: string | null;
  ackedId: string | null;
}): string | null {
  const listed =
    input.listedLiveId !== null && !input.finishedByEvent.has(input.listedLiveId)
      ? input.listedLiveId
      : null;
  if (listed) return listed;
  if (input.eventLiveId && !input.finishedByEvent.has(input.eventLiveId)) return input.eventLiveId;
  if (input.ackedId && !input.finishedByEvent.has(input.ackedId)) return input.ackedId;
  return null;
}

/** Send, Stop, or the short Stopping state. Stopping stays pressable: a second press is the hard stop. */
export type ComposerButton = "send" | "stop" | "stopping";

export function composerButton(input: {
  runActive: boolean;
  hasText: boolean;
  stopping: boolean;
}): ComposerButton {
  if (!input.runActive) return "send";
  if (input.stopping) return "stopping";
  if (input.hasText) return "send";
  return "stop";
}

/** Key caps of the composer's hint row and Send tooltip, for the "send with" setting. */
export interface ComposerKeys {
  /** Short cap in the hint row: "↵", "Ctrl ↵", "⌘ ↵". */
  send: string;
  /** Spelled out in the Send tooltip: "Enter", "Ctrl+Enter", "⌘+Enter". */
  sendName: string;
  /** The key that starts a new line instead. */
  newline: string;
}

export function composerKeys(sendWith: "enter" | "ctrl-enter", mac: boolean): ComposerKeys {
  if (sendWith === "ctrl-enter") {
    const mod = mac ? "⌘" : "Ctrl";
    return { send: `${mod} ↵`, sendName: `${mod}+Enter`, newline: "↵" };
  }
  return { send: "↵", sendName: "Enter", newline: "⇧ ↵" };
}

/** macOS (and iPadOS) name the modifier ⌘; everything else Ctrl. */
export function isMacPlatform(): boolean {
  try {
    return /Mac|iPhone|iPad/.test(globalThis.navigator?.platform || globalThis.navigator?.userAgent || "");
  } catch {
    return false;
  }
}
