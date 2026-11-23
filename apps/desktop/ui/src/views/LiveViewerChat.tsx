import { MessageSquare } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import { humanLine } from "../lib/humanError";
import { loadChatOpen, saveChatOpen } from "../lib/liveViewToolbar";
import { useDaemon } from "../state/DaemonContext";
import type { NeedsYouEntry } from "../state/timeline";
import styles from "./LiveViewer.module.css";

export interface LiveChatSlots {
  status: HTMLDivElement | null;
  composer: HTMLDivElement | null;
}

function PendingApproval({ approval, client }: { approval: NeedsYouEntry; client: ApiClient }) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const decide = async (decision: "approve" | "deny") => {
    setBusy(true);
    setError(null);
    try {
      await client.decideApproval(approval.callId, decision, decision === "approve" ? "once" : undefined);
    } catch (err) {
      setError(humanLine(err));
      setBusy(false);
    }
  };

  return (
    <li>
      <div className={styles.chatApproval}>
        <span>{approval.title}</span>
        <Button size="sm" variant="primary" disabled={busy} onClick={() => void decide("approve")}>
          {t("liveView.chat.approve")}
        </Button>
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => void decide("deny")}>
          {t("liveView.chat.reject")}
        </Button>
      </div>
      {error ? <p className={styles.errorInline} role="alert">{error}</p> : null}
    </li>
  );
}

export function LiveViewerChat({
  onChatSlots
}: {
  onChatSlots?: (slots: LiveChatSlots | null) => void;
}) {
  const { client, needs } = useDaemon();
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(() => loadChatOpen());
  const status = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLDivElement>(null);
  const approvals = needs.filter((entry) => entry.kind === "approval");

  useLayoutEffect(() => {
    if (!onChatSlots) return undefined;
    onChatSlots({ status: status.current, composer: composer.current });
    return () => onChatSlots(null);
  }, [onChatSlots, expanded]);

  if (!onChatSlots && (!client || approvals.length === 0)) return null;

  return (
    <section className={styles.chatOverlay} data-open={expanded ? "true" : "false"} data-testid="live-viewer-chat" aria-label={t("liveView.chat.title")}>
      {client && approvals.length ? (
        <ul className={styles.chatApprovals} aria-label={t("liveView.approvalsWaiting", { count: approvals.length })} data-testid="live-viewer-approvals">
          {approvals.map((approval) => <PendingApproval key={approval.callId} approval={approval} client={client} />)}
        </ul>
      ) : null}
      {onChatSlots ? (
        <>
          <div className={styles.chatOverlayBar}>
            <Button
              size="sm"
              variant="secondary"
              aria-expanded={expanded}
              aria-controls="live-viewer-chat-body"
              onClick={() =>
                setExpanded((open) => {
                  saveChatOpen(!open);
                  return !open;
                })
              }
            >
              <MessageSquare aria-hidden="true" />
              {t(expanded ? "liveView.chat.collapse" : "liveView.chat.title")}
            </Button>
            <div ref={status} className={styles.chatStatusSlot} />
          </div>
          {expanded ? <div ref={composer} id="live-viewer-chat-body" data-testid="live-viewer-composer" /> : null}
        </>
      ) : null}
    </section>
  );
}
