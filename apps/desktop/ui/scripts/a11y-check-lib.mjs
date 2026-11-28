// Shared helpers for scripts/check-a11y.mjs (budget, sharding, timeout detection).

/** @returns {{ index: number, total: number } | null} */
export function parseShardArg(value) {
  if (value == null || value === '') return null;
  const m = /^(\d+)\/(\d+)$/.exec(value);
  if (!m) throw new Error(`invalid --shard=${value} (expected i/n, e.g. 1/2)`);
  const index = Number(m[1]);
  const total = Number(m[2]);
  if (!Number.isInteger(index) || !Number.isInteger(total) || index < 1 || index > total || total < 1) {
    throw new Error(`invalid --shard=${value} (index must be 1..n)`);
  }
  return { index, total };
}

/** Deterministic split: case i goes to shard ((i % total) + 1). */
export function pickShard(cases, shard) {
  if (!shard) return cases;
  return cases.filter((_, i) => (i % shard.total) + 1 === shard.index);
}

/**
 * Wall-clock budget (seconds) for axe cases + keyboard ring + focus-return after the mock build.
 * CI uses measured ~1.5s/case on ubuntu-latest (128 cases ≈ 110s after build, 197s full step).
 */
export function resolveBudgetSeconds(caseCount, { explicit, ci }) {
  if (explicit != null && explicit !== '') return Number(explicit);
  if (ci) return Math.ceil(caseCount * 1.35 + 90);
  return 120;
}

export function isTimeoutError(err) {
  const msg = err instanceof Error ? err.message : String(err);
  return /\btimeout\b/i.test(msg) || err?.name === 'TimeoutError';
}