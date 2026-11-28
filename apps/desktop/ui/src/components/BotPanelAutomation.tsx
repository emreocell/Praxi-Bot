import { CalendarClock, CalendarDays, ChevronRight, Clock3, FileText, Home, ShieldCheck, FolderOpen, Mail, type LucideIcon, Rocket, RotateCcw, Rss, Sparkles, Webhook } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Schedule, ScheduleHistoryEntry } from "../api/types";
import { type AutomationKind, type AutomationRow, botAutomationRows, confirmableAuthor, dayWord, lastOutcome, localSpecLabel, primaryAction } from "../lib/automations";
import { nameFromTask, parseAutomationSentence } from "../lib/automationSentence";
import { formatTimeOfDay, formatWhen } from "../lib/dateFormat";
import { humanLine } from "../lib/humanError";
import { simpleProblem, simpleSpec } from "../lib/scheduleBuilder";
import { setScheduleIntent } from "../lib/scheduleIntent";
import { suggestScheduleNotify, writeScheduleNotify } from "../lib/scheduleNotify";
import { affectsSchedules, buildInput, emptyDraft, toggleInput } from "../lib/schedules";
import { newSource, newTrigger, triggerInput, type BotTrigger } from "../lib/triggers";
import { useDaemon } from "../state/DaemonContext";
import { BotPanelWebhooks } from "./BotPanelWebhooks";
import { TriggerBuilder, type TriggerEdit } from "./BotPanelTriggers";
import { Switch } from "./SettingsUi";
import { useToast } from "./Toast";
import styles from "./BotPanelAutomation.module.css";

const ICONS: Record<AutomationKind, LucideIcon> = { time: Clock3, mail: Mail, webhook: Webhook, file: FolderOpen, feed: Rss, page: FileText, calendar: CalendarDays, home_assistant: Home };
/** The event kinds offered as one-tap cards; the file watcher is explained separately (it starts from the chat). */
const EVENT_CARDS = ["mail", "webhook", "feed", "page", "calendar", "home_assistant"] as const;
const hostOf = (url: string) => { try { return new URL(url).host; } catch { return url; } };
const LOCAL_TZ = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

/**
 * The bot panel's Automation tab: everything this bot does without being asked, in one list,
 * and one sentence to add more ("Every weekday at 9:00, check my inbox and summarise").
 * Event triggers (mail, webhook, file) are explained in one line each; long jobs point to missions.
 */
