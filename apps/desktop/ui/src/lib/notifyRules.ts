import type { GlobalEnvelope } from "../api/types.ts";
import type { RunState, ToolItem } from "../state/timeline.ts";
import { isBatchable } from "./approvalBatch.ts";
import { isAlwaysBlockedAction } from "./approvalPlain.ts";
import { describeTool, hostOf, isSendTool, resolveToolCall } from "./toolLabels.ts";

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
  /** A secure form opens in the app. The notice carries no request details or actions. */
  secretRequest?: true;
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
 * Does the approval card offer a standing "always" choice? Mirrors ApprovalCard: not for a
 * once-only call, destructive/credential/payment actions, or unsafe taint-gate cards.
 */
export function alwaysOffered(details: unknown, toolName?: string, input?: unknown): boolean {
  const facts = isObject(details) ? details : undefined;
  const onceOnly =
    typeof facts?.once_only === "boolean" ? facts.once_only : details != null;
  if (typeof facts?.always_offered === "boolean") {
    return facts.always_offered && (!toolName || !isAlwaysBlockedAction(toolName, input, details));
  }
  if (toolName && isAlwaysBlockedAction(toolName, input, details)) {
    return false;
  }
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
  return alwaysOffered(a.details, a.toolName, a.input) ? ["allow", "always", "deny"] : ["allow", "deny"];
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
    case "secret_requested":
      return ctx.prefs.notifyNeedsYou && e.origin && env.ts_ms + e.expires_in_secs * 1000 > Date.now()
        ? { kind: "needsYou", runId: e.run_id, ...base, detail: "", secretRequest: true }
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
  /** Epoch ms when an automatic retry is scheduled, when reported by the error. */
  retryAtMs?: number;
}

type AppName = (server: string) => string | null;

/** `mcp:playwright/browser_evaluate` and `playwright.browser_evaluate` are the same call. */
function canonicalTool(name: string): string {
  const m = /^mcp:([^/]+)\/(.+)$/.exec(name);
  return m ? `${m[1]}.${m[2]}` : name;
}

/**
 * One short human sentence for an approval ("Praxi needs your OK to open webioo.com.tr"):
 * never the raw summary, the arguments, a device id or the `pc:` prefix.
 */
export function approvalSentence(
  a: ApprovalFacts,
  botName: string,
  t: Translate,
  brandName?: AppName
): string {
  const canonical = canonicalTool(a.toolName);
  const call = resolveToolCall(canonical, a.input);
  const rec = isObject(call.input) ? call.input : {};
  const site = typeof rec.url === "string" ? hostOf(rec.url) : null;

  if (call.name === "browser_open" || call.name === "pc_browser_open") {
    return site
      ? t("notify.approval.openSite", { bot: botName, site })
      : t("notify.approval.openPage", { bot: botName });
  }
  if (
    site &&
    (call.name === "web_extract" ||
      call.name === "web_scrape" ||
      call.name === "web_feed" ||
      call.name === "web_metadata" ||
      call.name === "web_sitemap")
  ) {
    return t("notify.approval.readSite", { bot: botName, site });
  }

  const label = describeTool(canonical, a.input, brandName);
  if (label.key.startsWith("chat.step.mcp")) {
    if (
      site &&
      (call.name.endsWith(".browser_navigate") ||
        call.name.endsWith(".browser_tab_new") ||
        call.name.endsWith(".browser_open"))
    ) {
      return t("notify.approval.openSite", { bot: botName, site });
    }
    return t("notify.approval.mcp", {
      bot: botName,
      app: String(label.values.app ?? ""),
      action: String(label.values.action ?? "")
    });
  }

  if (call.name === "shell_run" || call.name === "pc_shell_run" || call.name === "script_run") {
    return t("notify.approval.runCommand", { bot: botName });
  }
  if (call.name === "fs_delete") {
    return t("notify.approval.deleteFile", { bot: botName });
  }
  if (call.name === "fs_write" || call.name === "fs_edit" || call.name === "pc_fs_write") {
    return t("notify.approval.saveFile", { bot: botName });
  }
  if (call.name === "email_send") {
    return t("notify.approval.sendEmail", { bot: botName });
  }
  if (call.name.startsWith("schedule_")) {
    return t("notify.approval.schedule", { bot: botName });
  }

  return t("notify.approval.other", {
    bot: botName,
    what: t(`${label.key}.running`, label.values)
  });
}

