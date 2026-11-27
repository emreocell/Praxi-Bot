import type { ApiClient } from "../api/client.ts";
import type {
  Bot,
  ImportApp,
  ImportItem,
  ImportReason,
  ImportResult,
  ImportRunInput,
  ImportScanInput
} from "../api/types.ts";
import { conflictBot, profilePatch } from "./botPatch.ts";
import { draftFromBot } from "./botProfile.ts";

/**
 * "Import from other assistants": the pure rules of the screen (what is ticked at first, which
 * secrets the owner confirmed, how instruction text is added to a bot). The daemon does the
 * scanning and importing; it never sends a secret value to the app, and neither does this file.
 */

export const IMPORT_APPS: readonly ImportApp[] = [
  "claude",
  "cursor",
  "vscode",
  "codex",
  "openclaw",
  "hermes",
  "folder"
];

/** The apps the app can name in the intro (the folder is the owner's own path, not an app). */
export const IMPORT_NAMED_APPS: readonly ImportApp[] = IMPORT_APPS.filter((a) => a !== "folder");

/** Longest bot persona the daemon accepts (`MAX_PERSONA_CHARS`). */
export const PERSONA_MAX_CHARS = 8000;

const KNOWN_REASONS: readonly ImportReason[] = [
  "ready",
  "ready_with_secrets",
  "already_have",
  "duplicate",
  "unpinned",
  "never_list",
  "unsupported",
  "invalid",
  "unreadable",
  "secret_in_command",
  "variables",
  "review_first",
  "too_long",
  "never_copied",
  "not_imported",
  "not_found",
  "failed",
  "secret_failed"
];

/** A reason code this app has words for (a newer daemon may send one it does not know yet). */
export function knownReason(reason: string | null | undefined): ImportReason | null {
  return KNOWN_REASONS.find((r) => r === reason) ?? null;
}

/** Items the owner can tick: the daemon says so and the kind is one that is imported. */
export function isImportable(item: ImportItem): boolean {
  return item.import && item.kind !== "other";
}

export interface ImportGroups {
  mcp: ImportItem[];
  skills: ImportItem[];
  instructions: ImportItem[];
  /** `import: false` items and kind "other": listed with their reason, never ticked. */
  notImported: ImportItem[];
}

/** Items by kind, in scan order; everything that cannot be imported goes to one list. */
export function groupItems(items: readonly ImportItem[]): ImportGroups {
  const groups: ImportGroups = { mcp: [], skills: [], instructions: [], notImported: [] };
  for (const item of items) {
    if (!isImportable(item)) groups.notImported.push(item);
    else if (item.kind === "mcp_server") groups.mcp.push(item);
    else if (item.kind === "skill") groups.skills.push(item);
    else groups.instructions.push(item);
  }
  return groups;
}

/** What is ticked when the list first shows: importable items the daemon marks `default_on`. */
export function initialTicks(items: readonly ImportItem[]): Set<string> {
  return new Set(items.filter((i) => isImportable(i) && i.default_on).map((i) => i.id));
}

/** Secret slots start unticked: the owner confirms each one. */
export function initialSecretTicks(): Set<string> {
  return new Set();
}

export function toggled(set: ReadonlySet<string>, id: string, on: boolean): Set<string> {
  const next = new Set(set);
  if (on) next.add(id);
  else next.delete(id);
  return next;
}

/** Ids to import: ticked and importable, in scan order (never an id the daemon did not offer). */
export function selectedIds(items: readonly ImportItem[], ticks: ReadonlySet<string>): string[] {
  return items.filter((i) => isImportable(i) && ticks.has(i.id)).map((i) => i.id);
}

/**
 * Secret names the owner confirmed. A slot counts only while its item is ticked, so unticking an
 * item can never leave a confirmation behind.
 */
export function confirmedSecrets(
  items: readonly ImportItem[],
  ticks: ReadonlySet<string>,
  secretTicks: ReadonlySet<string>
): string[] {
  const out: string[] = [];
  for (const item of items) {
    if (!isImportable(item) || !ticks.has(item.id)) continue;
    for (const slot of item.secrets) {
      if (secretTicks.has(slot.name) && !out.includes(slot.name)) out.push(slot.name);
    }
  }
  return out;
}

/** Ticked instruction items that still have no bot chosen (the Import button waits for these). */
export function instructionsWithoutBot(
  items: readonly ImportItem[],
  ticks: ReadonlySet<string>,
  botFor: Readonly<Record<string, string>>,
  botIds: readonly string[]
): ImportItem[] {
  return items.filter(
    (i) =>
      isImportable(i) &&
      i.kind === "instructions" &&
      ticks.has(i.id) &&
      !botIds.includes(botFor[i.id] ?? "")
  );
}

/** The request body of `POST /v1/import/run`. */
export function buildRunInput(
  scan: ImportScanInput,
  items: readonly ImportItem[],
  ticks: ReadonlySet<string>,
  secretTicks: ReadonlySet<string>
): ImportRunInput {
  return {
    ...(scan.only ? { only: scan.only } : {}),
    ...(scan.folder ? { folder: scan.folder } : {}),
    ids: selectedIds(items, ticks),
    confirmed_secrets: confirmedSecrets(items, ticks, secretTicks)
  };
}

