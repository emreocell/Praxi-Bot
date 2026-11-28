import { ArrowUpRight, ChevronDown, CircleCheck, Rocket, ShieldAlert, Square, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Mission, MissionDetail } from "../api/types";
import type { DigestOutcome, DigestText } from "../lib/missionDigestText";
import { isActiveMission, MISSION_POLL_MS, missionForChat, waitingItems } from "../lib/missions";
import { requestOpenChat } from "../lib/openChatLink";
import { useDaemon } from "../state/DaemonContext";
import { Digest, MissionBar, MissionChipView, missionErrorText, MissionNow, MissionProgress, MissionSteer, NeedsYou, WaitingFor } from "./MissionBlock";
import blockStyles from "./MissionBlock.module.css";
import styles from "./MissionChatHeader.module.css";

export interface ChatMission {
  mission: Mission | null;
  detail: MissionDetail | null;
  /** Every mission of the chat's bot, newest first (the cards of missions started here come from it). */
  all: Mission[];
  refresh: () => Promise<void>;
}

/**
 * The mission that runs in this chat, if any (the daemon names a mission chat "Mission: ..."), and
 * the bot's other missions. It reads them once when the chat opens and keeps asking only while the
 * chat is on screen and its own mission, or one `watch` picks (the ones started from it), can still
 * change; any other chat costs one request.
 */
export function useChatMission(
  botId: string | undefined,
  chatId: string | undefined,
  visible = true,
  watch?: (all: readonly Mission[]) => boolean
): ChatMission {
  const { client } = useDaemon();
  const [state, setState] = useState<{ mission: Mission | null; detail: MissionDetail | null; all: Mission[] }>({ mission: null, detail: null, all: [] });
  const ticket = useRef(0);
  const load = useCallback(async () => {
    if (!client || !botId || !chatId) {
      setState({ mission: null, detail: null, all: [] });
      return;
    }
    const mine = ++ticket.current;
    try {
      const all = await client.listMissions(botId);
      const mission = missionForChat(all, chatId);
      const detail = mission ? await client.getMission(mission.id).catch(() => null) : null;
      if (mine === ticket.current) {
        setState({ mission: detail?.mission ?? mission, detail, all });
      }
    } catch {
      // Missions off on this daemon (503) or offline: the chat simply has no mission header.
      if (mine === ticket.current) {
        setState({ mission: null, detail: null, all: [] });
      }
    }
  }, [client, botId, chatId]);
  // A newer load (another chat, a poll) bumps the ticket, so an older answer never lands.
  useEffect(() => {
    void load();
  }, [load]);
  const live = ((state.mission !== null && isActiveMission(state.mission)) || (watch?.(state.all) ?? false)) && visible;
  useEffect(() => {
    if (!live) {
      return;
    }
    const timer = window.setInterval(() => void load(), MISSION_POLL_MS);
    return () => window.clearInterval(timer);
  }, [live, load]);
  return { ...state, refresh: load };
}

/**
 * A compact header over a mission's chat: the goal, "7 of 15 done", what it works on now, and the
 * sentence that says the owner can leave it alone. The blocked items open below it as ONE form;
 * an ended mission offers its report. The tool steps underneath stay quiet (#913).
 */
