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
  TargetInfo,
  TargetUnavailable
} from "../api/types.ts";
import { type ApiClient, ApiError } from "../api/client.ts";
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
  controlSnapshotKey,
  createInitialLiveViewState,
  describeRecoveryPrompt,
  isActivelyOwningTakeover,
  foreignPausedTakeover,
  isControlBusy,
  isLocalBaseUrl,
  needsPauseRecovery,
  pickDefaultTarget,
  planViewerClose,
  sessionProfile,
  type RecoveryPrompt,
  shouldOverrideOnStop,
  type LiveViewControllerState
} from "../lib/liveViewState.ts";
import type { TargetsStatus } from "../lib/computerLabel.ts";
import {
  type ClearTarget,
  runClearPause,
  runRelease
} from "../lib/liveViewRelease.ts";
import { admitSnapshot, createSnapshotSequencer } from "../lib/snapshotOrder.ts";
import { runReconnectLoop } from "../lib/liveViewReconnect.ts";
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
import { TakeoverStop } from "./takeoverStop.ts";
import { humanLine } from "../lib/humanError";

const SESSION_HEARTBEAT_MS = 5000;
const CONTROL_POLL_MS = 5000;
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
  targetsStatus: TargetsStatus;
  availability: TargetUnavailable[];
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
  /** `POST /v1/control/recover`: clear a paused latch (no view session needed). */
  clearPause: (kind?: "own" | "foreign" | "latch") => Promise<void>;
  /** This client is the local user (loopback daemon), not a paired peer. */
  isLocalClient: boolean;
  /**
   * Emergency stop through `POST /v1/control/override`. With `onlyIfControlled`
   * (the global Stop) it does nothing unless a screen takeover is held, so a plain
   * Stop never latches the whole daemon paused.
   */
  overrideStop: (opts?: { onlyIfControlled?: boolean }) => Promise<void>;
  /**
   * The view targets of the CURRENT daemon client, or `null` while that client's
   * list has not loaded (an older client's list never counts).
   */
  currentTargets: () => { client: ApiClient; targets: readonly TargetInfo[] } | null;
  enqueueInput: (event: InputEvent, flushNow?: boolean) => void;
  resetInputState: (flushNow?: boolean) => void;
  setComposing: (composing: boolean) => void;
  showPanelAction: (highlight: PanelActionHighlight) => void;
  registerBridge: (bridge: ControlBridge | null) => void;
  /** Feed a control snapshot the bridge re-read into state and the Stop gate. */
  ingestControlSnapshot: (dto: ControlStateDto, ticket?: number) => void;
  /** Ticket for a snapshot request about to be issued (orders it against actions). */
  issueSnapshotTicket: () => number;
}

const LiveViewContext = createContext<LiveViewContextValue | null>(null);
/**
 * THIS computer's live view, whichever computer the views show. While a server is home the nearest
 * `LiveViewContext` is the server's; the "my computer" screen of the right panel reads this one.
 */
const LocalLiveViewContext = createContext<LiveViewContextValue | null>(null);

function initialMockViewerOpen(): boolean {
  if (!isMockMode() || typeof window === "undefined") {
    return false;
  }
  return new URLSearchParams(window.location.search).get("lv") === "open";
}

