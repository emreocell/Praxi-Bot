/** Pure helpers for the cost insights charts: day/week/month grouping, limit overlays, shares. */

import type { DailyUsageLike, UsageTotalsLike } from "./usage.ts";

export type Granularity = "day" | "week" | "month";
export const GRANULARITIES: readonly Granularity[] = ["day", "week", "month"];

/** One chart column: the sum of the days it covers. `day` is the first day (`YYYY-MM-DD`). */
export interface GroupedUsage extends DailyUsageLike {
  /** Number of calendar days the column covers (7 for a full week, 28-31 for a month). */
  span: number;
}

function parseDay(day: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!m) {
    return null;
  }
  return new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
}

function fmt(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/** First day of the period a day belongs to: Monday for weeks (as the bot limits count), the 1st for months. */
export function periodStart(day: string, g: Granularity): string {
  const d = parseDay(day);
  if (!d || g === "day") {
    return day;
  }
  if (g === "month") {
    return fmt(new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1)));
  }
  const back = (d.getUTCDay() + 6) % 7; // Monday = 0
  return fmt(new Date(d.getTime() - back * 86_400_000));
}

function daysInMonthOf(day: string): number {
  const d = parseDay(day);
  return d ? new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 0)).getUTCDate() : 30;
}

/**
 * Sum the chronological daily series into weeks or months. A cost stays `null` (unknown) unless at
 * least one call in the column had a known price, so an unknown price is never shown as $0.
 */
export function groupDaily(daily: readonly DailyUsageLike[], g: Granularity): GroupedUsage[] {
  const out: GroupedUsage[] = [];
  for (const d of daily) {
    const key = periodStart(d.day, g);
    let cur = out[out.length - 1];
    if (!cur || cur.day !== key) {
      cur = {
        day: key,
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        model_calls: 0,
        cost_usd: null,
        calls_without_usage: 0,
        calls_without_cost: 0,
        span: 0
      };
      out.push(cur);
    }
    cur.prompt_tokens += d.prompt_tokens;
    cur.completion_tokens += d.completion_tokens;
    cur.cached_tokens += d.cached_tokens;
    cur.model_calls += d.model_calls;
    cur.calls_without_usage = (cur.calls_without_usage ?? 0) + (d.calls_without_usage ?? 0);
    cur.calls_without_cost = (cur.calls_without_cost ?? 0) + (d.calls_without_cost ?? 0);
    if (d.cost_usd !== null && d.cost_usd !== undefined) {
      cur.cost_usd = (cur.cost_usd ?? 0) + d.cost_usd;
    }
    cur.span += 1;
  }
  return out;
}

export interface LimitInput {
  limit_usd?: number | null;
}

export interface LimitOverlay {
  /** Hard monthly limit scaled to one column, in USD. */
  limit: number;
  /** Soft-alert level (80% of the limit) scaled to one column, in USD. */
  alert: number;
  /** True when the lines are the monthly figures share for a shorter column (day, week). */
  prorated: boolean;
}

/** Share of the soft alert in the limit (the daemon warns at 80%). */
export const ALERT_FRACTION = 0.8;

/**
 * The monthly limit and its 80% alert, for a column of `span` days that starts on `day`. For a whole
 * month the figures are exact; for days and weeks they are the even share of the month.
 */
export function limitOverlay(
  spend: LimitInput | null | undefined,
  g: Granularity,
  day: string
): LimitOverlay | null {
  const limit = spend?.limit_usd;
  if (limit === null || limit === undefined || !(limit > 0)) {
    return null;
  }
  const perDay = limit / daysInMonthOf(day);
  const scale = g === "month" ? limit : g === "week" ? perDay * 7 : perDay;
  return { limit: scale, alert: scale * ALERT_FRACTION, prorated: g !== "month" };
}

export interface ShareRow {
  key: string;
  /** Cost when known, else `null`. */
  cost: number | null;
  tokens: number;
  calls: number;
  /** True when no call in the row reported tokens. */
  tokensUnknown: boolean;
  /** 0..1 of the biggest row, for bar length; `null` when nothing known to compare. */
  fraction: number | null;
}

/** Bars for "by bot" / "by model": ranked by the chosen metric, unknown values kept last and flagged. */
export function shareRows(
  rows: readonly { key: string; totals: UsageTotalsLike; callsWithoutUsage?: number }[],
  metric: "cost" | "tokens"
): ShareRow[] {
  const mapped = rows.map((r) => {
    const tokens = r.totals.prompt_tokens + r.totals.completion_tokens;
    const tokensUnknown = r.totals.model_calls > 0 && (r.callsWithoutUsage ?? 0) >= r.totals.model_calls;
    return {
      key: r.key,
      cost: r.totals.cost_usd ?? null,
      tokens,
      calls: r.totals.model_calls,
      tokensUnknown
    };
  });
  const val = (r: (typeof mapped)[number]): number | null =>
    metric === "cost" ? r.cost : r.tokensUnknown ? null : r.tokens;
  const max = mapped.reduce((m, r) => Math.max(m, val(r) ?? 0), 0);
  return mapped
    .sort((a, b) => (val(b) ?? -1) - (val(a) ?? -1) || a.key.localeCompare(b.key))
    .map((r) => {
      const v = val(r);
      return { ...r, fraction: v === null || max <= 0 ? null : v / max };
    });
}
