import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { botView } from "../lib/botProfile";
import {
  beginAction,
  type ControlSnapshot,
  finishBotRun,
  getControlState,
  isTauriRuntime,
  overlayClick,
  startBotRun,
  subscribeOverlayEvents
} from "../overlay/events";
import type { ComputerTarget } from "../lib/settingsPrefs";
import { DEFAULT_SETTINGS, loadSettings } from "../store/settingsStore";
import { appRuntime } from "./appStop";
import { useDaemon } from "./DaemonContext";
import {
  ControlBridge,
  HEARTBEAT_MS,
  resolveTargetScope
} from "./controlBridge";
import { useLiveView } from "./LiveViewContext";
import { startLocalComputerPresence } from "./localComputerPresence";

/**
 * Connects the daemon to the on-screen overlay (task 017b) and live-view panel
 * overlay (task 173). All decisions live in `StopRuntime` (stopRuntime.ts, tested
 * with fakes at the native / daemon boundary); this component only forwards React
 * effects, subscriptions and timers to it and builds the bridge's native deps.
 */
export function ControlBridgeHost() {
  const { t } = useTranslation();
  const { client, localApi, bots, onDaemonEvent, eventsConnected } = useDaemon();
  const {
    targets,
    selectedTarget,
    showPanelAction,
    registerBridge,
    ingestControlSnapshot,
    issueSnapshotTicket,
    overrideStop,
    currentTargets
  } = useLiveView();

  const runtime = appRuntime();
  const ledger = runtime.ledger;
  const botsRef = useRef(bots);
  botsRef.current = bots;
  const selectedRef = useRef(selectedTarget);
  selectedRef.current = selectedTarget;
  const stepRef = useRef(t("control.starting"));
  stepRef.current = t("control.starting");

  // The takeover half of every Stop goes to LiveViewContext (input halt + owed override).
  useEffect(() => {
    runtime.setOverride((opts) => void overrideStop(opts));
  }, [runtime, overrideStop]);

  // Native Stops, for the window's lifetime: subscribe first, then read.
  useEffect(() => {
    if (!isTauriRuntime()) {
      return undefined;
    }
    const off = subscribeOverlayEvents({
      onControlStateChanged: (snap: ControlSnapshot) => {
        runtime.onNativeSnapshot(snap);
      }
    });
    void runtime.readNative();
    return off;
  }, [runtime]);

  // A server-homed chat reports to the server. The PC that actually serves the
  // desktop RPC still needs its own local presence and takeover heartbeat.
  useEffect(() => {
    if (!isTauriRuntime()) return undefined;
    return startLocalComputerPresence({
      active: client,
      local: localApi,
      read: getControlState,
      subscribe: (onControlStateChanged) => subscribeOverlayEvents({ onControlStateChanged }),
      intervalMs: HEARTBEAT_MS
    });
  }, [client, localApi]);

  // The event stream reconnected: wait for (and fetch) a fresh run list.
  useEffect(() => {
    runtime.eventsConnected(eventsConnected);
  }, [eventsConnected, runtime]);

  // This client's targets loaded: restored local runs waiting for their scope may drive.
  useEffect(() => {
    runtime.scopeChanged();
  }, [targets, runtime]);

  useEffect(() => {
    if (!client) {
      registerBridge(null);
      return undefined;
    }
    let bridge: ControlBridge | null = null;
    let offSettings = () => {};
    let liveForSettings = true;

    if (isTauriRuntime()) {
      let computerTarget: ComputerTarget = DEFAULT_SETTINGS.computerTarget;
      let settingsLoaded = false;
      const loadComputerTarget = () => {
        void loadSettings()
          .then((s) => {
            if (liveForSettings) {
              computerTarget = s.computerTarget;
              settingsLoaded = true;
              runtime.scopeChanged();
            }
          })
          .catch(() => {});
      };
      loadComputerTarget();
      offSettings = subscribeOverlayEvents({ onSettingsUpdated: loadComputerTarget });
      bridge = new ControlBridge({
        client,
        native: {
          beginAction: (rect, label, action) => beginAction(rect, label, action),
          startRun: async (bot, step) => {
            const snap = await startBotRun(bot, step);
            runtime.onNativeSnapshot(snap);
            return snap.runGeneration;
          },
          finishRun: async (generation) => {
            runtime.onNativeSnapshot(await finishBotRun(generation));
          },
          click: overlayClick,
          confirmDriving: () => runtime.confirmDriving()
        },
        ledger,
        // Unknown until THIS client's targets and the settings loaded: never guessed.
        resolveTargetScope: ({ runId, botId }) => {
          const loaded = currentTargets();
          if (!settingsLoaded || !loaded || loaded.client !== client) {
            return "unknown";
          }
          return resolveTargetScope({
            runId,
            botId,
            targets: loaded.targets,
            selectedTarget: selectedRef.current,
            computerTarget
          });
        },
        onSnapshot: ingestControlSnapshot,
        issueSnapshotTicket,
        panelOverlay: {
          showAction: showPanelAction
        },
        botMeta: (id) => {
          const bot = botsRef.current.find((b) => b.id === id);
          if (!bot) {
            return { botId: id, botName: id.slice(0, 8) };
          }
          const v = botView(bot);
          return {
            botId: v.id,
            botName: v.name,
            botShape: v.shape,
            botAvatar: v.avatar,
            botColor: v.color
          };
        },
        get startingStep() {
          return stepRef.current;
        },
        now: () => Date.now()
      });
      registerBridge(bridge);
    } else {
      registerBridge(null);
    }

    // Pending Stops are sent to this client before anything else runs on it.
    const detach = runtime.attach(client, bridge);
    const offEvents = onDaemonEvent((env) => runtime.onDaemonEvent(env));
    const beat = window.setInterval(() => runtime.tick(), HEARTBEAT_MS);

    return () => {
      // Revoke first, synchronously: nothing of this client acts after cleanup.
      liveForSettings = false;
      detach();
      registerBridge(null);
      offEvents();
      offSettings();
      window.clearInterval(beat);
    };
  }, [client, runtime, ledger, onDaemonEvent, registerBridge, showPanelAction, ingestControlSnapshot, issueSnapshotTicket, currentTargets]);

  return null;
}
