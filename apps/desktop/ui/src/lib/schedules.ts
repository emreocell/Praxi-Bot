import type {
  Schedule,
  ScheduleHistoryEntry,
  ScheduleInput
} from "../api/types";
import type { RunState } from "../state/timeline.ts";
import {
  DEFAULT_SIMPLE,
  type SimpleSchedule
} from "./scheduleBuilder.ts";

/** `ScheduleDto::spec`: when a schedule fires. */
export type ScheduleSpec =
  | { cron: string }
  | { every: { seconds: number } }
  | { at: { unix_ms: number } };

/** `ScheduleDto::payload`: what a schedule does. */
export type SchedulePayload =
  | { kind: "agent_prompt"; text: string }
  | { kind: "workflow"; name: string; params?: unknown }
  | { kind: "tool"; name: string; input?: unknown }
  | { kind: "unknown" };

export type MisfirePolicy = "skip" | "run_once" | "run_all";
export const MISFIRE_POLICIES: readonly MisfirePolicy[] = [
  "skip",
  "run_once",
  "run_all"
] as const;

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

/** Read the (untrusted) `spec` JSON, or `null` for a shape this UI does not know. */
export function parseSpec(raw: unknown): ScheduleSpec | null {
  if (!isObj(raw)) {
    return null;
  }
  if (typeof raw.cron === "string") {
    return { cron: raw.cron };
  }
  if (isObj(raw.every) && typeof raw.every.seconds === "number") {
    return { every: { seconds: raw.every.seconds } };
  }
  if (isObj(raw.at) && typeof raw.at.unix_ms === "number") {
    return { at: { unix_ms: raw.at.unix_ms } };
  }
  return null;
}

/** Read the (untrusted) `payload` JSON. */
export function parsePayload(raw: unknown): SchedulePayload {
  if (!isObj(raw)) {
    return { kind: "unknown" };
  }
  if (raw.kind === "agent_prompt" && typeof raw.text === "string") {
    return { kind: "agent_prompt", text: raw.text };
  }
  if (raw.kind === "workflow" && typeof raw.name === "string") {
    return { kind: "workflow", name: raw.name, params: raw.params };
  }
  if (raw.kind === "tool" && typeof raw.name === "string") {
    return { kind: "tool", name: raw.name, input: raw.input };
  }
  return { kind: "unknown" };
}

/** Picks the largest unit that divides `seconds` exactly. */
export function intervalParts(seconds: number): {
  unit: "days" | "hours" | "minutes" | "seconds";
  value: number;
} {
  if (seconds > 0 && seconds % 86400 === 0) {
    return { unit: "days", value: seconds / 86400 };
  }
  if (seconds > 0 && seconds % 3600 === 0) {
    return { unit: "hours", value: seconds / 3600 };
  }
  if (seconds > 0 && seconds % 60 === 0) {
    return { unit: "minutes", value: seconds / 60 };
  }
  return { unit: "seconds", value: seconds };
}

export interface SpecLabel {
  /** i18n key (`schedules.spec.*`) and its values. */
  i18nKey: string;
  values: Record<string, string | number>;
}

/** The label of a spec: `cron`, `every` (with unit), `at` or `unknown`. */
export function specLabel(
  raw: unknown,
  formatDate: (ms: number) => string
): SpecLabel {
  const spec = parseSpec(raw);
  if (!spec) {
    return { i18nKey: "schedules.spec.unknown", values: {} };
  }
  if ("cron" in spec) {
    return { i18nKey: "schedules.spec.cron", values: { cron: spec.cron } };
  }
  if ("every" in spec) {
    const p = intervalParts(spec.every.seconds);
    return {
      i18nKey: `schedules.spec.every.${p.unit}`,
      values: { count: p.value }
    };
  }
  return {
    i18nKey: "schedules.spec.at",
    values: { when: formatDate(spec.at.unix_ms) }
  };
}

export type ScheduleStatus = "enabled" | "disabled" | "finished";

/** A disabled schedule is paused; an enabled one without a next firing is done. */
export function scheduleStatus(
  s: Pick<Schedule, "enabled" | "next_run_at_ms">
): ScheduleStatus {
  if (!s.enabled) {
    return "disabled";
  }
  return s.next_run_at_ms === null || s.next_run_at_ms === undefined
    ? "finished"
    : "enabled";
}

