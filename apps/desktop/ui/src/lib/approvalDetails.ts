/** Structured facts a tool attaches to its approval card (`approval_needed.details`). */

export interface SchedulePermission {
  /** `full`, `ask_first`, `mixed` or `read_only`. */
  level: string;
  allow: string[];
  ask: string[];
  deny: string[];
}

export interface PreviousSchedule {
  name: string;
  prompt: string;
  schedule: string;
  timezone: string;
}

export type ScheduleAction = "create" | "update" | "resume" | "pause" | "delete";

/** What the `schedule_*` tools ask approval for. */
export interface ScheduleApprovalDetails {
  action: ScheduleAction;
  name: string;
  /** The exact prompt that will run at every firing (empty for pause / delete). */
  prompt: string;
  /** Prompt length in characters (so a long prompt's size is obvious). */
  promptChars: number;
  /** For an update: what it is now. */
  previous: PreviousSchedule | null;
  /** Human-readable timing. */
  schedule: string;
  timezone: string;
  /** The next firing times (RFC 3339, user's timezone). */
  nextRuns: string[];
  permission: SchedulePermission | null;
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

/** Parse `details` of an approval; `null` when it is not a schedule card. */
export function scheduleDetails(details: unknown): ScheduleApprovalDetails | null {
  if (!isObject(details) || details.kind !== "schedule") return null;
  const action = details.action;
  if (
    action !== "create" &&
    action !== "update" &&
    action !== "resume" &&
    action !== "pause" &&
    action !== "delete"
  ) {
    return null;
  }
  const perm = details.permission;
  const prev = details.previous;
  const prompt = typeof details.prompt === "string" ? details.prompt : "";
  return {
    action,
    name: typeof details.name === "string" ? details.name : "",
    prompt,
    promptChars:
      typeof details.prompt_chars === "number" ? details.prompt_chars : [...prompt].length,
    previous: isObject(prev)
      ? {
          name: typeof prev.name === "string" ? prev.name : "",
          prompt: typeof prev.prompt === "string" ? prev.prompt : "",
          schedule: typeof prev.schedule === "string" ? prev.schedule : "",
          timezone: typeof prev.timezone === "string" ? prev.timezone : ""
        }
      : null,
    schedule: typeof details.schedule === "string" ? details.schedule : "",
    timezone: typeof details.timezone === "string" ? details.timezone : "",
    nextRuns: strings(details.next_runs),
    permission: isObject(perm)
      ? {
          level: typeof perm.level === "string" ? perm.level : "",
          allow: strings(perm.allow),
          ask: strings(perm.ask),
          deny: strings(perm.deny)
        }
      : null
  };
}

export interface EgressApprovalDetails {
  reason: string;
  read: string[];
  host: string;
  payload: string;
  payloadKind: string;
}

/** Parse an egress card, including one nested beside a schedule card. */
export function egressDetails(details: unknown): EgressApprovalDetails | null {
  if (!isObject(details)) return null;
  const raw =
    details.kind === "egress"
      ? details
      : isObject(details.egress) && details.egress.kind === "egress"
        ? details.egress
        : null;
  if (!raw) return null;
  return {
    reason: typeof raw.reason === "string" ? raw.reason : "",
    read: strings(raw.read),
    host: typeof raw.host === "string" ? raw.host : "",
    payload: typeof raw.payload === "string" ? raw.payload : "",
    payloadKind: typeof raw.payload_kind === "string" ? raw.payload_kind : "body"
  };
}

/** `2026-11-13T10:30:00+03:00` as `Fri 13 Nov, 10:30` in the offset it carries. */
export function formatFiring(rfc3339: string, locale?: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(rfc3339);
  if (!m) return rfc3339;
  // The wall-clock fields already are the user's local time: format them as UTC
  // so the browser's own timezone cannot shift them.
  const d = new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5]));
  const date = new Intl.DateTimeFormat(locale, {
    weekday: "short",
    day: "numeric",
    month: "short",
    timeZone: "UTC"
  }).format(d);
  return `${date}, ${m[4]}:${m[5]}`;
}
