import { ArrowUpRight, BellOff, CheckCheck, Trash2 } from "lucide-react";
import { EmptyState } from "../components/EmptyState";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import formStyles from "../components/Form.module.css";
import { unreadCount } from "../lib/notificationHistory";
import {
  clearNotificationHistory,
  markAllNotificationsRead,
  markNotificationRead,
  useNotificationHistory
} from "../store/notificationHistoryStore";
import needsStyles from "./NeedsYouView.module.css";
import styles from "./NotificationHistoryView.module.css";

/** What was announced (toast shown) or held (quiet hours), newest first. */
export function NotificationHistoryView({
  onOpenChat,
  onOpenNeeds
}: {
  onOpenChat: (chatId: string) => void;
  onOpenNeeds: () => void;
}) {
  const { t, i18n } = useTranslation();
  const list = useNotificationHistory();
  const unread = unreadCount(list);
  return (
    <div className={needsStyles.pane}>
      <div className={needsStyles.container}>
        <header className={styles.head}>
          <div>
            <h1 className={needsStyles.title}>{t("notifyHistory.title")}</h1>
            <p className={formStyles.hint}>{t("notifyHistory.subtitle")}</p>
          </div>
          <div className={styles.actions}>
            <Button
              size="sm"
              variant="secondary"
              disabled={unread === 0}
              onClick={markAllNotificationsRead}
              data-testid="notify-history-mark-read"
            >
              <CheckCheck />
              {t("notifyHistory.markAllRead")}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              disabled={list.length === 0}
              onClick={clearNotificationHistory}
              data-testid="notify-history-clear"
            >
              <Trash2 />
              {t("notifyHistory.clear")}
            </Button>
          </div>
        </header>
        <span role="status" aria-live="polite" className={formStyles.hint}>
          {list.length > 0 ? t("notifyHistory.unreadCount", { count: unread }) : ""}
        </span>
        {list.length === 0 ? (
          <EmptyState
            icon={BellOff}
            title={t("notifyHistory.emptyTitle")}
            actionLabel={t("notifyHistory.emptyAction")}
            onAction={onOpenNeeds}
            testId="notify-history-empty"
          >
            {t("notifyHistory.empty")}
          </EmptyState>
        ) : (
          <ul className={needsStyles.list}>
            {list.map((n) => (
              <li key={n.id} data-testid="notify-history-item">
                <Card
                  padding="md"
                  className={`${needsStyles.row} ${n.read ? "" : styles.unread}`}
                >
                  <div className={needsStyles.text}>
                    <span className={needsStyles.kind}>
                      {t(`notifyHistory.kind.${n.kind}`)}
                      {n.botName ? ` · ${n.botName}` : ""}
                    </span>
                    <span className={needsStyles.summary}>
                      {n.title}
                      {n.body ? `: ${n.body}` : ""}
                    </span>
                    <span className={styles.meta}>
                      <span>{new Date(n.ts).toLocaleString(i18n.language)}</span>
                      <span className={n.shown ? undefined : styles.held}>
                        {t(
                          n.shown
                            ? "notifyHistory.shown"
                            : n.heldBy === "muted"
                              ? "notifyHistory.heldMuted"
                              : "notifyHistory.held"
                        )}
                      </span>
                      {n.read ? null : <span>{t("notifyHistory.unread")}</span>}
                    </span>
                  </div>
                  <Button
                    size="sm"
                    variant="primary"
                    onClick={() => {
                      markNotificationRead(n.id);
                      if (n.chatId) {
                        onOpenChat(n.chatId);
                      } else {
                        onOpenNeeds();
                      }
                    }}
                  >
                    <ArrowUpRight />
                    {t(n.chatId ? "needs.open" : "notifyHistory.openNeeds")}
                  </Button>
                </Card>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