type Translate = (key: string, values?: Record<string, string>) => string;

const MAX_BODY = 140;

function clip(text: string, max = MAX_BODY): string {
  const one = text.replace(/\s+/g, " ").trim();
  return one.length > max ? `${one.slice(0, max - 1)}…` : one;
}

function redactSummary(text: string): string {
  return text
    .replace(/\bBearer\s+[A-Za-z0-9._\-~+/]+=*/gi, "Bearer [redacted]")
    .replace(/\b(?:sk|pk|ghp|gho|glpat|xoxb|xoxp)-[A-Za-z0-9_-]{8,}/g, "[redacted]")
    .replace(/([?&][^=\s]+=)[^\s&]+/g, "$1[redacted]");
}

export interface FailedNotification {
  text: string;
  retryInSeconds?: number;
  retryAtMs?: number;
}

/** Turn a run failure string into short human wording ("Run failed: model is busy, retrying in 46 s"). */
export function formatFailedDetail(
  rawError: string,
  t: Translate,
  nowMs?: number
): FailedNotification {
  const clean = redactSummary(rawError)
    .replace(/^(?:error:\s*)?(?:step\s+'[^']+'\s+failed:\s*)?(?:execution error:\s*)?(?:provider error\s*\(\d+\):\s*)?/i, "")
    .replace(/\s+/g, " ")
    .trim();
  const retryMatch = /(?:retrying|retry)\s+in\s+(\d+)\s*s(?:ec(?:ond)?s?)?/i.exec(clean);
  const retryInSeconds = retryMatch ? Math.max(1, Number(retryMatch[1])) : undefined;
  const retryAtMs =
    retryInSeconds !== undefined && nowMs !== undefined
      ? nowMs + retryInSeconds * 1000
      : undefined;

  let reason: string;
  if (/model is busy|overloaded|provider(?: is)? busy|too many requests|rate.?limit|\b429\b|\b503\b/i.test(clean)) {
    reason = t("notify.failed.reasonBusy");
  } else if (/timed?\s*out|etimedout/i.test(clean)) {
    reason = t("notify.failed.reasonTimeout");
  } else if (/failed to fetch|networkerror|econnrefused|enotfound|connection (?:refused|reset|lost)|unreachable/i.test(clean)) {
    reason = t("notify.failed.reasonNetwork");
  } else if (/quota|usage limit|signed out|unauthorized|api key/i.test(clean)) {
    reason = t("notify.failed.reasonAuth");
  } else {
    const stripped = clean
      .replace(/,?\s*(?:retrying|retry)\s+in\s+\d+\s*s(?:ec(?:ond)?s?)?\.?$/i, "")
      .trim();
    reason = clip(stripped, 90);
  }

  if (!reason) {
    return { text: t("notify.generic.failed"), retryInSeconds, retryAtMs };
  }
  const text =
    retryInSeconds !== undefined
      ? t("notify.failed.retrying", { reason, seconds: String(retryInSeconds) })
      : t("notify.failed.withReason", { reason });
  return { text, retryInSeconds, retryAtMs };
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
  brandName?: AppName,
  nowMs?: number
): NotificationText {
  const generic = t(`notify.generic.${d.kind}`);
  if (privateMode) {
    return { title: botName, body: generic };
  }
  if (d.secretRequest) {
    return { title: t("secretRequest.title", { bot: botName }), body: generic };
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
  if (d.kind === "failed" && d.detail) {
    const failed = formatFailedDetail(d.detail, t, nowMs);
    return {
      title: botName,
      body: failed.text,
      ...(failed.retryAtMs !== undefined ? { retryAtMs: failed.retryAtMs } : {})
    };
  }
  if (!d.detail) {
    return { title: botName, body: generic };
  }
  return {
    title: botName,
    body: t(`notify.detail.${d.kind}`, { detail: clip(d.detail) })
  };
}
