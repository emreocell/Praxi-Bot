import { useCallback, useEffect, useState } from "react";
import type { ProvidersDto } from "../api/types";
import { useDaemon } from "./DaemonContext";

/** `GET /v1/providers`, loaded once and on `reload()`; `null` until known. */
export function useProviders(): {
  dto: ProvidersDto | null;
  error: string | null;
  reload: () => Promise<void>;
} {
  const { client } = useDaemon();
  const [dto, setDto] = useState<ProvidersDto | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setDto(await client.providers());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  return { dto, error, reload };
}
