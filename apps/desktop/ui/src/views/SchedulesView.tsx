import {
  ArrowUpRight,
  CalendarCheck,
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  HardDrive,
  Inbox,
  LayoutList,
  type LucideIcon,
  PenLine,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Sun,
  Trash2,
  TrendingDown
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import { ApiError } from "../api/client";
import type {
  ParsedSchedule,
  Schedule,
  ScheduleHistoryEntry
} from "../api/types";
import formStyles from "../components/Form.module.css";
import { botView } from "../lib/botProfile";
import scheduleTemplateFile from "../data/scheduleTemplates.json";
import { humanLine } from "../lib/humanError";
import {
  FREQUENCIES,
  type Frequency,
  simpleProblem,
  simpleSpec
} from "../lib/scheduleBuilder";
import {
  type ScheduleNotify,
  SCHEDULE_NOTIFY_CHOICES,
  scheduleNotifyOf,
  writeScheduleNotify
} from "../lib/scheduleNotify";
import { type ScheduleTemplate, templateText } from "../lib/scheduleTemplates";
import {
  affectsSchedules,
  buildInput,
  buildMonthGrid,
  draftFromSchedule,
  emptyDraft,
  filterScheduleHistory,
  formatDurationParts,
  type HistoryFilter,
  historyStatusTone,
  localDayKey,
  mergeLiveRunsIntoHistory,
  MISFIRE_POLICIES,
  moveCalendarFocusIndex,
  parsePayload,
  placeScheduleCalendarEvents,
  runsOfSchedule,
  type ScheduleDraft,
  scheduleStatus,
  sortSchedules,
  specLabel,
  toggleInput,
  validateDraft
} from "../lib/schedules";
import { useDaemon } from "../state/DaemonContext";
import styles from "./SchedulesView.module.css";

function errorText(e: unknown): string {
  return humanLine(e);
}

const TEMPLATES = scheduleTemplateFile.templates as ScheduleTemplate[];
const TEMPLATE_ICONS: Record<string, LucideIcon> = {
  sun: Sun,
  "calendar-check": CalendarCheck,
  inbox: Inbox,
  "trending-down": TrendingDown,
  "hard-drive": HardDrive
};

const LOCAL_TZ = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

const COMMON_TIMEZONES = Array.from(
  new Set([
    LOCAL_TZ,
    "UTC",
    "Europe/Istanbul",
    "Europe/Berlin",
    "Europe/London",
    "America/New_York",
    "America/Los_Angeles",
    "Asia/Tokyo"
  ])
);

function historyToneClass(status: string): string {
  const tone = historyStatusTone(status);
  return styles[tone] ?? styles.running;
}

/** Scheduled tasks: list, calendar, next-runs preview, create/edit, run history, retry. */
export function SchedulesView({
  onOpenChat
}: {
  onOpenChat: (chatId: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, events, onDaemonEvent } = useDaemon();
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);
  const [historyById, setHistoryById] = useState<
    Record<string, ScheduleHistoryEntry[]>
  >({});
  const [historyFilterById, setHistoryFilterById] = useState<
    Record<string, HistoryFilter>
  >({});
  const [viewMode, setViewMode] = useState<"list" | "calendar">("list");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  // A moved schedule whose server could not be asked: waiting for the owner to confirm.
  const [confirmReclaimId, setConfirmReclaimId] = useState<string | null>(null);
  /** `null` = no form; `"new"` or a schedule id being edited. */
  const [editing, setEditing] = useState<string | null>(null);

  const fmt = useCallback(
    (ms: number) =>
      new Date(ms).toLocaleString(i18n.language, {
        dateStyle: "medium",
        timeStyle: "short"
      }),
    [i18n.language]
  );

  const loadHistory = useCallback(
    async (list: readonly Schedule[]) => {
      if (!client) {
        return;
      }
      const pairs = await Promise.all(
        list.map(async (s) => {
          try {
            const entries = await client.getScheduleHistory(s.id);
            return [s.id, entries] as const;
          } catch {
            return [s.id, [] as ScheduleHistoryEntry[]] as const;
          }
        })
      );
      setHistoryById(Object.fromEntries(pairs));
    },
    [client]
  );

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      const next = await client.listSchedules();
      setSchedules(next);
      setError(null);
      void loadHistory(next);
    } catch (e) {
      setError(errorText(e));
      setSchedules((cur) => cur ?? []);
    }
  }, [client, loadHistory]);

  useEffect(() => {
    void load();
  }, [load]);

  // Live: a scheduled run starting / any run finishing moves last / next run.
  const reloadTimer = useRef<number | undefined>(undefined);
  useEffect(() => {
    const off = onDaemonEvent((env) => {
      if (affectsSchedules(env.event)) {
        window.clearTimeout(reloadTimer.current);
        reloadTimer.current = window.setTimeout(() => void load(), 300);
      }
    });
    return () => {
      off();
      window.clearTimeout(reloadTimer.current);
    };
  }, [onDaemonEvent, load]);

  const act = async (id: string, fn: () => Promise<unknown>, ok?: string) => {
    setBusyId(id);
    setNotice(null);
    try {
      await fn();
      setError(null);
      if (ok) {
        setNotice(ok);
      }
      await load();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusyId(null);
    }
  };

  const list = sortSchedules(schedules ?? []);
  const editingSchedule =
    editing && editing !== "new"
      ? (schedules ?? []).find((s) => s.id === editing)
      : undefined;

  return (
    <div className={styles.pane}>
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.headerText}>
            <h1 className={styles.title}>{t("schedules.title")}</h1>
            <p className={styles.subtitle}>{t("schedules.subtitle")}</p>
          </div>
          <div className={styles.headerActions}>
            <div
              className={styles.filterGroup}
              role="group"
              aria-label={t("schedules.title")}
            >
              <Button
                size="sm"
                variant={viewMode === "list" ? "secondary" : "ghost"}
                aria-pressed={viewMode === "list"}
                data-testid="schedules-view-list"
                onClick={() => setViewMode("list")}
              >
                <LayoutList />
                {t("schedules.viewList")}
              </Button>
              <Button
                size="sm"
                variant={viewMode === "calendar" ? "secondary" : "ghost"}
                aria-pressed={viewMode === "calendar"}
                data-testid="schedules-view-calendar"
                onClick={() => setViewMode("calendar")}
              >
                <CalendarDays />
                {t("schedules.viewCalendar")}
              </Button>
            </div>
            <Button size="sm" variant="ghost" onClick={() => void load()}>
              <RefreshCw />
              {t("schedules.refresh")}
            </Button>
            <Button
              size="sm"
              variant="primary"
              disabled={bots.length === 0}
              onClick={() => setEditing("new")}
            >
              <Plus />
              {t("schedules.new")}
            </Button>
          </div>
        </header>

        {error ? (
          <p className={formStyles.error} role="alert">
            {error}
          </p>
        ) : null}
        {notice ? (
          <p className={formStyles.success} role="status">
            {notice}
          </p>
        ) : null}

        {editing && (editing === "new" || editingSchedule) ? (
          <ScheduleForm
            key={editing}
            editing={editingSchedule}
            onCancel={() => setEditing(null)}
            onSaved={async () => {
              setEditing(null);
              await load();
            }}
          />
        ) : null}

        {schedules === null ? (
          <p className={formStyles.hint} role="status">
            {t("schedules.loading")}
          </p>
        ) : list.length === 0 ? (
          <Card padding="md" className={styles.empty}>
            <CalendarDays className={formStyles.emptyIcon} />
            <p className={styles.emptyTitle}>{t("schedules.empty.title")}</p>
            <p className={formStyles.hint}>
              {bots.length === 0
                ? t("schedules.empty.noBots")
                : t("schedules.empty.hint")}
            </p>
          </Card>
        ) : viewMode === "calendar" ? (
          <ScheduleCalendar
            schedules={list}
            historyById={historyById}
            busyId={busyId}
            fmt={fmt}
            onOpenChat={onOpenChat}
            onRetry={(scheduleId, name, dueMs) =>
              void act(
                scheduleId,
                () => client!.retryScheduleOccurrence(scheduleId, dueMs),
                t("schedules.retryStarted", { name })
              )
            }
            onRunNow={(scheduleId, name) =>
              void act(
                scheduleId,
                () => client!.runScheduleNow(scheduleId),
                t("schedules.runNowStarted", { name })
              )
            }
          />
        ) : (
          <ul className={styles.list} aria-label={t("schedules.title")}>
            {list.map((s) => {
              const bot = bots.find((b) => b.id === s.bot_id);
              const view = bot ? botView(bot) : null;
              const status = scheduleStatus(s);
              const label = specLabel(s.spec, fmt);
              const payload = parsePayload(s.payload);
              const liveRuns = runsOfSchedule(events.runs, s.id, 20);
              const mergedHistory = mergeLiveRunsIntoHistory(
                historyById[s.id] ?? [],
                liveRuns
              );
              const filter = historyFilterById[s.id] ?? "all";
              const visibleHistory = filterScheduleHistory(
                mergedHistory,
                filter
              );
              const nextRuns = s.next_runs_ms ?? [];
              const busy = busyId === s.id;
              return (
                <li key={s.id} data-testid="schedule-row">
                  <Card padding="md" className={styles.item}>
                    <div className={styles.itemHead}>
                      <div className={styles.itemTitle}>
                        <h2 className={styles.name}>{s.name}</h2>
                        <span
                          className={`${styles.badge} ${styles[status]}`}
                          data-testid="schedule-status"
                        >
                          {t(`schedules.status.${status}`)}
                        </span>
                        {s.origin === "bot" ? (
                          <span
                            className={styles.badge}
                            data-testid="schedule-origin-bot"
                            title={t("schedules.originBotHint")}
                          >
                            {t("schedules.originBot")}
                          </span>
                        ) : null}
                      </div>
                      <label className={styles.toggle}>
                        <input
                          type="checkbox"
                          checked={s.enabled}
                          disabled={busy}
                          onChange={() =>
                            void act(s.id, () =>
                              client!.updateSchedule(s.id, toggleInput(s))
                            )
                          }
                        />
                        <span>{t("schedules.enabled")}</span>
                      </label>
                    </div>

                    <dl className={styles.meta}>
                      <div>
                        <dt>{t("schedules.bot")}</dt>
                        <dd className={styles.botCell}>
                          {view ? (
                            <BotAvatar bot={view} size="xs" decorative />
                          ) : null}
                          {view?.name ?? t("schedules.deletedBot")}
                        </dd>
                      </div>
                      <div>
                        <dt>{t("schedules.when")}</dt>
                        <dd>{t(label.i18nKey, label.values)}</dd>
                      </div>
                      <div>
                        <dt>{t("schedules.does")}</dt>
                        <dd className={styles.clamp}>
                          {payload.kind === "agent_prompt"
                            ? payload.text
                            : payload.kind === "unknown"
                              ? t("schedules.payload.unknown")
                              : t(`schedules.payload.${payload.kind}`, {
                                  name: payload.name
                                })}
                        </dd>
                      </div>
                      <div>
                        <dt>{t("schedules.nextRun")}</dt>
                        <dd>
                          {s.next_run_at_ms !== null &&
                          s.next_run_at_ms !== undefined
                            ? fmt(s.next_run_at_ms)
                            : "—"}
                        </dd>
                      </div>
                      <div>
                        <dt>{t("schedules.lastRun")}</dt>
                        <dd>
                          {s.last_run_at_ms !== null &&
                          s.last_run_at_ms !== undefined
                            ? fmt(s.last_run_at_ms)
                            : t("schedules.never")}
                        </dd>
                      </div>
                      {(s.missed_occurrences ?? 0) > 0 ? (
                        <div data-testid="schedule-missed">
                          <dt>{t("schedules.missed")}</dt>
                          <dd>{s.missed_occurrences}</dd>
                        </div>
                      ) : null}
                    </dl>

                    {nextRuns.length > 0 ? (
                      <div
                        className={styles.upcomingSection}
                        data-testid="schedule-next-runs"
                      >
                        <h3 className={styles.runsTitle}>
                          {t("schedules.nextRuns")}
                        </h3>
                        <ul className={styles.upcomingList}>
                          {nextRuns.slice(0, 5).map((ts) => (
                            <li key={ts} className={styles.upcomingChip}>
                              {fmt(ts)}
                            </li>
                          ))}
                        </ul>
                      </div>
                    ) : null}

                    {s.moved_to ? (
                      <div
                        className={styles.actions}
                        data-testid="schedule-moved"
                      >
                        <p className={formStyles.hint}>
                          {t("schedules.movedTo", {
                            name: s.moved_to
                          })}
                        </p>
                        {confirmReclaimId === s.id ? (
                          <p className={formStyles.error} role="alert">
                            {t("schedules.reclaimUnreachable", {
                              name: s.moved_to
                            })}
                          </p>
                        ) : null}
                        <Button
                          size="sm"
                          variant="primary"
                          disabled={busy}
                          data-testid="schedule-reclaim"
                          onClick={() => {
                            const confirmed = confirmReclaimId === s.id;
                            setConfirmReclaimId(null);
                            void act(
                              s.id,
                              async () => {
                                try {
                                  await client!.reclaimSchedule(
                                    s.id,
                                    confirmed
                                  );
                                } catch (e) {
                                  if (
                                    e instanceof ApiError &&
                                    e.code === "host_unreachable"
                                  ) {
                                    setConfirmReclaimId(s.id);
                                  }
                                  throw e;
                                }
                              },
                              t("schedules.reclaimed", {
                                name: s.name
                              })
                            );
                          }}
                        >
                          {confirmReclaimId === s.id
                            ? t("schedules.reclaimAnyway")
                            : t("schedules.reclaim")}
                        </Button>
                      </div>
                    ) : null}
                    <div className={styles.actions}>
                      <Button
                        size="sm"
                        variant="primary"
                        disabled={busy || Boolean(s.moved_to)}
                        onClick={() =>
                          void act(
                            s.id,
                            () => client!.runScheduleNow(s.id),
                            t("schedules.runNowStarted", { name: s.name })
                          )
                        }
                      >
                        <Play />
                        {t("schedules.runNow")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy || Boolean(s.moved_to)}
                        onClick={() => setEditing(s.id)}
                      >
                        <PenLine />
                        {t("schedules.edit")}
                      </Button>
                      {confirmDeleteId === s.id ? (
                        <>
                          <Button
                            size="sm"
                            variant="danger"
                            disabled={busy}
                            onClick={() => {
                              setConfirmDeleteId(null);
                              void act(s.id, () =>
                                client!.deleteSchedule(s.id)
                              );
                            }}
                          >
                            {t("schedules.confirmDelete")}
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => setConfirmDeleteId(null)}
                          >
                            {t("schedules.cancel")}
                          </Button>
                        </>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy}
                          onClick={() => setConfirmDeleteId(s.id)}
                        >
                          <Trash2 />
                          {t("schedules.delete")}
                        </Button>
                      )}
                    </div>

                    <div className={styles.runs} data-testid="schedule-history">
                      <div className={styles.runsHead}>
                        <h3 className={styles.runsTitle}>
                          {t("schedules.history")}
                        </h3>
                        <div className={styles.filterGroup} role="group">
                          <button
                            type="button"
                            className={`${styles.filterChip} ${
                              filter === "all" ? styles.filterChipActive : ""
                            }`}
                            aria-pressed={filter === "all"}
                            onClick={() =>
                              setHistoryFilterById((prev) => ({
                                ...prev,
                                [s.id]: "all"
                              }))
                            }
                          >
                            {t("schedules.filterAll")}
                          </button>
                          <button
                            type="button"
                            className={`${styles.filterChip} ${
                              filter === "issues" ? styles.filterChipActive : ""
                            }`}
                            aria-pressed={filter === "issues"}
                            onClick={() =>
                              setHistoryFilterById((prev) => ({
                                ...prev,
                                [s.id]: "issues"
                              }))
                            }
                          >
                            {t("schedules.filterIssues")}
                          </button>
                        </div>
                      </div>
                      {visibleHistory.length === 0 ? (
                        <p className={formStyles.hint}>
                          {filter === "issues"
                            ? t("schedules.historyEmptyFiltered")
                            : t("schedules.historyEmpty")}
                        </p>
                      ) : (
                        <ul className={styles.runList}>
                          {visibleHistory.slice(0, 10).map((entry) => {
                            const key = `${entry.due_ms ?? "direct"}-${entry.run_id ?? entry.status}`;
                            const duration = formatDurationParts(
                              entry.started_at_ms,
                              entry.finished_at_ms
                            );
                            const when = entry.started_at_ms ?? entry.due_ms;
                            const canRunAgain =
                              !entry.retryable &&
                              entry.due_ms == null &&
                              (entry.status === "failed" ||
                                entry.status === "cancelled");
                            return (
                              <li
                                key={key}
                                className={styles.run}
                                data-testid="schedule-history-row"
                              >
                                <span
                                  className={`${styles.badge} ${historyToneClass(entry.status)}`}
                                >
                                  {t(`schedules.run.${entry.status}`, {
                                    defaultValue: entry.status
                                  })}
                                </span>
                                {when !== null && when !== undefined ? (
                                  <span className={formStyles.hint}>
                                    {entry.started_at_ms != null
                                      ? fmt(entry.started_at_ms)
                                      : t("schedules.dueAt", {
                                          when: fmt(when)
                                        })}
                                  </span>
                                ) : null}
                                {duration ? (
                                  <span className={formStyles.hint}>
                                    {t(duration.i18nKey, duration.values)}
                                  </span>
                                ) : null}
                                {entry.error ? (
                                  <span className={styles.runError}>
                                    {entry.error}
                                  </span>
                                ) : null}
                                {entry.chat_id ? (
                                  <Button
                                    size="sm"
                                    variant="ghost"
                                    onClick={() => onOpenChat(entry.chat_id!)}
                                  >
                                    <ArrowUpRight />
                                    {t("schedules.openRun")}
                                  </Button>
                                ) : null}
                                {entry.retryable &&
                                entry.due_ms !== null &&
                                entry.due_ms !== undefined ? (
                                  <Button
                                    size="sm"
                                    variant="secondary"
                                    disabled={busy || Boolean(s.moved_to)}
                                    data-testid="schedule-retry"
                                    onClick={() =>
                                      void act(
                                        s.id,
                                        () =>
                                          client!.retryScheduleOccurrence(
                                            s.id,
                                            entry.due_ms!
                                          ),
                                        t("schedules.retryStarted", {
                                          name: s.name
                                        })
                                      )
                                    }
                                  >
                                    <RotateCcw />
                                    {t("schedules.retry")}
                                  </Button>
                                ) : canRunAgain ? (
                                  <Button
                                    size="sm"
                                    variant="secondary"
                                    disabled={busy || Boolean(s.moved_to)}
                                    onClick={() =>
                                      void act(
                                        s.id,
                                        () => client!.runScheduleNow(s.id),
                                        t("schedules.runNowStarted", {
                                          name: s.name
                                        })
                                      )
                                    }
                                  >
                                    <RotateCcw />
                                    {t("schedules.runAgain")}
                                  </Button>
                                ) : null}
                              </li>
                            );
                          })}
                        </ul>
                      )}
                    </div>
                  </Card>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

function ScheduleCalendar({
  schedules,
  historyById,
  busyId,
  fmt,
  onOpenChat,
  onRetry,
  onRunNow
}: {
  schedules: readonly Schedule[];
  historyById: Readonly<Record<string, readonly ScheduleHistoryEntry[]>>;
  busyId: string | null;
  fmt: (ms: number) => string;
  onOpenChat: (chatId: string) => void;
  onRetry: (scheduleId: string, name: string, dueMs: number) => void;
  onRunNow: (scheduleId: string, name: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const now = new Date();
  const [cursor, setCursor] = useState(() => ({
    year: now.getFullYear(),
    month: now.getMonth()
  }));
  const [selectedDayKey, setSelectedDayKey] = useState(() =>
    localDayKey(Date.now())
  );
  const [upcomingById, setUpcomingById] = useState<Record<string, number[]>>(
    {}
  );
  const cellRefs = useRef<(HTMLButtonElement | null)[]>([]);

  const days = useMemo(
    () => buildMonthGrid(cursor.year, cursor.month, true),
    [cursor.year, cursor.month]
  );

  // Fetch up to 64 upcoming runs per enabled schedule anchored at the start of the month.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    const monthStart = new Date(cursor.year, cursor.month, 1).getTime();
    const anchor = Math.max(Date.now(), monthStart - 1);
    const active = schedules.filter((s) => s.enabled && !s.moved_to);
    void (async () => {
      const pairs = await Promise.all(
        active.map(async (s) => {
          try {
            const res = await client.previewSchedule({
              spec: s.spec,
              from_ms: anchor,
              count: 64
            });
            return [s.id, res.next_runs_ms ?? []] as const;
          } catch {
            return [s.id, s.next_runs_ms ?? []] as const;
          }
        })
      );
      if (!cancelled) {
        setUpcomingById(Object.fromEntries(pairs));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, schedules, cursor.year, cursor.month]);

  const cells = useMemo(
    () =>
      placeScheduleCalendarEvents(days, schedules, upcomingById, historyById),
    [days, schedules, upcomingById, historyById]
  );

  const monthLabel = useMemo(
    () =>
      new Intl.DateTimeFormat(i18n.language, {
        month: "long",
        year: "numeric"
      }).format(new Date(cursor.year, cursor.month, 1)),
    [i18n.language, cursor.year, cursor.month]
  );

  // Monday..Sunday headers localized via a known Monday (2026-01-05).
  const weekdayHeaders = useMemo(() => {
    const f = new Intl.DateTimeFormat(i18n.language, { weekday: "short" });
    return Array.from({ length: 7 }, (_, i) =>
      f.format(new Date(2026, 0, 5 + i))
    );
  }, [i18n.language]);

  const selectedCell =
    cells.find((c) => c.dayKey === selectedDayKey) ??
    cells.find((c) => c.inMonth) ??
    cells[0];

  const stepMonth = (delta: number) => {
    const d = new Date(cursor.year, cursor.month + delta, 1);
    setCursor({ year: d.getFullYear(), month: d.getMonth() });
    setSelectedDayKey(localDayKey(d.getTime()));
  };

  return (
    <Card
      padding="md"
      className={styles.calCard}
      data-testid="schedule-calendar"
    >
      <div className={styles.calToolbar}>
        <h2 className={styles.calMonthTitle}>{monthLabel}</h2>
        <div className={styles.calNav}>
          <Button
            size="sm"
            variant="ghost"
            aria-label={t("schedules.calendar.prevMonth")}
            onClick={() => stepMonth(-1)}
          >
            <ChevronLeft />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              const today = new Date();
              setCursor({
                year: today.getFullYear(),
                month: today.getMonth()
              });
              setSelectedDayKey(localDayKey(today.getTime()));
            }}
          >
            {t("schedules.calendar.today")}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            aria-label={t("schedules.calendar.nextMonth")}
            onClick={() => stepMonth(1)}
          >
            <ChevronRight />
          </Button>
        </div>
      </div>

      <div
        role="grid"
        aria-label={t("schedules.calendar.gridLabel", {
          month: monthLabel
        })}
      >
        <div className={styles.calWeekdays} role="row">
          {weekdayHeaders.map((label) => (
            <div key={label} className={styles.calWeekday} role="columnheader">
              {label}
            </div>
          ))}
        </div>
        <div className={styles.calGrid} role="rowgroup">
          {cells.map((cell, idx) => {
            const selected = selectedCell?.dayKey === cell.dayKey;
            return (
              <button
                key={cell.dayKey}
                ref={(el) => {
                  cellRefs.current[idx] = el;
                }}
                type="button"
                role="gridcell"
                aria-selected={selected}
                aria-label={t("schedules.calendar.daySummary", {
                  date: cell.dayKey,
                  upcoming: cell.upcoming.length,
                  outcomes: cell.outcomes.length
                })}
                tabIndex={selected ? 0 : -1}
                className={`${styles.calDay} ${
                  cell.inMonth ? "" : styles.calDayOutside
                } ${selected ? styles.calDaySelected : ""}`}
                onClick={() => setSelectedDayKey(cell.dayKey)}
                onKeyDown={(e) => {
                  const next = moveCalendarFocusIndex(idx, e.key, cells.length);
                  if (next !== null) {
                    e.preventDefault();
                    setSelectedDayKey(cells[next].dayKey);
                    cellRefs.current[next]?.focus();
                  }
                }}
              >
                <span className={styles.calDayNum}>{cell.dayOfMonth}</span>
                <span className={styles.calDots} aria-hidden="true">
                  {cell.outcomes.slice(0, 4).map((o, i) => {
                    const tone = historyStatusTone(o.status);
                    const dotClass =
                      tone === "succeeded"
                        ? styles.calDotSucceeded
                        : tone === "failed"
                          ? styles.calDotFailed
                          : styles.calDotMissed;
                    return (
                      <span
                        key={`${o.scheduleId}-${o.ts}-${i}`}
                        className={`${styles.calDot} ${dotClass}`}
                      />
                    );
                  })}
                  {cell.upcoming.slice(0, 4).map((u, i) => (
                    <span
                      key={`${u.scheduleId}-${u.ts}-${i}`}
                      className={`${styles.calDot} ${styles.calDotUpcoming}`}
                    />
                  ))}
                </span>
              </button>
            );
          })}
        </div>
      </div>

      {selectedCell ? (
        <div
          className={styles.calDetail}
          data-testid="schedule-calendar-detail"
        >
          <h3 className={styles.calDetailHeading}>{selectedCell.dayKey}</h3>
          {selectedCell.upcoming.length === 0 &&
          selectedCell.outcomes.length === 0 ? (
            <p className={formStyles.hint}>
              {t("schedules.calendar.dayEmpty", {
                date: selectedCell.dayKey
              })}
            </p>
          ) : (
            <>
              {selectedCell.upcoming.length > 0 ? (
                <div className={styles.runs}>
                  <span className={styles.runsTitle}>
                    {t("schedules.calendar.upcomingHeading")}
                  </span>
                  <ul className={styles.runList}>
                    {selectedCell.upcoming.map((u, i) => (
                      <li
                        key={`${u.scheduleId}-${u.ts}-${i}`}
                        className={styles.run}
                      >
                        <span className={`${styles.badge} ${styles.running}`}>
                          {u.scheduleName}
                        </span>
                        <span className={formStyles.hint}>{fmt(u.ts)}</span>
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
              {selectedCell.outcomes.length > 0 ? (
                <div className={styles.runs}>
                  <span className={styles.runsTitle}>
                    {t("schedules.calendar.historyHeading")}
                  </span>
                  <ul className={styles.runList}>
                    {selectedCell.outcomes.map((o, i) => (
                      <li
                        key={`${o.scheduleId}-${o.ts}-${i}`}
                        className={styles.run}
                      >
                        <span
                          className={`${styles.badge} ${historyToneClass(o.status)}`}
                        >
                          {t(`schedules.run.${o.status}`, {
                            defaultValue: o.status
                          })}
                        </span>
                        <span>{o.scheduleName}</span>
                        <span className={formStyles.hint}>{fmt(o.ts)}</span>
                        {o.entry.chat_id ? (
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => onOpenChat(o.entry.chat_id!)}
                          >
                            <ArrowUpRight />
                            {t("schedules.openRun")}
                          </Button>
                        ) : null}
                        {o.entry.retryable &&
                        o.entry.due_ms !== null &&
                        o.entry.due_ms !== undefined ? (
                          <Button
                            size="sm"
                            variant="secondary"
                            disabled={busyId === o.scheduleId}
                            onClick={() =>
                              onRetry(
                                o.scheduleId,
                                o.scheduleName,
                                o.entry.due_ms!
                              )
                            }
                          >
                            <RotateCcw />
                            {t("schedules.retry")}
                          </Button>
                        ) : !o.entry.retryable &&
                          o.entry.due_ms == null &&
                          (o.status === "failed" ||
                            o.status === "cancelled") ? (
                          <Button
                            size="sm"
                            variant="secondary"
                            disabled={busyId === o.scheduleId}
                            onClick={() =>
                              onRunNow(o.scheduleId, o.scheduleName)
                            }
                          >
                            <RotateCcw />
                            {t("schedules.runAgain")}
                          </Button>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </>
          )}
        </div>
      ) : null}
    </Card>
  );
}

function ScheduleForm({
  editing,
  onCancel,
  onSaved
}: {
  editing?: Schedule;
  onCancel: () => void;
  onSaved: () => Promise<void>;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [draft, setDraft] = useState<ScheduleDraft>(() =>
    editing
      ? draftFromSchedule(editing)
      : { ...emptyDraft(bots[0]?.id ?? ""), tz: LOCAL_TZ }
  );
  const [preview, setPreview] = useState<{
    key: string;
    result: ParsedSchedule;
  } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notify, setNotify] = useState<ScheduleNotify>(() =>
    editing ? scheduleNotifyOf(editing.id) : "problems"
  );
  const patchSimple = (p: Partial<ScheduleDraft["simple"]>) =>
    setDraft((d) => ({ ...d, simple: { ...d.simple, ...p } }));
  const pickFrequency = (f: Frequency) =>
    setDraft((d) => ({
      ...d,
      kind: f === "custom" ? "natural" : "simple",
      simple: { ...d.simple, freq: f }
    }));
  const applyTemplate = (tpl: ScheduleTemplate) => {
    const x = templateText(tpl, i18n.language);
    setDraft((d) => ({
      ...d,
      name: x.name,
      prompt: x.prompt,
      kind: "simple",
      simple: {
        ...d.simple,
        freq: tpl.freq,
        time: tpl.time,
        weekday: tpl.weekday ?? d.simple.weekday
      }
    }));
    setProblem(null);
  };
  const patch = (p: Partial<ScheduleDraft>) =>
    setDraft((d) => ({ ...d, ...p }));
  const keepsPayload =
    editing !== undefined &&
    parsePayload(editing.payload).kind !== "agent_prompt";
  const selectedTz = draft.tz ?? LOCAL_TZ;

  const fmt = (ms: number, tz?: string) =>
    new Date(ms).toLocaleString(i18n.language, {
      dateStyle: "medium",
      timeStyle: "short",
      weekday: "short",
      ...(tz ? { timeZone: tz } : {})
    });

  const simpleSpecNow =
    draft.kind === "simple" &&
    simpleProblem(draft.simple, selectedTz, Date.now()) === null
      ? simpleSpec(draft.simple, selectedTz)
      : null;
  const previewKey =
    draft.kind === "simple"
      ? `simple:${JSON.stringify(simpleSpecNow)}`
      : draft.kind === "natural"
        ? `natural:${draft.text.trim()}:${selectedTz}`
        : draft.kind === "cron"
          ? `cron:${draft.cron.trim()}:${draft.tz ?? ""}`
          : "keep";

  const runPreview = async (): Promise<ParsedSchedule | null> => {
    if (!client) {
      return null;
    }
    if (draft.kind === "natural" && draft.text.trim() === "") {
      return null;
    }
    if (draft.kind === "cron" && draft.cron.trim() === "") {
      return null;
    }
    if (draft.kind === "simple" && simpleSpecNow === null) {
      return null;
    }
    if (preview && preview.key === previewKey) {
      return preview.result;
    }
    try {
      const result =
        draft.kind === "natural"
          ? await client.parseSchedule(draft.text.trim(), selectedTz)
          : draft.kind === "simple"
            ? await client.previewSchedule({
                spec: simpleSpecNow,
                count: 5
              })
            : await client.previewSchedule({
                spec: { cron: draft.cron.trim() },
                tz: draft.tz?.trim() || undefined,
                count: 5
              });
      setPreview({ key: previewKey, result });
      setProblem(null);
      return result;
    } catch (e) {
      setPreview(null);
      setProblem(
        e instanceof ApiError && e.code === "bad_schedule"
          ? draft.kind === "natural"
            ? t("schedules.form.cannotParse")
            : t("schedules.form.cannotPreview")
          : errorText(e)
      );
      return null;
    }
  };

  // The easy builder shows the next runs as soon as the choices are complete.
  useEffect(() => {
    if (draft.kind === "simple" && simpleSpecNow !== null) {
      void runPreview();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [previewKey]);

  const submit = async () => {
    const invalid = validateDraft(draft, editing);
    if (invalid) {
      setProblem(t(`schedules.form.invalid.${invalid}`));
      return;
    }
    if (!client) {
      return;
    }
    if (draft.kind === "simple") {
      const why = simpleProblem(draft.simple, selectedTz, Date.now());
      if (why) {
        setProblem(t(`schedules.form.simpleProblem.${why}`));
        return;
      }
    }
    setBusy(true);
    try {
      let spec: unknown = null;
      if (draft.kind === "natural") {
        const shown = preview && preview.key === previewKey;
        const parsed = await runPreview();
        if (!parsed) {
          return;
        }
        if (!shown) {
          // First show what was understood; the person confirms with a second press.
          setProblem(t("schedules.form.confirmParsed"));
          return;
        }
        spec = parsed.spec;
      } else if (draft.kind === "simple") {
        spec = simpleSpecNow;
      }
      const input = buildInput(draft, spec, editing);
      let savedId: string;
      if (editing) {
        await client.updateSchedule(editing.id, input);
        savedId = editing.id;
      } else {
        savedId = (await client.createSchedule(input)).id;
      }
      writeScheduleNotify(savedId, notify);
      await onSaved();
    } catch (e) {
      setProblem(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const heading = editing
    ? t("schedules.form.editTitle")
    : t("schedules.form.newTitle");
  const activePreview =
    preview && preview.key === previewKey ? preview.result : null;
  const previewRuns =
    draft.kind === "keep" && editing
      ? (editing.next_runs_ms ?? [])
      : (activePreview?.next_runs_ms ?? []);

  return (
    <Card padding="md" className={styles.form}>
      <form
        className={styles.formBody}
        aria-label={heading}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <h2 className={styles.formTitle}>{heading}</h2>

        {!editing ? (
          <div className={formStyles.field}>
            <span className={formStyles.label}>
              {t("schedules.form.templates.title")}
            </span>
            <div className={styles.templateRow}>
              {TEMPLATES.map((tpl) => {
                const x = templateText(tpl, i18n.language);
                const Icon = TEMPLATE_ICONS[tpl.icon] ?? Sun;
                return (
                  <button
                    key={tpl.id}
                    type="button"
                    className={styles.template}
                    data-testid={`schedule-template-${tpl.id}`}
                    onClick={() => applyTemplate(tpl)}
                  >
                    <span className={styles.templateName}>
                      <Icon size={16} aria-hidden="true" />
                      {x.name}
                    </span>
                    <span className={styles.templateBlurb}>{x.blurb}</span>
                  </button>
                );
              })}
            </div>
            <span className={formStyles.hint}>
              {t("schedules.form.templates.hint")}
            </span>
          </div>
        ) : null}

        <label className={formStyles.field}>
          <span className={formStyles.label}>{t("schedules.form.name")}</span>
          <input
            className={formStyles.input}
            value={draft.name}
            required
            onChange={(e) => patch({ name: e.target.value })}
          />
        </label>

        <label className={formStyles.field}>
          <span className={formStyles.label}>{t("schedules.bot")}</span>
          <select
            className={formStyles.select}
            value={draft.botId}
            disabled={editing !== undefined}
            onChange={(e) => patch({ botId: e.target.value })}
          >
            {bots.map((b) => (
              <option key={b.id} value={b.id}>
                {botView(b).name}
              </option>
            ))}
          </select>
          {editing ? (
            <span className={formStyles.hint}>
              {t("schedules.form.botFixed")}
            </span>
          ) : null}
        </label>

        <fieldset className={styles.fieldset}>
          <legend className={formStyles.label}>
            {t("schedules.form.when")}
          </legend>
          <div
            className={styles.chipRow}
            role="radiogroup"
            aria-label={t("schedules.form.freq.label")}
          >
            {editing ? (
              <button
                type="button"
                role="radio"
                aria-checked={draft.kind === "keep"}
                className={styles.chip}
                onClick={() => patch({ kind: "keep" })}
              >
                {t("schedules.form.kind.keep")}
              </button>
            ) : null}
            {FREQUENCIES.map((f) => {
              const on =
                draft.kind === "keep" || draft.kind === "cron"
                  ? false
                  : f === "custom"
                    ? draft.kind === "natural"
                    : draft.kind === "simple" && draft.simple.freq === f;
              return (
                <button
                  key={f}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  className={styles.chip}
                  data-testid={`schedule-freq-${f}`}
                  onClick={() => pickFrequency(f)}
                >
                  {t(`schedules.form.freq.${f}`)}
                </button>
              );
            })}
          </div>

          {draft.kind === "keep" && editing ? (
            <p className={formStyles.hint}>
              {(() => {
                const l = specLabel(editing.spec, (ms) => fmt(ms));
                return t("schedules.form.keepHint", {
                  spec: t(l.i18nKey, l.values)
                });
              })()}
            </p>
          ) : null}

          {draft.kind !== "keep" ? (
            <label className={formStyles.field}>
              <span className={formStyles.label}>{t("schedules.form.tz")}</span>
              <select
                className={formStyles.select}
                value={draft.tz ?? ""}
                aria-label={t("schedules.form.tz")}
                onChange={(e) => patch({ tz: e.target.value })}
              >
                {draft.kind === "cron" ? <option value="">UTC</option> : null}
                {COMMON_TIMEZONES.map((tz) => (
                  <option key={tz} value={tz}>
                    {tz}
                  </option>
                ))}
              </select>
              {draft.kind === "cron" ? (
                <span className={formStyles.hint}>
                  {t("schedules.form.tzCronHint")}
                </span>
              ) : null}
            </label>
          ) : null}

          {draft.kind === "simple" ? (
            <div className={formStyles.field}>
              <div className={formStyles.row}>
                {draft.simple.freq === "once" ? (
                  <label className={formStyles.field}>
                    <span className={formStyles.label}>
                      {t("schedules.form.date")}
                    </span>
                    <input
                      type="date"
                      className={formStyles.input}
                      value={draft.simple.date}
                      onChange={(e) =>
                        patchSimple({
                          date: e.target.value
                        })
                      }
                    />
                  </label>
                ) : null}
                {draft.simple.freq === "weekly" ? (
                  <label className={formStyles.field}>
                    <span className={formStyles.label}>
                      {t("schedules.form.weekday")}
                    </span>
                    <select
                      className={formStyles.select}
                      value={draft.simple.weekday}
                      onChange={(e) =>
                        patchSimple({
                          weekday: Number(e.target.value)
                        })
                      }
                    >
                      {[1, 2, 3, 4, 5, 6, 0].map((d) => (
                        <option key={d} value={d}>
                          {new Date(
                            Date.UTC(2024, 0, 7 + d)
                          ).toLocaleDateString(i18n.language, {
                            weekday: "long",
                            timeZone: "UTC"
                          })}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : null}
                {draft.simple.freq === "monthly" ? (
                  <label className={formStyles.field}>
                    <span className={formStyles.label}>
                      {t("schedules.form.monthDay")}
                    </span>
                    <input
                      type="number"
                      min={1}
                      max={28}
                      className={formStyles.input}
                      value={draft.simple.monthDay}
                      onChange={(e) =>
                        patchSimple({
                          monthDay: Number(e.target.value)
                        })
                      }
                    />
                    <span className={formStyles.hint}>
                      {t("schedules.form.monthDayHint")}
                    </span>
                  </label>
                ) : null}
                <label className={formStyles.field}>
                  <span className={formStyles.label}>
                    {t("schedules.form.time")}
                  </span>
                  <input
                    type="time"
                    className={formStyles.input}
                    value={draft.simple.time}
                    onChange={(e) =>
                      patchSimple({
                        time: e.target.value
                      })
                    }
                  />
                </label>
              </div>
              <span className={formStyles.hint}>
                {t("schedules.form.tzShown", {
                  tz: selectedTz
                })}
              </span>
            </div>
          ) : null}

          {draft.kind === "natural" ? (
            <div className={formStyles.field}>
              <div className={formStyles.row}>
                <input
                  className={`${formStyles.input} ${formStyles.grow}`}
                  value={draft.text}
                  placeholder={t("schedules.form.textPlaceholder")}
                  aria-label={t("schedules.form.text")}
                  onChange={(e) => patch({ text: e.target.value })}
                />
                <Button
                  size="md"
                  variant="secondary"
                  disabled={draft.text.trim() === ""}
                  onClick={() => void runPreview()}
                >
                  {t("schedules.form.preview")}
                </Button>
              </div>
              {!activePreview ? (
                <span className={formStyles.hint}>
                  {t("schedules.form.customHint")}{" "}
                  {t("schedules.form.textHint", {
                    tz: selectedTz
                  })}
                </span>
              ) : null}
            </div>
          ) : null}

          {activePreview ||
          (draft.kind === "keep" && previewRuns.length > 0) ? (
            <div
              className={styles.previewBox}
              role="status"
              data-testid="schedule-preview"
            >
              {activePreview ? (
                <p className={formStyles.success}>
                  {(() => {
                    const l = specLabel(activePreview.spec, (ms) => fmt(ms));
                    const next = activePreview.next_run_at_ms;
                    return `${t(l.i18nKey, l.values)} · ${
                      next !== null && next !== undefined
                        ? t("schedules.form.firstRun", {
                            when: fmt(next)
                          })
                        : t("schedules.form.noFirstRun")
                    }`;
                  })()}
                </p>
              ) : null}
              {previewRuns.length > 0 ? (
                <>
                  <span className={styles.runsTitle}>
                    {t("schedules.form.nextRunsTitle", {
                      tz: draft.kind === "keep" ? LOCAL_TZ : selectedTz
                    })}
                  </span>
                  <ol className={styles.previewList}>
                    {previewRuns.slice(0, 5).map((ms) => (
                      <li key={ms}>
                        {fmt(
                          ms,
                          draft.kind === "keep" ? undefined : selectedTz
                        )}
                      </li>
                    ))}
                  </ol>
                </>
              ) : (
                <p className={formStyles.hint}>
                  {t("schedules.nextRunsEmpty")}
                </p>
              )}
            </div>
          ) : null}
        </fieldset>

        {keepsPayload ? (
          <p className={formStyles.hint}>{t("schedules.form.payloadKept")}</p>
        ) : (
          <label className={formStyles.field}>
            <span className={formStyles.label}>
              {t("schedules.form.prompt")}
            </span>
            <textarea
              className={formStyles.textarea}
              value={draft.prompt}
              onChange={(e) => patch({ prompt: e.target.value })}
            />
            <span className={formStyles.hint}>
              {t("schedules.form.promptHint")}
            </span>
          </label>
        )}

        <fieldset className={styles.fieldset}>
          <legend className={formStyles.label}>
            {t("schedules.form.notify.label")}
          </legend>
          <div
            className={styles.chipRow}
            role="radiogroup"
            aria-label={t("schedules.form.notify.label")}
          >
            {SCHEDULE_NOTIFY_CHOICES.map((c) => (
              <button
                key={c}
                type="button"
                role="radio"
                aria-checked={notify === c}
                className={styles.chip}
                data-testid={`schedule-notify-${c}`}
                onClick={() => setNotify(c)}
              >
                {t(`schedules.form.notify.${c}`)}
              </button>
            ))}
          </div>
          <span className={formStyles.hint}>
            {t("schedules.form.notify.hint")}
          </span>
        </fieldset>

        <details
          className={styles.advanced}
          open={draft.kind === "cron"}
          data-testid="schedule-advanced"
        >
          <summary>{t("schedules.form.advanced")}</summary>
          <div className={styles.advancedBody}>
            <span className={formStyles.hint}>
              {t("schedules.form.advancedHint")}
            </span>
            <label className={styles.radio}>
              <input
                type="checkbox"
                checked={draft.kind === "cron"}
                onChange={(e) =>
                  patch({
                    kind: e.target.checked ? "cron" : "simple"
                  })
                }
              />
              <span>{t("schedules.form.kind.cron")}</span>
            </label>
            {draft.kind === "cron" ? (
              <div className={formStyles.field}>
                <div className={formStyles.row}>
                  <input
                    className={`${formStyles.input} ${formStyles.grow}`}
                    value={draft.cron}
                    placeholder="0 9 * * 1-5 Europe/Istanbul"
                    aria-label={t("schedules.form.cron")}
                    onChange={(e) => patch({ cron: e.target.value })}
                  />
                  <Button
                    size="md"
                    variant="secondary"
                    disabled={draft.cron.trim() === ""}
                    onClick={() => void runPreview()}
                  >
                    {t("schedules.form.preview")}
                  </Button>
                </div>
                <span className={formStyles.hint}>
                  {t("schedules.form.cronHint")}
                </span>
              </div>
            ) : null}

            <span className={formStyles.hint}>
              {t("schedules.form.misfireHint")}
            </span>
            <label className={formStyles.field}>
              <span className={formStyles.label}>
                {t("schedules.form.misfire")}
              </span>
              <select
                className={formStyles.select}
                value={draft.misfire}
                onChange={(e) =>
                  patch({
                    misfire: e.target.value as ScheduleDraft["misfire"]
                  })
                }
              >
                {MISFIRE_POLICIES.map((m) => (
                  <option key={m} value={m}>
                    {t(`schedules.form.misfirePolicy.${m}`)}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </details>

        <label className={styles.toggle}>
          <input
            type="checkbox"
            checked={draft.enabled}
            onChange={(e) => patch({ enabled: e.target.checked })}
          />
          <span>{t("schedules.enabled")}</span>
        </label>

        {problem ? (
          <p className={formStyles.error} role="alert">
            {problem}
          </p>
        ) : null}

        <div className={styles.actions}>
          <Button type="submit" variant="primary" disabled={busy}>
            {editing ? t("schedules.form.save") : t("schedules.form.create")}
          </Button>
          <Button variant="ghost" disabled={busy} onClick={onCancel}>
            {t("schedules.cancel")}
          </Button>
        </div>
      </form>
    </Card>
  );
}
