import { Globe, Maximize2, Monitor } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import {
  isActivelyOwningTakeover,
  summarizeTarget,
  visibleActions
} from "../lib/liveViewState";
import { LOCAL_COMPUTER, useDaemon } from "../state/DaemonContext";
import {
  COMPUTER_FOOTER_KEY,
  computerFooter,
  computerPanel,
  noScreenCode,
  type PanelText
} from "../lib/computerLabel";
import {
  type ComputerSide,
  hasBothComputers,
  serverDesktopCommand,
  serverScreenIssue
} from "../lib/botComputer";
import { useLiveView, useLocalLiveView } from "../state/LiveViewContext";
import styles from "./BotComputerBlock.module.css";

function panelTone(
  hasTarget: boolean,
  viewState: string,
  unavailable: boolean
): "watch" | "active" | "acquiring" | "paused" | "idle" {
  if (!hasTarget || unavailable || viewState === "unavailable") {
    return "idle";
  }
  if (viewState === "active") {
    return "active";
  }
  if (viewState === "acquiring") {
    return "acquiring";
  }
  if (viewState === "paused") {
    return "paused";
  }
  return "watch";
}

/**
 * The computer the bot RUNS ON (card 266-A): the server's screen while a server is home, this
 * computer otherwise. While a server is home a small switcher also offers "my computer".
 */
