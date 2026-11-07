import type { GlobalEnvelope } from "../api/types.ts";
import type { RunState } from "../state/timeline.ts";

/** A run that took longer than this notifies when it finishes (unfocused). */
export const LONG_RUN_MS = 30_000;

export type NotifyKind = "needsYou" | "longRun" | "failed";

export interface NotifyPrefs {
  notifyNeedsYou: boolean;
  notifyLongRun: boolean;
  notifyFailed: boolean;
}

export interface NotifyDecision {
  kind: NotifyKind;
  runId: string;
  chatId: string;
  botId: string;
  /** What the notification is about: approval summary, question, or error. */
  detail: string;
}

export interface NotifyContext {
  /** The run as known before this event was folded into the state. */
  run: RunState | undefined;
  /** When the app (UI) started: older (replayed) events never notify. */
  appStartMs: number;
  /** The main window has focus. */
  focused: boolean;
  prefs: NotifyPrefs;
}

/** A user-initiated stop is not a failure. */
function isCancellation(error: string): boolean {
  return /cancel|stopped by|aborted by/i.test(error);
}

/**
 * Decide whether a daemon event deserves an OS notification (pure).
 *
 * - approval / question waiting + window unfocused
 * - a run over {@link LONG_RUN_MS} finished fine + window unfocused
 * - a run failed + window unfocused (when the window is focused the run error
 *   is already on screen)
 *
 * Replayed history (older than `appStartMs`) and child (subagent) run endings
 * never notify; an approval inside a subagent does, it still needs the user.
 */
export function decideNotification(
  env: GlobalEnvelope,
  ctx: NotifyContext
): NotifyDecision | null {
  if (env.ts_ms < ctx.appStartMs || ctx.focused) {
    return null;
  }
  const e = env.event;
  const run = ctx.run;
  const base = {
    chatId: run?.chatId ?? "",
    botId: run?.botId ?? ""
  };
  switch (e.type) {
    case "approval_needed":
      return ctx.prefs.notifyNeedsYou
        ? { kind: "needsYou", runId: e.run_id, ...base, detail: e.summary }
        : null;
    case "question_asked":
      return ctx.prefs.notifyNeedsYou
        ? { kind: "needsYou", runId: e.run_id, ...base, detail: e.question }
        : null;
    case "run_finished": {
      if (env.parent_run_id || run?.parentRunId) {
        return null;
      }
      if (e.error) {
        return ctx.prefs.notifyFailed && !isCancellation(e.error)
          ? { kind: "failed", runId: e.run_id, ...base, detail: e.error }
          : null;
      }
      const started = run?.startedMs ?? env.ts_ms;
      return ctx.prefs.notifyLongRun && env.ts_ms - started > LONG_RUN_MS
        ? { kind: "longRun", runId: e.run_id, ...base, detail: "" }
        : null;
    }
    default:
      return null;
  }
}

export interface NotificationText {
  title: string;
  body: string;
}

type Translate = (key: string, values?: Record<string, string>) => string;

const MAX_BODY = 140;

function clip(text: string): string {
  const one = text.replace(/\s+/g, " ").trim();
  return one.length > MAX_BODY ? `${one.slice(0, MAX_BODY - 1)}…` : one;
}

/**
 * Title and body of a notification.  With `privateMode` only the bot name and
 * a short generic text are used: no approval summary, question or error text.
 */
export function notificationText(
  d: NotifyDecision,
  botName: string,
  privateMode: boolean,
  t: Translate
): NotificationText {
  const generic = t(`notify.generic.${d.kind}`);
  if (privateMode || !d.detail) {
    return { title: botName, body: generic };
  }
  return {
    title: botName,
    body: t(`notify.detail.${d.kind}`, { detail: clip(d.detail) })
  };
}