/** Soonest next firing first, schedules without one last; ties by name. */
export function sortSchedules(list: readonly Schedule[]): Schedule[] {
  return [...list].sort((a, b) => {
    const an = a.enabled ? (a.next_run_at_ms ?? Infinity) : Infinity;
    const bn = b.enabled ? (b.next_run_at_ms ?? Infinity) : Infinity;
    if (an !== bn) {
      return an < bn ? -1 : 1;
    }
    return a.name.localeCompare(b.name);
  });
}

/** Recent runs started by schedule `id` (newest first), from the event state. */
export function runsOfSchedule(
  runs: Record<string, RunState>,
  id: string,
  limit = 5
): RunState[] {
  return Object.values(runs)
    .filter((r) => r.scheduleId === id && r.parentRunId === undefined)
    .sort((a, b) => b.startedMs - a.startedMs)
    .slice(0, limit);
}

export type RunOutcome = "running" | "succeeded" | "failed";

export function runOutcome(
  r: Pick<RunState, "finishedMs" | "error">
): RunOutcome {
  if (r.finishedMs === undefined) {
    return "running";
  }
  return r.error ? "failed" : "succeeded";
}

/** Whether a daemon event means the schedule list may have changed. */
export function affectsSchedules(event: {
  type: string;
  schedule_id?: string | null;
}): boolean {
  return (
    (event.type === "run_started" && !!event.schedule_id) ||
    event.type === "run_finished"
  );
}

export type ScheduleKind = "simple" | "natural" | "cron" | "keep";

export interface ScheduleDraft {
  name: string;
  botId: string;
  /**
   * `simple` = the chips and time picker, `natural` = phrase parsed by the daemon,
   * `cron` = raw expression (Advanced), `keep` = edit without changing the stored `spec`.
   */
  kind: ScheduleKind;
  simple: SimpleSchedule;
  text: string;
  cron: string;
  tz?: string;
  prompt: string;
  enabled: boolean;
  misfire: MisfirePolicy;
}

export function emptyDraft(botId: string): ScheduleDraft {
  return {
    name: "",
    botId,
    kind: "simple",
    simple: { ...DEFAULT_SIMPLE },
    text: "",
    cron: "",
    prompt: "",
    enabled: true,
    misfire: "skip"
  };
}

export function draftFromSchedule(s: Schedule): ScheduleDraft {
  const spec = parseSpec(s.spec);
  const payload = parsePayload(s.payload);
  const misfire =
    MISFIRE_POLICIES.find((m) => m === s.misfire_policy) ?? "skip";
  return {
    name: s.name,
    botId: s.bot_id,
    kind: spec && "cron" in spec ? "cron" : "keep",
    simple: { ...DEFAULT_SIMPLE },
    text: "",
    cron: spec && "cron" in spec ? spec.cron : "",
    prompt: payload.kind === "agent_prompt" ? payload.text : "",
    enabled: s.enabled,
    misfire
  };
}

export type DraftError = "name" | "bot" | "text" | "cron" | "prompt";

/** Whether an edit keeps the stored payload (a workflow / tool payload is not editable here). */
function keepsPayload(editing?: Schedule): boolean {
  return (
    editing !== undefined && parsePayload(editing.payload).kind !== "agent_prompt"
  );
}

/** The first problem of a draft, or `null`. */
export function validateDraft(
  d: ScheduleDraft,
  editing?: Schedule
): DraftError | null {
  if (d.name.trim() === "") {
    return "name";
  }
  if (d.botId === "") {
    return "bot";
  }
  if (d.kind === "natural" && d.text.trim() === "") {
    return "text";
  }
  if (d.kind === "cron" && d.cron.trim() === "") {
    return "cron";
  }
  if (!keepsPayload(editing) && d.prompt.trim() === "") {
    return "prompt";
  }
  return null;
}

/**
 * The request body for a draft.  `spec` is the resolved spec of the `natural`
 * kind (from the parse call); it throws when it is missing.
 */
export function buildInput(
  d: ScheduleDraft,
  spec: unknown,
  editing?: Schedule
): ScheduleInput {
  let resolved: unknown;
  switch (d.kind) {
    case "simple":
    case "natural":
      if (spec === null || spec === undefined) {
        throw new Error("schedule was not resolved");
      }
      resolved = spec;
      break;
    case "cron": {
      const rawCron = d.cron.trim();
      const tz = d.tz?.trim();
      const hasEmbeddedTz = rawCron.split(/\s+/).length !== 5;
      resolved = {
        cron: !hasEmbeddedTz && tz ? `${rawCron} ${tz}` : rawCron
      };
      break;
    }
    case "keep":
      if (!editing) {
        throw new Error("nothing to keep");
      }
      resolved = editing.spec;
      break;
  }
  const payload =
    editing && keepsPayload(editing)
      ? editing.payload
      : { kind: "agent_prompt", text: d.prompt.trim() };
  return {
    bot_id: d.botId,
    name: d.name.trim(),
    spec: resolved,
    payload,
    enabled: d.enabled,
    misfire_policy: d.misfire,
    // keep an existing chat on edit; a new schedule gets the daemon default
    chat_id: editing?.chat_id ?? null
  };
}

