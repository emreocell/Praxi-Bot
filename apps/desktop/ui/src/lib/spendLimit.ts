import type { SpendLimit } from "../api/types.ts";
import { parseUsdText } from "./limitInput.ts";

export const MIN_LIMIT_USD = 1;
export const MAX_LIMIT_USD = 1_000_000;

/** A limit typed by the user: at least $1, at most $1,000,000; a comma decimal is accepted. */
export function parseLimitInput(raw: string): number | null {
  // A thousands separator ("1.000", "1,000") is rejected, not read as a decimal point.
  const v = parseUsdText(raw);
  return v !== null && v >= MIN_LIMIT_USD && v <= MAX_LIMIT_USD ? v : null;
}

/** Where the month stands against the limit (drives the tone and the sentence under the bar). */
export type Pace = "off" | "reached" | "warn" | "over" | "under" | "noForecast";

export function paceOf(s: SpendLimit): Pace {
  if (s.limit_usd === null || s.limit_usd === undefined) {
    return "off";
  }
  if (s.spent_usd >= s.limit_usd) {
    return "reached";
  }
  if (s.spent_usd >= 0.8 * s.limit_usd) {
    return "warn";
  }
  if (s.forecast_usd === null || s.forecast_usd === undefined) {
    return "noForecast";
  }
  return s.forecast_usd > s.limit_usd ? "over" : "under";
}

/** Share of the limit already spent, clamped to 0..1 (0 without a limit). */
export function usedFraction(s: SpendLimit): number {
  return s.limit_usd ? Math.min(1, Math.max(0, s.spent_usd / s.limit_usd)) : 0;
}
