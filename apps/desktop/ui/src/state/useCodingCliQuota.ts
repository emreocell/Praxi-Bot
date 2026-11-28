import { useEffect, useState } from "react";
import { isMockMode } from "../api/mockMode";
import type { AccountQuota } from "../lib/quota";

const CACHE_MS = 15 * 60 * 1000;
const mem = new Map<string, { at: number; quota: AccountQuota | null }>();

function quotaForEngine(id: string, list: readonly AccountQuota[]): AccountQuota | null {
  const hit = list.find((q) => q.id === `cli:${id}` || q.id === id);
  return hit ?? null;
}

/**
 * Subscription window left for one coding CLI, when we can learn it without a slow probe.
 * Results are cached with a timestamp; nothing is invented in production (mock fills samples).
 */
export function useCodingCliQuota(engineId: string): { quota: AccountQuota | null; fetchedAtMs: number | null } {
  const [, setTick] = useState(0);
  const key = engineId;

  useEffect(() => {
    if (!(import.meta.env.DEV || import.meta.env.VITE_MOCK === "1") || !isMockMode()) {
      return undefined;
    }
    const cached = mem.get(key);
    if (cached && Date.now() - cached.at < CACHE_MS) {
      return undefined;
    }
    let live = true;
    void import("../api/mockQuotas").then((m) => {
      if (!live) {
        return;
      }
      const q = quotaForEngine(engineId, m.MOCK_QUOTAS);
      mem.set(key, { at: Date.now(), quota: q });
      setTick((n) => n + 1);
    });
    return () => {
      live = false;
    };
  }, [engineId, key]);

  // `setTick` re-renders once the mock quotas are cached.
  const cached = mem.get(key);
  return { quota: cached?.quota ?? null, fetchedAtMs: cached?.at ?? null };
}
