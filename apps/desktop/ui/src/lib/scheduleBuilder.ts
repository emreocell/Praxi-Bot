/**
 * The friendly schedule builder: a few choices (how often, what time) turn into a
 * schedule the daemon understands.  Pure and deterministic; no model call.
 */

export type Frequency =
  | "once"
  | "daily"
  | "weekdays"
  | "weekly"
  | "monthly"
  | "custom";

/** The chips, in the order they are shown. */
export const FREQUENCIES: readonly Frequency[] = [
  "once",
  "daily",
  "weekdays",
  "weekly",
  "monthly",
  "custom"
] as const;

export interface SimpleSchedule {
  freq: Frequency;
  /** `HH:MM`, 24 hour. */
  time: string;
  /** 0 = Sunday .. 6 = Saturday (weekly). */
  weekday: number;
  /** 1..28 (monthly): later days are missing from some months. */
  monthDay: number;
  /** `YYYY-MM-DD` (once). */
  date: string;
}

export const DEFAULT_SIMPLE: SimpleSchedule = {
  freq: "daily",
  time: "09:00",
  weekday: 1,
  monthDay: 1,
  date: ""
};

export function parseTime(time: string): { h: number; m: number } | null {
  const hit = /^(\d{1,2}):(\d{2})$/.exec(time.trim());
  if (!hit) {
    return null;
  }
  const h = Number(hit[1]);
  const m = Number(hit[2]);
  return h <= 23 && m <= 59 ? { h, m } : null;
}

/** Milliseconds of the wall-clock time `date` `time` in the IANA zone `tz`. */
export function zonedWallTimeToMs(
  date: string,
  time: string,
  tz: string
): number | null {
  const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  const t = parseTime(time);
  if (!d || !t) {
    return null;
  }
  const wall = Date.UTC(Number(d[1]), Number(d[2]) - 1, Number(d[3]), t.h, t.m);
  const offsetAt = (ms: number): number => {
    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone: tz,
      hourCycle: "h23",
      year: "numeric",
      month: "numeric",
      day: "numeric",
      hour: "numeric",
      minute: "numeric"
    }).formatToParts(new Date(ms));
    const get = (type: string) =>
      Number(parts.find((p) => p.type === type)?.value ?? "0");
    return (
      Date.UTC(get("year"), get("month") - 1, get("day"), get("hour"), get("minute")) -
      ms
    );
  };
  try {
    // Two passes settle the offset even when it changes between the guess and the answer.
    const first = wall - offsetAt(wall);
    return wall - offsetAt(first);
  } catch {
    return null;
  }
}

/** Why a simple schedule cannot be saved yet, or `null`. */
export function simpleProblem(
  s: SimpleSchedule,
  tz: string,
  nowMs: number
): "time" | "date" | "past" | null {
  if (s.freq === "custom") {
    return null;
  }
  if (parseTime(s.time) === null) {
    return "time";
  }
  if (s.freq === "once") {
    if (s.date === "") {
      return "date";
    }
    const ms = zonedWallTimeToMs(s.date, s.time, tz);
    if (ms === null) {
      return "date";
    }
    if (ms <= nowMs) {
      return "past";
    }
  }
  return null;
}

/**
 * The daemon `spec` of a simple schedule (`{cron}` with the zone appended, or `{at}`),
 * or `null` when it is not complete.  `custom` has no spec: the person types words.
 */
export function simpleSpec(
  s: SimpleSchedule,
  tz: string
): { cron: string } | { at: { unix_ms: number } } | null {
  const t = parseTime(s.time);
  if (!t || s.freq === "custom") {
    return null;
  }
  const zone = tz.trim();
  const withTz = (expr: string) => ({ cron: zone ? `${expr} ${zone}` : expr });
  switch (s.freq) {
    case "daily":
      return withTz(`${t.m} ${t.h} * * *`);
    case "weekdays":
      return withTz(`${t.m} ${t.h} * * 1-5`);
    case "weekly":
      return withTz(`${t.m} ${t.h} * * ${s.weekday}`);
    case "monthly":
      return withTz(`${t.m} ${t.h} ${Math.min(28, Math.max(1, s.monthDay))} * *`);
    case "once": {
      const ms = zonedWallTimeToMs(s.date, s.time, zone || "UTC");
      return ms === null ? null : { at: { unix_ms: ms } };
    }
  }
}
