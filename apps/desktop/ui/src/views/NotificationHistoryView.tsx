import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent
} from "react";
import {
  AlarmClock,
  ArrowUpRight,
  BellOff,
  Check,
  CheckCheck,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  MessageCircleQuestion,
  Moon,
  Reply,
  RotateCcw,
  RotateCw,
  Trash2
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import { EmptyState } from "../components/EmptyState";
import { useToast } from "../components/Toast";
import { botView } from "../lib/botProfile";
import { humanLine } from "../lib/humanError";
import { formatStamp } from "../lib/messageTime";
import {
  type CollapsedNotification,
  collapseDuplicates,
  filterEntries,
  type FilterCategory,
  groupInbox,
  heldByQuietCount,
  HISTORY_MAX,
  HISTORY_MAX_AGE_MS,
  type HistoryEntry,
  type HistoryFilter,
  NO_FILTER,
  unreadCount
} from "../lib/notificationHistory";
import { BOT_PREVIEW_MAX, inboxByBot, type NotifyPrimary, type NotifyType, primaryAction } from "../lib/notifyInbox";
import { openSettingsTarget } from "../lib/settingsLink";
import { ownerKey } from "../lib/ownerKey";
import { formatRelativeTime } from "../lib/relativeTime";
import { lastTurn } from "../lib/resendTurn";
import { useDaemon } from "../state/DaemonContext";
import {
  clearNotificationHistory,
  markAllNotificationsRead,
  markNotificationsRead,
  resolveNotifications,
  restoreNotificationHistory,
  snoozeNotifications,
  unsnoozeNotifications,
  useNotificationHistory
} from "../store/notificationHistoryStore";
import styles from "./NotificationHistoryView.module.css";

const SNOOZE_MS = 60 * 60 * 1000;
const CATEGORIES: readonly FilterCategory[] = ["all", "needsYou", "runs", "errors"];

/** The notification inbox: grouped, collapsed per bot/run, inline-actionable, keyboard-accessible. */
export function NotificationHistoryView({
  onOpenChat,
  onOpenNeeds,
  privateMode = false
}: {
  onOpenChat: (chatId: string) => void;
  onOpenNeeds: () => void;
  privateMode?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const { bots, needs, client, refreshRuns } = useDaemon();
  const toast = useToast();
  const list = useNotificationHistory();

  const [filter, setFilter] = useState<HistoryFilter>(NO_FILTER);
  const [now, setNow] = useState(() => Date.now());
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  // Bots whose whole list is open (otherwise the first BOT_PREVIEW_MAX rows and "N more").
  const [openBots, setOpenBots] = useState<Set<string>>(() => new Set());
  const [busyKeys, setBusyKeys] = useState<Set<string>>(() => new Set());
  const [undoList, setUndoList] = useState<HistoryEntry[] | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  const hasLiveCountdown = useMemo(
    () => list.some((e) => e.retryAtMs !== undefined && e.retryAtMs > now && e.resolved === undefined),
    [list, now]
  );

  // Tick every second only while a retry countdown is active, otherwise once every 30 s.
  useEffect(() => {
    const intervalMs = hasLiveCountdown ? 1_000 : 30_000;
    const id = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(id);
  }, [hasLiveCountdown]);

  // Clear undo banner when a new notification arrives.
  useEffect(() => {
    if (list.length > 0 && undoList !== null) {
      setUndoList(null);
    }
  }, [list.length, undoList]);

  const liveCallIds = useMemo(() => new Set(needs.map((n) => n.callId)), [needs]);

  const unread = unreadCount(list, now);
  const heldQuiet = heldByQuietCount(list, now);
  const visible = useMemo(() => filterEntries(list, filter, now), [list, filter, now]);
  const snoozedCount = useMemo(
    () => filterEntries(list, { ...NO_FILTER, snoozed: true }, now).length,
    [list, now]
  );
  const needsCount = useMemo(
    () =>
      groupInbox(
        filterEntries(list, { ...NO_FILTER }, now),
        now,
        liveCallIds
      ).needsYou.length,
    [list, now, liveCallIds]
  );

  const botGroups = useMemo(
    () => inboxByBot(collapseDuplicates(visible, liveCallIds), liveCallIds),
    [visible, liveCallIds]
  );

  const setBusy = (key: string, on: boolean) => {
    setBusyKeys((prev) => {
      const next = new Set(prev);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });
  };

  const handleUndo = useCallback(() => {
    if (!undoList) return;
    restoreNotificationHistory(undoList);
    setUndoList(null);
  }, [undoList]);

  const handleClear = () => {
    const prev = clearNotificationHistory();
    if (prev.length === 0) return;
    setUndoList(prev);
    toast.show(t("notifyHistory.clearedToast"), "info", {
      durationMs: 8000,
      action: {
        label: t("notifyHistory.undo"),
        onClick: () => {
          restoreNotificationHistory(prev);
          setUndoList(null);
        }
      }
    });
  };

  // Support Ctrl/Cmd+Z while the undo snapshot is active.
  useEffect(() => {
    if (!undoList) return undefined;
    const onKey = (ev: globalThis.KeyboardEvent) => {
      if ((ev.ctrlKey || ev.metaKey) && !ev.shiftKey && ev.key.toLowerCase() === "z") {
        ev.preventDefault();
        handleUndo();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [undoList, handleUndo]);

  const handleRetry = async (item: CollapsedNotification) => {
    const chatId = item.primary.chatId;
    if (!chatId) {
      onOpenNeeds();
      return;
    }
    if (!client) {
      markNotificationsRead(item.ids);
      onOpenChat(chatId);
      return;
    }
    setBusy(item.key, true);
    try {
      const messages = await client.listMessages(chatId);
      const turn = lastTurn(messages);
      if (turn?.resendable) {
        await client.resendLastTurn(chatId, turn.user.id, await ownerKey(client.baseUrl));
        await refreshRuns();
        resolveNotifications(item.ids, "retried");
        toast.show(t("notifyHistory.retriedToast"), "success");
      } else {
        markNotificationsRead(item.ids);
        onOpenChat(chatId);
      }
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(item.key, false);
    }
  };

  const openNotificationSettings = () => openSettingsTarget({ page: "general", card: "notifications", field: "" });

  const openEntry = (ids: readonly string[], chatId: string) => {
    markNotificationsRead(ids);
    if (chatId) {
      onOpenChat(chatId);
    } else {
      onOpenNeeds();
    }
  };

  const toggleExpanded = (key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  // Up/Down (or j/k) move focus between inbox rows; Home/End jump to ends.
  const onInboxKey = (ev: KeyboardEvent<HTMLDivElement>) => {
    const target = ev.target as HTMLElement | null;
    if (target && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
    const rows = [...(rootRef.current?.querySelectorAll<HTMLElement>("[data-notify-row]") ?? [])];
    if (rows.length === 0) return;
    const currentRow = target?.closest<HTMLElement>("[data-notify-row]") ?? null;
    const at = currentRow ? rows.indexOf(currentRow) : -1;

    if (ev.key === "ArrowDown" || ev.key === "j") {
      ev.preventDefault();
      rows[Math.min(rows.length - 1, at + 1)].focus();
    } else if (ev.key === "ArrowUp" || ev.key === "k") {
      ev.preventDefault();
      rows[Math.max(0, at <= 0 ? 0 : at - 1)].focus();
    } else if (ev.key === "Home") {
      ev.preventDefault();
      rows[0].focus();
    } else if (ev.key === "End") {
      ev.preventDefault();
      rows[rows.length - 1].focus();
    }
  };

  const days = Math.round(HISTORY_MAX_AGE_MS / 86_400_000);

  const TYPE_ICON: Record<NotifyType, typeof CircleCheck> = {
    needsYou: MessageCircleQuestion,
    failed: CircleAlert,
    finished: CircleCheck
  };
  const ACTION_ICON: Record<NotifyPrimary, typeof Reply> = { reply: Reply, retry: RotateCw, open: ArrowUpRight };

  const runPrimary = (action: NotifyPrimary, item: CollapsedNotification) => {
    if (action === "retry") void handleRetry(item);
    else openEntry(item.ids, item.primary.chatId);
  };

  const renderRow = (item: CollapsedNotification, type: NotifyType, botLabel: string) => {
    const n = item.primary;
    const isBusy = busyKeys.has(item.key);
    const isExpanded = expanded.has(item.key);
    const action = primaryAction(item, type, privateMode);
    const TypeIcon = TYPE_ICON[type];
    const ActionIcon = ACTION_ICON[action];

    const retryRemaining =
      n.retryAtMs !== undefined && n.resolved !== "retried"
        ? Math.max(0, Math.ceil((n.retryAtMs - now) / 1000))
        : null;
    const headline =
      n.kind === "failed" && retryRemaining !== null && retryRemaining > 0
        ? t("notify.failed.retrying", {
            reason: t("notify.failed.reasonBusy"),
            seconds: String(retryRemaining)
          })
        : n.title;

    const relTime = formatRelativeTime(item.latestTs, now, i18n.language, t);
    const fullStamp = formatStamp(item.latestTs, i18n.language);

    return (
      <li
        key={item.key}
        className={[styles.rowWrap, item.unread ? styles.rowUnread : ""].filter(Boolean).join(" ")}
        data-testid="notify-history-item"
        data-type={type}
      >
        <div
          className={styles.row}
          tabIndex={0}
          data-notify-row
          onKeyDown={(ev) => {
            if (ev.target !== ev.currentTarget) return;
            if (ev.key === "Enter" || ev.key === " ") {
              ev.preventDefault();
              runPrimary(action, item);
            } else if (ev.key === "r" && item.unread) {
              ev.preventDefault();
              markNotificationsRead(item.ids);
            } else if (ev.key === "s") {
              ev.preventDefault();
              if (filter.snoozed) unsnoozeNotifications(item.ids);
              else snoozeNotifications(item.ids, Date.now() + SNOOZE_MS);
            } else if (ev.key === "ArrowRight" && item.count > 1 && !isExpanded) {
              ev.preventDefault();
              toggleExpanded(item.key);
            } else if (ev.key === "ArrowLeft" && item.count > 1 && isExpanded) {
              ev.preventDefault();
              toggleExpanded(item.key);
            }
          }}
        >
          <span
            className={[styles.unreadDot, item.unread ? styles.unreadDotOn : ""].filter(Boolean).join(" ")}
            aria-hidden="true"
          />
          <span className={styles.typeIcon} data-type={type} aria-hidden="true">
            <TypeIcon />
          </span>
          <div className={styles.body}>
            <div className={styles.titleRow}>
              <span className={styles.headline}>{headline}</span>
              {item.count > 1 ? (
                <button
                  type="button"
                  className={styles.collapsePill}
                  aria-expanded={isExpanded}
                  onClick={() => toggleExpanded(item.key)}
                  data-testid="notify-collapse-toggle"
                >
                  {isExpanded ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />}
                  <span>
                    {n.kind === "needsYou"
                      ? t("notifyHistory.askedTimes", { bot: botLabel, count: item.count })
                      : t("notifyHistory.repeatedTimes", { count: item.count })}
                  </span>
                </button>
              ) : null}
            </div>
            {n.body ? <p className={styles.subline}>{n.body}</p> : null}
            <div className={styles.meta}>
              <span className={styles.typeLabel} data-type={type}>{t(`notifyHistory.type.${type}`)}</span>
              <span aria-hidden="true">·</span>
              <time dateTime={new Date(item.latestTs).toISOString()} title={fullStamp}>
                {relTime}
              </time>
              {!n.shown ? (
                <span className={styles.heldTag} title={t(n.heldBy === "muted" ? "notifyHistory.heldMuted" : "notifyHistory.held")}>
                  {n.heldBy === "muted" ? <BellOff aria-hidden="true" /> : <Moon aria-hidden="true" />}
                  {t(n.heldBy === "muted" ? "notifyHistory.heldMutedShort" : "notifyHistory.heldShort")}
                </span>
              ) : null}
              {n.resolved ? (
                <span className={styles.resolvedTag}>
                  {t(`notifyHistory.resolved.${n.resolved}`)}
                </span>
              ) : null}
              {item.unread ? <span className={styles.srOnly}>{t("notifyHistory.unread")}</span> : null}
            </div>
          </div>
          <div className={styles.rowActions}>
            <Button
              size="sm"
              variant={action === "open" ? "ghost" : action === "reply" ? "primary" : "secondary"}
              disabled={isBusy}
              onClick={() => runPrimary(action, item)}
              data-notify-open
              data-testid={`notify-action-${action}`}
            >
              <ActionIcon aria-hidden="true" />
              {t(`notifyHistory.action.${action}`)}
            </Button>
            {item.unread ? (
              <button
                type="button"
                className={styles.iconAction}
                title={t("notifyHistory.markRead")}
                aria-label={t("notifyHistory.markRead")}
                onClick={() => markNotificationsRead(item.ids)}
                data-testid="notify-mark-read"
              >
                <Check aria-hidden="true" />
              </button>
            ) : null}
            {filter.snoozed ? (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => unsnoozeNotifications(item.ids)}
                data-testid="notify-unsnooze"
              >
                {t("notifyHistory.unsnooze")}
              </Button>
            ) : (
              <button
                type="button"
                className={styles.iconAction}
                title={t("notifyHistory.snoozeHint")}
                aria-label={t("notifyHistory.snooze")}
                onClick={() => snoozeNotifications(item.ids, Date.now() + SNOOZE_MS)}
                data-testid="notify-snooze"
              >
                <AlarmClock aria-hidden="true" />
              </button>
            )}
          </div>
        </div>
        {item.count > 1 && isExpanded ? (
          <ul className={styles.subList} data-testid="notify-collapsed-children">
            {item.entries.map((sub) => (
              <li key={sub.id} className={styles.subRow}>
                <span className={styles.subTitle}>{sub.title}</span>
                <span className={styles.subTime} title={formatStamp(sub.ts, i18n.language)}>
                  {formatRelativeTime(sub.ts, now, i18n.language, t)}
                </span>
              </li>
            ))}
          </ul>
        ) : null}
      </li>
    );
  };

  const renderBot = (group: (typeof botGroups)[number]) => {
    const bot = group.botId ? bots.find((b) => b.id === group.botId) : undefined;
    const view = bot ? botView(bot) : null;
    const botLabel = view?.name || group.botName || t("notifyHistory.systemBot");
    const avatarBot = view ?? { id: group.botId || "praxi", name: botLabel };
    const headId = `notify-bot-${group.botId || "praxi"}`;
    const all = openBots.has(group.botId);
    const shown = all ? group.rows : group.rows.slice(0, BOT_PREVIEW_MAX);
    const hidden = group.rows.length - shown.length;
    const counts = (["needsYou", "failed", "finished"] as const).filter((k) => group.counts[k] > 0);
    return (
      <section key={group.botId || "praxi"} className={styles.group} aria-labelledby={headId} data-testid="notify-bot-group">
        <header className={styles.groupHeader}>
          <BotAvatar bot={avatarBot} size="sm" live={false} decorative />
          <h2 id={headId} className={styles.groupTitle}>{botLabel}</h2>
          <span className={styles.groupCounts}>
            {counts.map((k) => (
              <span key={k} className={styles.countTag} data-type={k}>
                {t(`notifyHistory.typeCount.${k}`, { count: group.counts[k] })}
              </span>
            ))}
          </span>
        </header>
        <ul className={styles.list}>
          {shown.map((r) => renderRow(r.item, r.type, botLabel))}
          {hidden > 0 || all ? (
            <li className={styles.moreRow}>
              <button
                type="button"
                className={styles.moreButton}
                aria-expanded={all}
                onClick={() =>
                  setOpenBots((prev) => {
                    const next = new Set(prev);
                    if (!next.delete(group.botId)) next.add(group.botId);
                    return next;
                  })
                }
                data-testid="notify-bot-more"
              >
                {all ? t("notifyHistory.showLess") : t("notifyHistory.showMore", { count: hidden })}
              </button>
            </li>
          ) : null}
        </ul>
      </section>
    );
  };

  return (
    <div className={styles.pane} ref={rootRef} onKeyDown={onInboxKey}>
      <div className={styles.container}>
        <header className={styles.head}>
          <div className={styles.titleBlock}>
            <div className={styles.titleRow}>
              <h1 className={styles.title}>{t("notifyHistory.title")}</h1>
              {unread > 0 ? (
                <span className={styles.unreadPill} data-testid="notify-unread-pill">
                  {t("notifyHistory.unreadCount", { count: unread })}
                </span>
              ) : list.length > 0 ? (
                <span className={styles.allRead} data-testid="notify-all-read">
                  <CheckCheck aria-hidden="true" />
                  {t("notifyHistory.allRead")}
                </span>
              ) : null}
            </div>
            <p className={styles.subtitle}>{t("notifyHistory.subtitle")}</p>
          </div>
          {list.length > 0 ? (
          <div className={styles.actions}>
            <Button
              size="sm"
              variant={unread > 0 ? "secondary" : "ghost"}
              disabled={unread === 0}
              onClick={markAllNotificationsRead}
              data-testid="notify-history-mark-read"
            >
              <CheckCheck aria-hidden="true" />
              {t("notifyHistory.markAllRead")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={list.length === 0}
              onClick={handleClear}
              data-testid="notify-history-clear"
            >
              <Trash2 aria-hidden="true" />
              {t("notifyHistory.clear")}
            </Button>
          </div>
          ) : null}
        </header>

        <span role="status" aria-live="polite" className={styles.srOnly}>
          {list.length > 0 ? t("notifyHistory.unreadCount", { count: unread }) : ""}
        </span>

        {undoList !== null && list.length === 0 ? (
          <div className={styles.undoBar} role="status" data-testid="notify-history-undo-bar">
            <span>{t("notifyHistory.clearedToast")}</span>
            <Button size="sm" variant="secondary" onClick={handleUndo} data-testid="notify-history-undo">
              <RotateCcw aria-hidden="true" />
              {t("notifyHistory.undo")}
            </Button>
          </div>
        ) : null}

        {heldQuiet > 0 ? (
          <div className={styles.quietBanner} data-testid="notify-history-quiet-note">
            <Moon aria-hidden="true" />
            <span>{t("notifyHistory.quietNote", { count: heldQuiet })}</span>
          </div>
        ) : null}

        {list.length > 0 ? (
          <div className={styles.toolbar}>
            <div className={styles.chips} role="group" aria-label={t("notifyHistory.filters")}>
              {CATEGORIES.map((cat) => {
                const active = filter.category === cat && !filter.snoozed;
                return (
                  <button
                    key={cat}
                    type="button"
                    aria-pressed={active}
                    className={[styles.chip, active ? styles.chipActive : ""].filter(Boolean).join(" ")}
                    onClick={() => setFilter({ ...filter, category: cat, snoozed: false })}
                    data-testid={`notify-filter-${cat}`}
                  >
                    <span>{t(`notifyHistory.filterChip.${cat}`)}</span>
                    {cat === "needsYou" && needsCount > 0 ? (
                      <span className={styles.chipCount}>{needsCount}</span>
                    ) : null}
                  </button>
                );
              })}
            </div>
            <div className={styles.chipsRight}>
              <button
                type="button"
                aria-pressed={filter.unreadOnly}
                className={[styles.chip, filter.unreadOnly ? styles.chipActive : ""].filter(Boolean).join(" ")}
                onClick={() => setFilter({ ...filter, unreadOnly: !filter.unreadOnly })}
                data-testid="notify-filter-unread"
              >
                {t("notifyHistory.onlyUnread")}
              </button>
              {snoozedCount > 0 || filter.snoozed ? (
                <button
                  type="button"
                  aria-pressed={filter.snoozed}
                  className={[styles.chip, filter.snoozed ? styles.chipActive : ""].filter(Boolean).join(" ")}
                  onClick={() => setFilter({ ...filter, snoozed: !filter.snoozed })}
                  data-testid="notify-filter-snoozed"
                >
                  {t("notifyHistory.showSnoozed", { count: snoozedCount })}
                </button>
              ) : null}
            </div>
          </div>
        ) : null}

        {list.length === 0 ? (
          <EmptyState
            icon={BellOff}
            title={t("notifyHistory.emptyTitle")}
            actionLabel={t("notifyHistory.emptyAction")}
            onAction={openNotificationSettings}
            testId="notify-history-empty"
          >
            {t("notifyHistory.empty")}
            <span className={styles.legend} aria-hidden="true">
              {(["needsYou", "failed", "finished"] as const).map((k) => (
                <span key={k} className={styles.countTag} data-type={k}>{t(`notifyHistory.type.${k}`)}</span>
              ))}
            </span>
          </EmptyState>
        ) : botGroups.length === 0 ? (
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
          <div className={styles.groups}>{botGroups.map(renderBot)}</div>
        )}

        <p className={styles.retention} data-testid="notify-history-retention">
          {t("notifyHistory.retention", { days, max: HISTORY_MAX })}
        </p>
      </div>
    </div>
  );
}
