import { markApprovalDecided } from "../lib/approvalTried";
import { MessageSquare, Minimize2, Maximize2, X } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import { humanLine } from "../lib/humanError";
import { isAlreadyDecidedError } from "../lib/popout";
import type { ChatDockAction, ChatDockMode } from "../lib/liveChatDock";
import { useDaemon } from "../state/DaemonContext";
import type { NeedsYouEntry } from "../state/timeline";
import styles from "./LiveViewer.module.css";

/**
 * Where the chat view puts itself while the full-screen live view is open.
 * - `root`: the whole chat (history, run status, composer). Set while the chat is small or large;
 *   it is one element for both, so growing or shrinking the chat never remounts the conversation.
 * - `status`: a one-line slot beside the closed "Chat" button for "Bot is replying...".
 */
export interface LiveChatSlots {
  root: HTMLDivElement | null;
  status: HTMLDivElement | null;
}

function PendingApproval({ approval, client }: { approval: NeedsYouEntry; client: ApiClient }) {
  const { t } = useTranslation();
  const { refreshRuns } = useDaemon();
  const [busy, setBusy] = useState(false);
  const [locked, setLocked] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const decide = async (decision: "approve" | "deny") => {
    setBusy(true);
    setError(null);
    try {
      await client.decideApproval(approval.callId, decision, decision === "approve" ? "once" : undefined);
      markApprovalDecided();
    } catch (err) {
      if (isAlreadyDecidedError(err)) {
        setLocked(true);
        setError(t("chat.approval.decidedElsewhere"));
        void refreshRuns();
      } else {
        setError(humanLine(err));
        setBusy(false);
      }
    }
  };

  return (
    <li>
      <div className={styles.chatApproval}>
        <span>{approval.title}</span>
        <Button size="sm" variant="primary" disabled={busy || locked} onClick={() => void decide("approve")}>
          {t("chat.approval.compact.allow")}
        </Button>
        <Button size="sm" variant="ghost" disabled={busy || locked} onClick={() => void decide("deny")}>
          {t("chat.approval.deny")}
        </Button>
      </div>
      {error ? <p className={styles.errorInline} role="alert">{error}</p> : null}
    </li>
  );
}

/**
 * The chat inside the full-screen live view: a "Chat" button, a small chat card, or a chat column
 * (see `lib/liveChatDock`). The conversation itself is the real chat view, which portals into
 * the slot this renders; waiting approvals stay answerable in every state.
 */
export function LiveViewerChat({
  mode,
  onAction,
  onChatSlots
}: {
  mode: ChatDockMode;
  onAction: (action: ChatDockAction) => void;
  onChatSlots?: (slots: LiveChatSlots | null) => void;
}) {
  const { client, needs } = useDaemon();
  const { t } = useTranslation();
  const slot = useRef<HTMLDivElement>(null);
  const status = useRef<HTMLDivElement>(null);
  const opener = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(false);
  const approvals = needs.filter((entry) => entry.kind === "approval");
  const hasChat = Boolean(onChatSlots);
  const shown: ChatDockMode = hasChat ? mode : "closed";

  useLayoutEffect(() => {
    if (!onChatSlots) return undefined;
    onChatSlots({
      root: shown === "closed" ? null : slot.current,
      status: shown === "closed" ? status.current : null
    });
    return () => onChatSlots(null);
  }, [onChatSlots, shown]);

  // Closing the chat hands the focus back to the button that opens it again.
  useLayoutEffect(() => {
    if (shown === "closed" && wasOpen.current) opener.current?.focus();
    wasOpen.current = shown !== "closed";
  }, [shown]);

  if (!hasChat && (!client || approvals.length === 0)) return null;

  const approvalList =
    client && approvals.length > 0 ? (
      <ul
        className={styles.chatApprovals}
        aria-label={t("liveView.approvalsWaiting", { count: approvals.length })}
        data-testid="live-viewer-approvals"
      >
        {approvals.map((approval) => (
          <PendingApproval key={approval.callId} approval={approval} client={client} />
        ))}
      </ul>
    ) : null;

  return (
    <section
      className={styles.chatDock}
      data-mode={shown}
      data-testid="live-viewer-chat"
      aria-label={t("liveView.chat.title")}
    >
      {shown === "closed" ? (
        <div className={styles.chatClosed}>
          {approvalList ? <div className={styles.chatApprovalsBox}>{approvalList}</div> : null}
          {hasChat ? (
            <div className={styles.chatPill}>
              <Button
                ref={opener}
                size="sm"
                variant="secondary"
                aria-expanded={false}
                aria-controls="live-viewer-chat-body"
                data-testid="live-viewer-chat-open"
                onClick={() => onAction("open")}
              >
                <MessageSquare aria-hidden="true" />
                {t("liveView.chat.button")}
              </Button>
              <div ref={status} className={styles.chatStatusSlot} />
            </div>
          ) : null}
        </div>
      ) : (
        <div className={styles.chatCard} data-mode={shown} data-testid="live-viewer-chat-card">
          <div className={styles.chatCardHead}>
            <MessageSquare aria-hidden="true" />
            <span className={styles.chatCardTitle}>{t("liveView.chat.title")}</span>
            {shown === "small" ? (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => onAction("expand")}
                data-testid="live-viewer-chat-expand"
              >
                <Maximize2 aria-hidden="true" />
                {t("liveView.chat.expand")}
              </Button>
            ) : (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => onAction("collapse")}
                data-testid="live-viewer-chat-collapse"
              >
                <Minimize2 aria-hidden="true" />
                {t("liveView.chat.collapse")}
              </Button>
            )}
            <button
              type="button"
              className={styles.iconBtn}
              title={t("liveView.chat.close")}
              aria-label={t("liveView.chat.close")}
              data-testid="live-viewer-chat-close"
              onClick={() => onAction("close")}
            >
              <X />
            </button>
          </div>
          {approvalList ? <div className={styles.chatApprovalsBox}>{approvalList}</div> : null}
          <div
            ref={slot}
            id="live-viewer-chat-body"
            className={styles.chatSlot}
            data-testid="live-viewer-composer"
          />
        </div>
      )}
    </section>
  );
}
