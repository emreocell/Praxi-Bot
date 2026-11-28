/** Standing-order helpers for the schedule editor (mirrors scheduler payload rules). */

export const DEFAULT_MAX_RUNS_PER_DAY = 24;
export const MAX_RUNS_PER_DAY = 288;

export interface StandingActiveHours {
  start: string;
  end: string;
  timezone: string;
}

export interface StandingOrderFields {
  checklist: string;
  activeHoursEnabled: boolean;
  activeHoursStart: string;
  activeHoursEnd: string;
  activeHoursTz: string;
  maxRunsPerDay: number;
}

export type StandingDraftError =
  | "checklist"
  | "maxRuns"
  | "hoursStart"
  | "hoursEnd"
  | "hoursTz";

const HHMM = /^(\d{2}):(\d{2})$/;

export function parseHhmm(s: string): { h: number; m: number } | null {
  const m = s.trim().match(HHMM);
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  if (h > 23 || min > 59) return null;
  return { h, m: min };
}

export function standingFromPayload(
  raw: unknown,
  localTz: string
): StandingOrderFields {
  const base: StandingOrderFields = {
    checklist: "",
    activeHoursEnabled: false,
    activeHoursStart: "08:00",
    activeHoursEnd: "22:00",
    activeHoursTz: localTz,
    maxRunsPerDay: DEFAULT_MAX_RUNS_PER_DAY
  };
  if (typeof raw !== "object" || raw === null) return base;
  const o = raw as Record<string, unknown>;
  if (o.kind !== "standing_order") return base;
  if (typeof o.checklist === "string") base.checklist = o.checklist;
  const cap = o.max_runs_per_day;
  if (typeof cap === "number" && Number.isFinite(cap)) {
    base.maxRunsPerDay = Math.trunc(cap);
  }
  const ah = o.active_hours;
  if (typeof ah === "object" && ah !== null) {
    const h = ah as Record<string, unknown>;
    if (typeof h.start === "string" && typeof h.end === "string" && typeof h.timezone === "string") {
      base.activeHoursEnabled = true;
      base.activeHoursStart = h.start;
      base.activeHoursEnd = h.end;
      base.activeHoursTz = h.timezone;
    }
  }
  return base;
}

export function validateStanding(fields: StandingOrderFields): StandingDraftError | null {
  if (fields.checklist.trim() === "") return "checklist";
  if (
    !Number.isFinite(fields.maxRunsPerDay) ||
    fields.maxRunsPerDay < 1 ||
    fields.maxRunsPerDay > MAX_RUNS_PER_DAY
  ) {
    return "maxRuns";
  }
  if (!fields.activeHoursEnabled) return null;
  if (parseHhmm(fields.activeHoursStart) === null) return "hoursStart";
  if (parseHhmm(fields.activeHoursEnd) === null) return "hoursEnd";
  if (fields.activeHoursStart === fields.activeHoursEnd) return "hoursEnd";
  try {
    new Intl.DateTimeFormat("en", { timeZone: fields.activeHoursTz.trim() });
  } catch {
    return "hoursTz";
  }
  return null;
}

export function buildStandingPayload(fields: StandingOrderFields): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    kind: "standing_order",
    checklist: fields.checklist.trim(),
    max_runs_per_day: fields.maxRunsPerDay
  };
  if (fields.activeHoursEnabled) {
    payload.active_hours = {
      start: fields.activeHoursStart.trim(),
      end: fields.activeHoursEnd.trim(),
      timezone: fields.activeHoursTz.trim()
    };
  }
  return payload;
}

export type StandingVerdictLabel = "ok" | "needs_owner" | "unclear";

/** Newest history row with a standing verdict. */
export function lastStandingVerdict(
  history: ReadonlyArray<{ standing_verdict?: string | null; status: string }>
): StandingVerdictLabel | null {
  for (const e of history) {
    const v = e.standing_verdict;
    if (v === "ok" || v === "needs_owner" || v === "unclear") return v;
    if (e.status === "succeeded") return "unclear";
  }
  return null;
}