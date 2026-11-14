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
  type CompositionEvent,
  type KeyboardEvent,
  type PointerEvent,
  type WheelEvent
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { HumanAction, TargetInfo } from "../api/types";
import {
  clampButtonsMask,
  classifyKeyDown,
  classifyKeyUp,
  computeContainedImageRect,
  domButtonToInputButton,
  extractModifiers,
  mapClientPointToNormalized,
  normalizeWheelDelta
} from "../lib/liveViewInput";
import { canSendLiveInput, summarizeTarget } from "../lib/liveViewState";
import { useDaemon } from "../state/DaemonContext";
import { useLiveView } from "../state/LiveViewContext";
import styles from "./LiveViewer.module.css";

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
  const { bots } = useDaemon();
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
  const activePointerIdRef = useRef<number | null>(null);

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

  useEffect(() => {
    if (!isInteractive) {
      activePointerIdRef.current = null;
      setStageFocused(false);
      setCaptureTab(false);
    }
  }, [isInteractive]);

  // Allow Escape to close viewer only when not actively driving remote screen.
  useEffect(() => {
    if (!viewerOpen) {
      return undefined;
    }
    const onGlobalKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape" && !e.shiftKey && !isInteractive && !stageFocused) {
        e.preventDefault();
        closeViewer();
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

  const handlePointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (!isInteractive) {
      return;
    }
    const btn = domButtonToInputButton(e.button);
    if (!btn) {
      return;
    }
    const pt = mapPointer(e.clientX, e.clientY, false);
    if (!pt) {
      return;
    }
    e.preventDefault();
    imeInputRef.current?.focus({ preventScroll: true });
    setStageFocused(true);
    activePointerIdRef.current = e.pointerId;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // synthetic/untrusted pointer events in tests may not carry native capture
    }
    enqueueInput({
      type: "pointer",
      phase: "down",
      x: pt.x,
      y: pt.y,
      button: btn,
      buttons: clampButtonsMask(e.buttons),
      modifiers: extractModifiers(e)
    });
  };

  const handlePointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (!isInteractive) {
      return;
    }
    const dragging = activePointerIdRef.current === e.pointerId;
    const pt = mapPointer(e.clientX, e.clientY, dragging);
    if (!pt) {
      return;
    }
    enqueueInput({
      type: "pointer",
      phase: "move",
      x: pt.x,
      y: pt.y,
      button: null,
      buttons: clampButtonsMask(e.buttons),
      modifiers: extractModifiers(e)
    });
  };

  const handlePointerUp = (e: PointerEvent<HTMLDivElement>) => {
    if (!isInteractive) {
      return;
    }
    const btn = domButtonToInputButton(e.button);
    if (!btn) {
      return;
    }
    const dragging = activePointerIdRef.current === e.pointerId;
    const pt = mapPointer(e.clientX, e.clientY, dragging);
    if (dragging) {
      activePointerIdRef.current = null;
      try {
        if (e.currentTarget.hasPointerCapture(e.pointerId)) {
          e.currentTarget.releasePointerCapture(e.pointerId);
        }
      } catch {
        // ignore release error
      }
    }
    if (!pt) {
      return;
    }
    enqueueInput({
      type: "pointer",
      phase: "up",
      x: pt.x,
      y: pt.y,
      button: btn,
      buttons: clampButtonsMask(e.buttons),
      modifiers: extractModifiers(e)
    });
  };

  const handleWheel = (e: WheelEvent<HTMLDivElement>) => {
    if (!isInteractive) {
      return;
    }
    const pt = mapPointer(e.clientX, e.clientY, false);
    if (!pt) {
      return;
    }
    enqueueInput({
      type: "wheel",
      x: pt.x,
      y: pt.y,
      delta_x: normalizeWheelDelta(e.deltaX, e.deltaMode, stageBox.w || 800),
      delta_y: normalizeWheelDelta(e.deltaY, e.deltaMode, stageBox.h || 600),
      modifiers: extractModifiers(e)
    });
  };

  const exitKeyboardFocus = () => {
    resetInputState(true);
    setStageFocused(false);
    doneButtonRef.current?.focus();
  };

  const handleStageKeyDown = (
    e: KeyboardEvent<HTMLDivElement | HTMLTextAreaElement>
  ) => {
    if (!isInteractive) {
      return;
    }
    // Never trap keyboard-only users: Shift+Escape always exits stage to Bitirdim.
    if (e.shiftKey && e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      exitKeyboardFocus();
      return;
    }
    if (e.key === "Tab" && !captureTab) {
      return;
    }
    const events = classifyKeyDown(
      {
        code: e.code,
        key: e.key,
        location: e.location,
        repeat: e.repeat,
        altKey: e.altKey,
        ctrlKey: e.ctrlKey,
        metaKey: e.metaKey,
        shiftKey: e.shiftKey,
        isComposing: e.nativeEvent.isComposing
      },
      e.nativeEvent.isComposing
    );
    if (events.length > 0) {
      e.preventDefault();
      for (const ev of events) {
        enqueueInput(ev);
      }
    }
  };

  const handleStageKeyUp = (
    e: KeyboardEvent<HTMLDivElement | HTMLTextAreaElement>
  ) => {
    if (!isInteractive) {
      return;
    }
    if (e.key === "Tab" && !captureTab) {
      return;
    }
    const events = classifyKeyUp(
      {
        code: e.code,
        key: e.key,
        location: e.location,
        repeat: false,
        altKey: e.altKey,
        ctrlKey: e.ctrlKey,
        metaKey: e.metaKey,
        shiftKey: e.shiftKey,
        isComposing: e.nativeEvent.isComposing
      },
      e.nativeEvent.isComposing
    );
    if (events.length > 0) {
      e.preventDefault();
      for (const ev of events) {
        enqueueInput(ev);
      }
    }
  };

  const handleCompositionStart = () => {
    setComposing(true);
  };

  const handleCompositionEnd = (e: CompositionEvent<HTMLTextAreaElement>) => {
    setComposing(false);
    const composed = e.data;
    setImeBuffer("");
    if (isInteractive && composed) {
      enqueueInput({ type: "text", text: composed });
    }
  };

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

  const tone = state.disconnected
    ? "paused"
    : state.viewState === "unavailable" || !selectedTarget
      ? "unavailable"
      : state.viewState;

  const badgeLabel = isReleasing
    ? t("liveView.states.finishing")
    : state.disconnected
      ? t("liveView.states.disconnected")
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
    >
      <header className={styles.header}>
        <div className={styles.headerLeft}>
          <Button
            variant="ghost"
            size="sm"
            onClick={closeViewer}
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
            {state.lastError.message}
          </span>
        ) : state.needsResync ? (
          <span className={styles.errorInline}>
            {t("liveView.reasons.resync_required")}
          </span>
        ) : null}
      </div>

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
              onFocus={() => {
                if (isInteractive) {
                  setStageFocused(true);
                  imeInputRef.current?.focus({ preventScroll: true });
                }
              }}
              onBlur={(e) => {
                if (
                  e.relatedTarget !== imeInputRef.current &&
                  e.relatedTarget !== stageRef.current
                ) {
                  setStageFocused(false);
                  resetInputState(true);
                }
              }}
              onPointerDown={handlePointerDown}
              onMouseDown={(e) => {
                if (isInteractive) {
                  e.preventDefault();
                  imeInputRef.current?.focus({ preventScroll: true });
                }
              }}
              onPointerMove={handlePointerMove}
              onPointerUp={handlePointerUp}
              onPointerCancel={handlePointerUp}
              onWheel={handleWheel}
              onKeyDown={handleStageKeyDown}
              onKeyUp={handleStageKeyUp}
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
                  onCompositionStart={handleCompositionStart}
                  onCompositionEnd={handleCompositionEnd}
                  onKeyDown={handleStageKeyDown}
                  onKeyUp={handleStageKeyUp}
                  onFocus={() => {
                    if (isInteractive) {
                      setStageFocused(true);
                    }
                  }}
                  onBlur={(e) => {
                    if (
                      e.relatedTarget !== stageRef.current &&
                      e.relatedTarget !== imeInputRef.current
                    ) {
                      setStageFocused(false);
                      resetInputState(true);
                    }
                  }}
                />
              ) : null}

              {isInteractive && stageFocused ? (
                <div className={styles.focusOverlayBar}>
                  <span>{t("liveView.focusTrapHint")}</span>
                  <button
                    type="button"
                    className={styles.tabToggle}
                    aria-pressed={captureTab}
                    onMouseDown={(e) => e.stopPropagation()}
                    onClick={() => setCaptureTab((v) => !v)}
                  >
                    {t("liveView.captureTab")}
                  </button>
                  <button
                    type="button"
                    className={styles.tabToggle}
                    onMouseDown={(e) => e.stopPropagation()}
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
        {state.viewState === "active" || isReleasing ? (
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
