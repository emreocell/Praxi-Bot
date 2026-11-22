import type { GlobalEnvelope } from "../api/types.ts";
import type { RunState, ToolItem } from "../state/timeline.ts";
import { isBatchable } from "./approvalBatch.ts";
import { describeTool, isSendTool, resolveToolCall } from "./toolLabels.ts";

/** A run that took longer than this notifies when it finishes (unfocused). */
export const LONG_RUN_MS = 30_000;

export type NotifyKind = "needsYou" | "longRun" | "failed" | "scheduleDone" | "digest";

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
  /** Set for an approval: what the call is, so the toast can say it in words and offer buttons. */
  approval?: ApprovalFacts;
}

export interface ApprovalFacts {
  callId: string;
  toolName: string;
  input: unknown;
  details: unknown;
}

export type NotifyActionId = "allow" | "always" | "deny";

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * Does the approval card offer a standing "always" choice?  Mirrors ApprovalCard: not for a
 * once-only call (older events: any details mean once only) and not for a taint-gate card.
 */
export function alwaysOffered(details: unknown): boolean {
  const facts = isObject(details) ? details : undefined;
  const onceOnly =
    typeof facts?.once_only === "boolean" ? facts.once_only : details != null;
  return !onceOnly && facts?.why !== "egress" && facts?.why !== "executes";
}

/**
 * The buttons a toast may carry for this approval.  Only a plain approval gets any: a call that
 * carries a payload to review, a schedule, a bot profile, a control grant or an outbound send
 * keeps its card (the owner has to read it), so its toast only opens the app.
 * Always-allow is offered exactly when the card would offer it.
 */
export function approvalActions(a: ApprovalFacts): NotifyActionId[] {
  const probe = {
    status: "awaiting_approval",
    details: a.details
  } as Pick<ToolItem, "status" | "details"> as ToolItem;
  const real = resolveToolCall(a.toolName, a.input).name;
  if (!isBatchable(probe) || isSendTool(a.toolName) || isSendTool(real)) {
    return [];
  }
  return alwaysOffered(a.details) ? ["allow", "always", "deny"] : ["allow", "deny"];
}

/** What a pressed toast button does: decide through the card's own path, or just open the app. */
export type ActionResolution =
  | { kind: "decide"; decision: "approve" | "deny"; scope?: "once" | "always" }
  | { kind: "open" };

/**
 * Re-check a pressed button against the approval that is pending NOW (it may have been answered,
 * or changed, since the toast appeared).  `pending` is undefined when the call is no longer waiting.
 * Hook for c474's scopes: "allow" is `once` here; a per-task scope would be chosen in this one place.
 */
export function resolveNotificationAction(
  action: string,
  pending: ApprovalFacts | undefined
): ActionResolution {
  if (!pending) {
    return { kind: "open" };
  }
  const offered = approvalActions(pending);
  if (action === "deny" && offered.includes("deny")) {
    return { kind: "decide", decision: "deny" };
  }
  if (action === "allow" && offered.includes("allow")) {
    return { kind: "decide", decision: "approve", scope: "once" };
  }
  if (action === "always" && offered.includes("always")) {
    return { kind: "decide", decision: "approve", scope: "always" };
  }
  return { kind: "open" };
}

export interface NotifyContext {
  /** The run as known before this event was folded into the state. */
  run: RunState | undefined;
  /** When the app (UI) started: older (replayed) events never notify. */
  appStartMs: number;
  /** The main window has focus. */
  focused: boolean;
  prefs: NotifyPrefs;
  /**
   * The person's choice for the scheduled job this run belongs to (`undefined` for a
   * normal chat run).  An approval always notifies; this only shapes how the run ends.
   */
  scheduleNotify?: "always" | "problems" | "never";
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
        ? {
            kind: "needsYou",
            runId: e.run_id,
            ...base,
            detail: e.summary,
            approval: {
              callId: e.call_id,
              toolName: e.tool_name,
              input: e.input,
              details: e.details
            }
          }
        : null;
    case "question_asked":
      return ctx.prefs.notifyNeedsYou
        ? { kind: "needsYou", runId: e.run_id, ...base, detail: e.question }
        : null;
    case "run_finished": {
      if (env.parent_run_id || run?.parentRunId) {
        return null;
      }
      const choice = ctx.scheduleNotify;
      if (choice === "never") {
        return null;
      }
      if (e.error) {
        return ctx.prefs.notifyFailed && !isCancellation(e.error)
          ? { kind: "failed", runId: e.run_id, ...base, detail: e.error }
          : null;
      }
      if (choice === "always") {
        return { kind: "scheduleDone", runId: e.run_id, ...base, detail: "" };
      }
      if (choice === "problems") {
        return null;
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
  /** Toast buttons (Windows only; elsewhere the notification stays plain). */
  actions?: { id: NotifyActionId; label: string }[];
  /** The approval the buttons decide. */
  callId?: string;
}

type AppName = (server: string) => string | null;

/** `mcp:playwright/browser_evaluate` and `playwright.browser_evaluate` are the same call. */
function canonicalTool(name: string): string {
  const m = /^mcp:([^/]+)\/(.+)$/.exec(name);
  return m ? `${m[1]}.${m[2]}` : name;
}

/**
 * One human sentence for an approval ("Praxi wants to use Playwright (browser evaluate)"):
 * never the raw summary, the arguments, a device id or the `pc:` prefix.
 */
export function approvalSentence(
  a: ApprovalFacts,
  botName: string,
  t: Translate,
  brandName?: AppName
): string {
  const label = describeTool(canonicalTool(a.toolName), a.input, brandName);
  if (label.key.startsWith("chat.step.mcp")) {
    return t("notify.approval.mcp", {
      bot: botName,
      app: String(label.values.app ?? ""),
      action: String(label.values.action ?? "")
    });
  }
  return t("notify.approval.other", {
    bot: botName,
    what: t(`${label.key}.running`, label.values)
  });
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
  t: Translate,
  brandName?: AppName
): NotificationText {
  const generic = t(`notify.generic.${d.kind}`);
  if (privateMode || !d.detail) {
    return { title: botName, body: generic };
  }
  if (d.approval) {
    const actions = approvalActions(d.approval).map((id) => ({
      id,
      label: t(`notify.action.${id}`)
    }));
    return {
      title: t("notify.approval.title"),
      body: approvalSentence(d.approval, botName, t, brandName),
      ...(actions.length > 0 ? { actions, callId: d.approval.callId } : {})
    };
  }
  return {
    title: botName,
    body: t(`notify.detail.${d.kind}`, { detail: clip(d.detail) })
  };
}