/** The same schedule with `enabled` flipped, as a `PUT` body. */
export function toggleInput(s: Schedule): ScheduleInput {
  return {
    bot_id: s.bot_id,
    chat_id: s.chat_id ?? null,
    name: s.name,
    spec: s.spec,
    payload: s.payload,
    enabled: !s.enabled,
    misfire_policy: s.misfire_policy
  };
}

export type HistoryFilter = "all" | "issues";

/** Whether a history status represents a failure, miss, interruption, or cancellation. */
export function isIssueStatus(status: string): boolean {
  return (
    status === "failed" ||
    status === "missed" ||
    status === "interrupted" ||
    status === "cancelled"
  );
}

/** Filter run history entries by `all` or `issues` (failed/missed/interrupted/cancelled). */
export function filterScheduleHistory<T extends { status: string }>(
  entries: readonly T[],
  filter: HistoryFilter
): T[] {
  if (filter === "all") {
    return [...entries];
  }
  return entries.filter((e) => isIssueStatus(e.status));
}

export type HistoryTone = "succeeded" | "failed" | "running" | "missed";

/** Visual badge tone for a schedule history status. */
export function historyStatusTone(status: string): HistoryTone {
  switch (status) {
    case "succeeded":
      return "succeeded";
    case "failed":
      return "failed";
    case "missed":
    case "interrupted":
    case "cancelled":
    case "paused":
      return "missed";
    default:
      return "running";
  }
}

export type DurationParts =
  | { i18nKey: "schedules.durationSeconds"; values: { seconds: number } }
  | {
      i18nKey: "schedules.durationMinutes";
      values: { minutes: number; seconds: number };
    }
  | {
      i18nKey: "schedules.durationHours";
      values: { hours: number; minutes: number };
    };

/** Compute i18n duration parts when both `startedMs` and `finishedMs` are known. */
export function formatDurationParts(
  startedMs?: number | null,
  finishedMs?: number | null
): DurationParts | null {
  if (
    startedMs === null ||
    startedMs === undefined ||
    finishedMs === null ||
    finishedMs === undefined ||
    finishedMs < startedMs
  ) {
    return null;
  }
  const totalSec = Math.round((finishedMs - startedMs) / 1000);
  if (totalSec < 60) {
    return {
      i18nKey: "schedules.durationSeconds",
      values: { seconds: totalSec }
    };
  }
  const totalMin = Math.floor(totalSec / 60);
  const remSec = totalSec % 60;
  if (totalMin < 60) {
    return {
      i18nKey: "schedules.durationMinutes",
      values: { minutes: totalMin, seconds: remSec }
    };
  }
  const hours = Math.floor(totalMin / 60);
  const remMin = totalMin % 60;
  return {
    i18nKey: "schedules.durationHours",
    values: { hours, minutes: remMin }
  };
}

/** Merge live event runs that haven't been persisted into history yet. */
export function mergeLiveRunsIntoHistory(
  history: readonly ScheduleHistoryEntry[],
  liveRuns: readonly RunState[]
): ScheduleHistoryEntry[] {
  const seenRunIds = new Set(
    history.map((h) => h.run_id).filter((id): id is string => Boolean(id))
  );
  const merged = [...history];
  for (const r of liveRuns) {
    if (seenRunIds.has(r.id)) {
      continue;
    }
    const status =
      r.finishedMs === undefined
        ? "running"
        : r.stopped
          ? "cancelled"
          : r.error
            ? "failed"
            : "succeeded";
    merged.push({
      run_id: r.id,
      chat_id: r.chatId || null,
      status,
      started_at_ms: r.startedMs,
      finished_at_ms: r.finishedMs ?? null,
      error: r.error ?? null,
      retryable: false
    });
  }
  return merged.sort((a, b) => {
    const ak = a.started_at_ms ?? a.due_ms ?? 0;
    const bk = b.started_at_ms ?? b.due_ms ?? 0;
    return bk - ak;
  });
}

