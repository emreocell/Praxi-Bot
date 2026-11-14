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
import { useDaemon } from "./DaemonContext";
import { ControlBridge, HEARTBEAT_MS } from "./controlBridge";

/**
 * Connects the daemon to the on-screen overlay (task 017b).  Renders nothing;
 * it exists only in the native app, where the overlay windows live.
 */
export function ControlBridgeHost() {
  const { t } = useTranslation();
  const { client, bots, onDaemonEvent } = useDaemon();
  const botsRef = useRef(bots);
  botsRef.current = bots;
  const startingStep = t("control.starting");
  const stepRef = useRef(startingStep);
  stepRef.current = startingStep;

  useEffect(() => {
    if (!client || !isTauriRuntime()) {
      return undefined;
    }
    const bridge = new ControlBridge({
      client,
      native: {
        beginAction: (rect, label, action) => beginAction(rect, label, action),
        startRun: async (bot, step) => {
          await startBotRun(bot, step);
        },
        stopRun: async () => {
          return (await stopBotRun()).runGeneration;
        },
        click: overlayClick
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
    const offEvents = onDaemonEvent((env) => void bridge.handleEvent(env));
    let nativeEvents = 0;
    const offState = subscribeOverlayEvents({
      onControlStateChanged: (snap) => {
        nativeEvents += 1;
        void bridge.onNativeState(snap.mode as ControlMode, snap.runGeneration);
      },
      onSettingsUpdated: () => {}
    });
    // Both subscriptions are installed before either initial snapshot starts.
    let disposed = false;
    let nativeReady = false;
    let initializing = false;
    const tick = async () => {
      if (disposed || initializing) return;
      if (!nativeReady) {
        initializing = true;
        const observed = nativeEvents;
        try {
          const snap = await getControlState();
          if (disposed) return;
          if (observed === nativeEvents) await bridge.initializeNativeState(snap.mode);
          nativeReady = true;
        } catch {
          // Retry initialization on the next heartbeat without changing control.
        } finally {
          initializing = false;
        }
      }
      if (disposed || !nativeReady) return;
      void bridge.restoreRuns();
      await bridge.heartbeat();
    };
    void tick();
    const beat = window.setInterval(() => void tick(), HEARTBEAT_MS);
    return () => {
      disposed = true;
      bridge.dispose();
      offEvents();
      offState();
      window.clearInterval(beat);
    };
  }, [client, onDaemonEvent]);

  return null;
}
