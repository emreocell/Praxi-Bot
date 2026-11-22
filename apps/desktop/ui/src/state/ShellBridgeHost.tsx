import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { GlobalEnvelope } from "../api/types";
import { scheduleNotifyOf } from "../lib/scheduleNotify";
import {
  decideNotification,
  type NotifyPrefs,
  notificationText,
  resolveNotificationAction
} from "../lib/notifyRules";
import { holdReason } from "../lib/botMute";
import type { QuietHours } from "../lib/quietHours";
import { recordNotification } from "../store/notificationHistoryStore";
import { brandIcon } from "../lib/brandIcons";
import { useDecisionActions } from "../components/PendingDecisions";
import {
  isAppFocused,
  onShellEvent,
  SHELL_EVENT_NOTIFICATION_ACTION,
  SHELL_EVENT_NOTIFICATION_ACTIVATED,
  SHELL_EVENT_TRAY_OPEN_NEEDS,
  SHELL_EVENT_TRAY_STOP_ALL,
  setNeedsCount,
  setTrayLabels,
  showNotification,
  takeNotificationActions
} from "../lib/shell";

const appName = (server: string): string | null => {
  const b = brandIcon(server);
  return b.known ? b.name : null;
};
import { useDaemon } from "./DaemonContext";
import type { RunState } from "./timeline";

export interface ShellBridgeSettings extends NotifyPrefs {
  notifyPrivate: boolean;
  quietHours: QuietHours;
  mutedBotIds: string[];
}

/**
 * Bridges the daemon to the OS shell (task 033): notifications for things that
 * need the user while the window is not focused, the tray's "needs you"
 * count and labels, and the tray / notification clicks.  Renders nothing.
 */
