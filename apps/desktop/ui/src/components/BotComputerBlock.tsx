import { Eye, Globe, Loader2, Maximize2, Monitor, Server, Star, Terminal } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { Button } from "ui";
import { isActivelyOwningTakeover, visibleActions } from "../lib/liveViewState";
import { useLiveView, useLocalLiveView } from "../state/LiveViewContext";
import { useTranslation } from "react-i18next";
import { type ComputerSide, hasBothComputers } from "../lib/botComputer";
import { formatLive, type ComputerView, type UnavailableReason } from "../lib/computerView";
import { noScreenDetailKey } from "../lib/computerLabel";
import { useDaemon } from "../state/DaemonContext";
import { useServerTerminal } from "../state/ServerTerminalContext";
import { computerPresence, mainBotLabel } from "../lib/computerStage";
import { useComputerView } from "../state/useComputerView";
import { streamHealth } from "../lib/liveViewAdaptive";
import { cardAspect } from "../lib/liveViewSizing";
import { StreamQuality } from "./StreamQuality";
import styles from "./BotComputerBlock.module.css";

/** This computer's system, for the status line ("Windows · bot burada çalışıyor"). */
function thisSystem(): string {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  return /Windows/i.test(ua) ? "Windows" : /Mac/i.test(ua) ? "macOS" : /Linux|X11/i.test(ua) ? "Linux" : "";
}

function reasonKey(reason: UnavailableReason | null): { key: string; detail?: string } {
  if (reason && typeof reason === "object") {
    return "noScreen" in reason
      ? { key: "computerCard.reason.noScreen", detail: noScreenDetailKey(reason.noScreen) }
      : "streamError" in reason
        ? { key: `liveView.errors.${reason.streamError}` }
        : { key: `liveView.reasons.${reason.targetReason}` };
  }
  return { key: `computerCard.reason.${reason ?? "failed"}` };
}

/** A frozen picture is never shown as live: after a few quiet seconds the card says so. */
function StallNotice({ lastActivityAtMs }: { lastActivityAtMs: number | null }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const health = streamHealth({ hasFrame: true, lastActivityAtMs, nowMs: now, ended: false });
  if (health !== "stalled") return null;
  return (
    <div className={styles.stalled} role="status" data-testid="bot-computer-stalled">
      <Loader2 aria-hidden="true" />
      <span>{t("computerCard.stalled")}</span>
    </div>
  );
}

/** The elapsed "CANLI 0:42" label: ticks once a second while live. */
function LiveClock({ since }: { since: number | null }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  return (
    <span className={styles.rec} data-testid="bot-computer-live">
      {t("computerCard.live")} {formatLive(since ? now - since : 0)}
    </span>
  );
}

/**
 * The computer card (design v7): the computer the bot RUNS ON in one small card. It never opens a
 * live session by itself: idle shows the bot's grainy gradient and one button; the screen is only
 * produced while the owner looks (`useComputerView` decides the states and ends the view).
 */
