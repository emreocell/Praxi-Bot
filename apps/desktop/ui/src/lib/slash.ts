/**
 * Composer triggers: a `/` command at the very start of the message and an
 * `@` bot mention anywhere. Pure text logic; the menu UI lives in ChatView.
 */
import { fold } from "./fuzzy.ts";

/** Commands the daemon router understands as a message (`/new`, `/stop`). */
export interface SlashCommand {
  name: string;
  /** i18n key of the one-line description. */
  i18nKey: string;
}

export const SLASH_COMMANDS: readonly SlashCommand[] = [
  { name: "new", i18nKey: "chat.slash.new" },
  { name: "stop", i18nKey: "chat.slash.stop" }
] as const;

export type Trigger =
  | { kind: "slash"; query: string; start: number; end: number }
  | { kind: "mention"; query: string; start: number; end: number };

/**
 * The trigger the caret is currently inside, or `null`.
 *
 * - slash: the message starts with `/` and the caret is still in that first
 *   word (`/ne|` yes, `/new |` no, `a /ne|` no).
 * - mention: the word before the caret starts with `@` at a word boundary.
 */
export function detectTrigger(text: string, caret: number): Trigger | null {
  const c = Math.max(0, Math.min(caret, text.length));
  if (text.startsWith("/")) {
    const head = text.slice(0, c);
    if (c >= 1 && !/\s/.test(head)) {
      return { kind: "slash", query: head.slice(1), start: 0, end: c };
    }
  }
  let i = c;
  while (i > 0 && !/\s/.test(text.charAt(i - 1))) {
    i--;
  }
  if (text.charAt(i) === "@" && i < c) {
    return { kind: "mention", query: text.slice(i + 1, c), start: i, end: c };
  }
  return null;
}

/** Replace the trigger text with `insert` (+ a space); returns the new caret. */
export function applyCompletion(
  text: string,
  trigger: Trigger,
  insert: string
): { text: string; caret: number } {
  const lead = trigger.kind === "slash" ? "/" : "@";
  const head = text.slice(0, trigger.start) + lead + insert + " ";
  // swallow one space already after the caret so we never double it
  return { text: head + text.slice(trigger.end).replace(/^ /, ""), caret: head.length };
}

/** `/name rest` -> `{ name, args }`; `null` for anything else. */
export function parseSlashInput(
  text: string
): { name: string; args: string } | null {
  const m = /^\/([\p{L}\p{N}_-]+)(?:\s+([\s\S]*))?$/u.exec(text.trim());
  return m ? { name: (m[1] ?? "").toLowerCase(), args: (m[2] ?? "").trim() } : null;
}

/** A bot's `@handle`: its name without whitespace. */
export function mentionHandle(name: string): string {
  return name.replace(/\s+/g, "");
}

/**
 * `@Atlas do the thing` -> the bot named Atlas and `do the thing`. Matching is
 * case/diacritic-insensitive; an unknown handle (or a bare `@x`) is `null`, so
 * the text is sent as-is.
 */
export function parseLeadingMention<B extends { name: string }>(
  text: string,
  bots: readonly B[]
): { bot: B; rest: string } | null {
  const m = /^\s*@(\S+)(?:\s+([\s\S]*))?$/.exec(text);
  if (!m) {
    return null;
  }
  const want = fold(m[1] ?? "");
  const bot = bots.find((b) => fold(mentionHandle(b.name)) === want);
  const rest = (m[2] ?? "").trim();
  return bot && rest ? { bot, rest } : null;
}
