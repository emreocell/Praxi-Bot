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
    if (!isMockMode()) {
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
