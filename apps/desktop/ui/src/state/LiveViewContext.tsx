import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState
} from "react";
import type {
  ControlStateDto,
  HumanAction,
  InputEvent,
  SessionInfo,
  TargetInfo
} from "../api/types.ts";
import { ApiError } from "../api/client.ts";
import { isMockMode } from "../api/mockMode.ts";
import {
  applyControlStateDto,
  applyFrameRendered,
  applyInputAck,
  applyRateLimited,
  applyReleaseConfirmed,
  applyReleaseFailed,
  applyReleaseStarted,
  applyResyncRequired,
  applySessionCreated,
  applyStatusRecord,
  applyStreamActivity,
  applyStreamDisconnected,
  applyStreamStalenessCheck,
  applyTakeoverAccepted,
  applyTakeoverFailed,
  applyTakeoverStarted,
  buildReleasePayload,
  buildSessionHeartbeat,
  buildTakeoverRequest,
  canSendLiveInput,
  computeWantSession,
  createInitialLiveViewState,
  describeRecoveryPrompt,
  isActivelyOwningTakeover,
  isControlBusy,
  pickDefaultTarget,
  planViewerClose,
  reconnectDelayMs,
  type RecoveryPrompt,
  shouldOverrideOnStop,
  type LiveViewControllerState
} from "../lib/liveViewState.ts";
import { runRelease } from "../lib/liveViewRelease.ts";
import {
  consumeLiveViewStream,
  type DecodedFrameRecord,
  type FramePresenter
} from "../lib/liveViewWire.ts";
import { LiveInputQueue } from "../lib/liveViewInput.ts";
import {
  createBridgeRegistry,
  type ControlBridge,
  type PanelActionHighlight
} from "./controlBridge.ts";
import { useDaemon } from "./DaemonContext.tsx";

const SESSION_HEARTBEAT_MS = 5000;
const STALE_CHECK_MS = 2000;
const FEEDBACK_THROTTLE_MS = 500;
const OVERLAY_ACTION_TTL_MS = 1400;

interface LiveViewContextValue {
  targets: TargetInfo[];
  selectedTarget: TargetInfo | null;
  controlState: ControlStateDto | null;
  state: LiveViewControllerState;
  frameUrl: string | null;
  viewerOpen: boolean;
  loadingTargets: boolean;
  panelAction: PanelActionHighlight | null;
  recoveryPrompt: RecoveryPrompt | null;
  openViewer: (targetId?: string) => void;
  /** Closes the viewer; while holding control it hands control back first. */
  closeViewer: () => Promise<void>;
  selectTarget: (targetId: string) => void;
  refreshTargets: () => Promise<void>;
  setThumbnailDemand: (active: boolean, botId: string | null) => void;
  requestTakeover: () => Promise<void>;
  releaseTakeover: (humanAction?: HumanAction | null) => Promise<void>;
  retryRelease: () => Promise<void>;
  recoverTakeover: () => Promise<void>;
  /**
   * Emergency stop through `POST /v1/control/override`. With `onlyIfControlled`
   * (the global Stop) it does nothing unless a screen takeover is held, so a plain
   * Stop never latches the whole daemon paused.
   */
  overrideStop: (opts?: { onlyIfControlled?: boolean }) => Promise<void>;
  enqueueInput: (event: InputEvent, flushNow?: boolean) => void;
  resetInputState: (flushNow?: boolean) => void;
  setComposing: (composing: boolean) => void;
  showPanelAction: (highlight: PanelActionHighlight) => void;
  registerBridge: (bridge: ControlBridge | null) => void;
}

const LiveViewContext = createContext<LiveViewContextValue | null>(null);

function initialMockViewerOpen(): boolean {
  if (!isMockMode() || typeof window === "undefined") {
    return false;
  }
  return new URLSearchParams(window.location.search).get("lv") === "open";
}