export function BotComputerBlock({
  botId,
  mainBotName,
  activity = null,
  visible
}: {
  botName?: string;
  botId?: string | null;
  /** The main bot's name, shown on the device stage. */
  mainBotName?: string;
  /** What the bot is doing right now (a sentence), or null while idle. */
  activity?: string | null;
  /** The panel is open: closing it ends the view. */
  visible: boolean;
}) {
  const { t } = useTranslation();
  const serverTerminal = useServerTerminal();
  const { home, workLocalNow, hostList, activeHostName } = useDaemon();
  const both = hasBothComputers(home, workLocalNow);
  const homeLink = home.kind === "host" ? (hostList?.hosts.find((h) => h.id === home.hostId) ?? null) : null;
  const pcAllowed = homeLink?.computer_access === true;
  const [side, setSide] = useState<ComputerSide>("bot");
  const showSwitch = both && pcAllowed;
  const showingMine = showSwitch && side === "mine";
  const server = home.kind === "host" && !workLocalNow && !showingMine;
  const view = useComputerView({ visible, botId, side: showingMine ? "mine" : "bot" });
  const serverName = activeHostName ?? (home.kind === "host" ? home.name : "") ?? "";
  const name = server ? serverName || t("computerPicker.host") : t("computerCard.thisComputer");
  const offline = view.kind === "unavailable" && (view.unavailable === "offline" || view.unavailable === "notPaired" || view.unavailable === "version");
  const system = thisSystem();
  const status = server
    ? offline
      ? t("computerCard.status.serverOffline")
      : t("computerCard.status.serverOnline")
    : system
      ? t("computerCard.status.local", { system })
      : t("computerCard.status.localNoSystem");
  const presence = computerPresence({ offline, working: activity !== null });
  const mainName = mainBotLabel(mainBotName);
  const { key: rk, detail } = reasonKey(view.unavailable);
  const reason = t(rk, {
    name: serverName || t("computerPicker.host"),
    detail: detail ? t(detail) : "",
    defaultValue: t("computerCard.reason.failed")
  });

  // Control can outlive the view (a paused latch, a failed hand-back): never leave the bot paused
  // with nothing to press, so these recovery actions stay on the card whatever the view says.
  const homeLive = useLiveView();
  const localLive = useLocalLiveView();
  const live = showingMine ? localLive : homeLive;
  const panelActions = visibleActions(live.state, live.controlState, live.canClearForeignPause).panel;
  const releasePending = live.state.pendingRequest?.kind === "release";
  const showRelease = isActivelyOwningTakeover(live.state) || releasePending;
  const showPause = panelActions.includes("clear_pause") && !showRelease && !live.pauseClearing;

  const hint =
    view.kind === "idle"
      ? (activity ?? t("computerCard.hint.idle"))
      : view.kind === "starting"
        ? t("computerCard.hint.starting")
        : view.kind === "paused"
          ? t("computerCard.hint.paused")
          : view.kind === "timeout"
            ? view.unavailable === "timeout" ? reason : `${t("computerCard.reason.timeout")}. ${reason}`
            : view.kind === "unavailable" ? reason : "";

  let body: ReactNode = null;
  if (view.kind === "idle") {
    body = (
      <button type="button" className={styles.cta} data-testid="bot-computer-show" onClick={view.start}>
        <Eye aria-hidden="true" />
        {t("computerCard.show")}
      </button>
    );
  } else if (view.kind === "starting") {
    body = <span className={styles.spin} role="status" aria-label={t("computerCard.connecting")} />;
  } else if (view.kind === "paused") {
    body = (
      <button type="button" className={styles.cta} data-testid="bot-computer-resume" onClick={view.start}>
        <Eye aria-hidden="true" />
        {t("computerCard.resume")}
      </button>
    );
  } else if (view.kind === "unavailable" || view.kind === "timeout") {
    body = (
      <button type="button" className={styles.cta} data-testid="bot-computer-retry" onClick={view.start}>
        {t("computerCard.retry")}
      </button>
    );
  }

  return (
    <section
      className={styles.block}
      aria-labelledby="bot-panel-computer"
      data-testid="bot-computer"
      data-kind={view.kind}
      onPointerMove={view.touch}
      onPointerDown={view.touch}
      onKeyDown={view.touch}
    >
      <h3 className={styles.label} id="bot-panel-computer">
        {t("computerCard.title")}
      </h3>
      <div className={styles.pc} data-computer={server ? "host" : showingMine ? "mine" : "local"}>
        <div className={styles.head}>
          {server ? <Server aria-hidden="true" className={styles.headIcon} /> : <Monitor aria-hidden="true" className={styles.headIcon} />}
          <div className={styles.title}>
            <b data-testid="bot-computer-where">{name}</b>
            <small data-testid="bot-computer-status">
              <i className={styles.dot} data-offline={offline ? "true" : undefined} aria-hidden="true" />
              <span>{status}</span>
            </small>
          </div>
          {showSwitch ? (
            <div className={styles.who} role="group" aria-label={t("botPanel.switcher")}>
              {(["bot", "mine"] as const).map((s) => (
                <button
                  key={s}
                  type="button"
                  aria-pressed={side === s}
                  data-testid={`bot-computer-side-${s}`}
                  onClick={() => setSide(s)}
                >
                  {t(s === "bot" ? "computerCard.sideBot" : "computerCard.sideMine")}
                </button>
              ))}
            </div>
          ) : null}
        </div>

        <div
          className={styles.view}
          data-kind={view.kind}
          data-testid="bot-computer-view"
          style={view.kind === "live" ? { aspectRatio: cardAspect(view.resolution) } : undefined}
        >
          {view.kind === "live" && view.frameUrl ? (
            <>
              <img className={styles.frame} data-testid="bot-computer-frame" src={view.frameUrl} alt={t("computerCard.frameAlt", { name })} title={t("liveView.privacyNotice")} draggable={false} />
              <LiveClock since={view.liveSinceMs} />
              <StallNotice lastActivityAtMs={live.state.lastActivityAtMs} />
              <div className={styles.quality}>
                <StreamQuality mode={live.qualityMode} onChange={live.setQualityMode} />
              </div>
              <div className={styles.over}>
                <button type="button" data-testid="bot-computer-big" onClick={view.bigView}>
                  <Maximize2 aria-hidden="true" />
                  {t("computerCard.big")}
                </button>
                <button type="button" data-testid="bot-computer-take" onClick={view.takeControl}>
                  {t("computerCard.take")}
                </button>
                <button type="button" data-testid="bot-computer-close" onClick={view.stop}>
                  {t("computerCard.close")}
                </button>
              </div>
            </>
          ) : (
            <>
              {view.previewUrl ? (
                <img className={styles.frame} src={view.previewUrl} alt="" draggable={false} />
              ) : (
                <div className={styles.stage} data-testid="bot-computer-stage" data-presence={presence} aria-hidden="true">
                  {server ? <Server className={styles.device} /> : <Monitor className={styles.device} />}
                </div>
              )}
              {view.previewUrl ? null : (
                <div className={styles.facts}>
                  <span className={styles.fact} data-presence={presence} data-testid="bot-computer-presence">
                    <i className={styles.dot} data-offline={presence === "offline" ? "true" : undefined} aria-hidden="true" />
                    {t(`computerCard.presence.${presence}`)}
                  </span>
                  {mainName ? (
                    <span className={styles.fact} data-testid="bot-computer-main" title={t("computerCard.mainBot", { name: mainName })}>
                      <Star aria-hidden="true" />
                      <span>{mainName}</span>
                    </span>
                  ) : null}
                </div>
              )}
              {body}
              {hint ? <div className={styles.hint}>{hint}</div> : null}
            </>
          )}
        </div>

        {showPause ? (
          <div className={styles.control} data-testid="bot-computer-paused-actions">
            {live.recoveryPrompt?.mode === "reacquire" ? (
              <Button variant="primary" size="sm" data-testid="bot-computer-reacquire" onClick={() => void live.recoverTakeover()}>
                {t("liveView.reacquireAction")}
              </Button>
            ) : null}
            <Button
              variant={live.recoveryPrompt?.mode === "reacquire" ? "secondary" : "primary"}
              size="sm"
              data-testid="bot-computer-clear-pause"
              onClick={() => void live.clearPause("own")}
            >
              {t("liveView.clearPauseAction")}
            </Button>
          </div>
        ) : null}
        {showRelease ? (
          <div className={styles.control}>
            <Button
              variant="primary"
              size="sm"
              data-testid="bot-computer-release"
              disabled={releasePending && !live.state.lastError}
              onClick={() => void (releasePending && live.state.lastError ? live.retryRelease() : live.releaseTakeover(null))}
            >
              {releasePending && live.state.lastError
                ? t("liveView.retryDone")
                : releasePending
                  ? t("liveView.retryingDone")
                  : t("liveView.doneAction")}
            </Button>
          </div>
        ) : null}

        <div className={styles.foot}>
          {server && serverTerminal.available ? (
            <button
              type="button"
              className={styles.lnk}
              data-testid="bot-computer-terminal"
              onClick={() => serverTerminal.open()}
            >
              <Terminal aria-hidden="true" />
              {t("computerCard.terminal")}
            </button>
          ) : null}
          <button type="button" className={styles.lnk} data-testid="bot-computer-browser" onClick={view.openBrowser}>
            <Globe aria-hidden="true" />
            {t("computerCard.browser")}
          </button>
          <ViewNote view={view} />
        </div>
      </div>
    </section>
  );
}

/** Right-aligned note: resolution and rate while live, "duraklatıldı" while paused. */
function ViewNote({ view }: { view: ComputerView }) {
  const { t } = useTranslation();
  const note =
    view.kind === "live"
      ? [view.resolution, view.fps ? t("computerCard.fps", { fps: view.fps }) : null].filter(Boolean).join(" · ")
      : view.kind === "paused"
        ? t("computerCard.pausedNote")
        : "";
  return note ? <span className={styles.note}>{note}</span> : <span className={styles.note} />;
}
