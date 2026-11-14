import { useEffect, useState } from "react";
import { isMockMode } from "../api/mockMode";
import { type AccountQuota, visibleQuotas } from "../lib/quota";

/**
 * Per-account quotas.  The daemon exposes no quota endpoint yet, so outside
 * mock mode this is empty and the UI hides the block; nothing is invented.
 * Mock mode loads sample data so the design can be reviewed.
 */
export function useQuotas(): AccountQuota[] {
  const [list, setList] = useState<AccountQuota[]>([]);
  useEffect(() => {
    // Same guard as DaemonContext: Vite drops the dynamic import (and the
    // sample data chunk) from production builds.
    if (!(import.meta.env.DEV || import.meta.env.VITE_MOCK === "1") || !isMockMode()) {
      return undefined;
    }
    let live = true;
    void import("../api/mockQuotas").then((m) => {
      if (live) {
        setList(visibleQuotas(m.MOCK_QUOTAS));
      }
    });
    return () => {
      live = false;
    };
  }, []);
  return list;
}
