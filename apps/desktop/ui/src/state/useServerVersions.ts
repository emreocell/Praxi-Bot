import { useCallback, useEffect, useState } from "react";
import type { ApiClient } from "../api/client";

/** What this computer knows about one server's version: a number, "asking", or "could not ask". */
export type ServerVersion = { state: "loading" } | { state: "known"; version: string; releaseTag: string | null } | { state: "unknown" };

/**
 * The versions of the given servers, asked one after the other (a server that is off simply stays
 * "unknown"). `recheck` asks again. Nothing is asked while `enabled` is false.
 */
export function useServerVersions(
  api: Pick<ApiClient, "getHostVersion"> | null,
  hostIds: string[],
  enabled: boolean
): { versions: Record<string, ServerVersion>; recheck: () => void } {
  const [versions, setVersions] = useState<Record<string, ServerVersion>>({});
  const [round, setRound] = useState(0);
  const key = hostIds.join(",");
  useEffect(() => {
    if (!enabled || !api || key === "") return undefined;
    let stopped = false;
    const ids = key.split(",");
    setVersions(Object.fromEntries(ids.map((id) => [id, { state: "loading" } as ServerVersion])));
    void (async () => {
      for (const id of ids) {
        let next: ServerVersion;
        try {
          const got = await api.getHostVersion(id);
          next = { state: "known", version: got.version, releaseTag: got.release_tag ?? null };
        } catch {
          next = { state: "unknown" };
        }
        if (stopped) return;
        setVersions((v) => ({ ...v, [id]: next }));
      }
    })();
    return () => {
      stopped = true;
    };
  }, [api, key, enabled, round]);
  const recheck = useCallback(() => setRound((n) => n + 1), []);
  return { versions, recheck };
}
