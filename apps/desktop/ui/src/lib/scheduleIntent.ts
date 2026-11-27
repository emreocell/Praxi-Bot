/**
 * A one-time note from the bot panel to the schedules screen: "open the form with this sentence"
 * or "show this schedule". The screen reads it while it mounts and clears it once mounted, so a
 * later visit starts clean. Kept out of the route so links and history stay plain.
 */
export type ScheduleIntent =
  | { kind: "draft"; botId: string; text: string }
  | { kind: "focus"; scheduleId: string };

let pending: ScheduleIntent | null = null;

export function setScheduleIntent(intent: ScheduleIntent): void {
  pending = intent;
}

/** The pending intent, left in place (a render may run twice): clear it from an effect. */
export function readScheduleIntent(): ScheduleIntent | null {
  return pending;
}

export function clearScheduleIntent(): void {
  pending = null;
}
