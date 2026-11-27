import { ChevronDown, CircleCheck, Rocket, ShieldAlert, Square, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
import type { Mission, MissionDetail } from "../api/types";
import { humanLine } from "../lib/humanError";
import type { DigestOutcome, DigestText } from "../lib/missionDigestText";
import { isActiveMission, MISSION_POLL_MS, missionChip, missionForChat } from "../lib/missions";
import { useDaemon } from "../state/DaemonContext";
import { CHIP_ICONS, Digest, MissionNow, MissionProgress, NeedsYou } from "./MissionBlock";
import blockStyles from "./MissionBlock.module.css";
import styles from "./MissionChatHeader.module.css";

export interface ChatMission {
  mission: Mission | null;
  detail: MissionDetail | null;
  refresh: () => Promise<void>;
}

/**
 * The mission that runs in this chat, if any (the daemon names a mission chat "Mission: ..."). It
 * reads the bot's missions once when the chat opens and keeps asking only while that mission can
 * still change and the chat is on screen; any other chat costs one request.
 */
export function useChatMission(botId: string | undefined, chatId: string | undefined, visible = true): ChatMission {
  const { client } = useDaemon();
  const [state, setState] = useState<{ mission: Mission | null; detail: MissionDetail | null }>({ mission: null, detail: null });
  const ticket = useRef(0);
  const load = useCallback(async () => {
    if (!client || !botId || !chatId) {
      setState({ mission: null, detail: null });
      return;
    }
    const mine = ++ticket.current;
    try {
      const mission = missionForChat(await client.listMissions(botId), chatId);
      const detail = mission ? await client.getMission(mission.id).catch(() => null) : null;
      if (mine === ticket.current) {
        setState({ mission: detail?.mission ?? mission, detail });
      }
    } catch {
      // Missions off on this daemon (503) or offline: the chat simply has no mission header.
      if (mine === ticket.current) {
        setState({ mission: null, detail: null });
      }
    }
  }, [client, botId, chatId]);
  // A newer load (another chat, a poll) bumps the ticket, so an older answer never lands.
  useEffect(() => {
    void load();
  }, [load]);
  const live = state.mission !== null && isActiveMission(state.mission) && visible;
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
  const [open, setOpen] = useState<"answers" | "report" | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const panelId = useId();
  const { mission: m, detail, refresh } = chat;
  if (!m) {
    return null;
  }
  const chip = missionChip(m.state);
  const ChipIcon = CHIP_ICONS[chip];
  const live = isActiveMission(m);
  const blocked = live ? (detail?.blocked ?? []) : [];
  const digest = !live ? (detail?.digest ?? null) : null;
  const shown = open === "answers" && blocked.length === 0 ? null : open === "report" && !digest ? null : open;
  const send = async (answers: Parameters<Parameters<typeof NeedsYou>[0]["onSend"]>[0]) => {
    if (!client || busy) {
      return false;
    }
    setBusy(true);
    setError(null);
    try {
      await client.answerMission(m.id, answers);
      await refresh();
      setOpen(null);
      return true;
    } catch (e) {
      setError(e instanceof ApiError && e.status === 400 ? e.message : humanLine(e));
      return false;
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={styles.root} data-testid="mission-chat-header" data-state={m.state} aria-label={t("missions.header.label")}>
      <div className={styles.top}>
        <Rocket className={styles.icon} aria-hidden="true" />
        <h2 className={styles.title} title={m.goal}>
          {m.title}
        </h2>
        <span className={blockStyles.chip} data-chip={chip}>
          <ChipIcon aria-hidden="true" />
          {t(`missions.chip.${chip}`)}
        </span>
      </div>
      <div className={styles.body}>
        <MissionProgress mission={m} />
        <MissionNow mission={m} items={detail?.items ?? []} />
        {blocked.length > 0 || digest ? (
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
      <div id={panelId} className={styles.panel} hidden={shown === null}>
        {error ? (
          <p className={blockStyles.error} role="alert">
            {error}
          </p>
        ) : null}
        {shown === "answers" ? <NeedsYou items={blocked} busy={busy} onSend={send} heading="h3" /> : null}
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
