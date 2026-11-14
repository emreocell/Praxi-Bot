/**
 * Per-account quota ("Kalan kota"): how much of a subscription window is left.
 * The daemon has no quota endpoint yet, so production code has no source and
 * shows nothing; numbers are never made up (see `useQuotas`).
 */
export interface AccountQuota {
  /** Stable id, e.g. `cli:claude`. */
  id: string;
  /** Brand name for the logo (`brandIcon`). */
  brand: string;
  /** Account name shown to assistive tech and as a tooltip. */
  label: string;
  /** Percent of the window still available, 0-100. */
  remainingPct: number;
  /** When the window resets, epoch ms. */
  resetsAtMs?: number | null;
}

/** Quotas with a usable number, clamped to 0-100 and rounded; anything else is dropped. */
export function visibleQuotas(list: readonly AccountQuota[] | null | undefined): AccountQuota[] {
  const out: AccountQuota[] = [];
  for (const q of list ?? []) {
    if (typeof q.remainingPct !== "number" || !Number.isFinite(q.remainingPct)) {
      continue;
    }
    out.push({ ...q, remainingPct: Math.round(Math.min(100, Math.max(0, q.remainingPct))) });
  }
  return out;
}

/** Radius of the ring in a 46-unit box (matches the prototype). */
export const RING_RADIUS = 20;

/** `stroke-dasharray` of the filled arc for a percentage. */
export function ringDash(pct: number): string {
  const c = 2 * Math.PI * RING_RADIUS;
  return `${((Math.min(100, Math.max(0, pct)) / 100) * c).toFixed(2)} ${c.toFixed(2)}`;
}

/** `low` below 15 %, `mid` below 40 %, else `ok`. */
export function quotaTone(pct: number): "ok" | "mid" | "low" {
  return pct < 15 ? "low" : pct < 40 ? "mid" : "ok";
}
