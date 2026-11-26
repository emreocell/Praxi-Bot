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

/** A prompt that starts like a reminder ("remind me ...", "tell me: ...", "let me know ..."). */
const REMINDER_START =
  /^\s*(please\s+)?(remind(\s+me)?|tell\s+me|let\s+me\s+know|notify\s+me|ping\s+me|hatırlat|bana\s+söyle|erinnere|sag\s+mir|recuérdame|recuerda|rappelle)/i;

/**
 * What to announce when the owner never chose: every time for a reminder (a one-shot, or a prompt
 * that starts like one: its whole point is to be seen), only problems for background work.
 */
export function suggestScheduleNotify(prompt: string, oneShot: boolean): ScheduleNotify {
  return oneShot || REMINDER_START.test(prompt) ? "always" : DEFAULT_SCHEDULE_NOTIFY;
}

/**
 * The stored choice of a schedule; without one, the reminder-aware default when `hint` (the
 * schedule's prompt and whether it runs once) is known, else only problems.
 */
export function scheduleNotifyOf(
  scheduleId: string,
  hint?: { prompt: string; oneShot: boolean }
): ScheduleNotify {
  return (
    readScheduleNotify()[scheduleId] ??
    (hint ? suggestScheduleNotify(hint.prompt, hint.oneShot) : DEFAULT_SCHEDULE_NOTIFY)
  );
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
