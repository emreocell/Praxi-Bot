/** Pure helpers for the Usage view: range queries, model sorting, SVG daily bars, formatting. */

export type UsageRange = "week" | "month" | "quarter" | "all";
export type TrendMetric = "tokens" | "calls" | "cost";

export const USAGE_RANGES: readonly UsageRange[] = ["week", "month", "quarter", "all"];

export const USAGE_RANGE_DAYS: Record<Exclude<UsageRange, "all">, number> = {
  week: 7,
  month: 30,
  quarter: 90
};

export interface UsageQueryParams {
  since_ms?: number;
  until_ms?: number;
  days?: number;
  tz?: string;
}

export interface UsageTotalsLike {
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  model_calls: number;
  cost_usd?: number | null;
}

export interface ModelUsageLike extends UsageTotalsLike {
  model: string;
  provider?: string | null;
  calls_without_usage?: number;
  calls_without_cost?: number;
}

export interface DailyUsageLike extends UsageTotalsLike {
  day: string;
  calls_without_usage?: number;
  calls_without_cost?: number;
}

export interface DailyBar {
  day: string;
  value: number | null;
  x: number;
  y: number;
  width: number;
  height: number;
  unknownOnly: boolean;
  calls: number;
  tokens: number;
  costUsd: number | null;
  callsWithoutUsage: number;
  callsWithoutCost: number;
}

export interface DailyChartModel {
  bars: DailyBar[];
  max: number;
  hasData: boolean;
  labelIndices: number[];
}

/** Browser's IANA time zone when available. */
export function localTimeZone(): string | undefined {
  try {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return tz && tz.trim().length > 0 ? tz : undefined;
  } catch {
    return undefined;
  }
}

/** Query parameters sent to `/v1/usage` and `/v1/usage/export` for a range preset. */
export function usageQueryForRange(range: UsageRange, timeZone?: string): UsageQueryParams {
  const tz = timeZone ?? localTimeZone();
  if (range === "all") {
    return tz ? { tz } : {};
  }
  return tz ? { days: USAGE_RANGE_DAYS[range], tz } : { days: USAGE_RANGE_DAYS[range] };
}

/** Deterministic download filename for the current range and date. */
export function exportFileName(range: UsageRange, now = new Date()): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  const suffix = range === "all" ? "all" : `${USAGE_RANGE_DAYS[range]}d`;
  return `praxi-usage-${suffix}-${y}-${m}-${d}.csv`;
}

/** Prompt + completion tokens (cached tokens are a subset of prompt tokens). */
export function totalTokens(u: Pick<UsageTotalsLike, "prompt_tokens" | "completion_tokens">): number {
  return u.prompt_tokens + u.completion_tokens;
}

/** True when calls exist in this bucket and none reported token counts. */
export function isAllUnknownUsage(u: { model_calls: number; calls_without_usage?: number }): boolean {
  return u.model_calls > 0 && (u.calls_without_usage ?? 0) >= u.model_calls;
}

/** Sort models for display: highest known cost first, then tokens, then calls, then name. */
export function sortModelUsage<T extends ModelUsageLike>(models: readonly T[]): T[] {
  return [...models].sort((a, b) => {
    const ca = a.cost_usd ?? -1;
    const cb = b.cost_usd ?? -1;
    if (ca !== cb) {
      return cb - ca;
    }
    const ta = totalTokens(a);
    const tb = totalTokens(b);
    if (ta !== tb) {
      return tb - ta;
    }
    if (a.model_calls !== b.model_calls) {
      return b.model_calls - a.model_calls;
    }
    return a.model.localeCompare(b.model);
  });
}

/** Format a USD cost or return `unknownLabel` when pricing is unknown. */
export function formatUsageCost(
  costUsd: number | null | undefined,
  locale: string,
  unknownLabel: string
): string {
  if (costUsd === null || costUsd === undefined) {
    return unknownLabel;
  }
  return new Intl.NumberFormat(locale, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 4
  }).format(costUsd);
}

