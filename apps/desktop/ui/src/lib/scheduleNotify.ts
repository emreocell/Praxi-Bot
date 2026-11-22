/**
 * Per-job notification choice: tell me always / only when something goes wrong / never.
 * Kept on this computer (it only decides what this app announces); a job that needs an
 * approval always stays in "Needs you", whatever is chosen here.
 */

export type ScheduleNotify = "always" | "problems" | "never";

export const SCHEDULE_NOTIFY_CHOICES: readonly ScheduleNotify[] = [
  "always",
  "problems",
  "never"
] as const;

export const DEFAULT_SCHEDULE_NOTIFY: ScheduleNotify = "problems";

const KEY = "praxi.scheduleNotify";

export function parseScheduleNotify(raw: unknown): Record<string, ScheduleNotify> {
  const out: Record<string, ScheduleNotify> = {};
  if (typeof raw !== "object" || raw === null) {
    return out;
  }
  for (const [id, v] of Object.entries(raw)) {
    if (v === "always" || v === "problems" || v === "never") {
      out[id] = v;
    }
  }
  return out;
}

export function readScheduleNotify(): Record<string, ScheduleNotify> {
  try {
    return parseScheduleNotify(JSON.parse(localStorage.getItem(KEY) ?? "{}"));
  } catch {
    return {};
  }
}

export function scheduleNotifyOf(scheduleId: string): ScheduleNotify {
  return readScheduleNotify()[scheduleId] ?? DEFAULT_SCHEDULE_NOTIFY;
}

export function writeScheduleNotify(scheduleId: string, choice: ScheduleNotify): void {
  try {
    const all = readScheduleNotify();
    if (choice === DEFAULT_SCHEDULE_NOTIFY) {
      delete all[scheduleId];
    } else {
      all[scheduleId] = choice;
    }
    localStorage.setItem(KEY, JSON.stringify(all));
  } catch {
    // storage unavailable: the default applies
  }
}