/** The scan request: nothing but the folder the owner typed (blank = no folder). */
export function buildScanInput(folder: string, only?: ImportApp): ImportScanInput {
  const trimmed = folder.trim();
  return {
    ...(only ? { only } : {}),
    ...(trimmed ? { folder: trimmed } : {})
  };
}

// --- instructions: appended to a bot's persona -----------------------------------------------

/** Characters the way the daemon counts them (code points, not UTF-16 units). */
export function charCount(text: string): number {
  return Array.from(text).length;
}

export type PersonaAppend = { ok: true; value: string } | { ok: false; reason: "too_long" | "empty" };

/** Old persona + two blank-line breaks + the new text; refused (not cut) when it would not fit. */
export function appendPersona(old: string, text: string): PersonaAppend {
  if (text.trim() === "") return { ok: false, reason: "empty" };
  const value = old.trim() === "" ? text : `${old}\n\n${text}`;
  return charCount(value) > PERSONA_MAX_CHARS ? { ok: false, reason: "too_long" } : { ok: true, value };
}

/** What happened to an instruction text after the daemon returned it. */
export type PersonaOutcome =
  | { kind: "saved"; bot: Bot }
  | { kind: "too_long" }
  | { kind: "empty" };

/**
 * Add `text` to the end of the bot's persona with the same field-level patch the bot dialog
 * uses (`PATCH /v1/bots/{id}`, only `/bot/persona`). A stale version reloads the bot once and
 * appends to the fresh persona; a second conflict is thrown.
 */
export async function appendToBotPersona(
  client: Pick<ApiClient, "getBot" | "patchBot">,
  bot: Bot,
  text: string
): Promise<PersonaOutcome> {
  let current = bot;
  for (let attempt = 0; ; attempt += 1) {
    const opened = draftFromBot(current);
    const next = appendPersona(opened.persona, text);
    if (!next.ok) return { kind: next.reason };
    const patch = profilePatch(opened, { ...opened, persona: next.value });
    try {
      const saved = await client.patchBot(current.id, {
        expected_version: current.version,
        ops: patch.ops
      });
      return { kind: "saved", bot: saved };
    } catch (e) {
      if (attempt >= 1 || (e as { status?: number } | null)?.status !== 409) throw e;
      current = conflictBot(e) ?? (await client.getBot(current.id));
    }
  }
}

/** A result as the screen shows it: the daemon's, plus what the app itself did with instructions. */
export interface ImportOutcome extends ImportResult {
  /** Set by the app when its own write to the bot decided the outcome. */
  local?: "persona_saved" | "persona_too_long" | "persona_failed";
  botName?: string;
}

/**
 * Finish the instruction results: for each imported `instructions` item with text, add the text
 * to the chosen bot. Everything else passes through unchanged. A failure on one item never
 * stops the others.
 */
export async function applyInstructionResults(
  client: Pick<ApiClient, "getBot" | "patchBot">,
  items: readonly ImportItem[],
  results: readonly ImportResult[],
  botFor: Readonly<Record<string, string>>,
  bots: readonly Bot[],
  botName: (bot: Bot) => string = (b) => b.name
): Promise<{ outcomes: ImportOutcome[]; bots: Bot[] }> {
  const latest = new Map(bots.map((b) => [b.id, b]));
  const outcomes: ImportOutcome[] = [];
  for (const result of results) {
    const item = items.find((i) => i.id === result.id);
    if (!item || item.kind !== "instructions" || result.status !== "imported") {
      outcomes.push({ ...result });
      continue;
    }
    const bot = latest.get(botFor[item.id] ?? "");
    if (!bot || !result.text) {
      outcomes.push({ ...result, status: "failed", local: "persona_failed" });
      continue;
    }
    try {
      const done = await appendToBotPersona(client, bot, result.text);
      if (done.kind === "saved") {
        latest.set(done.bot.id, done.bot);
        outcomes.push({ ...result, local: "persona_saved", botName: botName(done.bot) });
      } else {
        outcomes.push({
          ...result,
          status: "failed",
          local: done.kind === "too_long" ? "persona_too_long" : "persona_failed",
          botName: botName(bot)
        });
      }
    } catch {
      outcomes.push({ ...result, status: "failed", local: "persona_failed", botName: botName(bot) });
    }
  }
  return { outcomes, bots: [...latest.values()] };
}

export interface ImportTally {
  imported: number;
  skipped: number;
  failed: number;
}

export function tally(outcomes: readonly ImportResult[]): ImportTally {
  const t: ImportTally = { imported: 0, skipped: 0, failed: 0 };
  for (const o of outcomes) t[o.status] += 1;
  return t;
}

/** True when at least one MCP server was imported (they arrive switched off, so the screen says where to switch them on). */
export function importedServers(items: readonly ImportItem[], outcomes: readonly ImportResult[]): boolean {
  return outcomes.some(
    (o) => o.status === "imported" && items.find((i) => i.id === o.id)?.kind === "mcp_server"
  );
}
