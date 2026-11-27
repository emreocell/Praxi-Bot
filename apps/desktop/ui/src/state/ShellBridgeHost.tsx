import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { GlobalEnvelope, Schedule } from "../api/types";
import {
  readScheduleNotify,
  suggestScheduleNotify,
  type ScheduleNotify
} from "../lib/scheduleNotify";
import { parsePayload, parseSpec } from "../lib/schedules";
import {
  decideNotification,
  type NotifyPrefs,
  notificationText,
  resolveNotificationAction
} from "../lib/notifyRules";
import { holdReason } from "../lib/botMute";
import type { QuietHours } from "../lib/quietHours";
import {
  recordNotification,
  resolveNotificationByCallId
} from "../store/notificationHistoryStore";
import { brandIcon } from "../lib/brandIcons";
import { useDecisionActions } from "../components/PendingDecisions";
import {
  isAppFocused,
  onShellEvent,
  SHELL_EVENT_NOTIFICATION_ACTION,
  SHELL_EVENT_NOTIFICATION_ACTIVATED,
  SHELL_EVENT_TRAY_NOTICE_SHOWN,
  SHELL_EVENT_TRAY_OPEN_NEEDS,
  SHELL_EVENT_TRAY_STOP_ALL,
  configureQuickAsk,
  setKeepInTray,
  setNeedsCount,
  setTrayLabels,
  showNotification,
  takeNotificationActions
} from "../lib/shell";

const appName = (server: string): string | null => {
  const b = brandIcon(server);
  return b.known ? b.name : null;
};
import { setQuickAskProblem } from "../lib/quickAsk";
import { useDaemon } from "./DaemonContext";
import type { RunState } from "./timeline";

export interface ShellBridgeSettings extends NotifyPrefs {
  notifyPrivate: boolean;
  quietHours: QuietHours;
  mutedBotIds: string[];
  keepInTray: boolean;
  trayNoticeSeen: boolean;
  quickAskEnabled: boolean;
  quickAskHotkey: string;
}

/**
 * Bridges the daemon to the OS shell (task 033): notifications for things that
 * need the user while the window is not focused, the tray's "needs you"
 * count and labels, and the tray / notification clicks.  Renders nothing.
 */
/**
 * What to announce for a finished run of schedule `id`: the owner's stored choice, else a
 * reminder-aware default (a reminder is announced every time), else only problems.
 */
async function scheduleNotifyFor(
  client: { listSchedules(): Promise<Schedule[]> } | null,
  id: string
): Promise<ScheduleNotify> {
  const stored = readScheduleNotify()[id];
  if (stored) return stored;
  try {
    const found = (await client?.listSchedules())?.find((s) => s.id === id);
    if (found) {
      const payload = parsePayload(found.payload);
      const spec = parseSpec(found.spec);
      return suggestScheduleNotify(
        payload.kind === "agent_prompt" ? payload.text : "",
        spec !== null && "at" in spec
      );
    }
  } catch {
    // daemon unreachable: the default applies
  }
  return suggestScheduleNotify("", false);
}

