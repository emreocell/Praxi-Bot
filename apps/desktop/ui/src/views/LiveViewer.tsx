import {
  ArrowLeft,
  Globe,
  Monitor,
  OctagonX,
  RefreshCw,
  ShieldCheck
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FocusEvent
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { HumanAction, TargetInfo } from "../api/types";
import {
  computeContainedImageRect,
  mapClientPointToNormalized
} from "../lib/liveViewInput";
import {
  createStageController,
  focusLeftScope,
  type StageDeps
} from "../lib/liveViewStage";
import {
  canSendLiveInput,
  isControlBusy,
  isActivelyOwningTakeover,
  summarizeTarget
} from "../lib/liveViewState";
import { layerStack } from "../lib/modalStack";
import { useDaemon } from "../state/DaemonContext";
import { useLiveView } from "../state/LiveViewContext";
import styles from "./LiveViewer.module.css";

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function targetOptionLabel(
  tInfo: TargetInfo,
  botName: string | null,
  translate: (k: string, opts?: Record<string, unknown>) => string
): string {
  const kind = translate(`liveView.kind.${tInfo.kind}`);
  const summary = summarizeTarget(tInfo);
  const parts = [`${kind} · ${tInfo.host_id}`];
  if (botName) {
    parts.push(botName);
  }
  if (!summary.available && summary.reason) {
    parts.push(translate("liveView.states.unavailable"));
  } else if (!summary.canInput) {
    parts.push(translate("liveView.viewOnly"));
  }
  return parts.join(" — ");
}

export function LiveViewer() {
  const { t } = useTranslation();
  const { bots, needs } = useDaemon();
  const {
    targets,
    selectedTarget,
    controlState,
    state,
    frameUrl,
    viewerOpen,
    loadingTargets,
    panelAction,
    recoveryPrompt,
    closeViewer,
    selectTarget,
    refreshTargets,
    requestTakeover,
    releaseTakeover,
    retryRelease,
    recoverTakeover,
    overrideStop,
    enqueueInput,
    resetInputState,
    setComposing
  } = useLiveView();

  const [selectedOutcome, setSelectedOutcome] = useState<HumanAction | "none">(
    "none"
  );
  const [stageFocused, setStageFocused] = useState(false);
  const [captureTab, setCaptureTab] = useState(false);
  const [imeBuffer, setImeBuffer] = useState("");
  const [stageBox, setStageBox] = useState<{ w: number; h: number }>({
    w: 0,
    h: 0
  });

  const stageRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const imeInputRef = useRef<HTMLTextAreaElement | null>(null);
  const doneButtonRef = useRef<HTMLButtonElement | null>(null);
  const stageDepsRef = useRef<StageDeps | null>(null);
  // Handler objects are stable (pointer/key bookkeeping lives in the
  // controller); the latest deps are read through the ref so they never go stale.
  const stageController = useMemo(
    () => createStageController(() => stageDepsRef.current as StageDeps),
    []
  );
  const backdropRef = useRef<HTMLDivElement | null>(null);
  const layerSeqRef = useRef(0);

  const botNames = useMemo(
    () => new Map(bots.map((b) => [b.id, b.name])),
    [bots]
  );

  // Measure rendered container so contained panel overlays align accurately.
  useEffect(() => {
    if (!viewerOpen || !stageRef.current) {
      return undefined;
    }
    const el = stageRef.current;
    const updateSize = () => {
      const r = el.getBoundingClientRect();
      setStageBox({ w: r.width, h: r.height });
    };
    updateSize();
    if (typeof ResizeObserver === "undefined") {
      return undefined;
    }
    const obs = new ResizeObserver(updateSize);
    obs.observe(el);
    return () => obs.disconnect();
  }, [viewerOpen, frameUrl]);

  // Reset captured pointer/key state if stage ceases to allow input.
  const inputGate = canSendLiveInput(state, Date.now());
  const isInteractive = inputGate.ok;

  const isInteractiveRef = useRef(isInteractive);
  isInteractiveRef.current = isInteractive;
  const captureTabRef = useRef(captureTab);
  captureTabRef.current = captureTab;
  const stageBoxRef = useRef(stageBox);
  stageBoxRef.current = stageBox;

  useEffect(() => {
    if (!isInteractive) {
      stageController.reset();
      setStageFocused(false);
      setCaptureTab(false);
    }
  }, [isInteractive, stageController]);

  // Allow Escape to close viewer only when not actively driving remote screen.
  useEffect(() => {
    if (!viewerOpen) {
      return undefined;
    }
    const onGlobalKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape" && !e.shiftKey && !isInteractive && !stageFocused) {
        e.preventDefault();
        void closeViewer();
      }
    };
    window.addEventListener("keydown", onGlobalKey);
    return () => window.removeEventListener("keydown", onGlobalKey);
  }, [viewerOpen, isInteractive, stageFocused, closeViewer]);

  const mapPointer = useCallback(
    (clientX: number, clientY: number, clamp: boolean) => {
      const img = imgRef.current;
      const frame = state.latestFrame;
      if (!img || !frame) {
        return null;
      }
      const natW = img.naturalWidth || frame.width;
      const natH = img.naturalHeight || frame.height;
      return mapClientPointToNormalized(
        clientX,
        clientY,
        img.getBoundingClientRect(),
        natW,
        natH,
        { clamp }
      );
    },
    [state.latestFrame]
  );

  const exitKeyboardFocus = () => {
    resetInputState(true);
    setStageFocused(false);
    doneButtonRef.current?.focus();
  };

  stageDepsRef.current = {
    isInteractive: () => isInteractiveRef.current,
    captureTab: () => captureTabRef.current,
    mapPointer,
    stageSize: () => stageBoxRef.current,
    enqueue: enqueueInput,
    resetInput: () => resetInputState(true),
    focusIme: () => imeInputRef.current?.focus({ preventScroll: true }),
    markFocused: setStageFocused,
    exitKeyboardFocus,
    setComposing,
    clearImeBuffer: () => setImeBuffer("")
  };

  // Register as a layer so a Modal/Popover opened above (z-index 50/60) keeps
  // Esc/Tab to itself, and trap Tab inside the viewer while it is topmost.
  if (viewerOpen && layerSeqRef.current === 0) {
    layerSeqRef.current = layerStack.next();
  }
  if (!viewerOpen) {
    layerSeqRef.current = 0;
  }
  useEffect(() => {
    if (!viewerOpen) {
      return undefined;
    }
    const seq = layerSeqRef.current;
    layerStack.add(seq);
    const onKey = (e: globalThis.KeyboardEvent) => {
      const node = backdropRef.current;
      if (e.key !== "Tab" || !node || !layerStack.isTop(seq)) {
        return;
      }
      const items = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (items.length === 0) {
        e.preventDefault();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      if (!node.contains(active)) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      layerStack.remove(seq);
    };
  }, [viewerOpen]);

  if (!viewerOpen || typeof document === "undefined") {
    return null;
  }

  const targetSummary = selectedTarget ? summarizeTarget(selectedTarget) : null;
  const isReleasing = state.pendingRequest?.kind === "release";
  const otherOwner =
    state.takeover &&
    state.ownedTakeoverId !== state.takeover.id &&
    (state.takeover.state === "active" || state.takeover.state === "acquiring")
      ? state.takeover.owner
      : null;

  const tone = state.disconnected || state.streamStale
    ? "paused"
    : state.viewState === "unavailable" || !selectedTarget
      ? "unavailable"
      : state.viewState;

  const controlBusy = isControlBusy(state);
  const ownsActive =
    isActivelyOwningTakeover(state) && state.viewState === "active";

  const badgeLabel = isReleasing
    ? t("liveView.states.finishing")
    : state.disconnected
      ? t("liveView.states.disconnected")
      : state.streamStale
        ? t("liveView.states.stale")
        : t(`liveView.states.${state.viewState}`);

  const contained =
    state.latestFrame && stageBox.w > 0 && stageBox.h > 0
      ? computeContainedImageRect(
          stageBox.w,
          stageBox.h,
          state.latestFrame.width,
          state.latestFrame.height
        )
      : null;

  const actionOverlayStyle =
    panelAction?.rect && contained && state.latestFrame?.sourceRect
      ? {
          left: `${
            contained.x +
            (panelAction.rect.x / Math.max(1, state.latestFrame.sourceRect.width)) *
              contained.width
          }px`,
          top: `${
            contained.y +
            (panelAction.rect.y / Math.max(1, state.latestFrame.sourceRect.height)) *
              contained.height
          }px`,
          width: `${Math.max(
            24,
            (panelAction.rect.width /
              Math.max(1, state.latestFrame.sourceRect.width)) *
              contained.width
          )}px`,
          height: `${Math.max(
            20,
            (panelAction.rect.height /
              Math.max(1, state.latestFrame.sourceRect.height)) *
              contained.height
          )}px`
        }
      : null;

  const unavailableReason =
    state.reason ?? targetSummary?.reason ?? (selectedTarget ? null : "unavailable");

  const canTakeover =
    Boolean(selectedTarget) &&
    Boolean(targetSummary?.available) &&
    Boolean(targetSummary?.canInput) &&
    !state.disconnected &&
    !otherOwner &&
    state.viewState === "watch" &&
    state.pendingRequest === null;

  const KindIcon = selectedTarget?.kind === "browser" ? Globe : Monitor;

  return createPortal(
    <div
      className={styles.backdrop}
      role="dialog"
      aria-modal="true"
      aria-label={t("liveView.title")}
      data-testid="live-viewer"
      ref={backdropRef}
    >
      <header className={styles.header}>
        <div className={styles.headerLeft}>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void closeViewer()}
            disabled={isReleasing}
            title={ownsActive ? t("liveView.releaseOnClose") : undefined}
            data-testid="live-viewer-close"
          >
            <ArrowLeft />
            <span>{t("liveView.backToChat")}</span>
          </Button>

          <div className={styles.targetPicker}>
            <KindIcon aria-hidden="true" />
            <select
              className={styles.select}
              aria-label={t("liveView.targetsLabel")}
              data-testid="live-viewer-target-select"
              value={selectedTarget?.target_id ?? ""}
              disabled={controlBusy}
              title={controlBusy ? t("liveView.targetLocked") : undefined}
              onChange={(e) => selectTarget(e.target.value)}
            >
              {targets.length === 0 ? (
                <option value="">{t("liveView.noTargets")}</option>
              ) : (
                targets.map((trg) => (
                  <option key={trg.target_id} value={trg.target_id}>
                    {targetOptionLabel(
                      trg,
                      trg.bot_id ? botNames.get(trg.bot_id) ?? null : null,
                      t
                    )}
                  </option>
                ))
              )}
            </select>
            <button
              type="button"
              className={styles.iconBtn}
              title={t("liveView.refresh")}
              aria-label={t("liveView.refresh")}
              disabled={loadingTargets}
              onClick={() => void refreshTargets()}
            >
              <RefreshCw />
            </button>
          </div>
        </div>

        <div className={styles.headerRight}>
          <span
            className={styles.stateBadge}
            data-tone={tone}
            data-testid="live-viewer-state-badge"
          >
            <i className={styles.stateDot} aria-hidden="true" />
            <span>{badgeLabel}</span>
          </span>
          <Button
            variant="danger"
            size="sm"
            onClick={() => void overrideStop()}
            data-testid="live-viewer-stop"
          >
            <OctagonX />
            <span>{t("liveView.emergencyStop")}</span>
          </Button>
        </div>
      </header>

      <div className={styles.noticeBar}>
        <span className={styles.noticeLeft}>
          <ShieldCheck aria-hidden="true" />
          <span>{t("liveView.privacyNotice")}</span>
          <span>·</span>
          <span>{t("liveView.globalPauseNotice")}</span>
          {otherOwner ? (
            <>
              <span>·</span>
              <strong>{t("liveView.otherOwner", { owner: otherOwner })}</strong>
            </>
          ) : null}
        </span>
        {state.lastError ? (
          <span className={styles.errorInline} role="alert">
            {t(`liveView.errors.${state.lastError.code}`, {
              defaultValue: t(`liveView.reasons.${state.lastError.code}`, {
                defaultValue: t("liveView.errors.generic")
              })
            })}
          </span>
        ) : state.disconnected ? (
          <span className={styles.errorInline}>{t("liveView.reconnecting")}</span>
        ) : state.needsResync ? (
          <span className={styles.errorInline}>
            {t("liveView.reasons.resync_required")}
          </span>
        ) : null}
      </div>

      {needs.length > 0 ? (
        <div
          className={styles.approvalBar}
          role="status"
          data-testid="live-viewer-approvals"
        >
          <span>{t("liveView.approvalsWaiting", { count: needs.length })}</span>
          <Button
            variant="secondary"
            size="sm"
            disabled={isReleasing}
            onClick={() => void closeViewer()}
            data-testid="live-viewer-approvals-open"
          >
            {t("liveView.reviewApprovals")}
          </Button>
        </div>
      ) : null}

      <div className={styles.stageArea}>
        <div
          className={styles.stageFrame}
          data-active={isInteractive ? "true" : "false"}
        >
          {frameUrl && (!targetSummary || targetSummary.available) ? (
            <div
              ref={stageRef}
              className={styles.stageSurface}
              data-interactive={isInteractive ? "true" : "false"}
              data-testid="live-viewer-stage"
              tabIndex={isInteractive ? 0 : -1}
              role={isInteractive ? "application" : undefined}
              aria-roledescription={isInteractive ? "remote screen" : undefined}
              aria-label={
                isInteractive
                  ? t("liveView.stageHint")
                  : t("liveView.stageAlt", {
                      target: selectedTarget
                        ? t(`liveView.kind.${selectedTarget.kind}`)
                        : t("botPanel.computer")
                    })
              }
              onFocus={(e: FocusEvent<HTMLDivElement>) => {
                if (isInteractive) {
                  setStageFocused(true);
                  if (e.target === e.currentTarget) {
                    imeInputRef.current?.focus({ preventScroll: true });
                  }
                }
              }}
              onBlur={(e: FocusEvent<HTMLDivElement>) => {
                if (focusLeftScope(e.relatedTarget, stageRef.current)) {
                  setStageFocused(false);
                  stageController.reset();
                  resetInputState(true);
                }
              }}
              {...stageController.stage}
            >
              <img
                ref={imgRef}
                className={styles.stageImg}
                src={frameUrl}
                alt={t("liveView.stageAlt", {
                  target: selectedTarget
                    ? t(`liveView.kind.${selectedTarget.kind}`)
                    : t("botPanel.computer")
                })}
                draggable={false}
              />

              {isInteractive ? (
                <textarea
                  ref={imeInputRef}
                  className={styles.imeCapture}
                  aria-label={t("liveView.stageHint")}
                  tabIndex={-1}
                  value={imeBuffer}
                  onChange={(e) => setImeBuffer(e.target.value)}
                  {...stageController.ime}
                />
              ) : null}

              {isInteractive && stageFocused ? (
                <div
                  className={styles.focusOverlayBar}
                  data-testid="live-viewer-focus-bar"
                  {...stageController.toolbar}
                >
                  <span>{t("liveView.focusTrapHint")}</span>
                  <button
                    type="button"
                    className={styles.tabToggle}
                    aria-pressed={captureTab}
                    onClick={() => setCaptureTab((v) => !v)}
                  >
                    {t("liveView.captureTab")}
                  </button>
                  <button
                    type="button"
                    className={styles.tabToggle}
                    onClick={exitKeyboardFocus}
                  >
                    {t("liveView.releaseFocus")}
                  </button>
                </div>
              ) : null}

              {actionOverlayStyle && panelAction ? (
                <div
                  className={styles.actionBox}
                  style={actionOverlayStyle}
                  aria-hidden="true"
                >
                  <span className={styles.actionTag}>
                    {panelAction.label || panelAction.action}
                  </span>
                </div>
              ) : null}
            </div>
          ) : (
            <div className={styles.emptyStage} data-testid="live-viewer-unavailable">
              <KindIcon aria-hidden="true" />
              <p className={styles.emptyTitle}>
                {selectedTarget
                  ? t(`liveView.kind.${selectedTarget.kind}`)
                  : t("botPanel.computer")}
                {selectedTarget ? ` · ${selectedTarget.host_id}` : ""}
              </p>
              <p className={styles.emptyBody}>
                {unavailableReason
                  ? t(`liveView.reasons.${unavailableReason}`, {
                      defaultValue: t("liveView.reasons.unavailable")
                    })
                  : t("liveView.noTargets")}
              </p>
              <Button
                variant="secondary"
                size="sm"
                onClick={() => void refreshTargets()}
              >
                <RefreshCw />
                <span>{t("liveView.refresh")}</span>
              </Button>
            </div>
          )}
        </div>
      </div>

      <div className={styles.bottomDock} data-testid="live-view-bar">
        {ownsActive || isReleasing ? (
          <>
            <span className={styles.dockStatus}>
              {isReleasing
                ? t("liveView.retryingDone")
                : t("liveView.states.active")}
            </span>
            {!isReleasing ? (
              <select
                className={styles.outcomeSelect}
                aria-label={t("liveView.outcomeHint")}
                value={selectedOutcome}
                onChange={(e) =>
                  setSelectedOutcome(e.target.value as HumanAction | "none")
                }
              >
                <option value="none">{t("liveView.outcomes.none")}</option>
                <option value="login_completed">
                  {t("liveView.outcomes.login_completed")}
                </option>
                <option value="application_opened">
                  {t("liveView.outcomes.application_opened")}
                </option>
                <option value="other">{t("liveView.outcomes.other")}</option>
              </select>
            ) : null}
            <div className={styles.dockActions}>
              {isReleasing && state.lastError ? (
                <Button
                  ref={doneButtonRef}
                  variant="primary"
                  size="sm"
                  data-testid="live-view-retry-done"
                  onClick={() => void retryRelease()}
                >
                  {t("liveView.retryDone")}
                </Button>
              ) : (
                <Button
                  ref={doneButtonRef}
                  variant="primary"
                  size="sm"
                  disabled={isReleasing}
                  data-testid="live-view-done"
                  onClick={() =>
                    void releaseTakeover(
                      selectedOutcome === "none" ? null : selectedOutcome
                    )
                  }
                >
                  {t("liveView.doneAction")}
                </Button>
              )}
            </div>
          </>
        ) : state.viewState === "acquiring" ||
          state.pendingRequest?.kind === "takeover" ? (
          <>
            <span className={styles.dockStatus}>
              {t("liveView.states.acquiring")}
            </span>
            <Button variant="secondary" size="sm" disabled>
              {t("liveView.states.acquiring")}
            </Button>
          </>
        ) : state.viewState === "paused" && recoveryPrompt ? (
          <>
            <span className={styles.dockStatus}>
              {t(`liveView.reasons.${recoveryPrompt.reason}`, {
                defaultValue: t("liveView.states.paused")
              })}
            </span>
            <Button
              variant="primary"
              size="sm"
              data-testid="live-view-recover"
              onClick={() => void recoverTakeover()}
            >
              {recoveryPrompt.mode === "reacquire"
                ? t("liveView.reacquireAction")
                : recoveryPrompt.mode === "clear_pause"
                  ? t("liveView.clearPauseAction")
                  : t("liveView.recoverAction")}
            </Button>
          </>
        ) : (
          <>
            <span className={styles.dockStatus}>
              {state.disconnected
                ? t("liveView.reasons.disconnected")
                : otherOwner
                  ? t("liveView.otherOwner", { owner: otherOwner })
                  : selectedTarget && targetSummary?.available
                    ? t("liveView.states.watch")
                    : t("liveView.states.unavailable")}
            </span>
            <Button
              variant="primary"
              size="sm"
              disabled={!canTakeover}
              data-testid="live-view-takeover"
              onClick={() => void requestTakeover()}
            >
              {t("liveView.watchAction")}
            </Button>
            {controlState?.latched === "paused" && !state.takeover ? (
              <Button
                variant="secondary"
                size="sm"
                onClick={() => void recoverTakeover()}
              >
                {t("liveView.clearPauseAction")}
              </Button>
            ) : null}
          </>
        )}
      </div>
    </div>,
    document.body
  );
}
