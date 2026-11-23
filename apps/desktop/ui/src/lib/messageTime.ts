/**
 * Message and step times in the chat: the per-message stamp, its
 * hover / tooltip forms, the day separator and tool-step durations. Pure, so
 * the wording is tested without a browser. Times are local; the stored
 * millisecond timestamps are the daemon's, never invented here.
 */

/** Settings > General: how message times show. */
export type MessageTimes = "off" | "minutes" | "seconds";

export const MESSAGE_TIMES_VALUES: readonly MessageTimes[] = ["off", "minutes", "seconds"];

/** Narrow a stored value to a known mode (defaults to `minutes`). */
export function parseMessageTimes(v: unknown): MessageTimes {
  return v === "off" || v === "seconds" ? v : "minutes";
}

/** tr and de are always 24 h; other languages follow the OS locale. */
function clockLocale(lang: string): string | undefined {
  const l = lang.toLowerCase();
  return l.startsWith("tr") ? "tr-TR" : l.startsWith("de") ? "de-DE" : undefined;
}

function is24h(lang: string): boolean {
  const l = lang.toLowerCase();
  return l.startsWith("tr") || l.startsWith("de");
}

function clockOptions(lang: string, seconds: boolean): Intl.DateTimeFormatOptions {
  return {
    hour: "2-digit",
    minute: "2-digit",
    ...(seconds ? { second: "2-digit" as const } : {}),
    ...(is24h(lang) ? { hourCycle: "h23" as const } : {})
  };
}

/** "21:20" / "21:20:07". */
export function formatClock(ts: number, lang: string, seconds: boolean): string {
  return new Intl.DateTimeFormat(clockLocale(lang), clockOptions(lang, seconds)).format(ts);
}

/** Full local date and time for tooltips: "17 Nov 2026, 21:20:07". */
export function formatStamp(ts: number, lang: string): string {
  return new Intl.DateTimeFormat(clockLocale(lang), {
    year: "numeric",
    month: "short",
    day: "numeric",
    ...clockOptions(lang, true)
  }).format(ts);
}

/** Local date and time without seconds, for sentences: "17 Nov 2026, 21:20" (tr and de on 24 h). */
export function formatDateTime(ts: number, lang: string): string {
  return new Intl.DateTimeFormat(clockLocale(lang), {
    year: "numeric",
    month: "short",
    day: "numeric",
    ...clockOptions(lang, false)
  }).format(ts);
}

export interface MessageTimeLabel {
  /** What shows by default (HH:mm, or HH:mm:ss in seconds mode). */
  text: string;
  /** What shows on hover or focus (always with seconds). */
  hover: string;
  /** Tooltip: the full local date-time (plus the finish time when known). */
  title: string;
}

/** The stamp next to a message; `null` when the setting is off. */
export function messageTimeLabel(
  ts: number,
  mode: MessageTimes,
  lang: string,
  finishedTs?: number
): MessageTimeLabel | null {
  if (mode === "off") {
    return null;
  }
  const hover = formatClock(ts, lang, true);
  const finished =
    finishedTs !== undefined && finishedTs > ts ? ` → ${formatStamp(finishedTs, lang)}` : "";
  return {
    text: mode === "seconds" ? hover : formatClock(ts, lang, false),
    hover,
    title: `${formatStamp(ts, lang)}${finished}`
  };
}

function dayStart(ts: number): number {
  const d = new Date(ts);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/**
 * Day separator: "Today" / "Yesterday" / "15 November" (with the year when it
 * is not this year). The time is appended only when the messages carry none.
 */
export function dayHeader(
  ts: number,
  now: number,
  lang: string,
  words: { today: string; yesterday: string },
  withTime: boolean
): string {
  const days = Math.round((dayStart(now) - dayStart(ts)) / 86_400_000);
  const sameYear = new Date(ts).getFullYear() === new Date(now).getFullYear();
  const day =
    days === 0
      ? words.today
      : days === 1
        ? words.yesterday
        : new Intl.DateTimeFormat(clockLocale(lang), {
            day: "numeric",
            month: "long",
            ...(sameYear ? {} : { year: "numeric" as const })
          }).format(ts);
  return withTime ? `${day} ${formatClock(ts, lang, false)}` : day;
}

/**
 * A tool step's duration: "0,4 sn" / "2,4 sn" / "12 sn" / "1 dk 5 sn".
 * `units` are the short unit words of the language.
 */
export function formatStepDuration(
  ms: number,
  lang: string,
  units: { s: string; m: string }
): string {
  const clamped = Math.max(0, ms);
  if (clamped < 10_000) {
    const one = new Intl.NumberFormat(clockLocale(lang) ?? lang, {
      minimumFractionDigits: 1,
      maximumFractionDigits: 1
    }).format(Math.round(clamped / 100) / 10);
    return `${one} ${units.s}`;
  }
  const total = Math.round(clamped / 1000);
  if (total < 60) {
    return `${total} ${units.s}`;
  }
  const m = Math.floor(total / 60);
  const s = total % 60;
  return s > 0 ? `${m} ${units.m} ${s} ${units.s}` : `${m} ${units.m}`;
}
