/**
 * One list of everything that makes a bot act on its own: schedules (a time), mail and webhook
 * rules, and file watches the bot set up itself. Pure: the panel and the schedules screen read
 * the same rows, the same last result and the same one primary action.
 */
import type { Schedule, ScheduleHistoryEntry } from "../api/types";
import { historyStatusTone, sortSchedules, specLabel, type SpecLabel } from "./schedules.ts";
import type { BotTrigger } from "./triggers.ts";

export type AutomationKind = "time" | "mail" | "webhook" | "file" | "feed" | "page" | "calendar" | "home_assistant";

export type AutomationRow =
  | { key: string; kind: AutomationKind; name: string; enabled: boolean; schedule: Schedule; trigger?: undefined }
  | { key: string; kind: AutomationKind; name: string; enabled: boolean; trigger: BotTrigger; schedule?: undefined };

/**
 * Whether a row offers the one-click "I wrote this": a user-side schedule or stored rule whose
 * author the daemon could not prove (made before authorship was recorded, on a paired phone, or
 * moved from another computer). A bot-made row is never confirmable (editing it takes it over),
 * and neither is an event source that is not a stored rule yet. Only a Full bot shows it: the
 * confirmation matters only there, where an owner-written automation runs without cards.
 */
export function confirmableAuthor(row: AutomationRow, botIsFull: boolean): boolean {
  if (!botIsFull) return false;
  if (row.schedule) return row.schedule.origin === "user" && row.schedule.author === "unknown" && !row.schedule.moved_to;
  return row.trigger.origin === "rule" && row.trigger.author === "unknown";
}

/** What started a schedule: its event trigger (`file_watch`, `email`) or the clock. */
export function scheduleKind(s: Pick<Schedule, "trigger" | "has_webhook">): AutomationKind {
  const kind = (s.trigger as { kind?: unknown } | null | undefined)?.kind;
  if (kind === "file_watch") return "file";
  if (kind === "email") return "mail";
  return "time";
}

/**
 * The bot's automations: its schedules first (soonest next run first), then its rules.
 * The daemon also lists every schedule as an "existing" trigger (`origin: "schedule"`); those are
 * already here as schedules, with their next run and history, so they are not listed twice.
 */
export function botAutomationRows(
  botId: string,
  schedules: readonly Schedule[],
  triggers: readonly BotTrigger[]
): AutomationRow[] {
  const own = sortSchedules(schedules.filter((s) => s.bot_id === botId));
  const rows: AutomationRow[] = own.map((s) => ({
    key: `schedule:${s.id}`,
    kind: scheduleKind(s),
    name: s.name,
    enabled: s.enabled,
    schedule: s
  }));
  for (const t of triggers) {
    if (t.origin === "schedule") continue;
    rows.push({ key: `trigger:${t.id}`, kind: t.config.source.kind, name: t.name, enabled: t.enabled, trigger: t });
  }
  return rows;
}

export type OutcomeTone = "ok" | "failed" | "missed" | "running";

export interface Outcome {
  tone: OutcomeTone;
  /** The raw status, for the `schedules.run.*` label. */
  status: string;
  /** When it ran (or was due), if known. */
  atMs: number | null;
  entry: ScheduleHistoryEntry;
}

/** The newest entry of a history (already sorted newest first), as a result line. */
export function lastOutcome(history: readonly ScheduleHistoryEntry[]): Outcome | null {
  const entry = history[0];
  if (!entry) return null;
  const tone = historyStatusTone(entry.status);
  return {
    tone: tone === "succeeded" ? "ok" : tone,
    status: entry.status,
    atMs: entry.started_at_ms ?? entry.due_ms ?? null,
    entry
  };
}

/**
 * `specLabel` without the time zone when it is the one this computer uses: "Every weekday at
 * 09:00", not "… (Europe/Istanbul)". Another zone stays visible, because then it matters.
 */
export function localSpecLabel(
  spec: unknown,
  formatDate: (ms: number) => string,
  locale: string | undefined,
  localZone: string
): SpecLabel {
  const label = specLabel(spec, formatDate, locale);
  const zone = label.values.zone;
  return typeof zone === "string" && zone.trim() === `(${localZone})`
    ? { ...label, values: { ...label.values, zone: "" } }
    : label;
}

/** "today" / "tomorrow" / "yesterday" for a moment near `now` (local days), else `null`. */
export function dayWord(ms: number, now: number = Date.now()): "today" | "tomorrow" | "yesterday" | null {
  const day = (t: number) => {
    const d = new Date(t);
    return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()) / 86_400_000;
  };
  const diff = day(ms) - day(now);
  return diff === 0 ? "today" : diff === 1 ? "tomorrow" : diff === -1 ? "yesterday" : null;
}

export type PrimaryAction =
  | { kind: "reclaim" }
  | { kind: "retry"; dueMs: number }
  | { kind: "runNow" };

/**
 * The one button a schedule row shows. A schedule that moved to another computer can only be
 * taken back; a failed or missed occurrence that can still run is retried; otherwise "Run now".
 */
export function primaryAction(s: Pick<Schedule, "moved_to">, last: Outcome | null): PrimaryAction {
  if (s.moved_to) return { kind: "reclaim" };
  const due = last?.entry.due_ms;
  if (last && last.tone !== "ok" && last.tone !== "running" && last.entry.retryable && due !== null && due !== undefined) {
    return { kind: "retry", dueMs: due };
  }
  return { kind: "runNow" };
}
