import type { BotSpendLimits } from "../api/types.ts";
import { parseCountText, parseUsdText } from "./limitInput.ts";

export type LimitPeriod = "daily" | "weekly" | "monthly";
export type LimitField = "soft_usd" | "hard_usd" | "soft_tokens" | "hard_tokens";
export const LIMIT_PERIODS: readonly LimitPeriod[] = ["daily", "weekly", "monthly"];
export const LIMIT_FIELDS: readonly LimitField[] = ["soft_usd", "hard_usd", "soft_tokens", "hard_tokens"];

/** Window event: open the bot editor on its Limits section (the "Raise limit" button). */
export const OPEN_BOT_LIMITS_EVENT = "praxi:open-bot-limits";

export const MIN_USD = 0.01;
export const MAX_USD = 1_000_000;
export const MAX_TOKENS = 1_000_000_000_000;

export interface LimitReached {
  period: LimitPeriod;
  metric: "usd" | "tokens";
  /** When the counter resets (ms since epoch); null when unknown. */
  resetsMs: number | null;
}

const PERIODS = new Set<string>(LIMIT_PERIODS);

/** The daemon's typed stop: `[bot_limit_reached period=daily metric=usd resets_ms=…] …`. */
export function parseLimitReached(message: string): LimitReached | null {
  const m = /\[bot_limit_reached period=(\w+) metric=(\w+) resets_ms=(\d+)\]/.exec(message);
  if (!m) {
    return null;
  }
  const [, period = "", metric = "", resets = "0"] = m;
  const resetsMs = Number(resets);
  return {
    period: PERIODS.has(period) ? (period as LimitPeriod) : "daily",
    metric: metric === "tokens" ? "tokens" : "usd",
    resetsMs: resetsMs > 0 ? resetsMs : null
  };
}

export type LimitsDraft = Record<LimitPeriod, Record<LimitField, string>>;

const textOf = (v: number | null | undefined): string => (v === null || v === undefined ? "" : String(v));

export function draftFromLimits(l: BotSpendLimits["limits"]): LimitsDraft {
  const one = (p: BotSpendLimits["limits"]["daily"]): Record<LimitField, string> => ({
    soft_usd: textOf(p?.soft_usd),
    hard_usd: textOf(p?.hard_usd),
    soft_tokens: textOf(p?.soft_tokens),
    hard_tokens: textOf(p?.hard_tokens)
  });
  return { daily: one(l.daily), weekly: one(l.weekly), monthly: one(l.monthly) };
}

type Parsed = number | null | "invalid";

function parseField(field: LimitField, raw: string): Parsed {
  if (raw.trim() === "") {
    return null;
  }
  // Strict readers: "10.000" is rejected, never read as ten (see limitInput.ts).
  if (field.endsWith("usd")) {
    const v = parseUsdText(raw);
    return v !== null && v >= MIN_USD && v <= MAX_USD ? v : "invalid";
  }
  const v = parseCountText(raw);
  return v !== null && v >= 1 && v <= MAX_TOKENS ? v : "invalid";
}

export type LimitsResult =
  | { ok: true; limits: BotSpendLimits["limits"] }
  | { ok: false; period: LimitPeriod; field: LimitField | "order" };

/** Typed draft to the API shape; names the first bad field (soft above hard counts as `order`). */
export function limitsFromDraft(d: LimitsDraft): LimitsResult {
  const out = {} as Record<LimitPeriod, Record<LimitField, number | null>>;
  for (const period of LIMIT_PERIODS) {
    const p: Record<LimitField, number | null> = { soft_usd: null, hard_usd: null, soft_tokens: null, hard_tokens: null };
    for (const field of LIMIT_FIELDS) {
      const v = parseField(field, d[period][field]);
      if (v === "invalid") {
        return { ok: false, period, field };
      }
      p[field] = v;
    }
    out[period] = p;
    if ((p.soft_usd !== null && p.hard_usd !== null && p.soft_usd > p.hard_usd) || (p.soft_tokens !== null && p.hard_tokens !== null && p.soft_tokens > p.hard_tokens)) {
      return { ok: false, period, field: "order" };
    }
  }
  return { ok: true, limits: out };
}

/** Share of `limit` used, clamped to 0..1 (0 without a limit). */
export function usedFraction(used: number, limit: number | null | undefined): number {
  return limit ? Math.min(1, Math.max(0, used / limit)) : 0;
}
