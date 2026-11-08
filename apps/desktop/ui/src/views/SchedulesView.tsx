import { ArrowUpRight, CalendarDays, PenLine, Play, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import { ApiError } from "../api/client";
import type { ParsedSchedule, Schedule } from "../api/types";
import formStyles from "../components/Form.module.css";
import { botView } from "../lib/botProfile";
import {
  affectsSchedules,
  buildInput,
  draftFromSchedule,
  emptyDraft,
  MISFIRE_POLICIES,
  parsePayload,
  runOutcome,
  runsOfSchedule,
  type ScheduleDraft,
  scheduleStatus,
  type ScheduleKind,
  sortSchedules,
  specLabel,
  toggleInput,
  validateDraft
} from "../lib/schedules";
import { useDaemon } from "../state/DaemonContext";
import styles from "./SchedulesView.module.css";

function errorText(e: unknown): string {
  return e instanceof ApiError || e instanceof Error ? e.message : String(e);
}

const LOCAL_TZ = Intl.DateTimeFormat().resolvedOptions().timeZone;

/** Scheduled tasks: list, create / edit, enable, run now, delete, recent runs. */
export function SchedulesView({
  onOpenChat
}: {
  onOpenChat: (chatId: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, events, onDaemonEvent } = useDaemon();
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
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

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setSchedules(await client.listSchedules());
      setError(null);
    } catch (e) {
      setError(errorText(e));
      setSchedules((cur) => cur ?? []);
    }
  }, [client]);

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
            <Button size="sm" variant="ghost" onClick={() => void load()}>
              <RefreshCw />{t("schedules.refresh")}
            </Button>
            <Button
              size="sm"
              variant="primary"
              disabled={bots.length === 0}
              onClick={() => setEditing("new")}
            >
              <Plus />{t("schedules.new")}
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
          <Card padding="md" className={styles.empty}><CalendarDays className={formStyles.emptyIcon} />
            <p className={styles.emptyTitle}>{t("schedules.empty.title")}</p>
            <p className={formStyles.hint}>
              {bots.length === 0
                ? t("schedules.empty.noBots")
                : t("schedules.empty.hint")}
            </p>
          </Card>
        ) : (
          <ul className={styles.list} aria-label={t("schedules.title")}>
            {list.map((s) => {
              const bot = bots.find((b) => b.id === s.bot_id);
              const view = bot ? botView(bot) : null;
              const status = scheduleStatus(s);
              const label = specLabel(s.spec, fmt);
              const payload = parsePayload(s.payload);
              const runs = runsOfSchedule(events.runs, s.id);
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
                        <dd>{view?.name ?? t("schedules.deletedBot")}</dd>
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
                    </dl>

                    <div className={styles.actions}>
                      <Button
                        size="sm"
                        variant="primary"
                        disabled={busy}
                        onClick={() =>
                          void act(
                            s.id,
                            () => client!.runScheduleNow(s.id),
                            t("schedules.runNowStarted", { name: s.name })
                          )
                        }
                      >
                        <Play />{t("schedules.runNow")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy}
                        onClick={() => setEditing(s.id)}
                      >
                        <PenLine />{t("schedules.edit")}
                      </Button>
                      {confirmDeleteId === s.id ? (
                        <>
                          <Button
                            size="sm"
                            variant="danger"
                            disabled={busy}
                            onClick={() => {
                              setConfirmDeleteId(null);
                              void act(s.id, () => client!.deleteSchedule(s.id));
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
                          <Trash2 />{t("schedules.delete")}
                        </Button>
                      )}
                    </div>

                    <div className={styles.runs}>
                      <h3 className={styles.runsTitle}>
                        {t("schedules.recentRuns")}
                      </h3>
                      {runs.length === 0 ? (
                        <p className={formStyles.hint}>
                          {t("schedules.noRuns")}
                        </p>
                      ) : (
                        <ul className={styles.runList}>
                          {runs.map((r) => {
                            const outcome = runOutcome(r);
                            return (
                              <li key={r.id} className={styles.run}>
                                <span
                                  className={`${styles.badge} ${styles[outcome]}`}
                                >
                                  {t(`schedules.run.${outcome}`)}
                                </span>
                                <span className={formStyles.hint}>
                                  {fmt(r.startedMs)}
                                </span>
                                <Button
                                  size="sm"
                                  variant="ghost"
                                  disabled={!r.chatId}
                                  onClick={() => onOpenChat(r.chatId)}
                                >
                                  <ArrowUpRight />{t("schedules.openRun")}
                                </Button>
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
    editing ? draftFromSchedule(editing) : emptyDraft(bots[0]?.id ?? "")
  );
  const [preview, setPreview] = useState<{
    text: string;
    result: ParsedSchedule;
  } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const patch = (p: Partial<ScheduleDraft>) =>
    setDraft((d) => ({ ...d, ...p }));
  const keepsPayload =
    editing !== undefined && parsePayload(editing.payload).kind !== "agent_prompt";

  const fmt = (ms: number) =>
    new Date(ms).toLocaleString(i18n.language, {
      dateStyle: "medium",
      timeStyle: "short"
    });

  const runPreview = async (): Promise<ParsedSchedule | null> => {
    if (!client || draft.text.trim() === "") {
      return null;
    }
    const text = draft.text.trim();
    if (preview && preview.text === text) {
      return preview.result;
    }
    try {
      const result = await client.parseSchedule(text, LOCAL_TZ);
      setPreview({ text, result });
      setProblem(null);
      return result;
    } catch (e) {
      setPreview(null);
      setProblem(
        e instanceof ApiError && e.code === "bad_schedule"
          ? t("schedules.form.cannotParse")
          : errorText(e)
      );
      return null;
    }
  };

  const submit = async () => {
    const invalid = validateDraft(draft, editing);
    if (invalid) {
      setProblem(t(`schedules.form.invalid.${invalid}`));
      return;
    }
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      let spec: unknown = null;
      if (draft.kind === "natural") {
        const parsed = await runPreview();
        if (!parsed) {
          return;
        }
        spec = parsed.spec;
      }
      const input = buildInput(draft, spec, editing);
      if (editing) {
        await client.updateSchedule(editing.id, input);
      } else {
        await client.createSchedule(input);
      }
      await onSaved();
    } catch (e) {
      setProblem(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const kinds: ScheduleKind[] = editing
    ? ["keep", "natural", "cron"]
    : ["natural", "cron"];
  const heading = editing ? t("schedules.form.editTitle") : t("schedules.form.newTitle");

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
            <span className={formStyles.hint}>{t("schedules.form.botFixed")}</span>
          ) : null}
        </label>

        <fieldset className={styles.fieldset}>
          <legend className={formStyles.label}>
            {t("schedules.form.when")}
          </legend>
          <div className={styles.kindRow} role="radiogroup">
            {kinds.map((k) => (
              <label key={k} className={styles.radio}>
                <input
                  type="radio"
                  name="schedule-kind"
                  checked={draft.kind === k}
                  onChange={() => patch({ kind: k })}
                />
                <span>{t(`schedules.form.kind.${k}`)}</span>
              </label>
            ))}
          </div>

          {draft.kind === "keep" && editing ? (
            <p className={formStyles.hint}>
              {(() => {
                const l = specLabel(editing.spec, fmt);
                return t("schedules.form.keepHint", {
                  spec: t(l.i18nKey, l.values)
                });
              })()}
            </p>
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
              {preview && preview.text === draft.text.trim() ? (
                <p
                  className={formStyles.success}
                  role="status"
                  data-testid="schedule-preview"
                >
                  {(() => {
                    const l = specLabel(preview.result.spec, fmt);
                    const next = preview.result.next_run_at_ms;
                    return `${t(l.i18nKey, l.values)} · ${
                      next !== null && next !== undefined
                        ? t("schedules.form.firstRun", { when: fmt(next) })
                        : t("schedules.form.noFirstRun")
                    }`;
                  })()}
                </p>
              ) : (
                <span className={formStyles.hint}>
                  {t("schedules.form.textHint", { tz: LOCAL_TZ })}
                </span>
              )}
            </div>
          ) : null}

          {draft.kind === "cron" ? (
            <label className={formStyles.field}>
              <input
                className={formStyles.input}
                value={draft.cron}
                placeholder="0 9 * * 1-5 Europe/Istanbul"
                aria-label={t("schedules.form.cron")}
                onChange={(e) => patch({ cron: e.target.value })}
              />
              <span className={formStyles.hint}>
                {t("schedules.form.cronHint")}
              </span>
            </label>
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

        <label className={formStyles.field}>
          <span className={formStyles.label}>
            {t("schedules.form.misfire")}
          </span>
          <select
            className={formStyles.select}
            value={draft.misfire}
            onChange={(e) =>
              patch({ misfire: e.target.value as ScheduleDraft["misfire"] })
            }
          >
            {MISFIRE_POLICIES.map((m) => (
              <option key={m} value={m}>
                {t(`schedules.form.misfirePolicy.${m}`)}
              </option>
            ))}
          </select>
        </label>

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
