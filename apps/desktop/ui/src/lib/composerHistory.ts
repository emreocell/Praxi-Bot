/**
 * Recalling earlier messages in the composer (card 279): Up in an empty composer, or with the
 * caret on its first line, puts the previous message of THIS chat into the box (most recent
 * first); Down walks back towards the draft, which is kept; Esc restores it. Recall only: there
 * is no edit-and-resend. The slash / @mention popup keeps its own arrows, and nothing happens
 * while an IME is composing.
 */

export interface HistoryNav {
  /** Position in the chat's own messages (0 = oldest) while browsing; `null` = the draft. */
  index: number | null;
  /** What was in the box before browsing started. */
  draft: string;
}

export const FRESH: HistoryNav = { index: null, draft: "" };

export interface HistoryKey {
  key: string;
  shift: boolean;
  ctrl: boolean;
  meta: boolean;
  alt: boolean;
  composing: boolean;
  menuOpen: boolean;
  text: string;
  selStart: number;
  selEnd: number;
}

export interface HistoryResult {
  nav: HistoryNav;
  /** The new composer text. */
  text: string;
  /** Where the caret goes. */
  caret: number;
}

/** The chat's own sent messages, oldest first, without empties and immediate repeats. */
export function userMessageTexts(messages: ReadonlyArray<{ role: string; content: string }>): string[] {
  const out: string[] = [];
  for (const m of messages) {
    const text = m.content.trim();
    if (m.role === "user" && text !== "" && out[out.length - 1] !== text) {
      out.push(text);
    }
  }
  return out;
}

/**
 * What a key does to the composer, or `null` when the key is not ours (the browser's own
 * behaviour, or a popup, then applies).
 */
export function historyKey(nav: HistoryNav, history: readonly string[], k: HistoryKey): HistoryResult | null {
  // Popup open, IME composing, a modifier held or a range selected: not a recall.
  if (k.menuOpen || k.composing || k.shift || k.ctrl || k.meta || k.alt || k.selStart !== k.selEnd) {
    return null;
  }
  const browsing = nav.index !== null;
  if (k.key === "Escape") {
    return browsing ? { nav: FRESH, text: nav.draft, caret: nav.draft.length } : null;
  }
  if (k.key === "ArrowUp") {
    const onFirstLine = !k.text.slice(0, k.selStart).includes("\n");
    if (history.length === 0 || !onFirstLine) {
      return null;
    }
    const next = nav.index === null ? history.length - 1 : nav.index - 1;
    if (next < 0) {
      return null;
    }
    // The caret goes to the start so a further Up keeps walking back.
    return { nav: { index: next, draft: nav.index === null ? k.text : nav.draft }, text: history[next], caret: 0 };
  }
  if (k.key === "ArrowDown") {
    const onLastLine = !k.text.slice(k.selEnd).includes("\n");
    if (nav.index === null || !onLastLine) {
      return null;
    }
    const next = nav.index + 1;
    if (next >= history.length) {
      return { nav: FRESH, text: nav.draft, caret: nav.draft.length };
    }
    return { nav: { index: next, draft: nav.draft }, text: history[next], caret: history[next].length };
  }
  return null;
}

/** Typing while browsing makes what is in the box the new draft. */
export function afterEdit(nav: HistoryNav, text: string): HistoryNav {
  return nav.index === null ? nav : { index: null, draft: text };
}
