import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { botView } from "../lib/botProfile";
import {
  beginAction,
  type ControlMode,
  getControlState,
  isTauriRuntime,
  overlayClick,
  startBotRun,
  stopBotRun,
  subscribeOverlayEvents
} from "../overlay/events";
import type { ComputerTarget } from "../lib/settingsPrefs";
import { DEFAULT_SETTINGS, loadSettings } from "../store/settingsStore";
import { useDaemon } from "./DaemonContext";
import {
  ControlBridge,
  HEARTBEAT_MS,
  resolveTargetScope
} from "./controlBridge";
import { useLiveView } from "./LiveViewContext";

/**
 * Connects the daemon to the on-screen overlay (task 017b) and live-view
 * panel overlay (task 173). Renders nothing; registers native bridge hooks
 * only in the Tauri desktop runtime.
 */
export function ControlBridgeHost() {
  const { t } = useTranslation();
  const { client, bots, onDaemonEvent } = useDaemon();
  const { targets, selectedTarget, showPanelAction, registerBridge } =
    useLiveView();

  const botsRef = useRef(bots);
  botsRef.current = bots;
  const targetsRef = useRef(targets);
  targetsRef.current = targets;
  const selectedRef = useRef(selectedTarget);
  selectedRef.current = selectedTarget;
  const computerTargetRef = useRef<ComputerTarget>(DEFAULT_SETTINGS.computerTarget);
  const stepRef = useRef(t("control.starting"));
  stepRef.current = t("control.starting");

  useEffect(() => {
    if (!client || !isTauriRuntime()) {
      registerBridge(null);
      return undefined;
    }
    let alive = true;
    const refreshComputerTarget = () => {
      void loadSettings()
        .then((s) => {
          if (alive) {
            computerTargetRef.current = s.computerTarget;
          }
        })
        .catch(() => {});
    };
    refreshComputerTarget();
    const bridge = new ControlBridge({
      client,
      native: {
        beginAction: (rect, label, action) => beginAction(rect, label, action),
        startRun: async (bot, step) => {
          await startBotRun(bot, step);
        },
        stopRun: async () => {
          await stopBotRun();
        },
        click: overlayClick
      },
      resolveTargetScope: ({ runId, botId }) =>
        resolveTargetScope({
          runId,
          botId,
          targets: targetsRef.current,
          selectedTarget: selectedRef.current,
          computerTarget: computerTargetRef.current
        }),
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
    const offEvents = onDaemonEvent((env) => void bridge.handleEvent(env));
    const offState = subscribeOverlayEvents({
      onControlStateChanged: (snap) =>
        void bridge.onNativeState(snap.mode as ControlMode),
      onSettingsUpdated: refreshComputerTarget
    });
    void getControlState()
      .then((snap) => bridge.onNativeState(snap.mode))
      .catch(() => bridge.heartbeat());
    const beat = window.setInterval(() => void bridge.heartbeat(), HEARTBEAT_MS);
    return () => {
      alive = false;
      registerBridge(null);
      offEvents();
      offState();
      window.clearInterval(beat);
    };
  }, [client, onDaemonEvent, registerBridge, showPanelAction]);

  return null;
}