export function BotComputerBlock({
  botName,
  botId
}: {
  botName?: string;
  botId?: string | null;
}) {
  const { t } = useTranslation();
  const { activeComputerId, activeHostName, home, workLocalNow, hostState } = useDaemon();
  const both = hasBothComputers(home, workLocalNow);
  const [side, setSide] = useState<ComputerSide>("bot");
  const showingMine = both && side === "mine";
  const onHost = !showingMine && activeComputerId !== LOCAL_COMPUTER;
  const serverName = activeHostName ?? t("computerPicker.host");
  const homeView = useLiveView();
  const localView = useLocalLiveView();
  const view = showingMine ? localView : homeView;
  // This computer's screen is only streamed while it is the one on show.
  const setLocalDemand = localView.setThumbnailDemand;
  useEffect(() => {
    if (!showingMine) {
      return undefined;
    }
    setLocalDemand(true, botId ?? null);
    return () => setLocalDemand(false, null);
  }, [showingMine, botId, setLocalDemand]);
  const {
    selectedTarget,
    targetsStatus,
    availability,
    state,
    frameUrl,
    thumbUrl,
    pausedIdle,
    panelAction,
    openViewer,
    releaseTakeover,
    retryRelease,
    controlState,
    recoveryPrompt,
    recoverTakeover,
    clearPause,
    isLocalClient
  } = view;
  const panelActions = visibleActions(state, controlState, isLocalClient).panel;
  // Control can outlive the viewer (a failed hand-back keeps it): never leave the
  // bot paused with only a status pill, always offer Bitirdim here too.
  const releasePending = state.pendingRequest?.kind === "release";
  const showRelease = isActivelyOwningTakeover(state) || releasePending;

  const summary = selectedTarget ? summarizeTarget(selectedTarget) : null;
  const tone = panelTone(
    Boolean(selectedTarget),
    state.viewState,
    Boolean(summary && !summary.available)
  );

  const panel = computerPanel({
    hasTarget: Boolean(selectedTarget),
    available: Boolean(summary?.available),
    reason: summary?.reason ?? null,
    viewState: state.viewState,
    hasFrame: Boolean(frameUrl),
    targetsStatus,
    noScreen: noScreenCode(availability)
  });
  // Header pill and preview come from one model; the reason is a code the
  // daemon serves, worded here in the user's language.
  const say = (text: PanelText) =>
    t(text.key, {
      detail: text.detailKey ? t(text.detailKey) : "",
      defaultValue: t("liveView.reasons.unavailable")
    });

  const KindIcon = selectedTarget?.kind === "browser" ? Globe : Monitor;

  const actionPercent =
    panelAction?.rect && state.latestFrame?.sourceRect
      ? {
          left: `${Math.min(
            95,
            Math.max(
              5,
              ((panelAction.rect.x + panelAction.rect.width / 2) /
                Math.max(1, state.latestFrame.sourceRect.width)) *
                100
            )
          )}%`,
          top: `${Math.min(
            95,
            Math.max(
              5,
              ((panelAction.rect.y + panelAction.rect.height / 2) /
                Math.max(1, state.latestFrame.sourceRect.height)) *
                100
            )
          )}%`
        }
      : null;

  const issue = onHost ? serverScreenIssue(hostState) : null;
  const desktopCommand = onHost
    ? serverDesktopCommand(noScreenCode(availability), serverName)
    : null;
  // The server answered but would not list screens. Never a server command here: the paired
  // administrator desktop may watch the screen by default, so this is a plain failure to retry.
  const listFailed = onHost && !issue && targetsStatus === "failed";
  // Nothing is streaming while the card is only visible: a still image (at most one a minute)
  // or a placeholder that starts the live view when clicked.
  const shownImage = frameUrl ?? thumbUrl;
  const idleText = pausedIdle
    ? t("botPanel.pausedIdle")
    : summary?.available
      ? t("botPanel.showScreen")
      : null;

  return (
    <section className={styles.block} aria-labelledby="bot-panel-computer">
      <div className={styles.head}>
        <h3 className={styles.label} id="bot-panel-computer">
          {t("botPanel.computer")}
        </h3>
        <span
          className={styles.where}
          data-testid="bot-computer-where"
          data-computer={onHost ? "host" : showingMine ? "mine" : "local"}
        >
          {onHost
            ? t("botPanel.botsComputer", { bot: botName ?? t("botPanel.thisBot"), name: serverName })
            : showingMine
              ? t("botPanel.myComputer")
              : t("botPanel.runsOnLocal")}
        </span>
        <span className={styles.statusPill} data-testid="bot-computer-status">
          <i className={styles.dot} data-tone={tone} aria-hidden="true" />
          <span>{say(panel.pill)}</span>
        </span>
      </div>

      {both ? (
        <div className={styles.switcher} role="group" aria-label={t("botPanel.switcher")}>
          {(["bot", "mine"] as const).map((s) => (
            <button
              key={s}
              type="button"
              className={styles.switchBtn}
              aria-pressed={side === s}
              data-testid={`bot-computer-side-${s}`}
              onClick={() => setSide(s)}
            >
              {t(s === "bot" ? "botPanel.sideBot" : "botPanel.sideMine")}
            </button>
          ))}
        </div>
      ) : null}

      {issue ? (
        <div className={styles.note} data-testid="bot-computer-issue" data-issue={issue}>
          {t(`botPanel.serverIssue.${issue}`, { name: serverName })}
        </div>
      ) : (
      <button
        type="button"
        className={styles.card}
        data-testid="bot-computer-open"
        aria-label={t("liveView.expand")}
        onClick={() => openViewer(selectedTarget?.target_id)}
      >
        <div className={styles.preview}>
          {shownImage ? (
            <img
              className={styles.thumb}
              src={shownImage}
              alt={t("liveView.stageAlt", {
                target: selectedTarget
                  ? t(`liveView.kind.${selectedTarget.kind}`)
                  : t("botPanel.computer")
              })}
              draggable={false}
            />
          ) : (
            <div className={styles.placeholder}>
              <KindIcon aria-hidden="true" />
              <span data-testid="bot-computer-idle">{idleText ?? say(panel.preview)}</span>
            </div>
          )}
          {actionPercent ? (
            <span
              className={styles.actionDot}
              style={actionPercent}
              aria-hidden="true"
            />
          ) : null}
          <span className={styles.expandTag} aria-hidden="true">
            <Maximize2 />
          </span>
        </div>

        <div className={styles.footer}>
          <span className={styles.meta}>
            <KindIcon aria-hidden="true" />
            <span className={styles.host}>
              {selectedTarget
                ? `${t(`liveView.kind.${selectedTarget.kind}`)} · ${onHost ? serverName : selectedTarget.host_id}`
                : t("botPanel.computer")}
            </span>
          </span>
          <span className={styles.cap}>
            {t(COMPUTER_FOOTER_KEY[computerFooter(summary, targetsStatus)])}
          </span>
        </div>
      </button>
      )}

      {desktopCommand && !selectedTarget ? (
        <div className={styles.note} data-testid="bot-computer-desktop-hint">
          {t("botPanel.needsDesktop", { name: serverName })}
          <code className={styles.command}>{desktopCommand}</code>
        </div>
      ) : null}
      {listFailed ? (
        <div className={styles.note} data-testid="bot-computer-list-failed">
          {t("botPanel.screenListFailed", { name: serverName })}
        </div>
      ) : null}

      {panelActions.includes("open_screen") && summary?.available && !showRelease && !issue ? (
        <div className={styles.release}>
          <Button
            variant="secondary"
            size="sm"
            data-testid="bot-computer-open-screen"
            onClick={() => openViewer(selectedTarget?.target_id)}
          >
            <Maximize2 aria-hidden="true" />
            {t("liveView.openScreen")}
          </Button>
        </div>
      ) : null}

      {panelActions.includes("clear_pause") && !showRelease ? (
        <div className={styles.release} data-testid="bot-computer-paused-actions">
          {recoveryPrompt?.mode === "reacquire" ? (
            <Button
              variant="primary"
              size="sm"
              data-testid="bot-computer-reacquire"
              onClick={() => void recoverTakeover()}
            >
              {t("liveView.reacquireAction")}
            </Button>
          ) : null}
          <Button
            variant={recoveryPrompt?.mode === "reacquire" ? "secondary" : "primary"}
            size="sm"
            data-testid="bot-computer-clear-pause"
            onClick={() => void clearPause("own")}
          >
            {t("liveView.clearPauseAction")}
          </Button>
        </div>
      ) : null}

      {showRelease ? (
        <div className={styles.release}>
          <Button
            variant="primary"
            size="sm"
            data-testid="bot-computer-release"
            onClick={() =>
              void (releasePending && state.lastError
                ? retryRelease()
                : releaseTakeover(null))
            }
            disabled={releasePending && !state.lastError}
          >
            {releasePending && state.lastError
              ? t("liveView.retryDone")
              : releasePending
                ? t("liveView.retryingDone")
                : t("liveView.doneAction")}
          </Button>
        </div>
      ) : null}
    </section>
  );
}