export function LiveViewProvider({
  children,
  scope = "home"
}: {
  children: ReactNode;
  /** `local`: this computer's own provider (the outer one), also published as the local view. */
  scope?: "home" | "local";
}) {
  const { client, onDaemonEvent, bots } = useDaemon();

  const clientIdRef = useRef<string>(crypto.randomUUID());
  const controlStateRef = useRef<ControlStateDto | null>(null);
  /** The client whose snapshot `controlStateRef` holds (the Stop override decides from it). */
  const controlClientRef = useRef<unknown>(null);
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
  const clientRef = useRef(client);
  clientRef.current = client;
  // Targets together with the client that listed them (N7: a stale client's list
  // can never make an unknown scope look local for the new client).
  const targetsForRef = useRef<{ client: ApiClient; targets: TargetInfo[] } | null>(null);
  const currentTargets = useCallback(() => {
    const t = targetsForRef.current;
    return t && t.client === clientRef.current ? t : null;
  }, []);
  const [loadingTargets, setLoadingTargets] = useState(false);
  const [targetsStatus, setTargetsStatus] = useState<TargetsStatus>("unknown");
  const [availability, setAvailability] = useState<TargetUnavailable[]>([]);
  const [selectedTargetId, setSelectedTargetId] = useState<string | null>(null);
  const [controlState, setControlStateRaw] = useState<ControlStateDto | null>(
    null
  );
  // Orders snapshots by request time: a poll sent before an action must not
  // overwrite the newer state that action returned.
  const sequencer = useMemo(() => createSnapshotSequencer(), []);
  const setControlState = useCallback(
    (dto: ControlStateDto | null) => {
      sequencer.markFresh();
      controlStateRef.current = dto;
      controlClientRef.current = clientRef.current;
      setControlStateRaw(dto);
      // No pump here: every caller updates `stateRef` next and then pumps, so the
      // owed override never decides from a stale state.
    },
    [sequencer]
  );
  // The takeover half of every Stop: input halts synchronously, the override is owed
  // until delivered through the current client.
  const takeoverStopRef = useRef<TakeoverStop<ApiClient> | null>(null);
  const applyOverrideRef = useRef<(dto: ControlStateDto) => void>(() => {});
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
  if (!takeoverStopRef.current) {
    takeoverStopRef.current = new TakeoverStop<ApiClient>({
      client: () => clientRef.current,
      // Decided only from a snapshot of the CURRENT client; unknown otherwise.
      controlled: () =>
        clientRef.current !== null &&
        controlClientRef.current === clientRef.current &&
        controlStateRef.current !== null
          ? shouldOverrideOnStop(stateRef.current, controlStateRef.current)
          : null,
      post: (c, signal) => c.overrideControl(signal),
      apply: (_c, dto) => applyOverrideRef.current(dto),
      schedule: (fn, ms) => window.setTimeout(fn, ms),
      cancel: (handle) => window.clearTimeout(handle as number)
    });
  }
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

  // Snapshots the bridge re-reads (every heartbeat, on events) feed the same
  // ref the global Stop consults, so a takeover this UI never adopted is seen.
  const ingestControlSnapshot = useCallback(
    (dto: ControlStateDto, ticket?: number) => {
      if (!admitSnapshot(sequencer, ticket)) {
        return; // an older request resolved after a newer state was applied
      }
      const changed =
        controlStateRef.current === null ||
        controlSnapshotKey(controlStateRef.current) !== controlSnapshotKey(dto);
      controlStateRef.current = dto;
      controlClientRef.current = clientRef.current;
      if (changed) {
        setControlStateRaw(dto);
        updateState((prev) => applyControlStateDto(prev, dto));
      }
      // After the state ref is current: the owed override decides from the new snapshot.
      takeoverStopRef.current?.pump();
    },
    [sequencer, updateState]
  );
  const issueSnapshotTicket = useCallback(() => sequencer.issue(), [sequencer]);

  useEffect(() => {
    registry.setOwnedTakeoverId(state.ownedTakeoverId);
  }, [registry, state.ownedTakeoverId]);

  const refreshTargets = useCallback(async () => {
    if (!client) {
      return;
    }
    setLoadingTargets(true);
    try {
      const ticket = sequencer.issue();
      const [nextTargets, nextCtrl] = await Promise.all([
        client.listViewTargets(),
        client.getControlState().catch(() => null)
      ]);
      if (clientRef.current !== client) {
        return; // a replaced client's answer changes nothing
      }
      targetsForRef.current = { client, targets: nextTargets };
      setTargets(nextTargets);
      setTargetsStatus("ok");
      // Why a screen is missing is optional context: its failure never hides the targets.
      client
        .listViewAvailability()
        .then(setAvailability)
        .catch(() => setAvailability([]));
      if (nextCtrl && sequencer.accept(ticket)) {
        controlStateRef.current = nextCtrl;
        controlClientRef.current = clientRef.current;
        setControlStateRaw(nextCtrl);
        registry.syncControlState(nextCtrl);
        updateState((prev) => applyControlStateDto(prev, nextCtrl));
        takeoverStopRef.current?.pump();
      }
    } catch {
      // keep existing snapshot; next poll/action retries
      setTargetsStatus("failed");
    } finally {
      setLoadingTargets(false);
    }
  }, [client, registry, sequencer, updateState]);

  // Refresh targets & control snapshot on client connect and when panel/viewer opens.
  useEffect(() => {
    targetsForRef.current = null;
    // A new client: the owed override waits for THIS client's snapshot (or goes now if forced).
    controlClientRef.current = null;
    takeoverStopRef.current?.pump();
    if (!client) {
      setTargets([]);
      setTargetsStatus("unknown");
      setControlState(null);
      updateState(createInitialLiveViewState(null));
      return undefined;
    }
    void refreshTargets();
    return onDaemonEvent((env) => {
      // A takeover changed, or a screen appeared / went idle / attached / closed
      // (the bot's first browser call): refetch without reopening the panel.
      if (
        env.event.type === "control_state_changed" ||
        env.event.type === "view_targets_changed"
      ) {
        void refreshTargets();
      }
    });
  }, [client, onDaemonEvent, refreshTargets, setControlState, updateState]);

  // `control_state_changed` is not replayed: re-read the snapshot on an interval
  // for every client (the Tauri bridge polls too; this covers plain browsers).
  useEffect(() => {
    if (!client) {
      return undefined;
    }
    const id = window.setInterval(() => {
      // One poller: the control bridge (Tauri) polls on the same interval.
      if (registry.current()) {
        return;
      }
      const ticket = sequencer.issue();
      void client
        .getControlState()
        .then((dto) => {
          if (clientRef.current === client) {
            ingestControlSnapshot(dto, ticket);
          }
        })
        .catch(() => undefined);
    }, CONTROL_POLL_MS);
    return () => window.clearInterval(id);
  }, [client, ingestControlSnapshot, registry, sequencer]);

  useEffect(() => {
    if (client && (viewerOpen || thumbDemand.active)) {
      void refreshTargets();
    }
  }, [client, viewerOpen, thumbDemand.active, refreshTargets]);

  const botFocus = thumbDemand.botId ?? (bots.length > 0 ? bots[0].id : null);
  const selectedTarget = useMemo(
    () =>
      pickDefaultTarget(targets, {
        preferredTargetId: selectedTargetId,
        botId: botFocus
      }),
    [targets, selectedTargetId, botFocus]
  );

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
  const desiredProfile = sessionProfile(state);
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

    // Open and consume the frame stream of one session until it ends or throws.
    const openStream = async (
      sessionId: string,
      signal: AbortSignal
    ): Promise<void> => {
      const res = await client.openViewFrameStream(sessionId, signal);
      if (!res.body || signal.aborted) {
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
          if (signal.aborted) {
            revoke();
          } else {
            window.requestAnimationFrame(revoke);
          }
        }
      };

      await consumeLiveViewStream({
        stream: res.body,
        presenter,
        signal,
        onActivity: () => {
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
          if (due && !signal.aborted) {
            lastFeedbackAtRef.current = nowMs;
            lastGeometrySentRef.current = frame.header.geometry_revision;
            void client
              .updateViewSession(sessionId, {
                last_rendered_frame_seq: frame.header.frame_seq,
                decode_ms: metrics.decodeMs,
                buffered_frames: metrics.bufferedFrames
              })
              .catch(() => undefined);
          }
        }
      });
    };

    // A stream that fails OR ends is a disconnect: input and the lease heartbeat
    // stop, the dock shows the paused/disconnected state (Reacquire), and the
    // SAME session is reopened with backoff (a session that owns a takeover is
    // never deleted on a transient drop). Control is never resumed here.
    void runReconnectLoop(
      {
        createSession: () =>
          client.createViewSession({
            client_id: clientIdRef.current,
            target_id: selectedTarget.target_id,
            profile: sessionProfile(stateRef.current),
            codecs: selectedTarget.capabilities.codecs,
            max_width: viewerOpen ? 1600 : 960,
            max_height: viewerOpen ? 1000 : 600,
            // An automatically chosen screen is bound to the focused bot: the
            // daemon refuses another bot's browser. An explicit pick is not.
            ...(selectedTargetId === null && botFocus ? { bot_id: botFocus } : {})
          }),
        openStream,
        deleteSession: (id) => client.deleteViewSession(id)
      },
      {
        onSession: (session) => {
          mountedSessionId = session.session_id;
          activeSessionRef.current = session;
          lastFeedbackAtRef.current = 0;
          lastGeometrySentRef.current = null;
          updateState((prev) => applySessionCreated(prev, selectedTarget, session));
        },
        onDisconnected: () => {
          inputQueueRef.current.clear();
          updateState((prev) => applyStreamDisconnected(prev));
        },
        beforeRetry: () => {
          void refreshTargets();
        }
      },
      { signal: abort.signal, sleep, now: () => Date.now() }
    );

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
      // After a Stop this app no longer renews its input lease.
      const body = takeoverStopRef.current?.inputAllowed() === false ? {} : buildSessionHeartbeat(cur);
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
        if (!gate.ok || takeoverStopRef.current?.inputAllowed() === false) {
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
      if (!gate.ok || takeoverStopRef.current?.inputAllowed() === false) {
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
      if (gate.ok && flushNow && takeoverStopRef.current?.inputAllowed() !== false) {
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
    // Taking control again is a new user decision: input may flow again.
    takeoverStopRef.current?.resume();
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
      const message = humanLine(err);
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
      takeoverStopRef.current?.retire(); // the user's newer decision: no stale override
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
        takeoverStopRef.current?.pump();
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
        takeoverStopRef.current?.pump();
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

  // Clears a pause, verified against fresh daemon state first (see
  // `runClearPause`): our own paused takeover, another actor's paused takeover
  // (local user only, labelled with the owner) or a plain owner-less latch.
  // `POST /v1/control/recover` needs no view session, so it works with the
  // viewer and the panel closed.
  const clearPause = useCallback(
    async (kind: ClearTarget["kind"] = "own") => {
      if (!client) {
        return;
      }
      takeoverStopRef.current?.retire(); // the user's newer decision: no stale override
      const cur = stateRef.current;
      const ctl = controlStateRef.current;
      let target: ClearTarget | null = null;
      if (kind === "own") {
        const shown = needsPauseRecovery(cur, ctl);
        target = shown ? { kind: "own", takeoverId: shown.takeoverId } : null;
      } else if (kind === "foreign") {
        const f = foreignPausedTakeover(cur, ctl);
        target = f && isLocalBaseUrl(client.baseUrl) ? { kind: "foreign", expected: f } : null;
      } else {
        target = { kind: "latch" };
      }
      const outcome = await runClearPause(
        {
          getControlState: () => client.getControlState(),
          recover: () => client.recoverControl(),
          releaseLatch: () => client.releaseControl()
        },
        target
      );
      if (outcome.kind === "cleared") {
        setControlState(outcome.dto);
        registry.syncControlState(outcome.dto);
        updateState((prev) => ({
          ...applyControlStateDto(prev, outcome.dto),
          lastError: null
        }));
        takeoverStopRef.current?.pump();
        void refreshTargets();
        return;
      }
      if (outcome.kind === "aborted") {
        if (outcome.fresh) {
          ingestControlSnapshot(outcome.fresh);
        }
        updateState((prev) =>
          applyTakeoverFailed(
            prev,
            "pause_changed",
            "the control state changed; not cleared"
          )
        );
        return;
      }
      updateState((prev) =>
        applyTakeoverFailed(prev, outcome.code, outcome.message)
      );
    },
    [client, ingestControlSnapshot, refreshTargets, registry, setControlState, updateState]
  );

  // Reacquire a takeover the stream drop paused. Every other paused case
  // (local Stop, restart, revoked, ...) is a clear-pause: the daemon cannot
  // always verify a reset for the old target, `recover` can.
  const recoverTakeover = useCallback(async () => {
    if (!client) {
      return;
    }
    const prompt = describeRecoveryPrompt(stateRef.current, controlState);
    if (!prompt) {
      return;
    }
    if (prompt.mode !== "reacquire") {
      await clearPause();
      return;
    }
    const requestId = crypto.randomUUID();
    takeoverStopRef.current?.resume();
    const req = buildTakeoverRequest(stateRef.current, requestId, {
      reacquireTakeoverId: prompt.takeoverId
    });
    if (!req) {
      return; // no view session yet: open the screen first
    }
    updateState((prev) =>
      applyTakeoverStarted(prev, requestId, { reacquire: true, recover: false })
    );
    try {
      const accepted = await client.requestTakeover(req);
      updateState((prev) => applyTakeoverAccepted(prev, accepted));
      void refreshTargets();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "recover_failed";
      const message = humanLine(err);
      updateState((prev) => applyTakeoverFailed(prev, code, message));
    }
  }, [client, clearPause, controlState, refreshTargets, updateState]);

  // A Stop pauses a held takeover. Input halts synchronously here; the override is
  // owed until delivered through the CURRENT client (see takeoverStop.ts). With
  // `onlyIfControlled` (every entry point but the viewer button) it is sent only if
  // the current client's snapshot shows an acquiring / active takeover.
  const overrideStop = useCallback(
    async (opts: { onlyIfControlled?: boolean } = {}) => {
      inputQueueRef.current.clear();
      takeoverStopRef.current?.stop(!opts.onlyIfControlled);
    },
    []
  );
  applyOverrideRef.current = (dto: ControlStateDto) => {
    registry.syncControlState(dto);
    setControlState(dto);
    updateState((prev) => applyControlStateDto(prev, dto));
    takeoverStopRef.current?.pump();
    void refreshTargets();
  };

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
    if (plan === "dismiss") {
      // A hand-back is pending (failed or in flight): hide the viewer WITHOUT
      // pretending it succeeded. Ownership and the request id stay, and the
      // panel keeps Bitirdim / Retry.
      setViewerOpen(false);
      return;
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

  const isLocalClient = client ? isLocalBaseUrl(client.baseUrl) : false;

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
      targetsStatus,
      availability,
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
      clearPause,
      isLocalClient,
      overrideStop,
      currentTargets,
      enqueueInput,
      resetInputState,
      setComposing,
      showPanelAction,
      registerBridge,
      ingestControlSnapshot,
      issueSnapshotTicket
    }),
    [
      targets,
      selectedTarget,
      controlState,
      state,
      frameUrl,
      viewerOpen,
      loadingTargets,
      targetsStatus,
      availability,
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
      clearPause,
      isLocalClient,
      overrideStop,
      currentTargets,
      enqueueInput,
      resetInputState,
      setComposing,
      showPanelAction,
      registerBridge,
      ingestControlSnapshot,
      issueSnapshotTicket
    ]
  );

  return (
    <LiveViewContext.Provider value={value}>
      {scope === "local" ? (
        <LocalLiveViewContext.Provider value={value}>{children}</LocalLiveViewContext.Provider>
      ) : (
        children
      )}
    </LiveViewContext.Provider>
  );
}

/** This computer's live view (the same as `useLiveView()` while no server is selected). */
export function useLocalLiveView(): LiveViewContextValue {
  const local = useContext(LocalLiveViewContext);
  const home = useContext(LiveViewContext);
  const ctx = local ?? home;
  if (!ctx) {
    throw new Error("useLocalLiveView must be used inside LiveViewProvider");
  }
  return ctx;
}

export function useLiveView(): LiveViewContextValue {
  const ctx = useContext(LiveViewContext);
  if (!ctx) {
    throw new Error("useLiveView must be used inside LiveViewProvider");
  }
  return ctx;
}
