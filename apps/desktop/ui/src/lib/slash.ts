/**
 * Composer triggers: a `/` command at the very start of the message and an
 * `@` bot mention anywhere. Pure text logic; the menu UI lives in ChatView.
 */

/** Commands the daemon router understands as a message (`/new`, `/stop`). */
export interface SlashCommand {
  name: string;
  /** i18n key of the one-line description. */
  i18nKey: string;
  /** Other names that run the same command (`/görev` for `/mission`); the menu finds it by them too. */
  aliases?: readonly string[];
}

export const SLASH_COMMANDS: readonly SlashCommand[] = [
  { name: "new", i18nKey: "chat.slash.new" },
  { name: "stop", i18nKey: "chat.slash.stop" }
] as const;

/** Window event a composer command uses to ask the app shell to open something (detail: the target). */
export const APP_COMMAND_EVENT = "praxi:app-command";

/** Where an app command goes: the same targets the profile menu offers. */
export type AppCommandTarget = "help" | "settings" | "vault" | "apps" | "schedules" | "ai";

/** Commands the app itself runs (open a dialog or screen); they never reach the daemon. */
export interface AppCommand extends SlashCommand {
  target: AppCommandTarget;
}

export const APP_COMMANDS: readonly AppCommand[] = [
  { name: "help", i18nKey: "chat.slash.help", target: "help" },
  { name: "settings", i18nKey: "chat.slash.settings", target: "settings" },
  { name: "vault", i18nKey: "chat.slash.vault", target: "vault" },
  { name: "models", i18nKey: "chat.slash.models", target: "ai" },
  { name: "apps", i18nKey: "chat.slash.apps", target: "apps" },
  { name: "schedules", i18nKey: "chat.slash.schedules", target: "schedules" }
] as const;

/** Window event that puts text into the open chat's message box (detail: the text). */
export const COMPOSE_EVENT = "praxi:compose";

export function requestCompose(text: string): void {
  window.dispatchEvent(new CustomEvent<string>(COMPOSE_EVENT, { detail: text }));
}

/** `/mission <the job>` (Turkish `/görev`): the app starts a mission for this chat's bot; nothing goes to the model. */
export const MISSION_COMMAND: SlashCommand = { name: "mission", i18nKey: "missionChat.slash", aliases: ["görev", "gorev"] };

/** Commands the composer runs on the open chat itself (nothing is sent to the bot). */
export const LOCAL_COMMANDS: readonly SlashCommand[] = [{ name: "export", i18nKey: "chat.slash.export" }, MISSION_COMMAND] as const;

/** The mission command as the owner types it: `görev` in Turkish, `mission` elsewhere. */
export function missionCommandWord(lang: string): string {
  return lang.toLowerCase().startsWith("tr") ? "görev" : MISSION_COMMAND.name;
}

/**
 * The job of a `/mission` (or `/görev`) message: the text after the command, "" when there is
 * none yet, `null` when the message is not that command.
 */
export function missionCommandGoal(text: string): string | null {
  const parsed = parseSlashInput(text);
  if (!parsed || (parsed.name !== MISSION_COMMAND.name && !MISSION_COMMAND.aliases?.includes(parsed.name))) {
    return null;
  }
  return parsed.args;
}

/** True when the message is exactly `/export`. */
export function isExportCommand(text: string): boolean {
  const parsed = parseSlashInput(text);
  return parsed !== null && parsed.args === "" && parsed.name === "export";
}

/** The app command a message is, when it is exactly `/name` (no arguments); otherwise `null`. */
export function appCommandFor(text: string): AppCommand | null {
  const parsed = parseSlashInput(text);
  if (!parsed || parsed.args !== "") {
    return null;
  }
  return APP_COMMANDS.find((c) => c.name === parsed.name) ?? null;
}

/** Every command a message may start with: the daemon's (`/workflow` included), the app's and the composer's. */
const KNOWN_COMMAND_NAMES: ReadonlySet<string> = new Set([
  ...SLASH_COMMANDS.map((c) => c.name),
  ...APP_COMMANDS.map((c) => c.name),
  ...LOCAL_COMMANDS.flatMap((c) => [c.name, ...(c.aliases ?? [])]),
  "workflow"
]);

/**
 * The name when the message is a lone `/word` that no command answers (`/foobar`), else `null`.
 * Such a message would otherwise go to the model as a paid call.  Anything with arguments, paths
 * (`/usr/bin/ls`) or text around it is an ordinary message.
 */
export function unknownSlashCommand(text: string): string | null {
  const parsed = parseSlashInput(text);
  if (!parsed || parsed.args !== "" || KNOWN_COMMAND_NAMES.has(parsed.name)) {
    return null;
  }
  return parsed.name;
}

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
