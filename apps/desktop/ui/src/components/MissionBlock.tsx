import { CircleCheck, Clock, DoorOpen, Hourglass, Pause, Play, Rocket, ShieldAlert, Square, TriangleAlert } from "lucide-react";
import { type CSSProperties, useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
import type { Mission, MissionDigest, MissionItem } from "../api/types";
import { humanLine } from "../lib/humanError";
import { requestOpenChat } from "../lib/openChatLink";
import { missionCommandWord, requestCompose } from "../lib/slash";
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
  progressShort,
  shouldPoll,
  waitLine
} from "../lib/missions";
import { useDaemon } from "../state/DaemonContext";
import styles from "./MissionBlock.module.css";

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
export function missionErrorText(e: unknown): string {
  return e instanceof ApiError && e.status === 400 ? e.message : humanLine(e);
}

/**
 * The bot's missions, from the daemon: one list request. There is no event for them, so it asks
 * again every few seconds while one can still change and the Activity tab is on screen.
 */
function useMissions(botId: string, visible: boolean): Mission[] {
  const { client } = useDaemon();
  const [missions, setMissions] = useState<Mission[]>([]);
  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setMissions(await client.listMissions(botId));
    } catch {
      // the panel simply has nothing to say; the next poll tries again
    }
  }, [client, botId]);
  useEffect(() => {
    void load();
  }, [load]);
  const polling = shouldPoll(missions, visible);
  useEffect(() => {
    if (!polling) {
      return;
    }
    const timer = window.setInterval(() => void load(), MISSION_POLL_MS);
    return () => window.clearInterval(timer);
  }, [polling, load]);
  return missions;
}

/**
 * "Missions" in the bot panel: one line per mission (title, status chip, "7/15"), each opening the
 * mission's own chat, where its progress, answers, pause/stop and report live. Missions are started
 * from the chat (`/mission`, or by asking), so the panel only lists them.
 */
export function MissionBlock({ botId, visible = true }: { botId: string; visible?: boolean }) {
  const { t, i18n } = useTranslation();
  const shown = missionsToShow(useMissions(botId, visible));
  return (
    <section className={styles.block} aria-labelledby="bot-panel-missions" data-testid="bot-panel-missions">
      <h3 className={styles.label} id="bot-panel-missions">
        {t("missions.title")}
      </h3>
      {shown.length === 0 ? (
        <p className={styles.emptyLine} data-testid="mission-list-empty">
          <Rocket aria-hidden="true" />
          <span>
            {t("missionChat.panelEmpty")}{" "}
            <button type="button" className={styles.link} data-testid="mission-start-in-chat" onClick={() => requestCompose(`/${missionCommandWord(i18n.language)}`)}>
              {t("missionChat.startInChat")}
            </button>
          </span>
        </p>
      ) : (
        <ul className={styles.list} data-testid="mission-list">
          {shown.map((m) => (
            <li key={m.id}>
              <MissionRow mission={m} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** One line: the title, the status chip and "7/15", over a hairline of progress. Opens the mission's chat. */
function MissionRow({ mission: m }: { mission: Mission }) {
  const short = progressShort(m);
  return (
    <button
      type="button"
      className={styles.row}
      data-testid="mission-row"
      data-state={m.state}
      title={m.goal}
      onClick={() => requestOpenChat(m.chat_id)}
    >
      <span className={styles.rowTitle}>{m.title}</span>
      <MissionChipView state={m.state} />
      {short ? <span className={styles.rowCount}>{short}</span> : null}
      <span className={styles.rowBar} style={{ "--share": progressPercent(m.progress) } as CSSProperties} aria-hidden="true" />
    </button>
  );
}

/** The status chip: an icon and one word ("Running", "Needs you"). */
export function MissionChipView({ state }: { state: string }) {
  const { t } = useTranslation();
  const chip = missionChip(state);
  const Icon = CHIP_ICONS[chip];
  return (
    <span className={styles.chip} data-chip={chip} data-testid="mission-chip">
      <Icon aria-hidden="true" />
      {t(`missions.chip.${chip}`)}
    </span>
  );
}

/**
 * Pause or resume, and Stop behind one inline "Stop it?" (stopping ends the mission for good).
 * Nothing for a mission that ended.
 */
export function MissionSteer({
  mission: m,
  busy,
  onSteer
}: {
  mission: Mission;
  busy: boolean;
  onSteer: (action: "pause" | "resume" | "cancel") => Promise<boolean>;
}) {
  const { t } = useTranslation();
  const [confirmStop, setConfirmStop] = useState(false);
  if (!isActiveMission(m)) {
    return null;
  }
  return (
    <>
      {m.state === "paused" ? (
        <button type="button" className={styles.secondary} disabled={busy} data-testid="mission-resume" onClick={() => void onSteer("resume")}>
          <Play aria-hidden="true" />
          {t("missions.resume")}
        </button>
      ) : m.state === "running" || m.state === "waiting" ? (
        <button type="button" className={styles.secondary} disabled={busy} data-testid="mission-pause" onClick={() => void onSteer("pause")}>
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
            data-testid="mission-stop-yes"
            onClick={() => void onSteer("cancel").then(() => setConfirmStop(false))}
          >
            {t("missions.stopYes")}
          </button>
          <button type="button" className={styles.secondary} onClick={() => setConfirmStop(false)}>
            {t("missions.stopNo")}
          </button>
        </span>
      ) : (
        <button type="button" className={styles.secondary} disabled={busy} data-testid="mission-stop" onClick={() => setConfirmStop(true)}>
          <Square aria-hidden="true" />
          {t("missions.stop")}
        </button>
      )}
    </>
  );
}

/** The headline ("7 of 15 done, working on the rest") over the coloured bar. */
export function MissionBar({ mission: m }: { mission: Mission }) {
  const { t } = useTranslation();
  const counts = missionCounts(m.progress);
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
        aria-valuenow={progressPercent(m.progress)}
      >
        <span className={styles.barDone} style={{ "--share": pctOf(counts.done) } as CSSProperties} />
        <span className={styles.barWaiting} style={{ "--share": pctOf(counts.waiting) } as CSSProperties} />
        <span className={styles.barBlocked} style={{ "--share": pctOf(counts.blocked) } as CSSProperties} />
      </div>
    </>
  );
}

/** The bar and one line of numbers: "7 of 15 done · 2 need you · 6 left" (shared with the chat header). */
export function MissionProgress({ mission: m }: { mission: Mission }) {
  const { t } = useTranslation();
  const counts = missionCounts(m.progress);
  return (
    <>
      <MissionBar mission={m} />
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
export function WaitingFor({ items, heading: Heading = "h4" }: { items: readonly MissionItem[]; heading?: "h3" | "h4" }) {
  const { t, i18n } = useTranslation();
  const id = useId();
  const now = Date.now();
  return (
    <section className={styles.waits} aria-labelledby={`${id}-title`} data-testid="mission-waiting">
      <Heading className={styles.waitsTitle} id={`${id}-title`}>
        <Hourglass aria-hidden="true" />
        {t("missions.waiting.title", { count: items.length })}
      </Heading>
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
