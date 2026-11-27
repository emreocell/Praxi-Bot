import { ChevronDown, ExternalLink, Eye, Globe, Loader2, Maximize2, Monitor, Server, Star, Terminal, X } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { Button } from "ui";
import { isActivelyOwningTakeover, visibleActions } from "../lib/liveViewState";
import { liveComputerMenu } from "../lib/contextMenu";
import { useLiveView, useLocalLiveView } from "../state/LiveViewContext";
import { useTranslation } from "react-i18next";
import { type ComputerSide, hasBothComputers } from "../lib/botComputer";
import { formatLive, type ComputerView, type UnavailableReason } from "../lib/computerView";
import { noScreenDetailKey } from "../lib/computerLabel";
import { explainScreen, screenPlace } from "../lib/screenReason";
import { useDaemon } from "../state/DaemonContext";
import { useServerTerminal } from "../state/ServerTerminalContext";
import { computerPresence, mainBotLabel } from "../lib/computerStage";
import { useComputerView } from "../state/useComputerView";
import { QUALITY_MODES, type QualityMode, streamHealth } from "../lib/liveViewAdaptive";
import { cardAspect } from "../lib/liveViewSizing";
import { useContextMenu, useMenuTranslate } from "./ContextMenu";
import { useDragPopOut } from "./useDragPopOut";
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
  visible,
  onPopOut
}: {
  botName?: string;
  botId?: string | null;
  /** The main bot's name, shown on the device stage. */
  mainBotName?: string;
  /** What the bot is doing right now (a sentence), or null while idle. */
  activity?: string | null;
  /** The panel is open: closing it ends the view. */
  visible: boolean;
  /** Open the live computer view in a dedicated pop-out window. */
  onPopOut?: (side: ComputerSide, opts?: { nearCursor?: boolean; targetId?: string }) => void;
}) {
  const { t } = useTranslation();
  const menuT = useMenuTranslate();
  const cardMenu = useContextMenu();
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
  const presence = computerPresence({ offline, working: activity !== null });
  // One status line: is it on and is the bot working on it, then where ("Working · Windows").
  const status =
    server && offline
      ? t("computerCard.status.serverOffline")
      : [t(`computerCard.presence.${presence}`), server ? t("computerCard.status.server") : system].filter(Boolean).join(" · ");
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
  const popOut = onPopOut
    ? (opts?: { nearCursor?: boolean }) => onPopOut(side, { ...opts, targetId: live.selectedTarget?.target_id })
    : undefined;
  const { dragProps } = useDragPopOut({
    onPopOut: popOut,
    ignoreSelector: "button, a, input, select"
  });
  const cardMenuBind = onPopOut
    ? cardMenu.bind(() =>
        liveComputerMenu({
          t: menuT,
          onOpenFull: view.kind === "live" ? view.bigView : view.start,
          onPopOut: () => popOut?.()
        })
      )
    : {};

  // One vocabulary for "the screen did not start / is not available": the same words as the full view,
  // and the fix that fits THIS computer (a server gets the install command in the full view only).
  const un = view.unavailable;
  const place = screenPlace(!server);
  const screenWhy =
    view.kind === "timeout" && un === "timeout"
      ? explainScreen("timeout", null, place)
      : un && typeof un === "object" && "noScreen" in un
        ? explainScreen(null, un.noScreen, place)
        : un && typeof un === "object" && "targetReason" in un
          ? explainScreen(un.targetReason, null, place)
          : null;
  const whyText = screenWhy
    ? [
        t("computerCard.reason.noScreen", { detail: t(screenWhy.detailKey) }),
        screenWhy.command ? "" : t(screenWhy.fixKey)
      ]
        .filter(Boolean)
        .join(". ")
    : null;

  const hint =
    whyText && (view.kind === "timeout" || view.kind === "unavailable")
      ? whyText
      : view.kind === "idle"
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
      {cardMenu.menu}
      <div
        className={styles.pc}
        data-computer={server ? "host" : showingMine ? "mine" : "local"}
        {...dragProps}
        {...cardMenuBind}
      >
        <div className={styles.head}>
          {server ? <Server aria-hidden="true" className={styles.headIcon} /> : <Monitor aria-hidden="true" className={styles.headIcon} />}
          <div className={styles.title}>
            <b data-testid="bot-computer-where">{name}</b>
            <small data-testid="bot-computer-status" data-presence={presence}>
              <i className={styles.dot} data-presence={presence} aria-hidden="true" />
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
              {/* Compact controls: quality and close up top, the actions at the bottom, one primary. */}
              <div className={styles.topRight}>
                <QualitySelect mode={live.qualityMode} onChange={live.setQualityMode} />
                <button
                  type="button"
                  className={styles.closeView}
                  data-testid="bot-computer-close"
                  title={t("computerCard.closeView")}
                  aria-label={t("computerCard.closeView")}
                  onClick={view.stop}
                >
                  <X aria-hidden="true" />
                </button>
              </div>
              <div className={styles.over}>
                <button type="button" data-testid="bot-computer-take" onClick={view.takeControl}>
                  {t("computerCard.take")}
                </button>
                <button type="button" className={styles.overPrimary} data-testid="bot-computer-big" onClick={view.bigView}>
                  <Maximize2 aria-hidden="true" />
                  {t("computerCard.big")}
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
          {onPopOut ? (
            <button
              type="button"
              className={styles.lnk}
              data-testid="bot-computer-popout"
              onClick={() => popOut?.()}
            >
              <ExternalLink aria-hidden="true" />
              {t("computerCard.popOut")}
            </button>
          ) : null}
          <ViewNote view={view} />
        </div>
      </div>
    </section>
  );
}

/** The picture quality as one small menu over the preview (the three-button row covered the clock). */
function QualitySelect({ mode, onChange }: { mode: QualityMode; onChange: (mode: QualityMode) => void }) {
  const { t } = useTranslation();
  return (
    <span className={styles.qualitySelect} title={t(`liveView.quality.${mode}Hint`)}>
      <select
        value={mode}
        aria-label={t("liveView.quality.label")}
        data-testid="bot-computer-quality"
        onChange={(e) => onChange(e.target.value as QualityMode)}
      >
        {QUALITY_MODES.map((m) => (
          <option key={m} value={m}>
            {t(`liveView.quality.${m}`)}
          </option>
        ))}
      </select>
      <ChevronDown aria-hidden="true" />
    </span>
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
