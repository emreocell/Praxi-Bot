import { CalendarDays, LayoutGrid, List, MessageSquarePlus, Pause, Play, Plus, RefreshCw, Settings2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import type { BotSpendLimits } from "../api/types";
import formStyles from "../components/Form.module.css";
import { botView } from "../lib/botProfile";
import { humanLine } from "../lib/humanError";
import { dailyBudget, nativePauseAction, overviewBots, selectOverviewBots, type BotFilter, type BotSort } from "../lib/myBots";
import type { ControlSnapshot } from "../overlay/events";
import { useDaemon } from "../state/DaemonContext";
import styles from "./MyBotsView.module.css";

export function MyBotsView({ control, onPause, onNewBot, onNewChat, onSettings, onSchedule, onOpenChat, newChatBusy }: {
  control: ControlSnapshot;
  onPause: (botId: string) => Promise<void>;
  onNewBot: () => void;
  onNewChat: (botId: string) => void;
  onSettings: (botId: string) => void;
  onSchedule: (botId: string) => void;
  onOpenChat: (chatId: string) => void;
  newChatBusy: boolean;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, runs, events, needs, activeComputerId, activeHostName, homeStale, eventsConnected, refreshBots, refreshRuns, onDaemonEvent } = useDaemon();
  const local = activeComputerId === "local";
  const place = local ? t("myBots.thisComputer") : activeHostName || t("myBots.server");
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<BotFilter>("all");
  const [sort, setSort] = useState<BotSort>("activity");
  const [mode, setMode] = useState<"grid" | "list">("grid");
  const [spend, setSpend] = useState<Record<string, BotSpendLimits | null>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  // A generation belongs to one client and one bot list. Late replies never replace newer data.
  const generation = useRef(0);
  const invalidateSpend = useCallback(() => { ++generation.current; }, []);
  const loadSpend = useCallback(async () => {
    const request = ++generation.current;
    if (!client) return;
    const next: Record<string, BotSpendLimits | null> = {};
    // Keep the number of simultaneous requests bounded even with a large bot collection.
    for (let i = 0; i < bots.length; i += 4) {
      await Promise.all(bots.slice(i, i + 4).map(async (bot) => {
        try { next[bot.id] = await client.getBotSpendLimits(bot.id); }
        catch { next[bot.id] = null; }
      }));
      if (generation.current !== request) return;
    }
    if (generation.current === request) setSpend(next);
  }, [client, bots]);

  useEffect(() => {
    setSpend({});
    void loadSpend();
    return invalidateSpend;
  }, [loadSpend, invalidateSpend]);

  useEffect(() => {
    // Finishes update the ledger. Debounce bursts; periodically refresh in-flight usage and midnight.
    let timer: number | undefined;
    const off = onDaemonEvent(({ event }) => {
      if (event.type === "run_finished" || event.type === "error") {
        window.clearTimeout(timer);
        timer = window.setTimeout(() => void loadSpend(), 300);
      }
    });
    const interval = window.setInterval(() => void loadSpend(), 30_000);
    return () => { off(); window.clearTimeout(timer); window.clearInterval(interval); };
  }, [onDaemonEvent, loadSpend]);

  const rows = useMemo(() => overviewBots(bots, runs, events, needs, local ? control : null), [bots, runs, events, needs, local, control]);
  const shown = selectOverviewBots(rows, query, filter, sort, spend, i18n.language);
  const usd = new Intl.NumberFormat(i18n.language, { style: "currency", currency: "USD", maximumFractionDigits: 2 });
  const refresh = async () => {
    setBusy(true);
    setError(null);
    try { await Promise.all([refreshBots(), refreshRuns(), loadSpend()]); }
    catch (e) { setError(humanLine(e)); }
    finally { setBusy(false); }
  };
  const pause = async (botId: string, pausedRunId?: string) => {
    if (!client || busyId) return;
    setBusyId(botId);
    setError(null);
    try {
      if (nativePauseAction(botId, local, control)) await onPause(botId);
      else if (pausedRunId) await client.resumeRun(pausedRunId);
      await refreshRuns();
    } catch (e) { setError(humanLine(e)); }
    finally { setBusyId(null); }
  };

  return (
    <section className={styles.pane} aria-labelledby="my-bots-title" data-testid="my-bots">
      <div className={styles.container}>
        <header className={styles.header}>
          <div><h1 id="my-bots-title">{t("myBots.title")}</h1><p>{t("myBots.scope", { place })}</p></div>
          <div className={styles.actions}>
            <Button size="sm" disabled={busy || !client} onClick={() => void refresh()} aria-label={t("myBots.refresh")} title={t("myBots.refresh")}><RefreshCw aria-hidden="true" /></Button>
            <Button size="sm" variant="primary" onClick={onNewBot} disabled={!client || homeStale}><Plus aria-hidden="true" />{t("myBots.newBot")}</Button>
          </div>
        </header>
        {homeStale || !eventsConnected ? <p role="status" className={styles.notice}>{t("myBots.stale")}</p> : null}
        {error ? <p className={formStyles.error} role="alert">{error}</p> : null}
        <div className={styles.toolbar}>
          <label className={styles.search}>{t("myBots.search")}<input className={formStyles.input} type="search" value={query} onChange={(e) => setQuery(e.target.value)} /></label>
          <label>{t("myBots.filter")}<select className={formStyles.select} value={filter} onChange={(e) => setFilter(e.target.value as BotFilter)}>
            {(["all", "idle", "running", "approval", "paused"] as const).map((s) => <option key={s} value={s}>{t(s === "all" ? "myBots.all" : `myBots.status.${s}`)}</option>)}
          </select></label>
          <label>{t("myBots.sort")}<select className={formStyles.select} value={sort} onChange={(e) => setSort(e.target.value as BotSort)}>
            {(["activity", "name", "cost"] as const).map((s) => <option key={s} value={s}>{t(`myBots.sortBy.${s}`)}</option>)}
          </select></label>
          <div className={styles.actions} role="group" aria-label={t("myBots.layout")}>
            {(["grid", "list"] as const).map((m) => <Button key={m} size="sm" variant={mode === m ? "secondary" : "ghost"} aria-pressed={mode === m} aria-label={t(`myBots.${m}`)} title={t(`myBots.${m}`)} onClick={() => setMode(m)}>{m === "grid" ? <LayoutGrid aria-hidden="true" /> : <List aria-hidden="true" />}</Button>)}
          </div>
        </div>
        <p className={styles.count} role="status">{t("myBots.count", { shown: shown.length, total: bots.length })}</p>
        {shown.length === 0 ? <Card padding="md" className={styles.empty}>
          <h2>{t(bots.length ? "myBots.noMatches" : "myBots.emptyTitle")}</h2>
          <p>{t(bots.length ? "myBots.noMatchesHint" : "myBots.emptyHint")}</p>
          {bots.length ? <Button onClick={() => { setQuery(""); setFilter("all"); }}>{t("myBots.clear")}</Button> : <Button variant="primary" onClick={onNewBot} disabled={!client || homeStale}><Plus aria-hidden="true" />{t("myBots.newBot")}</Button>}
        </Card> : <ul className={mode === "grid" ? styles.grid : styles.list} data-testid={`my-bots-${mode}`}>
          {shown.map(({ bot, status, lastActivity, pausedRunId, needsChatId }) => {
            const view = botView(bot);
            const data = spend[bot.id];
            const budget = data ? dailyBudget(data) : null;
            const nativeAction = nativePauseAction(bot.id, local, control);
            const canResume = nativeAction === "resume" || !!pausedRunId;
            const disabled = !client || homeStale;
            return <li key={bot.id}>
              <Card padding="md" className={styles.card}>
                <div className={styles.identity}>
                  <BotAvatar bot={view} size="md" state={status === "running" ? "working" : status === "approval" ? "waiting-approval" : "idle"} decorative />
                  <div className={styles.name}><h2>{view.name}</h2><span>{place}</span></div>
                  <span className={styles.status} data-status={status}>{t(`myBots.status.${status}`)}</span>
                </div>
                <dl className={styles.facts}>
                  <div><dt>{t("myBots.lastActivity")}</dt><dd>{lastActivity ? <time dateTime={new Date(lastActivity).toISOString()}>{new Date(lastActivity).toLocaleString(i18n.language, { dateStyle: "short", timeStyle: "short" })}</time> : t("myBots.noActivity")}</dd></div>
                  <div><dt>{t("myBots.today")}</dt><dd>{budget ? t(budget.limit === null ? "myBots.costNoLimit" : "myBots.costLimit", { cost: usd.format(budget.used), limit: budget.limit === null ? "" : usd.format(budget.limit) }) : t(data === null ? "myBots.costUnavailable" : "myBots.loading")}</dd></div>
                </dl>
                {budget?.limit ? <progress className={styles.progress} value={Math.min(budget.used, budget.limit)} max={budget.limit} aria-label={t("myBots.dailyLimit", { name: view.name })} /> : null}
                <div className={styles.cardActions} role="group" aria-label={view.name}>
                  <Button size="sm" disabled={disabled || newChatBusy} onClick={() => onNewChat(bot.id)}><MessageSquarePlus aria-hidden="true" />{t("myBots.newChat")}</Button>
                  <Button size="sm" disabled={disabled || !!busyId || (!nativeAction && !pausedRunId)} title={!nativeAction && !pausedRunId ? t("myBots.pauseUnavailable") : undefined} onClick={() => void pause(bot.id, pausedRunId)}>{canResume ? <Play aria-hidden="true" /> : <Pause aria-hidden="true" />}{t(canResume ? "myBots.resume" : "myBots.pause")}</Button>
                  <Button size="sm" disabled={disabled} onClick={() => onSettings(bot.id)}><Settings2 aria-hidden="true" />{t("myBots.settings")}</Button>
                  <Button size="sm" disabled={disabled} onClick={() => onSchedule(bot.id)}><CalendarDays aria-hidden="true" />{t("myBots.schedule")}</Button>
                  {needsChatId ? <Button size="sm" disabled={disabled} onClick={() => onOpenChat(needsChatId)}>{t("myBots.review")}</Button> : null}
                </div>
              </Card>
            </li>;
          })}
        </ul>}
        <p className={styles.footnote}>{t("myBots.costHint")}</p>
        <p className={styles.footnote}>{t("myBots.pauseUnavailable")}</p>
      </div>
    </section>
  );
}