export function BotPanelAutomation({
  botId,
  botName,
  botIsFull = false,
  visible,
  onOpenSchedules,
  onOpenMissions
}: {
  botId: string;
  botName: string;
  /** The bot is on the Full level: its unconfirmed automations offer "I wrote this". */
  botIsFull?: boolean;
  visible: boolean;
  onOpenSchedules?: () => void;
  onOpenMissions: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, onDaemonEvent } = useDaemon();
  const toast = useToast();
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);
  const [triggers, setTriggers] = useState<BotTrigger[]>([]);
  const [history, setHistory] = useState<Record<string, ScheduleHistoryEntry[]>>({});
  const [error, setError] = useState<string | null>(null);
  const [builder, setBuilder] = useState<(TriggerEdit & { key: number }) | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const version = useRef<object>({});

  const load = useCallback(async () => {
    if (!client) return;
    const mine = {};
    version.current = mine;
    const [all, rules] = await Promise.allSettled([client.listSchedules(), client.listTriggers(botId)]);
    if (mine !== version.current) return;
    const own = all.status === "fulfilled" ? all.value.filter((s) => s.bot_id === botId) : [];
    setSchedules(own);
    setTriggers(rules.status === "fulfilled" ? rules.value : []);
    const failed = [all, rules].find((r) => r.status === "rejected") as PromiseRejectedResult | undefined;
    setError(failed ? humanLine(failed.reason) : null);
    const pairs = await Promise.all(
      own.map(async (s) => [s.id, await client.getScheduleHistory(s.id).catch(() => [] as ScheduleHistoryEntry[])] as const)
    );
    if (mine === version.current) setHistory(Object.fromEntries(pairs));
  }, [botId, client]);

  useEffect(() => {
    setSchedules(null);
    setBuilder(null);
    void load();
    const timer = window.setInterval(() => void load(), 30_000);
    return () => {
      version.current = {};
      window.clearInterval(timer);
    };
  }, [load]);
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

  const act = async (key: string, run: () => Promise<unknown>, ok?: string) => {
    if (busy) return;
    setBusy(key);
    try {
      await run();
      if (ok) toast.show(ok, "success");
      await load();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(null);
    }
  };

  const when = (ms: number) => {
    const word = dayWord(ms);
    return word ? t(`automation.day.${word}`, { time: formatTimeOfDay(ms, i18n.language) }) : formatWhen(ms, i18n.language);
  };
  const openBuilder = (edit: TriggerEdit) => setBuilder({ ...edit, key: Date.now() });
  const openSchedule = (s: Schedule) => {
    setScheduleIntent({ kind: "focus", scheduleId: s.id });
    onOpenSchedules?.();
  };

  const rows = schedules ? botAutomationRows(botId, schedules, triggers) : [];

  /** The two lines under a row's name: what starts it, then how it is doing. */
  const lines = (row: AutomationRow): { what: string; state: string; tone: "muted" | "danger" | "ok" } => {
    if (row.schedule) {
      const s = row.schedule;
      const trigger = s.trigger as { kind?: string; path?: string; glob?: string; from_filter?: string | null } | null | undefined;
      const label = localSpecLabel(s.spec, (ms) => formatWhen(ms, i18n.language), i18n.language, LOCAL_TZ);
      const what =
        row.kind === "file"
          ? t("automation.what.file", { path: [trigger?.path, trigger?.glob].filter(Boolean).join("/") })
          : row.kind === "mail"
            ? t("automation.what.mail", { from: trigger?.from_filter || t("automation.what.anyone") })
            : t(label.i18nKey, label.values);
      const last = lastOutcome(history[s.id] ?? []);
      if (s.moved_to) return { what, state: t("automation.state.moved", { name: s.moved_to }), tone: "muted" };
      if (!s.enabled) return { what, state: t("automation.state.off"), tone: "muted" };
      if (last && (last.tone === "failed" || last.tone === "missed") && last.atMs !== null) {
        return { what, state: t(`automation.state.${last.tone}`, { when: when(last.atMs) }), tone: "danger" };
      }
      if (last?.tone === "running") return { what, state: t("automation.state.running"), tone: "ok" };
      if (s.next_run_at_ms !== null && s.next_run_at_ms !== undefined) {
        return { what, state: t("automation.state.next", { when: when(s.next_run_at_ms) }), tone: "muted" };
      }
      return { what, state: row.kind === "time" ? t("automation.state.done") : t("automation.state.waiting"), tone: "muted" };
    }
    const tr = row.trigger;
    const source = tr.config.source;
    const what =
      source.kind === "mail"
        ? t("automation.what.mail", { from: tr.config.filters.sender || t("automation.what.anyone") })
        : source.kind === "webhook"
          ? t("automation.what.webhook")
          : source.kind === "feed"
            ? t("automation.what.feed", { host: hostOf(source.url) })
            : source.kind === "page"
              ? t("automation.what.page", { host: hostOf(source.url) })
              : source.kind === "calendar"
                ? t("automation.what.calendar", { minutes: source.minutes_before })
                : source.kind === "home_assistant"
                  ? t("automation.what.home_assistant", { entity: source.entity_id })
                  : (() => {
                      const label = localSpecLabel(source.spec, (ms) => formatWhen(ms, i18n.language), i18n.language, LOCAL_TZ);
                      return t(label.i18nKey, label.values);
                    })();
    if (tr.last_error) return { what, state: t("automation.state.attention"), tone: "danger" };
    if (!tr.enabled) return { what, state: tr.origin === "rule" ? t("automation.state.off") : t("automation.state.setUp"), tone: "muted" };
    if (tr.last_fired_at_ms) return { what, state: t("automation.state.lastFired", { when: when(tr.last_fired_at_ms) }), tone: "muted" };
    return { what, state: t("automation.state.waiting"), tone: "muted" };
  };

  return (
    <section className={styles.root} aria-labelledby="bot-automation-title" data-testid="bot-automation">
      <h3 className={styles.srOnly} id="bot-automation-title">
        {t("botPanel.tabs.automation")}
      </h3>
      <p className={styles.intro}>{t("automation.intro", { name: botName })}</p>

      <QuickCreate botId={botId} onCreated={load} onDetails={(text) => {
        setScheduleIntent({ kind: "draft", botId, text });
        onOpenSchedules?.();
      }} />

      <div className={styles.group}>
        <h4 className={styles.label}>
          {t("automation.listTitle")}
          {rows.length ? <span className={styles.count}>{rows.length}</span> : null}
        </h4>
        {error ? <p className={styles.error} role="alert">{error}</p> : null}
        {schedules === null ? (
          <p className={styles.muted} role="status">{t("automation.loading")}</p>
        ) : rows.length === 0 ? (
          <p className={styles.empty} data-testid="automation-empty">{t("automation.empty")}</p>
        ) : (
          <ul className={styles.list}>
            {rows.map((row) => {
              const Icon = ICONS[row.kind];
              const { what, state, tone } = lines(row);
              const s = row.schedule;
              const last = s ? lastOutcome(history[s.id] ?? []) : null;
              const action = s ? primaryAction(s, last) : null;
              const retryDue = action?.kind === "retry" ? action.dueMs : null;
              const rule = row.trigger?.origin === "rule" ? row.trigger : null;
              return (
                <li key={row.key} className={styles.row} data-testid="automation-row" data-kind={row.kind}>
                  <Icon className={styles.icon} aria-hidden="true" />
                  <div className={styles.body}>
                    <button
                      type="button"
                      className={styles.open}
                      title={s ? t("automation.openSchedule") : t("automation.openRule")}
                      onClick={() => {
                        if (s) openSchedule(s);
                        else if (row.trigger) {
                          const tr = row.trigger;
                          openBuilder({ draft: { ...triggerInput(tr), enabled: tr.origin === "rule" ? tr.enabled : false }, editing: tr.origin === "webhook" ? null : tr.id, adopting: tr.origin !== "rule", lastError: tr.last_error });
                        }
                      }}
                    >
                      <span className={styles.name}>{row.name}</span>
                      <span className={styles.sub}>{what}</span>
                      <span className={styles.state} data-tone={tone}>{state}</span>
                    </button>
                    {retryDue !== null && s ? (
                      <button
                        type="button"
                        className={styles.rowAction}
                        disabled={busy !== null}
                        data-testid="automation-retry"
                        onClick={() => void act(row.key, () => client!.retryScheduleOccurrence(s.id, retryDue), t("schedules.retryStarted", { name: s.name }))}
                      >
                        <RotateCcw aria-hidden="true" />
                        {last?.tone === "missed" ? t("automation.runMissed") : t("automation.retry")}
                      </button>
                    ) : null}
                    {confirmableAuthor(row, botIsFull) ? (
                      <div className={styles.confirm} data-testid="automation-confirm-row">
                        <span className={styles.muted}>{t("automation.confirm.line")}</span>
                        <button
                          type="button"
                          className={styles.rowAction}
                          disabled={busy !== null}
                          data-testid="automation-confirm"
                          aria-label={t("automation.confirm.label", { name: row.name })}
                          onClick={() =>
                            void act(
                              row.key,
                              () => (s ? client!.confirmSchedule(s.id, s.revision) : client!.confirmTrigger(botId, row.trigger!.id, row.trigger!.revision)),
                              t("automation.confirm.done", { name: row.name })
                            )
                          }
                        >
                          <ShieldCheck aria-hidden="true" />
                          {t("automation.confirm.action")}
                        </button>
                      </div>
                    ) : null}
                  </div>
                  {s && !s.moved_to ? (
                    <Switch
                      checked={s.enabled}
                      disabled={busy !== null}
                      label={t("automation.toggle", { name: row.name })}
                      onChange={() => void act(row.key, () => client!.updateSchedule(s.id, toggleInput(s)))}
                    />
                  ) : rule ? (
                    <Switch
                      checked={rule.enabled}
                      disabled={busy !== null}
                      label={t("automation.toggle", { name: row.name })}
                      onChange={(enabled) => void act(row.key, () => client!.updateTrigger(botId, rule.id, { ...triggerInput(rule), enabled }))}
                    />
                  ) : null}
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className={styles.group}>
        <h4 className={styles.label}>{t("automation.events.title")}</h4>
        <p className={styles.muted}>{t("automation.events.intro")}</p>
        <ul className={styles.kinds}>
          {EVENT_CARDS.map((kind) => {
            const Icon = ICONS[kind];
            return (
              <li key={kind}>
                <button
                  type="button"
                  className={styles.kind}
                  data-testid={`automation-add-${kind}`}
                  aria-expanded={builder !== null && !builder.editing && builder.draft.config.source.kind === kind}
                  onClick={() => {
                    const draft = newTrigger();
                    openBuilder({
                      draft: kind === "mail" ? draft : { ...draft, config: { ...draft.config, source: newSource(kind) } },
                      editing: null,
                      adopting: false
                    });
                  }}
                >
                  <Icon aria-hidden="true" />
                  <span className={styles.kindText}>
                    <b>{t(`automation.events.${kind}.title`)}</b>
                    <span>{t(`automation.events.${kind}.line`)}</span>
                  </span>
                  <ChevronRight aria-hidden="true" />
                </button>
              </li>
            );
          })}
          <li className={styles.kind} data-static="true" data-testid="automation-file">
            <FolderOpen aria-hidden="true" />
            <span className={styles.kindText}>
              <b>{t("automation.events.file.title")}</b>
              <span>{t("automation.events.file.line")}</span>
            </span>
          </li>
        </ul>
        {builder ? (
          <TriggerBuilder
            key={builder.key}
            botId={botId}
            start={builder}
            onClose={() => setBuilder(null)}
            onSaved={async () => {
              setBuilder(null);
              await load();
            }}
          />
        ) : null}
      </div>

      <div className={styles.mission} data-testid="automation-missions">
        <Rocket aria-hidden="true" />
        <div className={styles.kindText}>
          <b>{t("automation.missions.title")}</b>
          <span>{t("automation.missions.line")}</span>
          <button type="button" className={styles.link} onClick={onOpenMissions} data-testid="automation-open-missions">
            {t("automation.missions.action")}
          </button>
        </div>
      </div>

      <details className={styles.more}>
        <summary>
          <ChevronRight aria-hidden="true" />
          {t("botPanel.triggers.manageHooks")}
        </summary>
        <p className={styles.muted}>{t("automation.hooksLine")}</p>
        {visible ? <BotPanelWebhooks botId={botId} /> : null}
      </details>

      {onOpenSchedules ? (
        <button type="button" className={styles.allLink} data-testid="bot-panel-schedules" onClick={onOpenSchedules}>
          <CalendarClock aria-hidden="true" />
          {t("automation.allSchedules")}
          <ChevronRight aria-hidden="true" />
        </button>
      ) : null}
    </section>
  );
}

/** One sentence → a schedule. Shows what it understood before anything is saved. */
function QuickCreate({ botId, onCreated, onDetails }: { botId: string; onCreated: () => Promise<void>; onDetails: (text: string) => void }) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const id = useId();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const typed = text.trim();
  const parsed = typed ? parseAutomationSentence(typed) : null;
  const spec = parsed?.simple ? simpleSpec(parsed.simple, LOCAL_TZ) : null;
  const problem = parsed?.simple ? simpleProblem(parsed.simple, LOCAL_TZ, Date.now()) : null;
  const label = spec ? localSpecLabel(spec, (ms) => formatWhen(ms, i18n.language), i18n.language, LOCAL_TZ) : null;
  const ready = Boolean(parsed?.simple && spec && !problem && parsed.task);
  const hint = !parsed
    ? null
    : !parsed.simple
      ? t("automation.quick.noWhen")
      : !parsed.task
        ? t("automation.quick.noTask")
        : problem
          ? t(`schedules.form.simpleProblem.${problem}`)
          : null;

  const create = async () => {
    if (!client || !parsed?.simple || !spec || !ready) return;
    setBusy(true);
    try {
      const name = nameFromTask(parsed.task, i18n.language);
      const draft = { ...emptyDraft(botId), name, simple: parsed.simple, prompt: parsed.task, tz: LOCAL_TZ };
      const saved = await client.createSchedule(buildInput(draft, spec));
      writeScheduleNotify(saved.id, suggestScheduleNotify(parsed.task, parsed.simple.freq === "once"));
      setText("");
      toast.show(t("automation.quick.created", { name }), "success");
      await onCreated();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className={styles.quick}
      data-testid="automation-quick"
      onSubmit={(e) => {
        e.preventDefault();
        void create();
      }}
    >
      <label className={styles.quickLabel} htmlFor={`${id}-text`}>
        <Sparkles aria-hidden="true" />
        {t("automation.quick.label")}
      </label>
      <textarea
        id={`${id}-text`}
        className={styles.quickInput}
        rows={2}
        maxLength={4000}
        value={text}
        placeholder={t("automation.quick.placeholder")}
        aria-describedby={`${id}-read`}
        data-testid="automation-quick-text"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            void create();
          }
        }}
      />
      <div id={`${id}-read`} aria-live="polite">
        {parsed && label && parsed.task && !problem ? (
          <dl className={styles.read} data-testid="automation-quick-read">
            <div>
              <dt>{t("automation.quick.when")}</dt>
              <dd>{t(label.i18nKey, label.values)}</dd>
            </div>
            <div>
              <dt>{t("automation.quick.what")}</dt>
              <dd>{parsed.task}</dd>
            </div>
          </dl>
        ) : hint ? (
          <p className={styles.muted}>{hint}</p>
        ) : (
          <p className={styles.muted}>{t("automation.quick.help")}</p>
        )}
      </div>
      <div className={styles.quickActions}>
        <button type="submit" className={styles.primary} disabled={!ready || busy} data-testid="automation-quick-create">
          {t("automation.quick.create")}
        </button>
        <button type="button" className={styles.link} onClick={() => onDetails(typed)}>
          {t("automation.quick.details")}
        </button>
      </div>
    </form>
  );
}