export function LiveViewProvider({ children }: { children: ReactNode }) {
  const { client, onDaemonEvent, bots } = useDaemon();

  const clientIdRef = useRef<string>(crypto.randomUUID());
  const controlStateRef = useRef<ControlStateDto | null>(null);
  // One registry for the provider's lifetime: `registerBridge` keeps its identity
  // across control-state refreshes, so the bridge (and its live runs) survives them.
  const registry = useMemo(
    () => createBridgeRegistry(() => controlStateRef.current),
    []
  );
  const inputQueueRef = useRef<LiveInputQueue>(new LiveInputQueue());
  const activeUrlsRef = useRef<Set<string>>(new Set());
  const lastFeedbackAtRef = useRef<number>(0);
  const lastGeometrySentRef = useRef<number | null>(null);
  const flushingInputRef = useRef<boolean>(false);
  const flushAgainRef = useRef<boolean>(false);

  const [targets, setTargets] = useState<TargetInfo[]>([]);
  const [loadingTargets, setLoadingTargets] = useState(false);
  const [selectedTargetId, setSelectedTargetId] = useState<string | null>(null);
  const [controlState, setControlStateRaw] = useState<ControlStateDto | null>(
    null
  );
  const setControlState = useCallback((dto: ControlStateDto | null) => {
    controlStateRef.current = dto;
    setControlStateRaw(dto);
  }, []);
  const [viewerOpen, setViewerOpen] = useState<boolean>(initialMockViewerOpen);
  const [thumbDemand, setThumbDemand] = useState<{
    active: boolean;
    botId: string | null;
  }>({ active: false, botId: null });
  const [documentHidden, setDocumentHidden] = useState<boolean>(() =>
    typeof document !== "undefined" ? document.visibilityState === "hidden" : false
  );
  const [frameUrl, setFrameUrl] = useState<string | null>(null);
  const [panelAction, setPanelAction] = useState<PanelActionHighlight | null>(
    null
  );

  const [state, setState] = useState<LiveViewControllerState>(() =>
    createInitialLiveViewState(null)
  );
  const stateRef = useRef(state);
  const updateState = useCallback(
    (
      updater:
        | LiveViewControllerState
        | ((prev: LiveViewControllerState) => LiveViewControllerState)
    ) => {
      const next =
        typeof updater === "function" ? updater(stateRef.current) : updater;
      stateRef.current = next;
      setState(next);
    },
    []
  );

  // Clean up all lingering Blob URLs on unmount.
  useEffect(() => {
    const urls = activeUrlsRef.current;
    return () => {
      for (const u of urls) {
        URL.revokeObjectURL(u);
      }
      urls.clear();
    };
  }, []);

  // Track visibility so hidden windows release active capture sessions (`DELETE`).
  useEffect(() => {
    if (typeof document === "undefined") {
      return undefined;
    }
    const onVis = () => {
      const hidden = document.visibilityState === "hidden";
      setDocumentHidden(hidden);
      if (hidden) {
        inputQueueRef.current.clear();
      }
    };
    document.addEventListener("visibilitychange", onVis);
    return () => document.removeEventListener("visibilitychange", onVis);
  }, []);

  // Auto-clear panel-local bot action overlay highlight after brief display.
  useEffect(() => {
    if (!panelAction) {
      return undefined;
    }
    const timer = window.setTimeout(() => {
      setPanelAction((cur) =>
        cur?.requestId === panelAction.requestId ? null : cur
      );
    }, OVERLAY_ACTION_TTL_MS);
    return () => window.clearTimeout(timer);
  }, [panelAction]);

  const showPanelAction = useCallback((highlight: PanelActionHighlight) => {
    setPanelAction(highlight);
  }, []);

  const registerBridge: (bridge: ControlBridge | null) => void =
    registry.register;

  useEffect(() => {
    registry.setOwnedTakeoverId(state.ownedTakeoverId);
  }, [registry, state.ownedTakeoverId]);

  const refreshTargets = useCallback(async () => {
    if (!client) {
      return;
    }
    setLoadingTargets(true);
    try {
      const [nextTargets, nextCtrl] = await Promise.all([
        client.listViewTargets(),
        client.getControlState().catch(() => null)
      ]);
      setTargets(nextTargets);
      if (nextCtrl) {
        setControlState(nextCtrl);
        registry.syncControlState(nextCtrl);
        updateState((prev) => applyControlStateDto(prev, nextCtrl));
      }
    } catch {
      // keep existing snapshot; next poll/action retries
    } finally {
      setLoadingTargets(false);
    }
  }, [client, registry, setControlState, updateState]);

  // Refresh targets & control snapshot on client connect and when panel/viewer opens.
  useEffect(() => {
    if (!client) {
      setTargets([]);
      setControlState(null);
      updateState(createInitialLiveViewState(null));
      return undefined;
    }
    void refreshTargets();
    return onDaemonEvent((env) => {
      if (env.event.type === "control_state_changed") {
        void refreshTargets();
      }
    });
  }, [client, onDaemonEvent, refreshTargets, setControlState, updateState]);

  useEffect(() => {
    if (client && (viewerOpen || thumbDemand.active)) {
      void refreshTargets();
    }
  }, [client, viewerOpen, thumbDemand.active, refreshTargets]);

  const selectedTarget = useMemo(() => {
    const botFocus =
      thumbDemand.botId ??
      (bots.length > 0 ? bots[0].id : null);
    return pickDefaultTarget(targets, {
      preferredTargetId: selectedTargetId,
      botId: botFocus
    });
  }, [targets, selectedTargetId, thumbDemand.botId, bots]);

  // While holding/acquiring control the target must not follow chat/bot focus.
  useEffect(() => {
    if (selectedTargetId === null && selectedTarget && isControlBusy(state)) {
      setSelectedTargetId(selectedTarget.target_id);
    }
  }, [selectedTargetId, selectedTarget, state]);

  // Keep target metadata reflected when target selection or capability changes cleanly.
  useEffect(() => {
    updateState((prev) => {
      if (!selectedTarget) {
        return createInitialLiveViewState(null);
      }
      if (
        prev.target?.target_id === selectedTarget.target_id &&
        prev.target?.unavailable_reason === selectedTarget.unavailable_reason &&
        prev.target?.generation === selectedTarget.generation
      ) {
        return prev;
      }
      return {
        ...prev,
        target: selectedTarget,
        viewState: selectedTarget.unavailable_reason
          ? "unavailable"
          : prev.session
            ? prev.viewState
            : "watch",
        reason: selectedTarget.unavailable_reason ?? prev.reason
      };
    });
  }, [selectedTarget, updateState]);

  // Determine whether a view session is currently needed and at what quality/profile.
  const wantSession = computeWantSession({
    hasClient: Boolean(client),
    documentHidden,
    target: selectedTarget,
    viewerOpen,
    panelActive: thumbDemand.active
  });

  const owning = isActivelyOwningTakeover(state);
  const desiredProfile = owning ? "live" : "watch";
  const desiredMaxWidth = viewerOpen ? 1600 : 960;
  const desiredMaxHeight = viewerOpen ? 1000 : 600;
  const desiredQuality = owning ? 78 : viewerOpen ? 72 : 55;

  const activeSessionRef = useRef<SessionInfo | null>(null);

  // Create/delete view session strictly based on panel/viewer demand and target identity.
  useEffect(() => {
    if (!wantSession || !client || !selectedTarget) {
      const stale = activeSessionRef.current;
      activeSessionRef.current = null;
      setFrameUrl(null);
      if (stale && client) {
        void client.deleteViewSession(stale.session_id).catch(() => undefined);
      }
      updateState((prev) =>
        prev.session
          ? { ...prev, session: null, inputAllowed: false }
          : prev
      );
      return undefined;
    }

    const abort = new AbortController();
    const inputQueue = inputQueueRef.current;
    let mountedSessionId: string | null = null;

    const sleep = (ms: number) =>
      new Promise<void>((resolve) => {
        const timer = window.setTimeout(resolve, ms);
        abort.signal.addEventListener(
          "abort",
          () => {
            window.clearTimeout(timer);
            resolve();
          },
          { once: true }
        );
      });

    // One connection attempt: new session, frame stream, until it ends or throws.
    const connectOnce = async (onHealthy: () => void): Promise<void> => {
      const session = await client.createViewSession({
        client_id: clientIdRef.current,
        target_id: selectedTarget.target_id,
        profile: isActivelyOwningTakeover(stateRef.current) ? "live" : "watch",
        codecs: selectedTarget.capabilities.codecs,
        max_width: viewerOpen ? 1600 : 960,
        max_height: viewerOpen ? 1000 : 600
      });
      if (abort.signal.aborted) {
        void client.deleteViewSession(session.session_id).catch(() => undefined);
        return;
      }
      mountedSessionId = session.session_id;
      activeSessionRef.current = session;
      lastFeedbackAtRef.current = 0;
      lastGeometrySentRef.current = null;
      updateState((prev) => applySessionCreated(prev, selectedTarget, session));

      const res = await client.openViewFrameStream(
        session.session_id,
        abort.signal
      );
      if (!res.body || abort.signal.aborted) {
        return;
      }

      const presenter: FramePresenter<string> = {
        decodeAndBind: async (frame: DecodedFrameRecord) => {
          const mime =
            frame.header.codec === "webp" ? "image/webp" : "image/jpeg";
          const blob = new Blob([new Uint8Array(frame.payload)], {
            type: mime
          });
          const url = URL.createObjectURL(blob);
          activeUrlsRef.current.add(url);
          try {
            const img = new Image();
            img.src = url;
            await img.decode().catch(() => undefined);
            return url;
          } catch (err) {
            activeUrlsRef.current.delete(url);
            URL.revokeObjectURL(url);
            throw err;
          }
        },
        release: (url: string) => {
          const revoke = () => {
            if (activeUrlsRef.current.has(url)) {
              activeUrlsRef.current.delete(url);
              URL.revokeObjectURL(url);
            }
          };
          if (abort.signal.aborted) {
            revoke();
          } else {
            window.requestAnimationFrame(revoke);
          }
        }
      };

      await consumeLiveViewStream({
        stream: res.body,
        presenter,
        signal: abort.signal,
        onActivity: () => {
          onHealthy();
          updateState((prev) => applyStreamActivity(prev, Date.now()));
        },
        onStatus: (statusHeader) => {
          updateState((prev) =>
            applyStatusRecord(prev, statusHeader, Date.now())
          );
        },
        onFramePresented: (frame, url, metrics) => {
          setFrameUrl(url);
          const nowMs = Date.now();
          updateState((prev) => applyFrameRendered(prev, frame.header, nowMs));

          const geometryChanged =
            lastGeometrySentRef.current !== frame.header.geometry_revision;
          const due =
            geometryChanged ||
            nowMs - lastFeedbackAtRef.current >= FEEDBACK_THROTTLE_MS;
          if (due && !abort.signal.aborted) {
            lastFeedbackAtRef.current = nowMs;
            lastGeometrySentRef.current = frame.header.geometry_revision;
            void client
              .updateViewSession(session.session_id, {
                last_rendered_frame_seq: frame.header.frame_seq,
                decode_ms: metrics.decodeMs,
                buffered_frames: metrics.bufferedFrames
              })
              .catch(() => undefined);
          }
        }
      });
    };

    // A stream that fails OR ends (daemon restart, network blip, budget end) is a
    // disconnect: input and the lease heartbeat stop, the badge says so, and a new
    // session is created with bounded backoff. Control is never resumed here; the
    // user reacquires explicitly once fresh control state is read.
    const runSession = async () => {
      let attempt = 0;
      while (!abort.signal.aborted) {
        try {
          await connectOnce(() => {
            attempt = 0;
          });
        } catch {
          // handled as a disconnect below
        }
        if (abort.signal.aborted) {
          return;
        }
        inputQueueRef.current.clear();
        updateState((prev) => applyStreamDisconnected(prev));
        const dead = mountedSessionId;
        mountedSessionId = null;
        if (dead) {
          void client.deleteViewSession(dead).catch(() => undefined);
        }
        attempt += 1;
        await sleep(reconnectDelayMs(attempt));
        if (abort.signal.aborted) {
          return;
        }
        void refreshTargets();
      }
    };

    void runSession();

    return () => {
      abort.abort();
      activeSessionRef.current = null;
      setFrameUrl(null);
      inputQueue.clear();
      if (mountedSessionId) {
        void client.deleteViewSession(mountedSessionId).catch(() => undefined);
      }
    };
    // Only recreate stream when viewer/panel demand toggles or selected target identity/generation changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    wantSession,
    client,
    selectedTarget?.target_id,
    selectedTarget?.generation
  ]);

  // Ensure DELETE is sent on pagehide/unload as best-effort cleanly.
  useEffect(() => {
    const onUnload = () => {
      const s = activeSessionRef.current;
      if (s && client) {
        void client.deleteViewSession(s.session_id).catch(() => undefined);
      }
    };
    window.addEventListener("pagehide", onUnload);
    return () => window.removeEventListener("pagehide", onUnload);
  }, [client]);

  // Adapt session profile/resolution smoothly when expanding/collapsing viewer or taking control.
  useEffect(() => {
    const session = state.session;
    if (!session || !client || !wantSession) {
      return;
    }
    void client
      .updateViewSession(session.session_id, {
        profile: desiredProfile,
        max_width: desiredMaxWidth,
        max_height: desiredMaxHeight,
        quality: desiredQuality
      })
      .catch(() => undefined);
  }, [
    client,
    wantSession,
    state.session?.session_id,
    desiredProfile,
    desiredMaxWidth,
    desiredMaxHeight,
    desiredQuality,
    state.session
  ]);

  // 5 s view-session heartbeat (carries {takeover_id, control_epoch} ONLY while actively owning).
  useEffect(() => {
    if (!client || !state.session || !wantSession) {
      return undefined;
    }
    const id = window.setInterval(() => {
      const cur = stateRef.current;
      if (!cur.session) {
        return;
      }
      const body = buildSessionHeartbeat(cur);
      void client
        .heartbeatViewSession(cur.session.session_id, body)
        .catch(() => undefined);
    }, SESSION_HEARTBEAT_MS);
    return () => window.clearInterval(id);
  }, [client, wantSession, state.session]);

  // Periodic stream staleness watchdog so silent drops disable interactive input safely.
  useEffect(() => {
    if (!wantSession || !state.session) {
      return undefined;
    }
    const id = window.setInterval(() => {
      updateState((prev) => applyStreamStalenessCheck(prev, Date.now()));
    }, STALE_CHECK_MS);
    return () => window.clearInterval(id);
  }, [wantSession, state.session, updateState]);

  const flushInputQueue = useCallback(async () => {
    if (!client) {
      return;
    }
    if (flushingInputRef.current) {
      flushAgainRef.current = true;
      return;
    }
    flushingInputRef.current = true;
    try {
      while (inputQueueRef.current.length > 0) {
        const cur = stateRef.current;
        const gate = canSendLiveInput(cur, Date.now());
        if (!gate.ok) {
          inputQueueRef.current.clear();
          break;
        }
        const batchEvents = inputQueueRef.current.drainBatch();
        if (batchEvents.length === 0) {
          break;
        }
        try {
          const res = await client.sendViewInput(gate.sessionId, {
            v: 1,
            takeover_id: gate.takeoverId,
            control_epoch: gate.controlEpoch,
            generation: gate.generation,
            geometry_revision: gate.geometryRevision,
            first_seq: cur.nextInputSeq,
            events: batchEvents
          });
          updateState((prev) =>
            applyInputAck(prev, batchEvents.length, res.state)
          );
        } catch (err) {
          if (err instanceof ApiError && err.status === 429) {
            inputQueueRef.current.reset();
            updateState((prev) => applyRateLimited(prev, Date.now()));
            break;
          }
          if (err instanceof ApiError && err.status === 409) {
            inputQueueRef.current.clear();
            updateState((prev) => applyResyncRequired(prev));
            void refreshTargets();
            break;
          }
          inputQueueRef.current.clear();
          void refreshTargets();
          break;
        }
      }
    } finally {
      flushingInputRef.current = false;
      if (flushAgainRef.current) {
        flushAgainRef.current = false;
        void flushInputQueue();
      }
    }
  }, [client, refreshTargets, updateState]);

  const enqueueInput = useCallback(
    (event: InputEvent, flushNow = true) => {
      const gate = canSendLiveInput(stateRef.current, Date.now());
      if (!gate.ok) {
        return;
      }
      inputQueueRef.current.enqueue(event);
      if (flushNow) {
        void flushInputQueue();
      }
    },
    [flushInputQueue]
  );

  const resetInputState = useCallback(
    (flushNow = true) => {
      const hadHeld = inputQueueRef.current.hasPressedState;
      inputQueueRef.current.clear();
      if (!hadHeld) {
        return;
      }
      const gate = canSendLiveInput(stateRef.current, Date.now());
      if (gate.ok && flushNow) {
        inputQueueRef.current.enqueue({ type: "reset" });
        void flushInputQueue();
      }
    },
    [flushInputQueue]
  );

  const setComposing = useCallback((composing: boolean) => {
    inputQueueRef.current.setComposing(composing);
  }, []);

  const requestTakeover = useCallback(async () => {
    if (!client) {
      return;
    }
    // A takeover whose response was lost is retried with the SAME request id
    // (the daemon treats it idempotently); a definitive error starts afresh.
    const requestId =
      stateRef.current.retryTakeoverRequestId ?? crypto.randomUUID();
    const req = buildTakeoverRequest(stateRef.current, requestId);
    if (!req) {
      return;
    }
    updateState((prev) => applyTakeoverStarted(prev, requestId));
    try {
      const accepted = await client.requestTakeover(req);
      updateState((prev) => applyTakeoverAccepted(prev, accepted));
      void refreshTargets();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "takeover_failed";
      const message = err instanceof Error ? err.message : String(err);
      updateState((prev) =>
        applyTakeoverFailed(prev, code, message, {
          retainRequestId: !(err instanceof ApiError)
        })
      );
      // The daemon may have accepted: a snapshot lets the state adopt it.
      void refreshTargets();
    }
  }, [client, refreshTargets, updateState]);

  const performRelease = useCallback(
    async (
      humanAction: HumanAction | null,
      reusePending: boolean
    ): Promise<boolean> => {
      if (!client) {
        return false;
      }
      const cur = stateRef.current;
      const requestId =
        reusePending && cur.pendingRequest?.kind === "release"
          ? cur.pendingRequest.requestId
          : crypto.randomUUID();
      const payload = buildReleasePayload(cur, requestId, humanAction);
      if (!payload) {
        return false;
      }
      inputQueueRef.current.clear();
      updateState((prev) =>
        applyReleaseStarted(prev, payload.request_id, payload.human_action ?? null)
      );
      const outcome = await runRelease(
        {
          release: (req) => {
            const bridge = registry.current();
            return bridge
              ? bridge.releaseOwnedTakeover(req)
              : client.releaseControl(req);
          },
          getControlState: () => client.getControlState()
        },
        payload
      );
      if (outcome.kind === "confirmed") {
        setControlState(outcome.dto);
        registry.syncControlState(outcome.dto);
        updateState((prev) => applyReleaseConfirmed(prev, outcome.dto));
        void refreshTargets();
        return true;
      }
      // Not confirmed: keep ownership and the request id so "Retry" replays it.
      const failed = outcome;
      if (failed.dto) {
        const dto = failed.dto;
        setControlState(dto);
        registry.syncControlState(dto);
        updateState((prev) => applyControlStateDto(prev, dto));
      }
      updateState((prev) =>
        prev.pendingRequest?.kind === "release"
          ? applyReleaseFailed(prev, failed.code, failed.message)
          : prev
      );
      return false;
    },
    [client, refreshTargets, registry, setControlState, updateState]
  );

  const releaseTakeover = useCallback(
    async (humanAction: HumanAction | null = null) => {
      await performRelease(humanAction, false);
    },
    [performRelease]
  );

  const retryRelease = useCallback(async () => {
    await performRelease(
      stateRef.current.pendingRequest?.kind === "release"
        ? stateRef.current.pendingRequest.humanAction
        : null,
      true
    );
  }, [performRelease]);

  const recoverTakeover = useCallback(async () => {
    if (!client) {
      return;
    }
    const prompt = describeRecoveryPrompt(stateRef.current, controlState);
    if (!prompt) {
      return;
    }
    if (prompt.mode === "clear_pause") {
      try {
        const dto = await client.recoverControl();
        setControlState(dto);
        registry.syncControlState(dto);
        updateState((prev) => applyControlStateDto(prev, dto));
        void refreshTargets();
      } catch (err) {
        const code = err instanceof ApiError ? err.code : "recover_failed";
        const message = err instanceof Error ? err.message : String(err);
        updateState((prev) => applyTakeoverFailed(prev, code, message));
      }
      return;
    }
    const requestId = crypto.randomUUID();
    const req = buildTakeoverRequest(
      stateRef.current,
      requestId,
      prompt.mode === "reacquire"
        ? { reacquireTakeoverId: prompt.takeoverId }
        : { recover: true, reason: prompt.recoveryReason ?? "owner_lost" }
    );
    if (!req) {
      try {
        const dto = await client.recoverControl();
        setControlState(dto);
        updateState((prev) => applyControlStateDto(prev, dto));
      } catch {
        // surfaced via refreshTargets
      }
      return;
    }
    updateState((prev) =>
      applyTakeoverStarted(prev, requestId, {
        reacquire: prompt.mode === "reacquire",
        recover: prompt.mode === "recover"
      })
    );
    try {
      const accepted = await client.requestTakeover(req);
      updateState((prev) => applyTakeoverAccepted(prev, accepted));
      void refreshTargets();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "recover_failed";
      const message = err instanceof Error ? err.message : String(err);
      updateState((prev) => applyTakeoverFailed(prev, code, message));
    }
  }, [client, controlState, refreshTargets, registry, setControlState, updateState]);

  const overrideStop = useCallback(
    async (opts: { onlyIfControlled?: boolean } = {}) => {
      if (!client) {
        return;
      }
      if (
        opts.onlyIfControlled &&
        !shouldOverrideOnStop(stateRef.current, controlStateRef.current)
      ) {
        return;
      }
      inputQueueRef.current.clear();
      try {
        const bridge = registry.current();
        const dto = bridge
          ? await bridge.emergencyStop()
          : await client.overrideControl();
        if (dto) {
          setControlState(dto);
          updateState((prev) => applyControlStateDto(prev, dto));
        }
        void refreshTargets();
      } catch {
        void refreshTargets();
      }
    },
    [client, refreshTargets, registry, setControlState, updateState]
  );

  const openViewer = useCallback((targetId?: string) => {
    // Never retarget while holding/acquiring control.
    if (targetId && !isControlBusy(stateRef.current)) {
      setSelectedTargetId(targetId);
    }
    setViewerOpen(true);
  }, []);

  const closeViewer = useCallback(async () => {
    inputQueueRef.current.clear();
    const plan = planViewerClose(stateRef.current);
    if (plan === "wait") {
      return; // a hand-back is already pending; Retry lives in the dock
    }
    if (plan === "release_first") {
      // Closing must not leave the bot paused behind a hidden screen: hand back
      // first, and stay open (with the error and a retry) when that fails.
      const released = await performRelease(null, false);
      if (!released) {
        return;
      }
    }
    setViewerOpen(false);
  }, [performRelease]);

  const selectTarget = useCallback((targetId: string) => {
    if (isControlBusy(stateRef.current)) {
      return;
    }
    inputQueueRef.current.clear();
    setSelectedTargetId(targetId);
  }, []);

  const setThumbnailDemand = useCallback(
    (active: boolean, botId: string | null) => {
      setThumbDemand((prev) =>
        prev.active === active && prev.botId === botId
          ? prev
          : { active, botId }
      );
    },
    []
  );

  const recoveryPrompt = useMemo(
    () => describeRecoveryPrompt(state, controlState),
    [state, controlState]
  );

  const value = useMemo<LiveViewContextValue>(
    () => ({
      targets,
      selectedTarget,
      controlState,
      state,
      frameUrl,
      viewerOpen,
      loadingTargets,
      panelAction,
      recoveryPrompt,
      openViewer,
      closeViewer,
      selectTarget,
      refreshTargets,
      setThumbnailDemand,
      requestTakeover,
      releaseTakeover,
      retryRelease,
      recoverTakeover,
      overrideStop,
      enqueueInput,
      resetInputState,
      setComposing,
      showPanelAction,
      registerBridge
    }),
    [
      targets,
      selectedTarget,
      controlState,
      state,
      frameUrl,
      viewerOpen,
      loadingTargets,
      panelAction,
      recoveryPrompt,
      openViewer,
      closeViewer,
      selectTarget,
      refreshTargets,
      setThumbnailDemand,
      requestTakeover,
      releaseTakeover,
      retryRelease,
      recoverTakeover,
      overrideStop,
      enqueueInput,
      resetInputState,
      setComposing,
      showPanelAction,
      registerBridge
    ]
  );

  return (
    <LiveViewContext.Provider value={value}>
      {children}
    </LiveViewContext.Provider>
  );
}

export function useLiveView(): LiveViewContextValue {
  const ctx = useContext(LiveViewContext);
  if (!ctx) {
    throw new Error("useLiveView must be used inside LiveViewProvider");
  }
  return ctx;
}
