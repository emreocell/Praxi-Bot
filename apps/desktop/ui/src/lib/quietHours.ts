/** Quiet hours: when the app stays silent (OS toasts held, kept in the history instead). */
export interface QuietHours {
  enabled: boolean;
  /** "HH:MM" 24 h, wall clock in `timezone`. */
  from: string;
  to: string;
  /** Weekdays the window STARTS on (0 = Sunday ... 6 = Saturday). */
  days: number[];
  /** IANA zone, or "" for the system zone. */
  timezone: string;
  /** Approvals / questions a bot is blocked on still show a toast. */
  allowUrgent: boolean;
}

export const DEFAULT_QUIET_HOURS: QuietHours = {
  enabled: false,
  from: "22:00",
  to: "07:00",
  days: [0, 1, 2, 3, 4, 5, 6],
  timezone: "",
  allowUrgent: true
};

const TIME = /^([01]\d|2[0-3]):([0-5]\d)$/;

export function parseMinutes(hhmm: string): number | null {
  const m = TIME.exec(hhmm);
  return m ? Number(m[1]) * 60 + Number(m[2]) : null;
}

export function isValidTimezone(tz: string): boolean {
  if (tz === "") {
    return true;
  }
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: tz });
    return true;
  } catch {
    return false;
  }
}

/** Tolerant parse of stored data: anything odd falls back to the default field. */
export function parseQuietHours(raw: unknown): QuietHours {
  const o = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {};
  const d = DEFAULT_QUIET_HOURS;
  const time = (v: unknown, fb: string) =>
    typeof v === "string" && parseMinutes(v) !== null ? v : fb;
  const days = Array.isArray(o.days)
    ? [...new Set(o.days.filter((x): x is number => Number.isInteger(x) && x >= 0 && x <= 6))].sort()
    : d.days;
  return {
    enabled: o.enabled === true,
    from: time(o.from, d.from),
    to: time(o.to, d.to),
    days,
    timezone: typeof o.timezone === "string" && isValidTimezone(o.timezone) ? o.timezone : "",
    allowUrgent: typeof o.allowUrgent === "boolean" ? o.allowUrgent : d.allowUrgent
  };
}

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** Wall-clock weekday and minute-of-day of an instant in a zone (DST handled by Intl). */
export function wallClock(at: Date, timezone: string): { day: number; minutes: number } {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: timezone || undefined,
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23"
  }).formatToParts(at);
  const get = (type: string) => parts.find((p) => p.type === type)?.value ?? "";
  return {
    day: Math.max(0, WEEKDAYS.indexOf(get("weekday"))),
    minutes: (Number(get("hour")) % 24) * 60 + Number(get("minute"))
  };
}

/**
 * Is `at` inside the quiet window?  An overnight window (22:00 to 07:00) belongs to the day it
 * starts on: Tuesday 03:00 is part of Monday's window.  from == to is an empty window.
 */
export function isQuiet(at: Date, q: QuietHours): boolean {
  if (!q.enabled || q.days.length === 0) {
    return false;
  }
  const from = parseMinutes(q.from);
  const to = parseMinutes(q.to);
  if (from === null || to === null || from === to) {
    return false;
  }
  const { day, minutes } = wallClock(at, q.timezone);
  if (from < to) {
    return q.days.includes(day) && minutes >= from && minutes < to;
  }
  if (minutes >= from) {
    return q.days.includes(day);
  }
  return minutes < to && q.days.includes((day + 6) % 7);
}

/** Should a toast be held back?  `urgent` = a bot is blocked waiting for the user. */
export function shouldHold(at: Date, q: QuietHours, urgent: boolean): boolean {
  return isQuiet(at, q) && !(urgent && q.allowUrgent);
}
