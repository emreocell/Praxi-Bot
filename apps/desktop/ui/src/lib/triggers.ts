import { parseSpec, type ScheduleSpec } from "./schedules.ts";

export type TriggerSource =
  | { kind: "mail"; account: string; folder: string; own_address: string; poll_seconds: number }
  | { kind: "webhook"; id: string }
  | { kind: "time"; spec: ScheduleSpec }
  | { kind: "feed"; url: string; poll_seconds: number }
  | { kind: "page"; url: string; selector: string; poll_seconds: number }
  | { kind: "calendar"; account: string | null; minutes_before: number; poll_seconds: number }
  | { kind: "home_assistant"; entity_id: string; to_state: string | null; poll_seconds: number };

export type TriggerKind = TriggerSource["kind"];
/** The event kinds the owner can add under "When something happens", in the order they are offered. */
export const EVENT_KINDS = ["mail", "webhook", "feed", "page", "calendar", "home_assistant"] as const satisfies readonly TriggerKind[];
/** Web addresses are other people's servers: the daemon refuses a check more often than this. */
export const WEB_MIN_POLL_SECONDS = 900;

/** A fresh source of `kind`, with the daemon's default pace. */
export function newSource(kind: TriggerKind): TriggerSource {
  switch (kind) {
    case "mail": return { kind, account: "", folder: "INBOX", own_address: "", poll_seconds: 120 };
    case "webhook": return { kind, id: "" };
    case "time": return { kind, spec: { cron: `0 9 * * * ${Intl.DateTimeFormat().resolvedOptions().timeZone}` } };
    case "feed": return { kind, url: "", poll_seconds: 1800 };
    case "page": return { kind, url: "", selector: "", poll_seconds: 3600 };
    case "calendar": return { kind, account: null, minutes_before: 10, poll_seconds: 120 };
    case "home_assistant": return { kind, entity_id: "", to_state: null, poll_seconds: 60 };
  }
}

/** An example of what the bot receives for `source`, for the no-model "test with a sample" preview. */
export function sampleEvent(source: TriggerSource, text: string): Record<string, unknown> {
  switch (source.kind) {
    case "feed": return { title: text || "A new post", link: source.url, published: "", summary: "", received_at: 0 };
    case "page": return { url: source.url, selector: source.selector, text: text || "New text", received_at: 0 };
    case "calendar": return { summary: text || "Team meeting", start: "", minutes_until_start: source.minutes_before, location: "" };
    case "home_assistant": return { entity_id: source.entity_id, name: null, state: text || source.to_state || "on", previous_state: "off", received_at: 0 };
    default: return { body: text, received_at: 0 };
  }
}
export interface TriggerConfig {
  source: TriggerSource;
  filters: { sender: string; sender_match: "equals" | "domain"; subject: string; has_attachment: boolean };
  quiet_hours: { start: number; end: number; timezone: string; drop: boolean } | null;
  limits: { per_hour: number; per_day: number };
  bot_limits: { per_hour: number; per_day: number };
}
export interface TriggerInput {
  name: string;
  instruction: string;
  config: TriggerConfig;
  enabled: boolean;
}
export interface BotTrigger extends TriggerInput {
  origin: "rule" | "schedule" | "webhook";
  /** Who wrote the rule, as the daemon recorded it. Only an `owner` rule runs without cards on a Full bot. */
  author: "owner" | "bot" | "unknown";
  /** Version of the rule; a confirmation names the one the owner looked at. */
  revision: number;
  id: string;
  bot_id: string;
  last_fired_at_ms: number | null;
  count_today: number | null;
  queued: number | null;
  last_error: "run" | "source" | "queue" | "sender" | null;
}
export interface TriggerPreview { text: string; matches: boolean }

/** New rules are deliberately off. No browser-side evaluator or prompt wrapper. */
export function newTrigger(): TriggerInput {
  return {
    name: "", instruction: "", enabled: false,
    config: {
      source: newSource("mail"),
      filters: { sender: "", sender_match: "equals", subject: "", has_attachment: false },
      quiet_hours: null, limits: { per_hour: 10, per_day: 50 }, bot_limits: { per_hour: 30, per_day: 150 }
    }
  };
}
export function minutes(time: string): number {
  const [hours, mins] = time.split(":").map(Number);
  return hours * 60 + mins;
}
export function clockTime(minute: number): string {
  return `${Math.floor(minute / 60).toString().padStart(2, "0")}:${(minute % 60).toString().padStart(2, "0")}`;
}

/** Only owner-editable definition fields go back to the daemon. */
export function triggerInput(row: TriggerInput): TriggerInput {
  return { name: row.name, instruction: row.instruction, config: structuredClone(row.config), enabled: row.enabled };
}

/** Daily controls must never invent a daily time for another saved frequency. */
export function dailyTime(source: TriggerSource): { time: string; zone: string } | null {
  if (source.kind === "time" && "cron" in source.spec) {
    const parts=source.spec.cron.trim().split(/\s+/);
    if ((parts.length === 5 || parts.length === 6)
      && parts.slice(2, 5).every(part => part === "*")
      && /^\d{1,2}$/.test(parts[0] ?? "") && Number(parts[0]) < 60
      && /^\d{1,2}$/.test(parts[1] ?? "") && Number(parts[1]) < 24) {
      return {time: clockTime(Number(parts[1])*60+Number(parts[0])),zone:parts[5] ?? "UTC"};
    }
  }
  return null;
}

/** Preserve interval/one-time JSON while an owner edits advanced timing. */
export function triggerSpec(text: string): ScheduleSpec {
  try { return parseSpec(JSON.parse(text)) ?? { cron: text }; }
  catch { return { cron: text }; }
}
