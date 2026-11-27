import { formatTimeOfDay, formatWhen } from "../lib/dateFormat";
import { formatInScheduleZone, formatRunPreview, specTimeZone } from "../lib/scheduleTime";
import {
  ArrowUpRight,
  CalendarCheck,
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  Clock3,
  HardDrive,
  History,
  Inbox,
  LayoutList,
  type LucideIcon,
  PenLine,
  Play,
  Plus,
  RefreshCw,
  Rocket,
  RotateCcw,
  Sparkles,
  Sun,
  Trash2,
  TrendingDown,
  Zap
} from "lucide-react";
import { type Ref, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import { ApiError } from "../api/client";
import type {
  ParsedSchedule,
  Schedule,
  ScheduleHistoryEntry
} from "../api/types";
import { EmptyState } from "../components/EmptyState";
import { Switch } from "../components/SettingsUi";
import formStyles from "../components/Form.module.css";
import { botHasFullAccess, botView } from "../lib/botProfile";
import { POLICY_CHANGED_EVENT } from "./settings/SafetyLevelsCard";
import scheduleTemplateFile from "../data/scheduleTemplates.json";
import { humanLine } from "../lib/humanError";
import { dayWord, lastOutcome, localSpecLabel, primaryAction, scheduleKind } from "../lib/automations";
import { nameFromTask, parseAutomationSentence } from "../lib/automationSentence";
import { clearScheduleIntent, readScheduleIntent } from "../lib/scheduleIntent";
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
  suggestScheduleNotify,
  writeScheduleNotify
} from "../lib/scheduleNotify";
import { weekdayName, whenSummary, zoneCity } from "../lib/scheduleSummary";
import { type ScheduleTemplate, templateText } from "../lib/scheduleTemplates";
import {
  affectsSchedules,
  buildInput,
  buildMonthGrid,
  draftFromSchedule,
  emptyDraft,
  isOneShotSpec,
  isTimeZoneName,
  filterScheduleHistory,
  formatDurationParts,
  type HistoryFilter,
  historyStatusTone,
  localDayKey,
  mergeLiveRunsIntoHistory,
  CATCHUP_WINDOWS,
  MISFIRE_POLICIES,
  moveCalendarFocusIndex,
  parsePayload,
  placeScheduleCalendarEvents,
  runsOfSchedule,
  type ScheduleDraft,
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

/** Every zone this runtime knows (the full IANA list), the local one first. */
const ALL_TIMEZONES: string[] = Array.from(
  new Set([
    LOCAL_TZ,
    "UTC",
    ...(typeof Intl.supportedValuesOf === "function"
      ? Intl.supportedValuesOf("timeZone")
      : ["Europe/Istanbul", "Europe/Berlin", "Europe/London", "America/New_York", "Asia/Tokyo"])
  ])
);

/**
 * Timezone field with type-to-search over the whole IANA list. A value that is not a zone is kept
 * as typed (so typing works) but never reaches the draft: the last good zone stays until a valid
 * one is picked.
 */
function TimezonePicker({
  value,
  allowEmpty,
  inputRef,
  onChange
}: {
  inputRef?: Ref<HTMLInputElement>;
  value: string;
  /** `true`: an empty field means UTC (a 5-field cron). */
  allowEmpty: boolean;
  onChange: (tz: string) => void;
}) {
  const { t } = useTranslation();
  const listId = useId();
  const [text, setText] = useState(value || (allowEmpty ? "UTC" : ""));
  useEffect(() => {
    setText(value || (allowEmpty ? "UTC" : ""));
  }, [value, allowEmpty]);
  const typed = text.trim();
  const valid = typed !== "" && isTimeZoneName(typed);
  return (
    <>
      <input
        ref={inputRef}
        className={formStyles.input}
        list={listId}
        value={text}
        spellCheck={false}
        autoComplete="off"
        aria-label={t("schedules.form.tz")}
        aria-invalid={!valid}
        placeholder={t("schedules.form.tzSearch")}
        data-testid="schedule-tz"
        onChange={(e) => {
          setText(e.target.value);
          const v = e.target.value.trim();
          if (v !== "" && isTimeZoneName(v)) onChange(v === "UTC" && allowEmpty ? "" : v);
        }}
      />
      <datalist id={listId}>
        {ALL_TIMEZONES.map((tz) => (
          <option key={tz} value={tz} />
        ))}
      </datalist>
      {!valid ? <span className={formStyles.error} role="alert">{t("schedules.form.tzUnknown")}</span> : null}
    </>
  );
}

function historyToneClass(status: string): string {
  const tone = historyStatusTone(status);
  return styles[tone] ?? styles.running;
}

/** Scheduled tasks: list, calendar, next-runs preview, create/edit, run history, retry. */
export function SchedulesView({
  onOpenChat,
  initialBotId
}: {
  onOpenChat: (chatId: string) => void;
  initialBotId?: string;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, events, onDaemonEvent } = useDaemon();
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);
  const [historyById, setHistoryById] = useState<
    Record<string, ScheduleHistoryEntry[]>
  >({});
  const [viewMode, setViewMode] = useState<"list" | "calendar">("list");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  // A moved schedule whose server could not be asked: waiting for the owner to confirm.
  const [confirmReclaimId, setConfirmReclaimId] = useState<string | null>(null);
  // A note from the bot panel: open the form with a sentence, or show one schedule.
  const [intent] = useState(readScheduleIntent);
  useEffect(() => clearScheduleIntent(), []);
  /** `null` = no form; `"new"` or a schedule id being edited. */
  const [editing, setEditing] = useState<string | null>(
    initialBotId || intent?.kind === "draft" ? "new" : null
  );
  /** The schedule whose next runs and history are open. */
  const [expandedId, setExpandedId] = useState<string | null>(
    intent?.kind === "focus" ? intent.scheduleId : null
  );

  const fmt = useCallback(
    (ms: number, zone?: string) =>
      zone
        ? formatInScheduleZone(ms, i18n.language, zone, LOCAL_TZ)
        : formatWhen(ms, i18n.language),
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
  // Opened from the bot panel on one schedule: bring it into view once the list is there.
  const focusId = intent?.kind === "focus" ? intent.scheduleId : null;
  const loaded = schedules !== null;
  useEffect(() => {
    if (!focusId || !loaded) return;
    document
      .querySelector(`[data-schedule-id="${CSS.escape(focusId)}"]`)
      ?.scrollIntoView?.({ block: "center" });
  }, [focusId, loaded]);
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
            <ul className={styles.explain} data-testid="schedules-explain">
              <li>
                <Clock3 aria-hidden="true" />
                {t("automation.explain.schedule")}
              </li>
              <li>
                <Zap aria-hidden="true" />
                {t("automation.explain.events")}
              </li>
              <li>
                <Rocket aria-hidden="true" />
                {t("automation.explain.missions")}
              </li>
            </ul>
          </div>
          <div className={styles.headerActions}>
            <div
              className={styles.filterGroup}
              role="group"
              aria-label={t("schedules.viewLabel")}
            >
              <Button
                size="sm"
                variant={viewMode === "list" ? "secondary" : "ghost"}
                className={styles.headerGhost}
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
                className={styles.headerGhost}
                aria-pressed={viewMode === "calendar"}
                data-testid="schedules-view-calendar"
                onClick={() => setViewMode("calendar")}
              >
                <CalendarDays />
                {t("schedules.viewCalendar")}
              </Button>
            </div>
            <Button size="sm" variant="ghost" className={styles.headerGhost} onClick={() => void load()}>
              <RefreshCw />
              {t("schedules.refresh")}
            </Button>
            <Button
              size="sm"
              variant="primary"
              disabled={bots.length === 0}
              title={bots.length === 0 ? t("schedules.newNeedsBot") : undefined}
              data-testid="schedules-new"
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
            initialBotId={intent?.kind === "draft" ? intent.botId : initialBotId}
            initialText={intent?.kind === "draft" ? intent.text : undefined}
            onDelete={
              editingSchedule
                ? async () => {
                    await client!.deleteSchedule(editingSchedule.id);
                    setEditing(null);
                    await load();
                  }
                : undefined
            }
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
          <EmptyState
            icon={CalendarDays}
            title={t("schedules.empty.title")}
            actionLabel={bots.length > 0 ? t("schedules.new") : undefined}
            primary={false}
            onAction={() => setEditing("new")}
            testId="schedules-empty"
            actionTestId="schedules-empty-new"
          >
            {bots.length === 0
              ? t("schedules.empty.noBots")
              : t("schedules.empty.hint")}
          </EmptyState>
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
            {list.map((s) => (
              <ScheduleRow
                key={s.id}
                schedule={s}
                history={mergeLiveRunsIntoHistory(historyById[s.id] ?? [], runsOfSchedule(events.runs, s.id, 20))}
                busy={busyId === s.id}
                expanded={expandedId === s.id}
                confirmReclaim={confirmReclaimId === s.id}
                fmt={fmt}
                onToggleExpanded={() => setExpandedId((cur) => (cur === s.id ? null : s.id))}
                onEdit={() => setEditing(s.id)}
                onOpenChat={onOpenChat}
                onAct={(fn, ok) => void act(s.id, fn, ok)}
                onReclaim={() => {
                  const confirmed = confirmReclaimId === s.id;
                  setConfirmReclaimId(null);
                  void act(
                    s.id,
                    async () => {
                      try {
                        await client!.reclaimSchedule(s.id, confirmed);
                      } catch (e) {
                        if (e instanceof ApiError && e.code === "host_unreachable") {
                          setConfirmReclaimId(s.id);
                        }
                        throw e;
                      }
                    },
                    t("schedules.reclaimed", { name: s.name })
                  );
                }}
              />
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

/** One schedule: what and when at a glance, its last result, one primary button; history folds. */
function ScheduleRow({
  schedule: s,
  history,
  busy,
  expanded,
  confirmReclaim,
  fmt,
  onToggleExpanded,
  onEdit,
  onOpenChat,
  onAct,
  onReclaim
}: {
  schedule: Schedule;
  history: ScheduleHistoryEntry[];
  busy: boolean;
  expanded: boolean;
  confirmReclaim: boolean;
  fmt: (ms: number, zone?: string) => string;
  onToggleExpanded: () => void;
  onEdit: () => void;
  onOpenChat: (chatId: string) => void;
  onAct: (fn: () => Promise<unknown>, ok?: string) => void;
  onReclaim: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const bot = bots.find((b) => b.id === s.bot_id);
  const view = bot ? botView(bot) : null;
  const zone = specTimeZone(s.spec);
  const label = localSpecLabel(s.spec, (ms) => fmt(ms, zone), i18n.language, LOCAL_TZ);
  const payload = parsePayload(s.payload);
  const last = lastOutcome(history);
  const primary = primaryAction(s, last);
  const nextRuns = s.next_runs_ms ?? [];
  const visibleHistory = filterScheduleHistory(history, filter);
  const detailsId = `schedule-details-${s.id}`;
  const near = (ms: number) => {
    const word = dayWord(ms);
    return word ? t(`automation.day.${word}`, { time: formatTimeOfDay(ms, i18n.language) }) : fmt(ms, zone);
  };
  const kind = scheduleKind(s);
  const when =
    kind === "file" ? t("automation.what.fileShort") : kind === "mail" ? t("automation.what.mailShort") : t(label.i18nKey, label.values);
  const next = s.moved_to
    ? t("automation.state.moved", { name: s.moved_to })
    : !s.enabled
      ? t("automation.state.off")
      : s.next_run_at_ms !== null && s.next_run_at_ms !== undefined
        ? t("automation.state.next", { when: near(s.next_run_at_ms) })
        : kind === "time"
          ? t("automation.state.done")
          : t("automation.state.waiting");
  const lastLine = last
    ? t(`automation.last.${last.tone}`, { when: last.atMs !== null ? near(last.atMs) : "" })
    : t("automation.last.never");
  return (
    <li data-testid="schedule-row" data-schedule-id={s.id}>
      <Card padding="md" className={styles.item}>
        <div className={styles.rowMain}>
          {view ? <BotAvatar bot={view} size="sm" decorative /> : <span className={styles.rowAvatar} aria-hidden="true" />}
          <div className={styles.rowText}>
            <div className={styles.itemTitle}>
              <h2 className={styles.name}>{s.name}</h2>
              {s.origin === "bot" ? (
                <span className={styles.badge} data-testid="schedule-origin-bot" title={t("schedules.originBotHint")}>
                  {t("schedules.originBot")}
                </span>
              ) : null}
            </div>
            <p className={styles.rowWhen}>
              {when} · {view?.name ?? t("schedules.deletedBot")}
            </p>
            <p className={styles.rowFacts}>
              <span data-testid="schedule-status">{next}</span>
              <span className={styles.lastResult} data-tone={last?.tone ?? "none"} data-testid="schedule-last">
                <i aria-hidden="true" />
                {lastLine}
              </span>
              {(s.missed_occurrences ?? 0) > 0 ? (
                <span data-testid="schedule-missed">{t("automation.missedCount", { count: s.missed_occurrences ?? 0 })}</span>
              ) : null}
            </p>
            <p className={`${styles.rowDoes} ${styles.clamp}`}>
              {payload.kind === "agent_prompt"
                ? payload.text
                : payload.kind === "unknown"
                  ? t("schedules.payload.unknown")
                  : t(`schedules.payload.${payload.kind}`, { name: payload.name })}
            </p>
          </div>
          <div className={styles.rowControls}>
            {s.moved_to ? null : (
              <Switch
                checked={s.enabled}
                disabled={busy}
                label={t("automation.toggle", { name: s.name })}
                onChange={() => onAct(() => client!.updateSchedule(s.id, toggleInput(s)))}
              />
            )}
          </div>
        </div>
        {s.moved_to ? (
          <p className={formStyles.hint} data-testid="schedule-moved">
            {t("schedules.movedTo", { name: s.moved_to })}
          </p>
        ) : null}
        {confirmReclaim && s.moved_to ? (
          <p className={formStyles.error} role="alert">
            {t("schedules.reclaimUnreachable", { name: s.moved_to })}
          </p>
        ) : null}
        <div className={styles.actions}>
          {primary.kind === "reclaim" ? (
            <Button size="sm" variant="secondary" disabled={busy} data-testid="schedule-reclaim" onClick={onReclaim}>
              <RotateCcw />
              {confirmReclaim ? t("schedules.reclaimAnyway") : t("schedules.reclaim")}
            </Button>
          ) : primary.kind === "retry" ? (
            <Button
              size="sm"
              variant="secondary"
              disabled={busy}
              data-testid="schedule-primary-retry"
              onClick={() =>
                onAct(() => client!.retryScheduleOccurrence(s.id, primary.dueMs), t("schedules.retryStarted", { name: s.name }))
              }
            >
              <RotateCcw />
              {last?.tone === "missed" ? t("automation.runMissed") : t("schedules.retry")}
            </Button>
          ) : (
            <Button
              size="sm"
              variant="secondary"
              disabled={busy}
              data-testid="schedule-run-now"
              onClick={() => onAct(() => client!.runScheduleNow(s.id), t("schedules.runNowStarted", { name: s.name }))}
            >
              <Play />
              {t("schedules.runNow")}
            </Button>
          )}
          <Button size="sm" variant="ghost" disabled={busy || Boolean(s.moved_to)} data-testid="schedule-edit" onClick={onEdit}>
            <PenLine />
            {t("schedules.edit")}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            aria-expanded={expanded}
            aria-controls={detailsId}
            data-testid="schedule-history-toggle"
            onClick={onToggleExpanded}
          >
            <History />
            {t("automation.historyToggle", { count: history.length })}
          </Button>
        </div>

        {expanded ? (
          <div className={styles.details} id={detailsId}>
            {nextRuns.length > 0 ? (
              <div className={styles.upcomingSection} data-testid="schedule-next-runs">
                <h3 className={styles.runsTitle}>{t("schedules.nextRuns")}</h3>
                <p className={styles.upcomingLine}>
                  {nextRuns
                    .slice(0, 5)
                    .map((ts) => fmt(ts, zone))
                    .join(" · ")}
                </p>
              </div>
            ) : null}
            <div className={styles.runs} data-testid="schedule-history">
              <div className={styles.runsHead}>
                <h3 className={styles.runsTitle}>{t("schedules.history")}</h3>
                <div className={styles.filterGroup} role="group">
                  {(["all", "issues"] as const).map((f) => (
                    <button
                      key={f}
                      type="button"
                      className={`${styles.filterChip} ${filter === f ? styles.filterChipActive : ""}`}
                      aria-pressed={filter === f}
                      onClick={() => setFilter(f)}
                    >
                      {f === "all" ? t("schedules.filterAll") : t("schedules.filterIssues")}
                    </button>
                  ))}
                </div>
              </div>
              {visibleHistory.length === 0 ? (
                <p className={formStyles.hint}>
                  {filter === "issues" ? t("schedules.historyEmptyFiltered") : t("schedules.historyEmpty")}
                </p>
              ) : (
                <ul className={styles.runList}>
                  {visibleHistory.slice(0, 10).map((entry) => {
                    const key = `${entry.due_ms ?? "direct"}-${entry.run_id ?? entry.status}`;
                    const duration = formatDurationParts(entry.started_at_ms, entry.finished_at_ms);
                    const at = entry.started_at_ms ?? entry.due_ms;
                    return (
                      <li key={key} className={styles.run} data-testid="schedule-history-row">
                        <span className={`${styles.badge} ${historyToneClass(entry.status)}`}>
                          {t(`schedules.run.${entry.status}`, { defaultValue: entry.status })}
                        </span>
                        {at !== null && at !== undefined ? (
                          <span className={formStyles.hint}>
                            {entry.started_at_ms != null ? fmt(entry.started_at_ms) : t("schedules.dueAt", { when: fmt(at) })}
                          </span>
                        ) : null}
                        {duration ? <span className={formStyles.hint}>{t(duration.i18nKey, duration.values)}</span> : null}
                        {entry.status === "missed" ? <span className={formStyles.hint}>{t("schedules.missedWhy")}</span> : null}
                        {entry.error ? (
                          <span className={styles.runError}>
                            {entry.error.startsWith("The model service cannot be reached.")
                              ? t("offline.scheduleSkipped")
                              : entry.error.startsWith("The model service cannot be reached after bounded retries.")
                                ? t("offline.scheduleRetry")
                                : entry.error.startsWith("Waiting for a connection to the model service.")
                                  ? t("offline.scheduleWaiting")
                                  : entry.error}
                          </span>
                        ) : null}
                        {entry.chat_id ? (
                          <Button size="sm" variant="ghost" onClick={() => onOpenChat(entry.chat_id!)}>
                            <ArrowUpRight />
                            {t("schedules.openRun")}
                          </Button>
                        ) : null}
                        {entry.retryable && entry.due_ms !== null && entry.due_ms !== undefined && entry !== last?.entry ? (
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={busy || Boolean(s.moved_to)}
                            data-testid="schedule-retry"
                            onClick={() =>
                              onAct(() => client!.retryScheduleOccurrence(s.id, entry.due_ms!), t("schedules.retryStarted", { name: s.name }))
                            }
                          >
                            <RotateCcw />
                            {entry.status === "missed" ? t("automation.runMissed") : t("schedules.retry")}
                          </Button>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </div>
        ) : null}
      </Card>
    </li>
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
                            className={styles.cardGhost}
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
  initialBotId,
  initialText,
  onCancel,
  onSaved,
  onDelete
}: {
  editing?: Schedule;
  initialBotId?: string;
  /** A sentence from the bot panel ("Every weekday at 9:00, check my inbox"), read on open. */
  initialText?: string;
  onCancel: () => void;
  onSaved: () => Promise<void>;
  /** Editing only: removes the schedule (asks once, inline). */
  onDelete?: () => Promise<void>;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [draft, setDraft] = useState<ScheduleDraft>(() =>
    editing
      ? draftFromSchedule(editing)
      : { ...emptyDraft(bots.find((b) => b.id === initialBotId)?.id ?? bots[0]?.id ?? ""), tz: LOCAL_TZ }
  );
  const [preview, setPreview] = useState<{
    key: string;
    result: ParsedSchedule;
  } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notify, setNotifyState] = useState<ScheduleNotify>(() =>
    editing
      ? scheduleNotifyOf(editing.id, {
          prompt: draftFromSchedule(editing).prompt,
          oneShot: isOneShotSpec(editing.spec)
        })
      : "problems"
  );
  // Until the owner picks one, the choice follows the schedule's purpose: a reminder is announced
  // every time, background work only when something goes wrong.
  const [notifyTouched, setNotifyTouched] = useState(false);
  const setNotify = (c: ScheduleNotify) => {
    setNotifyTouched(true);
    setNotifyState(c);
  };
  const suggested = suggestScheduleNotify(
    draft.prompt,
    draft.kind === "simple" && draft.simple.freq === "once"
  );
  useEffect(() => {
    if (!editing && !notifyTouched) setNotifyState(suggested);
  }, [editing, notifyTouched, suggested]);
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
  // "In your own words": one sentence fills how often, the time, the task and a name.
  const [sentence, setSentence] = useState(initialText ?? "");
  const [sentenceNote, setSentenceNote] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  // A bot at Full access deletes its own schedule without the extra confirmation.
  const [globalPreset, setGlobalPreset] = useState<string | null>(null);
  useEffect(() => {
    if (!client) return undefined;
    let revision = 0;
    const loadPolicy = () => {
      const request = ++revision;
      setGlobalPreset(null);
      void client.getPolicy().then((policy) => {
        if (revision === request && !policy.load_error) setGlobalPreset(policy.preset ?? null);
      }).catch(() => {});
    };
    loadPolicy();
    window.addEventListener(POLICY_CHANGED_EVENT, loadPolicy);
    return () => {
      revision += 1;
      window.removeEventListener(POLICY_CHANGED_EVENT, loadPolicy);
    };
  }, [client]);
  const runDelete = () => {
    if (!onDelete) return;
    setBusy(true);
    void onDelete()
      .catch((e) => setProblem(errorText(e)))
      .finally(() => setBusy(false));
  };
  const applySentence = (text: string) => {
    const read = parseAutomationSentence(text);
    if (!read.task && !read.simple) {
      return;
    }
    setDraft((d) => ({
      ...d,
      ...(read.simple ? { kind: "simple" as const, simple: read.simple } : {}),
      prompt: read.task || d.prompt,
      name: d.name.trim() ? d.name : nameFromTask(read.task, i18n.language)
    }));
    setProblem(null);
    if (read.simple) {
      const spec = simpleSpec(read.simple, draft.tz ?? LOCAL_TZ);
      const l = spec ? localSpecLabel(spec, (ms) => formatRunPreview(ms, i18n.language), i18n.language, LOCAL_TZ) : null;
      setSentenceNote(l ? t("automation.form.understood", { when: t(l.i18nKey, l.values) }) : null);
    } else {
      setSentenceNote(t("automation.form.noWhen"));
    }
  };
  const appliedInitial = useRef(false);
  useEffect(() => {
    if (initialText && !appliedInitial.current) {
      appliedInitial.current = true;
      applySentence(initialText);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const keepsPayload =
    editing !== undefined &&
    parsePayload(editing.payload).kind !== "agent_prompt";
  const selectedTz = draft.tz ?? LOCAL_TZ;

  const fmt = (ms: number, tz?: string) => formatRunPreview(ms, i18n.language, tz);

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

  // The "when" summary: the easy builder's choice as one sentence ("Every weekday at 09:00,
  // Istanbul"); for words, cron or a kept schedule, what the daemon read from them.
  const whenLine = (() => {
    if (draft.kind === "keep" && editing) {
      const l = specLabel(editing.spec, (ms) => fmt(ms), i18n.language);
      return t("schedules.form.keepHint", { spec: t(l.i18nKey, l.values) });
    }
    if (draft.kind === "simple") {
      const said = whenSummary(draft.simple, selectedTz, i18n.language);
      return said ? t(said.key, said.values) : t("schedules.form.simpleProblem.time");
    }
    if (activePreview) {
      const l = specLabel(activePreview.spec, (ms) => fmt(ms), i18n.language);
      return t(l.i18nKey, l.values);
    }
    return draft.kind === "natural"
      ? `${t("schedules.form.customHint")} ${t("schedules.form.textHint", { tz: selectedTz })}`
      : t("schedules.form.cronHint");
  })();
  const firstRunAt = draft.kind === "keep" ? editing?.next_run_at_ms : activePreview?.next_run_at_ms;
  const firstRunLine =
    draft.kind === "keep" || activePreview
      ? firstRunAt !== null && firstRunAt !== undefined
        ? t("schedules.form.firstRun", { when: fmt(firstRunAt, draft.kind === "keep" ? undefined : selectedTz) })
        : t("schedules.form.noFirstRun")
      : null;
  // "More options" holds the time zone: "Change" next to the summary opens it there.
  const [moreOpen, setMoreOpen] = useState(draft.kind === "cron");
  const zoneRef = useRef<HTMLInputElement | null>(null);
  const openZone = () => {
    setMoreOpen(true);
    requestAnimationFrame(() => zoneRef.current?.focus());
  };

  // The form opens above the list: bring it into view and put the keyboard on its title.
  const titleRef = useRef<HTMLHeadingElement | null>(null);
  const sentenceId = useId();
  useEffect(() => {
    const el = titleRef.current;
    el?.scrollIntoView?.({ block: "nearest" });
    el?.focus({ preventScroll: true });
  }, []);

  return (
    <Card padding="md" className={styles.form}>
      <form
        className={styles.formBody}
        aria-label={heading}
        // The app words the problem (in the UI language); the browser's own bubble would use the browser's.
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <h2 className={styles.formTitle} ref={titleRef} tabIndex={-1}>
          {heading}
        </h2>

        {!editing ? (
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor={`${sentenceId}-text`}>
              {t("automation.form.sentenceLabel")}
            </label>
            <div className={formStyles.row}>
              <input
                id={`${sentenceId}-text`}
                className={`${formStyles.input} ${formStyles.grow}`}
                value={sentence}
                placeholder={t("automation.quick.placeholder")}
                data-testid="schedule-sentence"
                onChange={(e) => setSentence(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    applySentence(sentence);
                  }
                }}
              />
              <Button
                size="md"
                variant="secondary"
                disabled={sentence.trim() === ""}
                data-testid="schedule-sentence-apply"
                onClick={() => applySentence(sentence)}
              >
                <Sparkles />
                {t("automation.form.fill")}
              </Button>
            </div>
            <span className={formStyles.hint} role="status" data-testid="schedule-sentence-note">
              {sentenceNote ?? t("automation.form.sentenceHint")}
            </span>
            {/* The examples are a row of small chips, not a wall of cards: one click fills the form. */}
            <div className={styles.exampleRow} role="group" aria-labelledby={`${sentenceId}-examples`} data-testid="schedule-examples">
              <span className={styles.exampleLabel} id={`${sentenceId}-examples`}>
                {t("scheduleEditor.examples")}
              </span>
              {TEMPLATES.map((tpl) => {
                const x = templateText(tpl, i18n.language);
                const Icon = TEMPLATE_ICONS[tpl.icon] ?? Sun;
                return (
                  <button
                    key={tpl.id}
                    type="button"
                    className={styles.exampleChip}
                    title={x.blurb}
                    data-testid={`schedule-template-${tpl.id}`}
                    onClick={() => applyTemplate(tpl)}
                  >
                    <Icon aria-hidden="true" />
                    {x.name}
                  </button>
                );
              })}
            </div>
          </div>
        ) : null}

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

          {draft.kind === "simple" ? (
            <div className={styles.whenRow}>
              {draft.simple.freq === "once" ? (
                <label className={styles.whenField}>
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
                <label className={styles.whenField}>
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
                        {weekdayName(d, i18n.language)}
                      </option>
                    ))}
                  </select>
                </label>
              ) : null}
              {draft.simple.freq === "monthly" ? (
                <label className={styles.whenField}>
                  <span className={formStyles.label}>
                    {t("schedules.form.monthDay")}
                  </span>
                  <input
                    type="number"
                    min={1}
                    max={28}
                    className={formStyles.input}
                    value={draft.simple.monthDay}
                    title={t("schedules.form.monthDayHint")}
                    onChange={(e) =>
                      patchSimple({
                        monthDay: Number(e.target.value)
                      })
                    }
                  />
                </label>
              ) : null}
              <label className={styles.whenField}>
                <span className={formStyles.label}>
                  {t("schedules.form.time")}
                </span>
                <input
                  type="time"
                  className={formStyles.input}
                  value={draft.simple.time}
                  data-testid="schedule-time"
                  onChange={(e) =>
                    patchSimple({
                      time: e.target.value
                    })
                  }
                />
              </label>
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
            </div>
          ) : null}

          {/* One readable line for the choice above, then the first run; the next runs fold. */}
          <div className={styles.whenSummary} role="status" data-testid="schedule-preview">
            <Clock3 className={styles.whenIcon} aria-hidden="true" />
            <div className={styles.whenLines}>
              <p className={styles.whenText} data-testid="schedule-when-summary">
                {whenLine}
                {draft.kind !== "keep" ? (
                  <button type="button" className={styles.linkButton} onClick={openZone} data-testid="schedule-change-zone">
                    {t("scheduleEditor.changeZone")}
                  </button>
                ) : null}
              </p>
              {firstRunLine ? <p className={formStyles.hint}>{firstRunLine}</p> : null}
              {previewRuns.length > 0 ? (
                <details className={styles.nextRuns}>
                  <summary>
                    {t("schedules.form.nextRunsTitle", {
                      tz: draft.kind === "keep" ? LOCAL_TZ : selectedTz
                    })}
                  </summary>
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
                </details>
              ) : null}
            </div>
          </div>
        </fieldset>

        <div className={styles.formPair}>
          <label className={formStyles.field}>
            <span className={formStyles.label}>{t("schedules.bot")}</span>
            <select
              className={formStyles.select}
              value={draft.botId}
              disabled={editing !== undefined}
              data-testid="schedule-bot"
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
          <label className={formStyles.field}>
            <span className={formStyles.label}>{t("schedules.form.name")}</span>
            <input
              className={formStyles.input}
              value={draft.name}
              aria-required="true"
              data-testid="schedule-name"
              onChange={(e) => patch({ name: e.target.value })}
            />
          </label>
        </div>

        {keepsPayload ? (
          <p className={formStyles.hint}>{t("schedules.form.payloadKept")}</p>
        ) : (
          <label className={formStyles.field}>
            <span className={formStyles.label}>
              {t("scheduleEditor.whatToDo")}
            </span>
            <textarea
              className={formStyles.textarea}
              value={draft.prompt}
              placeholder={t("scheduleEditor.whatPlaceholder")}
              data-testid="schedule-prompt"
              onChange={(e) => patch({ prompt: e.target.value })}
            />
            <span className={formStyles.hint}>
              {t("schedules.form.promptHint")}
            </span>
          </label>
        )}

        <details
          className={styles.advanced}
          open={moreOpen}
          onToggle={(e) => setMoreOpen(e.currentTarget.open)}
          data-testid="schedule-advanced"
        >
          <summary>
            <span>{t("scheduleEditor.more")}</span>
            <span className={styles.moreLine}>
              {t("scheduleEditor.moreLine", {
                notify: t(`schedules.form.notify.${notify}`),
                zone: zoneCity(selectedTz)
              })}
            </span>
          </summary>
          <div className={styles.advancedBody}>
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

            {draft.kind !== "keep" ? (
              <label className={formStyles.field}>
                <span className={formStyles.label}>{t("schedules.form.tz")}</span>
                <TimezonePicker
                  inputRef={zoneRef}
                  value={draft.tz ?? ""}
                  allowEmpty={draft.kind === "cron"}
                  onChange={(tz) => patch({ tz })}
                />
                {draft.kind === "cron" ? (
                  <span className={formStyles.hint}>
                    {t("schedules.form.tzCronHint")}
                  </span>
                ) : null}
              </label>
            ) : null}

            <label className={styles.toggle}>
              <input
                type="checkbox"
                checked={draft.enabled}
                onChange={(e) => patch({ enabled: e.target.checked })}
              />
              <span>{t("schedules.enabled")}</span>
            </label>

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
            {draft.misfire !== "skip" ? (
              <label className={formStyles.field}>
                <span className={formStyles.label}>
                  {t("schedules.form.catchupWindow")}
                </span>
                <select
                  className={formStyles.select}
                  value={draft.catchupWindow}
                  data-testid="schedule-catchup-window"
                  onChange={(e) =>
                    patch({ catchupWindow: Number(e.target.value) })
                  }
                >
                  {CATCHUP_WINDOWS.map((w) => (
                    <option key={w} value={w}>
                      {t(`schedules.form.catchupWindowOption.${w}`)}
                    </option>
                  ))}
                </select>
                <span className={formStyles.hint}>
                  {t("schedules.form.catchupWindowHint")}
                </span>
              </label>
            ) : null}
          </div>
        </details>

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
          {onDelete ? (
            <span className={styles.deleteArea}>
              {confirmDelete ? (
                <>
                  <span className={formStyles.hint}>{t("automation.form.deleteAsk")}</span>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={busy}
                    data-testid="schedule-delete-confirm"
                    onClick={runDelete}
                  >
                    {t("schedules.confirmDelete")}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setConfirmDelete(false)}>
                    {t("schedules.cancel")}
                  </Button>
                </>
              ) : (
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  data-testid="schedule-delete"
                  onClick={() => {
                    if (botHasFullAccess(bots.find((b) => b.id === editing?.bot_id), globalPreset)) runDelete();
                    else setConfirmDelete(true);
                  }}
                >
                  <Trash2 />
                  {t("schedules.delete")}
                </Button>
              )}
            </span>
          ) : null}
        </div>
      </form>
    </Card>
  );
}