export interface CalendarGridDay {
  dayKey: string;
  dayOfMonth: number;
  inMonth: boolean;
}

export interface CalendarUpcomingEvent {
  scheduleId: string;
  scheduleName: string;
  ts: number;
}

export interface CalendarOutcomeEvent {
  scheduleId: string;
  scheduleName: string;
  status: string;
  ts: number;
  entry: ScheduleHistoryEntry;
}

export interface CalendarCell extends CalendarGridDay {
  upcoming: CalendarUpcomingEvent[];
  outcomes: CalendarOutcomeEvent[];
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** Format a Unix ms timestamp into `YYYY-MM-DD` in the given or local timezone. */
export function localDayKey(ms: number, timeZone?: string): string {
  if (timeZone) {
    try {
      return new Intl.DateTimeFormat("en-CA", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        timeZone
      }).format(new Date(ms));
    } catch {
      // Fallback to local calendar date if invalid timezone.
    }
  }
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** Build a 7-column month calendar grid (Monday-first by default). */
export function buildMonthGrid(
  year: number,
  monthZeroBased: number,
  weekStartsOnMonday = true
): CalendarGridDay[] {
  const first = new Date(year, monthZeroBased, 1);
  const startDow = first.getDay(); // 0 = Sun .. 6 = Sat
  const lead = weekStartsOnMonday ? (startDow + 6) % 7 : startDow;
  const daysInMonth = new Date(year, monthZeroBased + 1, 0).getDate();
  const totalCells = Math.ceil((lead + daysInMonth) / 7) * 7;
  const cells: CalendarGridDay[] = [];
  for (let i = 0; i < totalCells; i += 1) {
    const d = new Date(year, monthZeroBased, 1 - lead + i);
    const dayKey = `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
    cells.push({
      dayKey,
      dayOfMonth: d.getDate(),
      inMonth: d.getMonth() === monthZeroBased && d.getFullYear() === year
    });
  }
  return cells;
}

/** Populate a month grid with upcoming occurrences and recorded outcomes. */
export function placeScheduleCalendarEvents(
  days: readonly CalendarGridDay[],
  schedules: readonly Schedule[],
  upcomingBySchedule: Readonly<Record<string, readonly number[]>>,
  historyBySchedule: Readonly<Record<string, readonly ScheduleHistoryEntry[]>>,
  timeZone?: string
): CalendarCell[] {
  const byDay = new Map<string, CalendarCell>();
  const cells: CalendarCell[] = days.map((d) => {
    const cell: CalendarCell = { ...d, upcoming: [], outcomes: [] };
    byDay.set(d.dayKey, cell);
    return cell;
  });

  for (const s of schedules) {
    if (s.enabled && !s.moved_to) {
      const timestamps = upcomingBySchedule[s.id] ?? s.next_runs_ms ?? [];
      for (const ts of timestamps) {
        const key = localDayKey(ts, timeZone);
        byDay.get(key)?.upcoming.push({
          scheduleId: s.id,
          scheduleName: s.name,
          ts
        });
      }
    }
    const entries = historyBySchedule[s.id] ?? [];
    for (const entry of entries) {
      const ts = entry.started_at_ms ?? entry.due_ms;
      if (ts === null || ts === undefined) {
        continue;
      }
      const key = localDayKey(ts, timeZone);
      byDay.get(key)?.outcomes.push({
        scheduleId: s.id,
        scheduleName: s.name,
        status: entry.status,
        ts,
        entry
      });
    }
  }

  for (const cell of cells) {
    cell.upcoming.sort((a, b) => a.ts - b.ts);
    cell.outcomes.sort((a, b) => b.ts - a.ts);
  }
  return cells;
}

/** Arrow/Home/End keyboard navigation index inside a 7-column calendar grid. */
export function moveCalendarFocusIndex(
  currentIdx: number,
  key: string,
  totalCells: number
): number | null {
  if (totalCells <= 0) {
    return null;
  }
  switch (key) {
    case "ArrowLeft":
      return Math.max(0, currentIdx - 1);
    case "ArrowRight":
      return Math.min(totalCells - 1, currentIdx + 1);
    case "ArrowUp":
      return Math.max(0, currentIdx - 7);
    case "ArrowDown":
      return Math.min(totalCells - 1, currentIdx + 7);
    case "Home":
      return Math.floor(currentIdx / 7) * 7;
    case "End":
      return Math.min(totalCells - 1, Math.floor(currentIdx / 7) * 7 + 6);
    default:
      return null;
  }
}