/** Format a token count or return `unknownLabel` when no call in the bucket reported tokens. */
export function formatTokenCount(
  value: number,
  allUnknown: boolean,
  locale: string,
  unknownLabel: string
): string {
  if (allUnknown) {
    return unknownLabel;
  }
  return new Intl.NumberFormat(locale).format(value);
}

/** Format `YYYY-MM-DD` as a short localized date without timezone off-by-one shifts. */
export function shortDayLabel(day: string, locale: string): string {
  const parts = day.split("-").map(Number);
  if (parts.length !== 3 || parts.some((n) => !Number.isFinite(n))) {
    return day;
  }
  const [y, m, d] = parts;
  return new Intl.DateTimeFormat(locale, {
    month: "short",
    day: "numeric",
    timeZone: "UTC"
  }).format(new Date(Date.UTC(y, m - 1, d)));
}

/** Pick evenly spaced X-axis label indices so 7-, 30- and 90-day charts stay legible. */
export function pickLabelIndices(count: number, maxLabels = 5): number[] {
  if (count <= 0) {
    return [];
  }
  if (count <= maxLabels) {
    return Array.from({ length: count }, (_, i) => i);
  }
  const indices = new Set<number>([0, count - 1]);
  const segments = maxLabels - 1;
  for (let i = 1; i < segments; i++) {
    indices.add(Math.round((i * (count - 1)) / segments));
  }
  return [...indices].sort((a, b) => a - b);
}

/** Compute inline-SVG bar coordinates for the daily trend chart. */
export function buildDailyBars(
  daily: readonly DailyUsageLike[],
  metric: TrendMetric,
  plotWidth = 560,
  plotHeight = 100,
  /** Keep the scale at least this high (so limit lines drawn above the data stay on the chart). */
  scaleFloor = 0
): DailyChartModel {
  if (daily.length === 0) {
    return { bars: [], max: 0, hasData: false, labelIndices: [] };
  }

  const raw = daily.map((d) => {
    const tokens = totalTokens(d);
    const calls = d.model_calls;
    const costUsd = d.cost_usd ?? null;
    const callsWithoutUsage = d.calls_without_usage ?? 0;
    const callsWithoutCost = d.calls_without_cost ?? 0;
    const unknownOnly =
      metric === "cost"
        ? calls > 0 && costUsd === null
        : metric === "tokens"
          ? isAllUnknownUsage(d)
          : false;
    const value =
      metric === "tokens"
        ? unknownOnly
          ? null
          : tokens
        : metric === "calls"
          ? calls
          : costUsd;
    return {
      day: d.day,
      value,
      unknownOnly,
      calls,
      tokens,
      costUsd,
      callsWithoutUsage,
      callsWithoutCost
    };
  });

  const dataMax = raw.reduce((m, r) => (r.value !== null && r.value > m ? r.value : m), 0);
  const max = Math.max(dataMax, scaleFloor);
  const hasData = raw.some((r) => r.calls > 0);
  const slot = plotWidth / raw.length;
  const gap = Math.min(4, slot * 0.25);
  const width = Math.max(2, slot - gap);
  const minBar = 3;
  const unknownBar = Math.min(10, Math.max(4, plotHeight * 0.1));

  const bars: DailyBar[] = raw.map((r, i) => {
    const x = i * slot + (slot - width) / 2;
    let height = 0;
    if (r.unknownOnly) {
      height = unknownBar;
    } else if (r.value !== null && r.value > 0 && max > 0) {
      height = Math.max(minBar, (r.value / max) * plotHeight);
    }
    const y = plotHeight - height;
    return {
      ...r,
      x: Number(x.toFixed(2)),
      y: Number(y.toFixed(2)),
      width: Number(width.toFixed(2)),
      height: Number(height.toFixed(2))
    };
  });

  return {
    bars,
    max,
    hasData,
    labelIndices: pickLabelIndices(raw.length, 5)
  };
}