export function ShellBridgeHost({
  settings,
  onOpenChat,
  onOpenNeeds,
  onTrayNoticeShown
}: {
  settings: ShellBridgeSettings;
  onOpenChat: (chatId: string) => void;
  onOpenNeeds: () => void;
  onTrayNoticeShown: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { bots, needs, onDaemonEvent, refreshRuns, client } = useDaemon();
  const clientRef = useRef(client);
  clientRef.current = client;

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
  const trayNoticeRef = useRef(onTrayNoticeShown);
  trayNoticeRef.current = onTrayNoticeShown;
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
    const pendingSecrets = new Set<string>();
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
      if (e.type === "tool_call_approved") {
        resolveNotificationByCallId(e.call_id, "approved");
        return;
      }
      if (e.type === "tool_call_completed" && pendingSecrets.delete(e.call_id)) {
        resolveNotificationByCallId(e.call_id, e.error ? "denied" : "answered");
        return;
      }
      if (e.type === "tool_call_denied") {
        pendingSecrets.delete(e.call_id);
        resolveNotificationByCallId(e.call_id, "denied");
        return;
      }
      if (e.type === "secret_requested" && e.origin && !e.parked && env.ts_ms >= appStartMs) {
        pendingSecrets.add(e.call_id);
      }
      const runId = "run_id" in e && typeof e.run_id === "string" ? e.run_id : "";
      const run = runId ? runs.get(runId) : undefined;
      // A cheap pre-check keeps the async focus probe off the hot path.
      if (
        e.type !== "approval_needed" &&
        e.type !== "question_asked" &&
        e.type !== "secret_requested" &&
        e.type !== "run_finished"
      ) {
        return;
      }
      const choice: Promise<ScheduleNotify | undefined> = run?.scheduleId
        ? scheduleNotifyFor(clientRef.current, run.scheduleId)
        : Promise.resolve(undefined);
      void Promise.all([isAppFocused(), choice]).then(([focused, scheduleNotify]) => {
        const s = settingsRef.current;
        const d = decideNotification(env, {
          run,
          appStartMs,
          focused,
          prefs: s,
          ...(scheduleNotify ? { scheduleNotify } : {})
        });
        if (!d || (e.type === "secret_requested" && !e.parked && !pendingSecrets.has(e.call_id))) {
          return;
        }
        const bot = botsRef.current.find((b) => b.id === d.botId);
        const name = bot?.name ?? "Praxi-Bot";
        const now = new Date();
        const text = notificationText(
          d,
          name,
          s.notifyPrivate,
          (k, v) => tRef.current(k, v),
          appName,
          now.getTime()
        );
        // Quiet hours or a bot mute hold the toast; it still lands in the history, and a
        // pending approval stays in "Needs you" regardless (a mute never holds one).
        const held = holdReason(now, s.quietHours, d.kind, d.botId, s.mutedBotIds);
        const hold = held !== null;
        const callId = !s.notifyPrivate
          ? d.approval?.callId ?? (e.type === "question_asked" || e.type === "secret_requested" ? e.call_id : undefined)
          : undefined;
        const isQuestion = !s.notifyPrivate && e.type === "question_asked";
        recordNotification({
          id: `${env.ts_ms}-${d.runId}-${d.kind}-${callId ?? ""}`,
          kind: d.kind,
          title: isQuestion ? tRef.current("notify.question.title", { bot: name }) : d.secretRequest ? text.title : text.body,
          body: isQuestion ? text.body : "",
          botId: d.botId,
          botName: name,
          chatId: d.chatId,
          ...(!s.notifyPrivate && d.runId ? { runId: d.runId } : {}),
          ...(callId ? { callId } : {}),
          ...(text.retryAtMs !== undefined ? { retryAtMs: text.retryAtMs } : {}),
          ...(!s.notifyPrivate && d.kind === "failed" && d.chatId ? { retryable: true } : {}),
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
      pauseAll: t("tray.pauseAll"),
      stopAll: t("tray.stopAll"),
      quit: t("tray.quit"),
      tooltip: t("tray.tooltip"),
      tooltipNeeds: t("tray.tooltipNeeds"),
      stillRunningTitle: t("tray.stillRunningTitle"),
      stillRunningBody: t("tray.stillRunningBody")
    }).catch(() => {});
  }, [t, language]);

  // The shell decides what closing the window does: tell it the user's choice.
  useEffect(() => {
    void setKeepInTray(settings.keepInTray, settings.trayNoticeSeen).catch(
      () => {}
    );
  }, [settings.keepInTray, settings.trayNoticeSeen]);

  // The global Quick ask hotkey follows the setting; a refusal is shown in Settings in words.
  useEffect(() => {
    let current = true;
    void configureQuickAsk(settings.quickAskEnabled, settings.quickAskHotkey).then((r) => {
      if (current) setQuickAskProblem(r.ok ? null : r.problem);
    });
    return () => {
      current = false;
    };
  }, [settings.quickAskEnabled, settings.quickAskHotkey]);

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
                resolveNotificationByCallId(
                  press.call_id,
                  res.decision === "approve" ? "approved" : "denied"
                );
              } catch {
                openNeedsRef.current();
              }
            }
          })
          .catch(() => {});
      }),
      onShellEvent<null>(SHELL_EVENT_TRAY_NOTICE_SHOWN, () =>
        trayNoticeRef.current()
      ),
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
