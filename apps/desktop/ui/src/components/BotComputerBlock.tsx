import { Globe, Maximize2, Monitor } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { isActivelyOwningTakeover, summarizeTarget } from "../lib/liveViewState";
import { useLiveView } from "../state/LiveViewContext";
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

export function BotComputerBlock() {
  const { t } = useTranslation();
  const {
    selectedTarget,
    state,
    frameUrl,
    panelAction,
    openViewer,
    releaseTakeover,
    retryRelease
  } = useLiveView();
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

  const summaryKey = !selectedTarget
    ? "liveView.panelSummary.empty"
    : state.viewState === "active"
      ? "liveView.panelSummary.active"
      : state.viewState === "acquiring"
        ? "liveView.panelSummary.acquiring"
        : state.viewState === "paused"
          ? "liveView.panelSummary.paused"
          : summary && !summary.available
            ? "liveView.panelSummary.unavailable"
            : "liveView.panelSummary.watch";

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

  return (
    <section className={styles.block} aria-labelledby="bot-panel-computer">
      <div className={styles.head}>
        <h3 className={styles.label} id="bot-panel-computer">
          {t("botPanel.computer")}
        </h3>
        <span className={styles.statusPill} data-testid="bot-computer-status">
          <i className={styles.dot} data-tone={tone} aria-hidden="true" />
          <span>{t(summaryKey)}</span>
        </span>
      </div>

      <button
        type="button"
        className={styles.card}
        data-testid="bot-computer-open"
        aria-label={t("liveView.expand")}
        onClick={() => openViewer(selectedTarget?.target_id)}
      >
        <div className={styles.preview}>
          {frameUrl ? (
            <img
              className={styles.thumb}
              src={frameUrl}
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
              <span>
                {summary?.reason
                  ? t(`liveView.reasons.${summary.reason}`, {
                      defaultValue: t("liveView.reasons.unavailable")
                    })
                  : t("liveView.noTargets")}
              </span>
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
                ? `${t(`liveView.kind.${selectedTarget.kind}`)} · ${selectedTarget.host_id}`
                : t("botPanel.computer")}
            </span>
          </span>
          <span className={styles.cap}>
            {summary?.available
              ? summary.canInput
                ? t("liveView.interactive")
                : t("liveView.viewOnly")
              : t("liveView.states.unavailable")}
          </span>
        </div>
      </button>

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
