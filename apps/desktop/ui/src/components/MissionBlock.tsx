import { CircleCheck, Clock, DoorOpen, Hourglass, Pause, Play, Rocket, ShieldAlert, Square, TriangleAlert } from "lucide-react";
import { type CSSProperties, useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { type ApiClient, ApiError } from "../api/client";
import type { Mission, MissionDetail, MissionDigest, MissionItem } from "../api/types";
import { humanLine } from "../lib/humanError";
import {
  type AnswerDraft,
  buildAnswers,
  currentItem,
  isActiveMission,
  itemLabel,
  MISSION_POLL_MS,
  missionChip,
  missionCounts,
  missionHeadline,
  missionNote,
  missionsToShow,
  type MissionChip,
  progressPercent,
  shouldPoll,
  waitingItems,
  waitLine
} from "../lib/missions";
import { useDaemon } from "../state/DaemonContext";
import styles from "./MissionBlock.module.css";

export interface MissionView {
  mission: Mission;
  /** The ledger's blocked and waiting items and digest; fetched for missions that have blocked or waiting items or ended. */
  detail: MissionDetail | null;
}

export const CHIP_ICONS: Record<MissionChip, typeof Rocket> = {
  running: Rocket,
  waiting: Hourglass,
  paused: Pause,
  needsYou: ShieldAlert,
  done: CircleCheck,
  failed: TriangleAlert,
  stopped: Square
};

/** A 400 carries the daemon's own explanation (the bot is not on the Full level); the rest is humanised. */
function errorText(e: unknown): string {
  return e instanceof ApiError && e.status === 400 ? e.message : humanLine(e);
}

/**
 * The bot's missions, from the daemon. There is no event for them, so it asks again every few
 * seconds while one can still change and the Activity tab is on screen, and after every owner action.
 */
function useMissions(botId: string, visible: boolean) {
  const { client } = useDaemon();
  const [views, setViews] = useState<MissionView[]>([]);
  const version = useRef(0);
  const active = useRef<Mission[]>([]);
  const ended = useRef<Mission | null>(null);
  const details = useRef(new Map<string, { at: number; detail: MissionDetail }>());

  /** `full` also reads the finished missions; a poll only asks for the active ones. */
  const load = useCallback(
    async (full: boolean) => {
      if (!client) {
        return;
      }
      const ticket = ++version.current;
      try {
        let all = await client.listMissions(botId, full ? undefined : "active");
        let readAll = full;
        if (!full && active.current.some((m) => !all.some((x) => x.id === m.id))) {
          // One that was active is gone from the active list: it ended, read what it left behind.
          all = await client.listMissions(botId);
          readAll = true;
        }
        const shown = readAll ? missionsToShow(all) : [...missionsToShow(all), ...(ended.current ? [ended.current] : [])];
        const next = await Promise.all(
          shown.map(async (mission): Promise<MissionView> => {
            // A running mission's ledger says what it works on now; a quiet paused one needs nothing.
            if (mission.progress.blocked === 0 && mission.progress.waiting === 0 && mission.state === "paused") {
              return { mission, detail: null };
            }
            const hit = details.current.get(mission.id);
            if (hit && hit.at === mission.updated_at_ms) {
              return { mission, detail: hit.detail };
            }
            try {
              const detail = await client.getMission(mission.id);
              details.current.set(mission.id, { at: mission.updated_at_ms, detail });
              return { mission, detail };
            } catch {
              return { mission, detail: null };
            }
          })
        );
        if (ticket !== version.current) {
          return;
        }
        active.current = shown.filter(isActiveMission);
        if (readAll) {
          ended.current = shown.find((m) => !isActiveMission(m)) ?? null;
        }
        setViews(next);
      } catch {
        // the panel simply has nothing to say; the next poll or action tries again
      }
    },
    [client, botId]
  );
  useEffect(() => {
    void load(true);
  }, [load]);
  const polling = shouldPoll(
    views.map((v) => v.mission),
    visible
  );
  useEffect(() => {
    if (!polling) {
      return;
    }
    const timer = window.setInterval(() => void load(false), MISSION_POLL_MS);
    return () => window.clearInterval(timer);
  }, [polling, load]);
  /** After an owner action: forget the cached ledgers and read everything again. */
  const refresh = useCallback(async () => {
    details.current.clear();
    await load(true);
  }, [load]);
  return { views, refresh };
}

/** "Missions": long jobs the bot works through alone; their progress, what needs the owner, the report. */
export function MissionBlock({ botId, visible = true }: { botId: string; visible?: boolean }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const { views, refresh } = useMissions(botId, visible);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const hasActive = views.some((v) => isActiveMission(v.mission));
  /** Run one owner action, then read the missions again. */
  const act = async (run: (c: ApiClient) => Promise<unknown>): Promise<boolean> => {
    if (!client || busy) {
      return false;
    }
    setBusy(true);
    setError(null);
    try {
      await run(client);
      await refresh();
      return true;
    } catch (e) {
      setError(errorText(e));
      return false;
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={styles.block} aria-labelledby="bot-panel-missions" data-testid="bot-panel-missions">
      <h3 className={styles.label} id="bot-panel-missions">
        {t("missions.title")}
      </h3>
      {!hasActive ? (
        <StartForm
          busy={busy}
          onStart={(goal) => act((c) => c.startMission({ bot_id: botId, goal }))}
        />
      ) : null}
      {error ? (
        <p className={styles.error} role="alert" data-testid="mission-error">
          {error}
        </p>
      ) : null}
      {views.map((v) => (
        <MissionCard
          key={v.mission.id}
          view={v}
          busy={busy}
          onSteer={(action) => act((c) => c.steerMission(v.mission.id, action))}
          onAnswer={(answers) => act((c) => c.answerMission(v.mission.id, answers))}
        />
      ))}
    </section>
  );
}

function StartForm({ busy, onStart }: { busy: boolean; onStart: (goal: string) => Promise<boolean> }) {
  const { t } = useTranslation();
  const [goal, setGoal] = useState("");
  const id = useId();
  return (
    <details className={styles.start} data-testid="mission-start">
      <summary className={styles.startSummary}>
        <Rocket aria-hidden="true" />
        {t("missions.start.summary")}
      </summary>
      <form
        className={styles.startForm}
        onSubmit={(e) => {
          e.preventDefault();
          void onStart(goal.trim()).then((started) => started && setGoal(""));
        }}
      >
        <label className={styles.field} htmlFor={`${id}-goal`}>
          {t("missions.start.goalLabel")}
        </label>
        <p className={styles.hint} id={`${id}-help`}>
          {t("missions.start.help")}
        </p>
        <textarea
          id={`${id}-goal`}
          className={styles.textarea}
          rows={4}
          maxLength={4000}
          value={goal}
          placeholder={t("missions.start.placeholder")}
          aria-describedby={`${id}-help`}
          onChange={(e) => setGoal(e.target.value)}
        />
        <button type="submit" className={styles.primary} disabled={busy || goal.trim() === ""}>
          <Rocket aria-hidden="true" />
          {busy ? t("missions.start.starting") : t("missions.start.button")}
        </button>
      </form>
    </details>
  );
}

function MissionCard({
  view,
  busy,
  onSteer,
  onAnswer
}: {
  view: MissionView;
  busy: boolean;
  onSteer: (action: "pause" | "resume" | "cancel") => Promise<boolean>;
  onAnswer: (answers: ReturnType<typeof buildAnswers>["answers"]) => Promise<boolean>;
}) {
  const { t, i18n } = useTranslation();
  const { mission: m, detail } = view;
  const titleId = useId();
  const [confirmStop, setConfirmStop] = useState(false);
  const chip = missionChip(m.state);
  const ChipIcon = CHIP_ICONS[chip];
  const live = isActiveMission(m);
  const blocked = detail?.blocked ?? [];
  const waiting = waitingItems(detail?.items ?? []);
  const when = (ms: number) => new Date(ms).toLocaleString(i18n.language, { dateStyle: "medium", timeStyle: "short" });
  return (
    <article className={styles.card} aria-labelledby={titleId} data-testid="mission-card" data-state={m.state}>
      <header className={styles.head}>
        <h4 className={styles.title} id={titleId} title={m.goal}>
          {m.title}
        </h4>
        <span className={styles.chip} data-chip={chip} data-testid="mission-chip">
          <ChipIcon aria-hidden="true" />
          {t(`missions.chip.${chip}`)}
        </span>
      </header>
      <MissionProgress mission={m} />
      <MissionNow mission={m} items={detail?.items ?? []} />
      <p className={styles.meta}>
        {[
          t("missions.sessions", { count: m.sessions }),
          t("missions.modelCalls", { count: m.model_steps }),
          !live && m.finished_at_ms ? t("missions.endedAt", { when: when(m.finished_at_ms) }) : null
        ]
          .filter(Boolean)
          .join(" · ")}
      </p>
      {m.state_reason && m.state !== "running" ? (
        <p className={styles.reason} data-testid="mission-reason">
          {m.state_reason}
        </p>
      ) : null}
      {live ? (
        <div className={styles.actions}>
          {m.state === "paused" ? (
            <button type="button" className={styles.secondary} disabled={busy} onClick={() => void onSteer("resume")}>
              <Play aria-hidden="true" />
              {t("missions.resume")}
            </button>
          ) : m.state === "running" || m.state === "waiting" ? (
            <button type="button" className={styles.secondary} disabled={busy} onClick={() => void onSteer("pause")}>
              <Pause aria-hidden="true" />
              {t("missions.pause")}
            </button>
          ) : null}
          {confirmStop ? (
            <span className={styles.confirm} role="group" aria-label={t("missions.stopConfirm")}>
              <span>{t("missions.stopConfirm")}</span>
              <button
                type="button"
                className={styles.danger}
                disabled={busy}
                onClick={() => void onSteer("cancel").then(() => setConfirmStop(false))}
              >
                {t("missions.stopYes")}
              </button>
              <button type="button" className={styles.secondary} onClick={() => setConfirmStop(false)}>
                {t("missions.stopNo")}
              </button>
            </span>
          ) : (
            <button type="button" className={styles.secondary} disabled={busy} onClick={() => setConfirmStop(true)}>
              <Square aria-hidden="true" />
              {t("missions.stop")}
            </button>
          )}
        </div>
      ) : null}
      {live && waiting.length > 0 ? <WaitingFor items={waiting} /> : null}
      {live && blocked.length > 0 ? <NeedsYou items={blocked} busy={busy} onSend={onAnswer} /> : null}
      {!live && detail?.digest ? <Digest digest={detail.digest} /> : null}
    </article>
  );
}

/** The bar and one line of numbers: "7 of 15 done · 2 need you · 6 left" (shared with the chat header). */
export function MissionProgress({ mission: m }: { mission: Mission }) {
  const { t } = useTranslation();
  const counts = missionCounts(m.progress);
  const pct = progressPercent(m.progress);
  const headline = missionHeadline(m);
  const pctOf = (n: number) => (m.progress.total > 0 ? (n / m.progress.total) * 100 : 0);
  return (
    <>
      <p className={styles.headline}>{t(headline.key, headline.values)}</p>
      <div
        className={styles.bar}
        role="progressbar"
        aria-label={t("missions.progressLabel", { done: counts.done, total: m.progress.total })}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
      >
        <span className={styles.barDone} style={{ "--share": pctOf(counts.done) } as CSSProperties} />
        <span className={styles.barWaiting} style={{ "--share": pctOf(counts.waiting) } as CSSProperties} />
        <span className={styles.barBlocked} style={{ "--share": pctOf(counts.blocked) } as CSSProperties} />
      </div>
      <dl className={styles.counts}>
        <div className={styles.count} data-kind="done">
          <dt>{t("missions.counts.done")}</dt>
          <dd>{counts.done}</dd>
        </div>
        {counts.waiting > 0 ? (
          <div className={styles.count} data-kind="waiting">
            <dt>{t("missions.counts.waiting")}</dt>
            <dd>{counts.waiting}</dd>
          </div>
        ) : null}
        {counts.blocked > 0 ? (
          <div className={styles.count} data-kind="blocked">
            <dt>{t("missions.counts.blocked")}</dt>
            <dd>{counts.blocked}</dd>
          </div>
        ) : null}
        {counts.left > 0 || isActiveMission(m) ? (
          <div className={styles.count} data-kind="left">
            <dt>{t("missions.counts.left")}</dt>
            <dd>{counts.left}</dd>
          </div>
        ) : null}
      </dl>
    </>
  );
}

/**
 * "Now: Register on india.dev", then the one sentence on what the owner can do: leave it alone
 * while it runs (it writes in its chat when done or stuck), answer while it waits, resume if paused.
 */
export function MissionNow({ mission: m, items }: { mission: Mission; items: readonly MissionItem[] }) {
  const { t } = useTranslation();
  const doing = m.state === "running" ? currentItem(items) : null;
  const note = missionNote(m);
  if (!doing && !note) {
    return null;
  }
  return (
    <>
      {doing ? (
        <p className={styles.now} data-testid="mission-now">
          <span className={styles.nowDot} aria-hidden="true" />
          <span>{t("missions.now", { item: itemLabel(doing) })}</span>
        </p>
      ) : null}
      {note ? (
        <p className={styles.note} data-note={note} data-testid="mission-note">
          {note === "leave" ? <DoorOpen aria-hidden="true" /> : note === "answer" ? <ShieldAlert aria-hidden="true" /> : <Pause aria-hidden="true" />}
          <span>{t(`missions.note.${note}`)}</span>
        </p>
      ) : null}
    </>
  );
}

/**
 * The items parked on a wake: "waiting for mail from example.com", "waiting until 14:00". The bot's
 * own reason is shown as plain text; nothing here can be answered, the runtime wakes the item.
 */
function WaitingFor({ items }: { items: readonly MissionItem[] }) {
  const { t, i18n } = useTranslation();
  const id = useId();
  const now = Date.now();
  return (
    <section className={styles.waits} aria-labelledby={`${id}-title`} data-testid="mission-waiting">
      <h4 className={styles.waitsTitle} id={`${id}-title`}>
        <Hourglass aria-hidden="true" />
        {t("missions.waiting.title", { count: items.length })}
      </h4>
      <ul className={styles.needsList}>
        {items.map((it) => (
          <li key={it.key} className={styles.needsItem} data-testid="mission-waiting-item">
            <p className={styles.itemTitle}>
              <b>{it.key}</b>
              {it.title && it.title !== it.key ? <span> · {it.title}</span> : null}
            </p>
            {it.waiting_for.map((w) => {
              const line = waitLine(w, now, i18n.language);
              return (
                <p key={`${w.kind}-${w.due_at_ms}`} className={styles.itemReason} data-testid="mission-wait-line">
                  {t(line.key, line.values)}
                  {w.kind !== "after" ? <span className={styles.hint}> {t("missions.waiting.giveUp", { when: line.values.when })}</span> : null}
                </p>
              );
            })}
            {it.waiting_for[0]?.reason ? <p className={styles.hint}>{it.waiting_for[0].reason}</p> : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

/** ONE list for every item parked on the owner, and ONE button that answers them all. */
export function NeedsYou({
  items,
  busy,
  onSend,
  heading: Heading = "h4"
}: {
  items: readonly MissionItem[];
  busy: boolean;
  /** The title's level where it sits (h4 in the panel's card, h3 under the chat header's h2). */
  heading?: "h3" | "h4";
  onSend: (answers: ReturnType<typeof buildAnswers>["answers"]) => Promise<boolean>;
}) {
  const { t } = useTranslation();
  const id = useId();
  const [drafts, setDrafts] = useState<Record<string, AnswerDraft>>({});
  const { answers, unanswered, canSend } = buildAnswers(items, drafts);
  const touch = (key: string, patch: Partial<AnswerDraft>) =>
    setDrafts((d) => ({ ...d, [key]: { ...(d[key] ?? { text: "", skip: false }), ...patch } }));
  return (
    <form
      className={styles.needs}
      aria-labelledby={`${id}-title`}
      data-testid="mission-needs-you"
      onSubmit={(e) => {
        e.preventDefault();
        if (canSend) {
          void onSend(answers).then((sent) => sent && setDrafts({}));
        }
      }}
    >
      <Heading className={styles.needsTitle} id={`${id}-title`}>
        <ShieldAlert aria-hidden="true" />
        {t("missions.needsYou.title", { count: items.length })}
      </Heading>
      <p className={styles.hint}>{t("missions.needsYou.intro")}</p>
      <ul className={styles.needsList}>
        {items.map((it) => {
          const draft = drafts[it.key];
          const missing = unanswered.includes(it.key);
          return (
            <li key={it.key} className={styles.needsItem} data-testid="mission-blocked-item">
              <p className={styles.itemTitle}>
                <b>{itemLabel(it)}</b>
              </p>
              {it.blocked_reason ? <p className={styles.itemReason}>{it.blocked_reason}</p> : null}
              {it.needs_live_owner ? (
                // A code is never taken as an answer: nothing to type here, only to skip.
                <p className={styles.hint}>{t("missions.needsYou.liveOnly")}</p>
              ) : (
                <textarea
                  className={styles.textarea}
                  rows={2}
                  maxLength={2000}
                  value={draft?.text ?? ""}
                  disabled={draft?.skip === true}
                  placeholder={t("missions.needsYou.placeholder")}
                  aria-label={t("missions.needsYou.answerFor", { key: it.key })}
                  aria-invalid={missing}
                  aria-describedby={missing ? `${id}-${it.key}-missing` : undefined}
                  onChange={(e) => touch(it.key, { text: e.target.value })}
                />
              )}
              <label className={styles.skip}>
                <input
                  type="checkbox"
                  checked={draft?.skip === true}
                  onChange={(e) => touch(it.key, { skip: e.target.checked })}
                />
                {t("missions.needsYou.skip")}
              </label>
              {missing ? (
                <p className={styles.error} id={`${id}-${it.key}-missing`}>
                  {t("missions.needsYou.missing")}
                </p>
              ) : null}
            </li>
          );
        })}
      </ul>
      <button type="submit" className={styles.primary} disabled={busy || !canSend} data-testid="mission-send-answers">
        {busy
          ? t("missions.needsYou.sending")
          : answers.length > 0
            ? t("missions.needsYou.sendCount", { count: answers.length })
            : t("missions.needsYou.send")}
      </button>
    </form>
  );
}

function DigestList({ title, rows, heading: Heading }: { title: string; rows: MissionDigest["blocked"]; heading: "h3" | "h5" }) {
  if (rows.length === 0) {
    return null;
  }
  return (
    <>
      <Heading className={styles.digestHead}>{title}</Heading>
      <ul className={styles.digestList}>
        {rows.map((r) => (
          <li key={r.key}>
            <b>{r.key}</b>
            {r.text ? <span> · {r.text}</span> : null}
          </li>
        ))}
      </ul>
    </>
  );
}

/** What the bot reports when a mission ends: the counts, what is stuck, what it gave up on, what it decided. */
export function Digest({ digest, bare = false }: { digest: MissionDigest; bare?: boolean }) {
  const { t, i18n } = useTranslation();
  // Under the panel card's h4, or right under the chat header's h2 when bare.
  const heading = bare ? "h3" : "h5";
  const Heading = heading;
  const p = digest.progress;
  const content = (
    <div className={styles.digestBody}>
      <p className={styles.meta}>
        {new Date(digest.at_ms).toLocaleString(i18n.language, { dateStyle: "medium", timeStyle: "short" })}
      </p>
      <p>{t("missions.digest.counts", { done: p.done, blocked: p.blocked, failed: p.failed, total: p.total })}</p>
      {digest.reason ? <p>{digest.reason}</p> : null}
      <DigestList title={t("missions.digest.blocked")} rows={digest.blocked} heading={heading} />
      <DigestList title={t("missions.digest.failed")} rows={digest.failed} heading={heading} />
      <DigestList title={t("missions.digest.done")} rows={digest.done_sample} heading={heading} />
      {digest.decisions.length > 0 ? (
        <>
          <Heading className={styles.digestHead}>{t("missions.digest.decisions")}</Heading>
          <ul className={styles.digestList}>
            {digest.decisions.map((d) => (
              <li key={d}>{d}</li>
            ))}
          </ul>
        </>
      ) : null}
    </div>
  );
  if (bare) {
    return <div data-testid="mission-digest">{content}</div>;
  }
  return (
    <details className={styles.digest} data-testid="mission-digest">
      <summary className={styles.digestSummary}>
        <Clock aria-hidden="true" />
        {t("missions.digest.summary")}
      </summary>
      {content}
    </details>
  );
}
