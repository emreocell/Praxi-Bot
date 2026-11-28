/**
 * The one readable line under the schedule editor's "when" presets: "Every weekday at 09:00,
 * Istanbul". Pure: it reads the easy builder's choices, never asks the daemon (the next runs
 * still come from its preview).
 */
import { parseTime, type SimpleSchedule } from "./scheduleBuilder.ts";

/** The place people know a zone by: "Europe/Istanbul" -> "Istanbul", "America/Argentina/Buenos_Aires" -> "Buenos Aires". */
export function zoneCity(tz: string): string {
  const name = tz.trim();
  if (!name) {
    return "UTC";
  }
  const last = name.split("/").pop() ?? name;
  return last.replace(/_/g, " ");
}

/** A weekday (0 = Sunday) in the owner's language: "Monday", "Pazartesi". */
export function weekdayName(day: number, locale: string): string {
  // 7 Jan 2024 was a Sunday.
  return new Date(Date.UTC(2024, 0, 7 + (((day % 7) + 7) % 7))).toLocaleDateString(locale, { weekday: "long", timeZone: "UTC" });
}

/** A `YYYY-MM-DD` day as the owner reads it: "Fri, 28 Nov", "28 Kas Cum". */
function dayText(date: string, locale: string): string | null {
  const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!d) {
    return null;
  }
  const at = new Date(Date.UTC(Number(d[1]), Number(d[2]) - 1, Number(d[3])));
  return at.toLocaleDateString(locale, { weekday: "short", day: "numeric", month: "short", timeZone: "UTC" });
}

/** An i18n key (under `scheduleEditor.summary`) and its values, or null while a choice is missing. */
export interface WhenSummary {
  key: string;
  values: Record<string, string | number>;
}

/**
 * The easy builder's choices as one sentence. `custom` ("Other") has no sentence of its own: the
 * daemon's reading of the typed words is shown instead. A time that is not `HH:MM` gives null.
 */
export function whenSummary(s: SimpleSchedule, tz: string, locale: string): WhenSummary | null {
  const time = parseTime(s.time);
  if (!time || s.freq === "custom") {
    return null;
  }
  const values = {
    time: `${String(time.h).padStart(2, "0")}:${String(time.m).padStart(2, "0")}`,
    zone: zoneCity(tz)
  };
  switch (s.freq) {
    case "once": {
      const date = dayText(s.date, locale);
      return date ? { key: "scheduleEditor.summary.once", values: { ...values, date } } : { key: "scheduleEditor.summary.onceNoDate", values };
    }
    case "weekly":
      return { key: "scheduleEditor.summary.weekly", values: { ...values, weekday: weekdayName(s.weekday, locale) } };
    case "monthly":
      return { key: "scheduleEditor.summary.monthly", values: { ...values, day: s.monthDay } };
    case "weekdays":
      return { key: "scheduleEditor.summary.weekdays", values };
    default:
      return { key: "scheduleEditor.summary.daily", values };
  }
}
