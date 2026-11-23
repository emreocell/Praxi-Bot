/** Helpers for the "Helpers (optional)" settings card (embeddings). Pure, no network. */

/** Every state the daemon can report for a provider (also the i18n keys under `helpers.state.`). */
export const PROVIDER_STATES = [
  "ok",
  "not_allowed",
  "key_missing",
  "cooling_down",
  "key_rejected",
  "rate_limited",
  "offline",
  "dimension_mismatch",
  "rejected",
  "unavailable"
] as const;

export type ProviderState = (typeof PROVIDER_STATES)[number];

/** A state string from the daemon, or `rejected` for one this app does not know yet. */
export function knownState(state: string): ProviderState {
  return (PROVIDER_STATES as readonly string[]).includes(state) ? (state as ProviderState) : "rejected";
}

/** How full a usage bar is, 0..1; 0 when the provider publishes no limit. */
export function meterFraction(used: number, limit?: number | null): number {
  if (!limit || limit <= 0) {
    return 0;
  }
  return Math.min(1, Math.max(0, used / limit));
}

/** Colour tone of a usage bar: calm, getting close, or used up. */
export function meterTone(fraction: number): "ok" | "warn" | "full" {
  if (fraction >= 1) {
    return "full";
  }
  return fraction >= 0.8 ? "warn" : "ok";
}

/** A short count: 1.2K, 30K, 250. */
export function shortCount(n: number): string {
  if (n >= 1_000_000) {
    return `${+(n / 1_000_000).toFixed(1)}M`;
  }
  if (n >= 1000) {
    return `${+(n / 1000).toFixed(1)}K`;
  }
  return String(n);
}

/** Lower-case id from a label: letters, digits and dashes, at most 40 characters. */
export function providerId(label: string): string {
  const id = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return id || "provider";
}

/** The first id based on `base` that no provider uses yet (`base`, `base-2`, ...). */
export function freshId(base: string, taken: readonly string[]): string {
  let id = base;
  for (let n = 2; taken.includes(id); n += 1) {
    id = `${base.slice(0, 36)}-${n}`;
  }
  return id;
}