export function ShellBridgeHost({
  settings,
  onOpenChat,
  onOpenNeeds
}: {
  settings: ShellBridgeSettings;
  onOpenChat: (chatId: string) => void;
  onOpenNeeds: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { bots, needs, onDaemonEvent, refreshRuns } = useDaemon();

  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const botsRef = useRef(bots);
  botsRef.current = bots;
  const tRef = useRef(t);
  tRef.current = t;
  const openChatRef = useRef(onOpenChat);
  openChatRef.current = onOpenChat;
  const openNeedsRef = useRef(onOpenNeeds);
  openNeedsRef.current = onOpenNeeds;
  const needsRef = useRef(needs);
  needsRef.current = needs;
  // The same decision path as the approval card (owner key for a standing choice included).
  const { decide } = useDecisionActions();
  const decideRef = useRef(decide);
  decideRef.current = decide;

  // 1. notifications (live events only: the stream replays history on start)
  useEffect(() => {
    const appStartMs = Date.now();
    // The listener runs before the timeline state folds the event in, so keep
    // our own little run index (chat / bot / start) for the decision.
    const runs = new Map<string, RunState>();
    const known = (id: string): RunState =>
      runs.get(id) ?? {
        id,
        chatId: "",
        botId: "",
        startedMs: Date.now(),
        items: []
      };

    const index = (env: GlobalEnvelope) => {
      const e = env.event;
      if (e.type === "run_started") {
        runs.set(e.run_id, {
          ...known(e.run_id),
          chatId: e.chat_id,
          botId: e.bot_id,
          startedMs: env.ts_ms
        });
      } else if (e.type === "subagent_started") {
        const parent = runs.get(e.parent_run_id);
        runs.set(e.run_id, {
          ...known(e.run_id),
          chatId: parent?.chatId ?? "",
          botId: parent?.botId ?? "",
          parentRunId: e.parent_run_id
        });
      }
      if (env.parent_run_id && "run_id" in e && typeof e.run_id === "string") {
        const cur = known(e.run_id);
        const parent = runs.get(env.parent_run_id);
        runs.set(e.run_id, {
          ...cur,
          chatId: cur.chatId || parent?.chatId || "",
          botId: cur.botId || parent?.botId || "",
          parentRunId: env.parent_run_id
        });
      }
    };

    return onDaemonEvent((env) => {
      index(env);
      const e = env.event;
      const runId = "run_id" in e && typeof e.run_id === "string" ? e.run_id : "";
      const run = runId ? runs.get(runId) : undefined;
      // A cheap pre-check keeps the async focus probe off the hot path.
      if (
        e.type !== "approval_needed" &&
        e.type !== "question_asked" &&
        e.type !== "run_finished"
      ) {
        return;
      }
      void isAppFocused().then((focused) => {
        const s = settingsRef.current;
        const d = decideNotification(env, {
          run,
          appStartMs,
          focused,
          prefs: s,
          ...(run?.scheduleId ? { scheduleNotify: scheduleNotifyOf(run.scheduleId) } : {})
        });
        if (!d) {
          return;
        }
        const bot = botsRef.current.find((b) => b.id === d.botId);
        const name = bot?.name ?? "Praxi-Bot";
        const text = notificationText(d, name, s.notifyPrivate, (k, v) =>
          tRef.current(k, v),
          appName
        );
        // Quiet hours or a bot mute hold the toast; it still lands in the history, and a
        // pending approval stays in "Needs you" regardless (a mute never holds one).
        const now = new Date();
        const held = holdReason(now, s.quietHours, d.kind, d.botId, s.mutedBotIds);
        const hold = held !== null;
        recordNotification({
          id: `${env.ts_ms}-${d.runId}-${d.kind}-${d.approval?.callId ?? ""}`,
          kind: d.kind,
          title: text.title,
          body: text.body,
          botId: d.botId,
          botName: name,
          chatId: d.chatId,
          ts: now.getTime(),
          shown: !hold,
          ...(held ? { heldBy: held } : {}),
          read: false
        });
        if (hold) {
          return;
        }
        void showNotification({ ...text, chatId: d.chatId || undefined }).catch(
          () => {}
        );
      });
    });
  }, [onDaemonEvent]);

  // 2. tray: count and labels
  useEffect(() => {
    void setNeedsCount(needs.length).catch(() => {});
  }, [needs.length]);

  const language = i18n.language;
  useEffect(() => {
    void setTrayLabels({
      toggle: t("tray.toggle"),
      needs: t("tray.needs"),
      stopAll: t("tray.stopAll"),
      quit: t("tray.quit"),
      tooltip: t("tray.tooltip"),
      tooltipNeeds: t("tray.tooltipNeeds")
    }).catch(() => {});
  }, [t, language]);

  // 3. tray / notification clicks
  useEffect(() => {
    const offs = [
      onShellEvent<{ chat_id: string | null }>(
        SHELL_EVENT_NOTIFICATION_ACTIVATED,
        (p) => {
          if (p?.chat_id) {
            openChatRef.current(p.chat_id);
          } else {
            openNeedsRef.current();
          }
        }
      ),
      // A toast button: the event is only a nudge. The presses come from the shell's own queue and
      // are checked against the approval that is pending now, then decided like a card click.
      onShellEvent<null>(SHELL_EVENT_NOTIFICATION_ACTION, () => {
        void takeNotificationActions()
          .then(async (presses) => {
            for (const press of presses) {
              const tool = needsRef.current.find(
                (n) => n.kind === "approval" && n.callId === press.call_id
              )?.tool;
              const res = resolveNotificationAction(
                press.action,
                tool
                  ? {
                      callId: press.call_id,
                      toolName: tool.name,
                      input: tool.input,
                      details: tool.details
                    }
                  : undefined
              );
              if (res.kind === "open") {
                openNeedsRef.current();
                continue;
              }
              try {
                await decideRef.current(press.call_id)(res.decision, res.scope);
              } catch {
                openNeedsRef.current();
              }
            }
          })
          .catch(() => {});
      }),
      onShellEvent<null>(SHELL_EVENT_TRAY_OPEN_NEEDS, () =>
        openNeedsRef.current()
      ),
      // Tray "Stop all" is a native user Stop (`stop_all` raises `stopSeq`); the
      // Stop ledger turns it into ONE Stop decision. Here we only refresh the list.
      onShellEvent<null>(SHELL_EVENT_TRAY_STOP_ALL, () => {
        void refreshRuns().catch(() => {});
      })
    ];
    return () => {
      for (const off of offs) {
        off();
      }
    };
  }, [refreshRuns]);

  return null;
}
