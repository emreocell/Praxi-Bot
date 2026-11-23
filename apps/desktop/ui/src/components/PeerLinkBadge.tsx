import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client.ts";
import type { PeerLink } from "../api/types.ts";
import { useDaemon } from "../state/DaemonContext";

export function PeerLinkBadge({
  enabled,
  id,
  className
}: {
  enabled: boolean;
  id?: string;
  className?: string;
}) {
  const { client } = useDaemon();
  const { t } = useTranslation();
  const [value, setValue] = useState<{
    client: ApiClient;
    link: PeerLink | null;
  } | null>(null);

  useEffect(() => {
    if (!enabled || !client) return undefined;
    const abort = new AbortController();
    let pending = false;
    const refresh = async () => {
      if (pending || document.hidden) return;
      pending = true;
      const link = await client.peerLink(abort.signal).catch(() => null);
      pending = false;
      if (!abort.signal.aborted) setValue({ client, link });
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5_000);
    return () => {
      abort.abort();
      window.clearInterval(timer);
      setValue(null);
    };
  }, [client, enabled]);

  const link = enabled && value?.client === client ? value.link : null;
  return link ? (
    <small
      id={id}
      className={className}
      data-testid="peer-link-badge"
      tabIndex={0}
      title={t(link.relay ? "peerLink.relayHint" : "peerLink.directHint")}
    >
      {t(link.relay ? "peerLink.relay" : "peerLink.direct", { rtt: link.rtt_ms })}
    </small>
  ) : null;
}
