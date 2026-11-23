/** Date + time for the schedule builder's run preview, e.g. "Mon, Nov 23, 2026, 9:00 AM".
 *  Do not combine `dateStyle`/`timeStyle` with `weekday`: browsers throw a RangeError and the whole
 *  builder goes blank. Explicit fields are always safe. */
export function formatRunPreview(ms: number, locale: string, timeZone?: string): string {
  return new Date(ms).toLocaleString(locale, {
    weekday: "short",
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    ...(timeZone ? { timeZone } : {})
  });
}
