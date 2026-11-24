import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { AlarmClock, ArrowUpRight, BellOff, CheckCheck, Trash2 } from "lucide-react";
import { EmptyState } from "../components/EmptyState";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import formStyles from "../components/Form.module.css";
import {
  filterEntries,
  heldByQuietCount,
  HISTORY_MAX,
  HISTORY_MAX_AGE_MS,
  type HistoryFilter,
  NO_FILTER,
  unreadCount
} from "../lib/notificationHistory";
import {
  clearNotificationHistory,
  markAllNotificationsRead,
  markNotificationRead,
  snoozeNotification,
  unsnoozeNotification,
  useNotificationHistory
} from "../store/notificationHistoryStore";
import needsStyles from "./NeedsYouView.module.css";
import styles from "./NotificationHistoryView.module.css";

const KINDS = ["needsYou", "failed", "longRun", "scheduleDone", "digest"] as const;
const SNOOZE_MS = 60 * 60 * 1000;

/** The notification inbox: what was announced (toast shown) or held (quiet hours), newest first. */
export function NotificationHistoryView({
  onOpenChat,
  onOpenNeeds
}: {
  onOpenChat: (chatId: string) => void;
  onOpenNeeds: () => void;
}) {
  const { t, i18n } = useTranslation();
  const list = useNotificationHistory();
  const [filter, setFilter] = useState<HistoryFilter>(NO_FILTER);
  const [now, setNow] = useState(() => Date.now());
  // Re-check once a minute so a snooze that ran out brings its entry back.
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(id);
  }, []);
  const unread = unreadCount(list, now);
  const heldQuiet = heldByQuietCount(list, now);
  const visible = useMemo(() => filterEntries(list, filter, now), [list, filter, now]);
  const snoozedCount = useMemo(
    () => filterEntries(list, { ...NO_FILTER, snoozed: true }, now).length,
    [list, now]
  );
  const bots = useMemo(() => {
    const m = new Map<string, string>();
    for (const n of list) if (n.botId) m.set(n.botId, n.botName || n.botId);
    return [...m.entries()];
  }, [list]);
  const listRef = useRef<HTMLUListElement>(null);
  // Up/Down (or j/k) walk through the entries; Enter or Space presses the focused button.
  const onListKey = (ev: KeyboardEvent<HTMLUListElement>) => {
    const down = ev.key === "ArrowDown" || ev.key === "j";
    const up = ev.key === "ArrowUp" || ev.key === "k";
    if (!down && !up) return;
    const rows = [...(listRef.current?.querySelectorAll<HTMLElement>("[data-notify-open]") ?? [])];
    const at = rows.indexOf(document.activeElement as HTMLElement);
    const next = rows[Math.max(0, Math.min(rows.length - 1, at + (down ? 1 : -1)))];
    if (next) {
      ev.preventDefault();
      next.focus();
    }
  };
  const days = Math.round(HISTORY_MAX_AGE_MS / 86_400_000);
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
        <p className={formStyles.hint} data-testid="notify-history-retention">
          {t("notifyHistory.retention", { days, max: HISTORY_MAX })}
        </p>
        {heldQuiet > 0 ? (
          <p className={styles.heldNote} data-testid="notify-history-quiet-note">
            {t("notifyHistory.quietNote", { count: heldQuiet })}
          </p>
        ) : null}
        {list.length > 0 ? (
          <div className={styles.filters} role="group" aria-label={t("notifyHistory.filters")}>
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={filter.unreadOnly}
                onChange={(e) => setFilter({ ...filter, unreadOnly: e.target.checked })}
                data-testid="notify-filter-unread"
              />
              {t("notifyHistory.onlyUnread")}
            </label>
            <label className={styles.field}>
              <span>{t("notifyHistory.filterType")}</span>
              <select
                className={formStyles.select}
                value={filter.kind}
                onChange={(e) => setFilter({ ...filter, kind: e.target.value as HistoryFilter["kind"] })}
                data-testid="notify-filter-kind"
              >
                <option value="all">{t("notifyHistory.allTypes")}</option>
                {KINDS.map((k) => (
                  <option key={k} value={k}>
                    {t(`notifyHistory.kind.${k}`)}
                  </option>
                ))}
              </select>
            </label>
            {bots.length > 1 ? (
              <label className={styles.field}>
                <span>{t("notifyHistory.filterBot")}</span>
                <select
                  className={formStyles.select}
                  value={filter.botId}
                  onChange={(e) => setFilter({ ...filter, botId: e.target.value })}
                  data-testid="notify-filter-bot"
                >
                  <option value="">{t("notifyHistory.allBots")}</option>
                  {bots.map(([id, name]) => (
                    <option key={id} value={id}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
            ) : null}
            {snoozedCount > 0 || filter.snoozed ? (
              <label className={styles.check}>
                <input
                  type="checkbox"
                  checked={filter.snoozed}
                  onChange={(e) => setFilter({ ...filter, snoozed: e.target.checked })}
                  data-testid="notify-filter-snoozed"
                />
                {t("notifyHistory.showSnoozed", { count: snoozedCount })}
              </label>
            ) : null}
          </div>
        ) : null}
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
        ) : visible.length === 0 ? (
          <EmptyState
            icon={BellOff}
            title={t("notifyHistory.noMatchTitle")}
            actionLabel={t("notifyHistory.resetFilters")}
            onAction={() => setFilter(NO_FILTER)}
            testId="notify-history-no-match"
          >
            {t("notifyHistory.noMatch")}
          </EmptyState>
        ) : (
          <ul className={needsStyles.list} ref={listRef} onKeyDown={onListKey}>
            {visible.map((n) => (
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
                  {filter.snoozed ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => unsnoozeNotification(n.id)}
                      data-testid="notify-unsnooze"
                    >
                      {t("notifyHistory.unsnooze")}
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => snoozeNotification(n.id, Date.now() + SNOOZE_MS)}
                      title={t("notifyHistory.snoozeHint")}
                      data-testid="notify-snooze"
                    >
                      <AlarmClock />
                      {t("notifyHistory.snooze")}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="primary"
                    data-notify-open
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