export function MissionChatHeader({ chat }: { chat: ChatMission }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [open, setOpen] = useState<"answers" | "waiting" | "report" | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const panelId = useId();
  const { mission: m, detail, refresh } = chat;
  if (!m) {
    return null;
  }
  const live = isActiveMission(m);
  const blocked = live ? (detail?.blocked ?? []) : [];
  const waiting = live ? waitingItems(detail?.items ?? []) : [];
  const digest = !live ? (detail?.digest ?? null) : null;
  const shown =
    (open === "answers" && blocked.length === 0) || (open === "waiting" && waiting.length === 0) || (open === "report" && !digest) ? null : open;
  /** Run one owner action, then read the mission again. */
  const act = async (run: () => Promise<unknown>, after?: () => void) => {
    if (!client || busy) {
      return false;
    }
    setBusy(true);
    setError(null);
    try {
      await run();
      await refresh();
      after?.();
      return true;
    } catch (e) {
      setError(missionErrorText(e));
      return false;
    } finally {
      setBusy(false);
    }
  };
  const send = (answers: Parameters<Parameters<typeof NeedsYou>[0]["onSend"]>[0]) =>
    act(() => client!.answerMission(m.id, answers), () => setOpen(null));
  return (
    <section className={styles.root} data-testid="mission-chat-header" data-state={m.state} aria-label={t("missions.header.label")}>
      <div className={styles.top}>
        <Rocket className={styles.icon} aria-hidden="true" />
        <h2 className={styles.title} title={m.goal}>
          {m.title}
        </h2>
        <MissionChipView state={m.state} />
      </div>
      <div className={styles.body}>
        <MissionProgress mission={m} />
        <MissionNow mission={m} items={detail?.items ?? []} />
        {live || digest ? (
          <div className={styles.actions}>
            {blocked.length > 0 ? (
              <button
                type="button"
                className={styles.toggle}
                data-tone="warning"
                aria-expanded={shown === "answers"}
                aria-controls={panelId}
                data-testid="mission-header-answer"
                onClick={() => setOpen(shown === "answers" ? null : "answers")}
              >
                <ShieldAlert aria-hidden="true" />
                {t("missions.header.answer", { count: blocked.length })}
                <ChevronDown aria-hidden="true" className={styles.chevron} />
              </button>
            ) : null}
            {waiting.length > 0 ? (
              <button
                type="button"
                className={styles.toggle}
                aria-expanded={shown === "waiting"}
                aria-controls={panelId}
                data-testid="mission-header-waiting"
                onClick={() => setOpen(shown === "waiting" ? null : "waiting")}
              >
                {t("missionChat.waiting", { count: waiting.length })}
                <ChevronDown aria-hidden="true" className={styles.chevron} />
              </button>
            ) : null}
            <MissionSteer mission={m} busy={busy} onSteer={(action) => act(() => client!.steerMission(m.id, action))} />
            {digest ? (
              <button
                type="button"
                className={styles.toggle}
                aria-expanded={shown === "report"}
                aria-controls={panelId}
                data-testid="mission-header-report"
                onClick={() => setOpen(shown === "report" ? null : "report")}
              >
                {t("missions.header.report")}
                <ChevronDown aria-hidden="true" className={styles.chevron} />
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
      {error ? (
        <p className={`${blockStyles.error} ${styles.error}`} role="alert">
          {error}
        </p>
      ) : null}
      <div id={panelId} className={styles.panel} hidden={shown === null}>
        {shown === "answers" ? <NeedsYou items={blocked} busy={busy} onSend={send} heading="h3" /> : null}
        {shown === "waiting" ? <WaitingFor items={waiting} heading="h3" /> : null}
        {shown === "report" && digest ? <Digest digest={digest} bare /> : null}
      </div>
    </section>
  );
}

const OUTCOME_ICONS: Record<DigestOutcome, typeof Rocket> = {
  done: CircleCheck,
  blocked: ShieldAlert,
  failed: TriangleAlert,
  cancelled: Square,
  update: Rocket
};

/**
 * The report a mission posts to its chat, as a card in the owner's language instead of the
 * bilingual plain text the daemon writes for channels.
 */
export function MissionDigestMessage({ digest: d }: { digest: DigestText }) {
  const { t } = useTranslation();
  const Icon = OUTCOME_ICONS[d.outcome];
  const c = d.counts;
  const parts = c
    ? [
        t("missions.report.done", { done: c.done, total: c.total }),
        c.blocked > 0 ? t("missions.report.blocked", { count: c.blocked }) : null,
        c.failed > 0 ? t("missions.report.failed", { count: c.failed }) : null,
        c.skipped > 0 ? t("missions.report.skipped", { count: c.skipped }) : null,
        c.open > 0 ? t("missions.report.open", { count: c.open }) : null
      ].filter(Boolean)
    : [];
  return (
    <article className={styles.report} data-outcome={d.outcome} data-testid="mission-digest-message">
      <h2 className={styles.reportHead}>
        <Icon aria-hidden="true" />
        <span>{t(`missions.report.outcome.${d.outcome}`)}</span>
      </h2>
      <p className={styles.reportTitle}>{d.title}</p>
      {parts.length > 0 ? <p className={styles.reportCounts}>{parts.join(" · ")}</p> : null}
      {d.reason ? <p className={styles.reportReason}>{d.reason}</p> : null}
      <ReportList title={t("missions.digest.blocked")} rows={d.needs.map((r) => `${r.key}: ${r.text}`)} />
      <ReportList title={t("missions.digest.failed")} rows={d.failed.map((r) => `${r.key}: ${r.text}`)} />
      <ReportList title={t("missions.digest.decisions")} rows={d.decisions} />
    </article>
  );
}

function ReportList({ title, rows }: { title: string; rows: readonly string[] }) {
  if (rows.length === 0) {
    return null;
  }
  return (
    <div className={styles.reportSection}>
      <h3>{title}</h3>
      <ul>
        {rows.map((r, i) => (
          <li key={`${i}-${r}`}>{r}</li>
        ))}
      </ul>
    </div>
  );
}

/**
 * A mission started from this chat (`/mission`, or the bot starting one from the owner's message),
 * as a compact card in the stream: the title and status, the bar, pause/stop, and the way to the
 * mission's own chat, where its answers and report are.
 */
export function MissionStartedCard({ mission: m, onChanged }: { mission: Mission; onChanged: () => Promise<void> }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const titleId = useId();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const live = isActiveMission(m);
  const steer = async (action: "pause" | "resume" | "cancel") => {
    if (!client || busy) {
      return false;
    }
    setBusy(true);
    setError(null);
    try {
      await client.steerMission(m.id, action);
      await onChanged();
      return true;
    } catch (e) {
      setError(missionErrorText(e));
      return false;
    } finally {
      setBusy(false);
    }
  };
  const open = m.progress.blocked > 0 && live
    ? t("missionChat.openAnswers", { count: m.progress.blocked })
    : live
      ? t("missionChat.open")
      : t("missionChat.openReport");
  return (
    <article className={styles.card} data-testid="mission-started-card" data-state={m.state} aria-labelledby={titleId}>
      <p className={styles.cardEyebrow}>
        <Rocket aria-hidden="true" />
        {t("missionChat.eyebrow")}
      </p>
      <div className={styles.top}>
        <h2 className={styles.title} id={titleId} title={m.goal}>
          {m.title}
        </h2>
        <MissionChipView state={m.state} />
      </div>
      <MissionBar mission={m} />
      <div className={styles.actions}>
        <MissionSteer mission={m} busy={busy} onSteer={steer} />
        <button
          type="button"
          className={styles.toggle}
          data-tone={m.progress.blocked > 0 && live ? "warning" : undefined}
          data-testid="mission-open"
          onClick={() => requestOpenChat(m.chat_id)}
        >
          {open}
          <ArrowUpRight aria-hidden="true" />
        </button>
      </div>
      {error ? (
        <p className={blockStyles.error} role="alert">
          {error}
        </p>
      ) : null}
    </article>
  );
}
