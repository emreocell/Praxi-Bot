/**
 * Short relative timestamps for the notification inbox ("just now", "2 min ago",
 * "3 h ago", "Yesterday", or local date for older items). Pure and locale-safe.
 */

export type RelativeTimeTranslate = (key: string, values?: Record<string, unknown>) => string;

const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

function startOfDay(ts: number): number {
  const d = new Date(ts);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

export function formatRelativeTime(
  ts: number,
  now: number,
  lang: string,
  t: RelativeTimeTranslate
): string {
  if (!Number.isFinite(ts)) {
    return "";
  }
  const diff = Math.max(0, now - ts);
  if (diff < 45_000) {
    return t("notifyHistory.time.justNow");
  }
  if (diff < HOUR_MS) {
    const count = Math.max(1, Math.floor(diff / MINUTE_MS));
    return t("notifyHistory.time.minutesAgo", { count });
  }
  const dayDiff = Math.round((startOfDay(now) - startOfDay(ts)) / DAY_MS);
  if (diff < DAY_MS && dayDiff === 0) {
    const count = Math.max(1, Math.floor(diff / HOUR_MS));
    return t("notifyHistory.time.hoursAgo", { count });
  }
  if (dayDiff === 1) {
    return t("notifyHistory.time.yesterday");
  }
  const sameYear = new Date(ts).getFullYear() === new Date(now).getFullYear();
  try {
    return new Intl.DateTimeFormat(lang || undefined, {
      month: "short",
      day: "numeric",
      ...(sameYear ? {} : { year: "numeric" })
    }).format(ts);
  } catch {
    return new Date(ts).toLocaleDateString();
  }
}
