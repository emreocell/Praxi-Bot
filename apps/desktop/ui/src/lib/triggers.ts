import { parseSpec, type ScheduleSpec } from "./schedules.ts";

export type TriggerSource =
  | { kind: "mail"; account: string; folder: string; own_address: string; poll_seconds: number }
  | { kind: "webhook"; id: string }
  | { kind: "time"; spec: ScheduleSpec };
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
      source: { kind: "mail", account: "", folder: "INBOX", own_address: "", poll_seconds: 120 },
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
