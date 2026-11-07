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
          await stopBotRun();
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
    const offState = subscribeOverlayEvents({
      onControlStateChanged: (snap) =>
        void bridge.onNativeState(snap.mode as ControlMode),
      onSettingsUpdated: () => {}
    });
    // Register as controller now (with the mode the state machine is in) and
    // keep the heartbeat going.
    void getControlState()
      .then((snap) => bridge.onNativeState(snap.mode))
      .catch(() => bridge.heartbeat());
    const beat = window.setInterval(() => void bridge.heartbeat(), HEARTBEAT_MS);
    return () => {
      offEvents();
      offState();
      window.clearInterval(beat);
    };
  }, [client, onDaemonEvent]);

  return null;
}
