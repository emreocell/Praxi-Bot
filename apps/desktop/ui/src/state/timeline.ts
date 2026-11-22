import type { DaemonEvent, GlobalEnvelope, TodoItem } from "../api/types";
import { parseRoute, type RouteState } from "../lib/route.ts";

export type ToolStatus =
  | "requested"
  | "awaiting_approval"
  | "running"
  | "denied"
  | "done"
  | "error"
  | "expired";

export interface TextItem {
  kind: "text";
  key: string;
  ts: number;
  text: string;
}

export interface ToolItem {
  kind: "tool";
  key: string;
  callId: string;
  name: string;
  input: unknown;
  status: ToolStatus;
  requestedMs: number;
  completedMs?: number;
  /** Time the tool itself ran (daemon-measured; excludes approval waits). */
  durationMs?: number;
  /** Redacted start of the tool output (at most 2 KB). */
  outputPreview?: string;
  /** Human-readable approval summary (when approval was needed). */
  summary?: string;
  /** Structured approval facts supplied by the tool (e.g. a schedule). */
  details?: unknown;
  /** The call went through an approval gate. */
  gated: boolean;
  error?: string;
  denyReason?: string;
}

export interface QuestionItem {
  kind: "question";
  key: string;
  callId: string;
  question: string;
  options: string[];
  requestedMs: number;
  /** Locally recorded answer, or `closed` when the run moved on without it. */
  answer?: string;
  closed?: boolean;
}

/**
 * A bot's credential request (`secret_requested`). Holds only what the event carries (no value:
 * the typed value lives in the card's own form state and goes straight to the daemon).
 */
export interface SecretRequestItem {
  kind: "secret";
  key: string;
  callId: string;
  /** Vault entry name (without the `fill/` prefix). */
  name: string;
  /** Untrusted model text. */
  purpose: string;
  fields: "secret" | "login";
  replacesExisting: boolean;
  untrustedContent: boolean;
  requestedMs: number;
  expiresMs: number;
  /** How the owner answered here; `closed` when the run moved on or the request expired. */
  outcome?: "stored" | "cancelled";
  closed?: boolean;
}

export interface TodoListItem {
  kind: "todo";
  key: string;
  ts: number;
  items: TodoItem[];
}

export interface NoticeItem {
  kind: "notice";
  key: string;
  ts: number;
  tone: "info" | "warning" | "danger";
  /** i18n key under `chat.notice.*` and its interpolation values. */
  i18nKey: string;
  values: Record<string, string>;
}

export type TimelineItem =
  | TextItem
  | ToolItem
  | QuestionItem
  | SecretRequestItem
  | TodoListItem
  | NoticeItem;

/** What the daemon reports (error event and `run_finished.error`) for a run the user stopped. */
export const RUN_CANCELLED = "cancelled";

/** Shown for a run the daemon stopped listing without a finish event: we cannot know how it ended. */
export const RUN_INTERRUPTED = "Interrupted: the daemon no longer lists this run, so how it ended is unknown.";

export interface RunState {
  id: string;
  chatId: string;
  botId: string;
  startedMs: number;
  finishedMs?: number;
  /** `undefined` while running; `null` = finished fine (or stopped by the user, see `stopped`). */
  error?: string | null;
  /** The user (or a hard stop) cancelled the run: not an error. */
  stopped?: boolean;
  items: TimelineItem[];
  /** The schedule that started this run (scheduled runs only). */
  scheduleId?: string;
  /** Set on child (subagent) runs: the run that spawned this one. */
  parentRunId?: string;
  /** Subagent task description. */
  task?: string;
  /** The parent's `spawn_subagent` tool call id. */
  callId?: string;
  subagentState?: SubagentState;
  /** Final summary of a finished subagent. */
  summary?: string;
  /** How the turn was routed (`route_decided`), when the router is on. */
  route?: RouteState;
}

export type SubagentState =
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "paused";

export interface EventsState {
  runs: Record<string, RunState>;
  /** Highest daemon-wide cursor seen. */
  lastGseq: number;
}

export const EMPTY_EVENTS: EventsState = { runs: {}, lastGseq: 0 };

/** When an item appeared (for interleaving with user messages). */
export function itemTs(i: TimelineItem): number {
  return i.kind === "tool" || i.kind === "question" || i.kind === "secret"
    ? i.requestedMs
    : i.ts;
}

/** What the daemon says when it closes a card nobody answered (`tool_call_denied.reason`). */
export function isExpiredReason(reason: string | null | undefined): boolean {
  return typeof reason === "string" && reason.startsWith("expired");
}

/** Tool names that spawn another agent (rendered as a subagent card). */
export function isSubagentTool(name: string): boolean {
  return /^(subagent|spawn_agent|spawn_subagent|delegate)/.test(name);
}

function withRun(
  state: EventsState,
  runId: string,
  ts: number,
  change: (run: RunState) => RunState
): EventsState {
  const existing: RunState = state.runs[runId] ?? {
    id: runId,
    chatId: "",
    botId: "",
    startedMs: ts,
    items: []
  };
  return { ...state, runs: { ...state.runs, [runId]: change(existing) } };
}

function updateTool(
  run: RunState,
  callId: string,
  fn: (t: ToolItem) => ToolItem
): RunState {
  return {
    ...run,
    items: run.items.map((i) =>
      i.kind === "tool" && i.callId === callId ? fn(i) : i
    )
  };
}

function ensureTool(
  run: RunState,
  e: {
    call_id: string;
    tool_name: string;
    input: unknown;
  },
  ts: number
): RunState {
  if (run.items.some((i) => i.kind === "tool" && i.callId === e.call_id)) {
    return run;
  }
  const item: ToolItem = {
    kind: "tool",
    key: `tool-${e.call_id}`,
    callId: e.call_id,
    name: e.tool_name,
    input: e.input,
    status: "requested",
    requestedMs: ts,
    gated: false
  };
  return { ...run, items: [...run.items, item] };
}

/** Fold one daemon event into the state (pure). */
export function applyEvent(
  state: EventsState,
  env: GlobalEnvelope
): EventsState {
  if (env.gseq > 0 && env.gseq <= state.lastGseq) {
    return state; // duplicate delivered across a reconnect
  }
  const next = adoptParent(
    reduce(state, env.event, env.ts_ms, env.seq),
    env
  );
  return env.gseq > next.lastGseq ? { ...next, lastGseq: env.gseq } : next;
}

/**
 * Child-run events may arrive before `subagent_started`: record the parent
 * link from the envelope and inherit chat / bot from the parent run.
 */
function adoptParent(state: EventsState, env: GlobalEnvelope): EventsState {
  const parentId = env.parent_run_id;
  const e = env.event as { run_id?: string | null };
  if (!parentId || !e.run_id) {
    return state;
  }
  const run = state.runs[e.run_id];
  if (!run) {
    return state;
  }
  const parent = state.runs[parentId];
  const chatId = run.chatId || parent?.chatId || "";
  const botId = run.botId || parent?.botId || "";
  if (
    run.parentRunId === parentId &&
    run.chatId === chatId &&
    run.botId === botId
  ) {
    return state;
  }
  return {
    ...state,
    runs: {
      ...state.runs,
      [e.run_id]: { ...run, parentRunId: parentId, chatId, botId }
    }
  };
}

function closeOpenItems(items: TimelineItem[]): TimelineItem[] {
  return items.map((i) =>
    i.kind === "question" && i.answer === undefined
      ? { ...i, closed: true }
      : i.kind === "secret"
        ? { ...i, closed: true }
        : i.kind === "tool" &&
          (i.status === "awaiting_approval" ||
            i.status === "requested" ||
            i.status === "running")
        ? { ...i, status: "expired" as const }
        : i
  );
}

function reduce(
  state: EventsState,
  e: DaemonEvent,
  ts: number,
  seq: number
): EventsState {
  switch (e.type) {
    case "run_started":
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        chatId: e.chat_id,
        botId: e.bot_id,
        scheduleId: e.schedule_id ?? undefined,
        startedMs: ts
      }));
    case "run_finished":
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        finishedMs: ts,
        // A user Stop is not a failure: the daemon reports it as the error "cancelled".
        error: e.error === RUN_CANCELLED ? null : e.error,
        stopped: e.error === RUN_CANCELLED,
        // an unanswered question cannot be answered once the run is over
        items: closeOpenItems(r.items)
      }));
    case "subagent_started":
      return withRun(state, e.run_id, ts, (r) => {
        const parent = state.runs[e.parent_run_id];
        return {
          ...r,
          chatId: r.chatId || parent?.chatId || "",
          botId: r.botId || parent?.botId || "",
          parentRunId: e.parent_run_id,
          task: e.task,
          callId: e.call_id ?? r.callId,
          // A restart re-announces an adopted child: it runs again.
          subagentState: "running" as const,
          finishedMs: undefined,
          summary: undefined,
          startedMs: r.items.length === 0 ? ts : r.startedMs
        };
      });
    case "subagent_finished":
      return withRun(state, e.run_id, ts, (r) => {
        const parent = state.runs[e.parent_run_id];
        return {
          ...r,
          chatId: r.chatId || parent?.chatId || "",
          botId: r.botId || parent?.botId || "",
          parentRunId: e.parent_run_id,
          callId: e.call_id ?? r.callId,
          finishedMs: ts,
          subagentState: e.state,
          summary: e.summary,
          items: closeOpenItems(r.items)
        };
      });
    case "message_delta":
      return withRun(state, e.run_id, ts, (r) => {
        const last = r.items[r.items.length - 1];
        if (last?.kind === "text") {
          return {
            ...r,
            items: [
              ...r.items.slice(0, -1),
              { ...last, text: last.text + e.text }
            ]
          };
        }
        return {
          ...r,
          items: [
            ...r.items,
            { kind: "text", key: `text-${seq}`, ts, text: e.text }
          ]
        };
      });
    case "tool_call_requested":
      return withRun(state, e.run_id, ts, (r) => ensureTool(r, e, ts));
    case "approval_needed":
      return withRun(state, e.run_id, ts, (r) =>
        updateTool(ensureTool(r, e, ts), e.call_id, (t) => ({
          ...t,
          status: "awaiting_approval",
          summary: e.summary,
          details: e.details,
          gated: true
        }))
      );
    case "tool_call_approved":
      return withRun(state, e.run_id, ts, (r) =>
        updateTool(r, e.call_id, (t) => ({
          ...t,
          status: "running",
          gated: true
        }))
      );
    case "tool_call_denied":
      return withRun(state, e.run_id, ts, (r) => {
        const expired = isExpiredReason(e.reason);
        const denied = updateTool(r, e.call_id, (t) => ({
          ...t,
          // A card nobody answered in time (or whose wait ended) is "expired", not a refusal.
          status: expired ? "expired" : "denied",
          denyReason: e.reason ?? undefined,
          completedMs: ts
        }));
        return {
          ...denied,
          items: denied.items.map((i) =>
            expired &&
            i.kind === "question" &&
            i.callId === e.call_id &&
            i.answer === undefined
              ? { ...i, closed: true }
              : i.kind === "secret" && i.callId === e.call_id
                ? { ...i, closed: true }
                : i
          )
        };
      });
    case "tool_call_completed":
      return withRun(state, e.run_id, ts, (r) => {
        const done = updateTool(r, e.call_id, (t) =>
          t.status === "denied"
            ? t
            : {
                ...t,
                status: e.error ? "error" : "done",
                error: e.error ?? undefined,
                completedMs: ts,
                durationMs: e.duration_ms ?? undefined,
                outputPreview: e.output_preview ?? undefined
              }
        );
        return {
          ...done,
          items: done.items.map((i) =>
            i.kind === "question" &&
            i.callId === e.call_id &&
            i.answer === undefined
              ? { ...i, closed: true }
              : i.kind === "secret" && i.callId === e.call_id
                ? { ...i, closed: true }
                : i
          )
        };
      });
    case "todo_updated":
      return withRun(state, e.run_id, ts, (r) => {
        const idx = r.items.findIndex((i) => i.kind === "todo");
        const item: TodoListItem = {
          kind: "todo",
          key: idx >= 0 ? r.items[idx].key : `todo-${seq}`,
          ts: idx >= 0 ? itemTs(r.items[idx]) : ts,
          items: e.items
        };
        return {
          ...r,
          items:
            idx >= 0
              ? r.items.map((i, n) => (n === idx ? item : i))
              : [...r.items, item]
        };
      });
    case "question_asked":
      return withRun(state, e.run_id, ts, (r) => {
        const q: QuestionItem = {
          kind: "question",
          key: `question-${e.call_id}`,
          callId: e.call_id,
          question: e.question,
          options: e.options,
          requestedMs: ts
        };
        const idx = r.items.findIndex(
          (i) => i.kind === "tool" && i.callId === e.call_id
        );
        return {
          ...r,
          // the `ask_user` tool card becomes the question card
          items:
            idx >= 0
              ? r.items.map((i, n) => (n === idx ? q : i))
              : [...r.items, q]
        };
      });
    case "model_fallback":
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        items: [
          ...r.items,
          {
            kind: "notice",
            key: `notice-${seq}`,
            ts,
            tone: "warning",
            i18nKey: "chat.notice.modelFallback",
            values: { from: e.from, to: e.to, reason: e.reason }
          }
        ]
      }));
    case "error":
      if (e.message === RUN_CANCELLED) {
        return state; // the end of a user Stop, shown as "Stopped" instead of an error
      }
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        items: [
          ...r.items,
          {
            kind: "notice",
            key: `notice-${seq}`,
            ts,
            tone: e.recoverable ? "warning" : "danger",
            i18nKey: "chat.notice.error",
            values: { message: e.message }
          }
        ]
      }));
    case "workflow_drift":
      if (!e.run_id) {
        return state;
      }
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        items: [
          ...r.items,
          {
            kind: "notice",
            key: `notice-${seq}`,
            ts,
            tone: "warning",
            i18nKey: "chat.notice.workflowDrift",
            values: { workflow: e.workflow, reason: e.reason }
          }
        ]
      }));

    case "checkpoint_restored":
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        items: [
          ...r.items,
          {
            kind: "notice",
            key: `notice-${seq}`,
            ts,
            tone: e.partial ? "danger" : "info",
            i18nKey: e.partial ? "chat.notice.checkpointRestorePartial" : "chat.notice.checkpointRestored",
            values: {
              count: String(e.restored.length),
              checkpointId: e.checkpoint_id,
              safetyId: e.safety_checkpoint_id
            }
          }
        ]
      }));
    case "route_decided":
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        route: {
          level: e.level,
          route: parseRoute(e.route),
          confidence: e.confidence,
          latencyMs: e.latency_ms,
          reason: e.reason
        }
      }));
    case "workflow_step_started":
    case "workflow_step_completed":
    case "control_requested":
    case "control_state_changed":
    case "view_targets_changed":
    case "mcp_server_changed":
      return state;
    case "secret_requested":
      return withRun(state, e.run_id, ts, (r) => {
        const item: SecretRequestItem = {
          kind: "secret",
          key: `secret-${e.call_id}`,
          callId: e.call_id,
          name: e.name,
          purpose: e.purpose,
          fields: e.fields,
          replacesExisting: e.replaces_existing,
          untrustedContent: e.untrusted_content,
          requestedMs: ts,
          expiresMs: ts + Math.max(0, e.expires_in_secs) * 1000
        };
        const idx = r.items.findIndex(
          (i) => (i.kind === "tool" || i.kind === "secret") && i.callId === e.call_id
        );
        return {
          ...r,
          // the `secret_request` tool card becomes the form card
          items:
            idx >= 0
              ? r.items.map((i, n) => (n === idx ? item : i))
              : [...r.items, item]
        };
      });
  }
}

/** Child runs of `parentRunId` (optionally only those for one tool call). */
export function childRunsOf(
  state: EventsState,
  parentRunId: string,
  callId?: string
): RunState[] {
  return Object.values(state.runs)
    .filter(
      (r) =>
        r.parentRunId === parentRunId &&
        (callId === undefined || r.callId === callId)
    )
    .sort((a, b) => a.startedMs - b.startedMs);
}

/**
 * Match each subagent tool call of `run` to its child run: by `callId`, then
 * (old logs) the unmatched children in order. Keyed by tool call id.
 */
export function assignChildRuns(
  state: EventsState,
  run: RunState
): Record<string, RunState> {
  const children = childRunsOf(state, run.id);
  const out: Record<string, RunState> = {};
  const tools = run.items.filter(
    (i): i is ToolItem => i.kind === "tool" && isSubagentTool(i.name)
  );
  const used = new Set<string>();
  for (const t of tools) {
    const m = children.find((c) => c.callId === t.callId);
    if (m) {
      out[t.callId] = m;
      used.add(m.id);
    }
  }
  const toolCallIds = new Set(tools.map((t) => t.callId));
  const spare = children.filter(
    (c) =>
      !used.has(c.id) && (c.callId === undefined || !toolCallIds.has(c.callId))
  );
  for (const t of tools) {
    if (!out[t.callId] && spare.length > 0) {
      out[t.callId] = spare.shift() as RunState;
    }
  }
  return out;
}

/** Runs shown as top-level chat runs (children render inside their card). */
export function topLevelRuns(state: EventsState, chatId: string): RunState[] {
  return Object.values(state.runs)
    .filter((r) => r.chatId === chatId && r.parentRunId === undefined)
    .sort((a, b) => a.startedMs - b.startedMs);
}

/** Live status of a subagent run for its card chip. */
export type SubagentStatus =
  | "running"
  | "waiting_approval"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "paused";

export function subagentStatus(run: RunState): SubagentStatus {
  if (run.finishedMs !== undefined) {
    const s = run.subagentState;
    if (s && s !== "running") {
      return s;
    }
    return run.error ? "failed" : "succeeded";
  }
  return run.items.some(
    (i) => i.kind === "tool" && i.status === "awaiting_approval"
  )
    ? "waiting_approval"
    : "running";
}

/** Record the user's answer to a question card locally (optimistic). */
export function markAnswered(
  state: EventsState,
  runId: string,
  callId: string,
  answer: string
): EventsState {
  const run = state.runs[runId];
  if (!run) {
    return state;
  }
  return {
    ...state,
    runs: {
      ...state.runs,
      [runId]: {
        ...run,
        items: run.items.map((i) =>
          i.kind === "question" && i.callId === callId ? { ...i, answer } : i
        )
      }
    }
  };
}

/** Remember how the owner answered a credential request (never a value). */
export function recordSecretOutcome(
  state: EventsState,
  runId: string,
  callId: string,
  outcome: "stored" | "cancelled"
): EventsState {
  const run = state.runs[runId];
  if (!run) {
    return state;
  }
  return {
    ...state,
    runs: {
      ...state.runs,
      [runId]: {
        ...run,
        items: run.items.map((i) =>
          i.kind === "secret" && i.callId === callId ? { ...i, outcome } : i
        )
      }
    }
  };
}

/**
 * Cross-check approvals / questions with the daemon's own picture: anything
 * still shown as pending but unknown to the daemon (e.g. from a previous daemon
 * process that was replayed from the event log) is marked expired / closed.
 * Only items older than `asOfMs` are touched (guards against racing a fresh
 * event).
 */
export function reconcile(
  state: EventsState,
  pendingApprovals: ReadonlySet<string>,
  liveRunIds: ReadonlySet<string>,
  asOfMs: number
): EventsState {
  let changed = false;
  const runs: Record<string, RunState> = {};
  for (const [id, run] of Object.entries(state.runs)) {
    let runChanged = false;
    const items = run.items.map((i) => {
      if (
        i.kind === "tool" &&
        i.status === "awaiting_approval" &&
        i.requestedMs < asOfMs &&
        !pendingApprovals.has(i.callId)
      ) {
        runChanged = true;
        return { ...i, status: "expired" as const };
      }
      if (
        i.kind === "question" &&
        i.answer === undefined &&
        !i.closed &&
        i.requestedMs < asOfMs &&
        !liveRunIds.has(id)
      ) {
        runChanged = true;
        return { ...i, closed: true };
      }
      if (
        i.kind === "secret" &&
        !i.closed &&
        i.outcome === undefined &&
        i.requestedMs < asOfMs &&
        !liveRunIds.has(id)
      ) {
        runChanged = true;
        return { ...i, closed: true };
      }
      return i;
    });
    // The daemon is the source of truth for "is it running": an unfinished run it does not list as
    // live lost its end event (stopped elsewhere, daemon restart, dropped stream). Close it, or the
    // panel keeps showing its step as running forever while Stop finds nothing to stop.
    const orphan = run.finishedMs === undefined && run.startedMs < asOfMs && !liveRunIds.has(id);
    if (orphan) {
      changed = true;
      runs[id] = { ...run, items: closeOpenItems(items), finishedMs: asOfMs, error: RUN_INTERRUPTED, stopped: false };
    } else if (runChanged) {
      changed = true;
      runs[id] = { ...run, items };
    } else {
      runs[id] = run;
    }
  }
  return changed ? { ...state, runs } : state;
}

export interface NeedsYouEntry {
  kind: "approval" | "question" | "secret";
  runId: string;
  chatId: string;
  botId: string;
  callId: string;
  title: string;
  sinceMs: number;
  /** The card to render, for an approval. */
  tool?: ToolItem;
  /** The card to render, for a question. */
  question?: QuestionItem;
  /** The card to render, for a credential request. */
  secret?: SecretRequestItem;
}

/** What the daemon's run list says about a run (enough to place a run the events never introduced). */
export interface RunFact {
  id: string;
  chat_id?: string | null;
  bot_id: string;
  parent_run_id?: string | null;
  state: string;
  created_at_ms: number;
}

/** A pending approval of the daemon's list (`GET /v1/approvals`). */
export interface PendingApprovalFact {
  call_id: string;
  run_id: string;
  tool_name: string;
  summary: string;
  input?: unknown;
  details?: unknown;
  decision?: string | null;
  requested_at_ms: number;
}

/**
 * Everything waiting for a human decision, oldest first. `runs` (the daemon's run list) places a
 * run whose `run_started` the events never carried: without it such an entry has no chat and no
 * card could ever show it.
 */
export function needsYou(
  state: EventsState,
  runs: readonly RunFact[] = [],
  nowMs: number = Date.now()
): NeedsYouEntry[] {
  const out: NeedsYouEntry[] = [];
  for (const run of Object.values(state.runs)) {
    if (run.finishedMs !== undefined) {
      continue;
    }
    const fact = runs.find((r) => r.id === run.id);
    const chatId = run.chatId || fact?.chat_id || "";
    const botId = run.botId || fact?.bot_id || "";
    for (const i of run.items) {
      if (i.kind === "tool" && i.status === "awaiting_approval") {
        out.push({
          kind: "approval",
          runId: run.id,
          chatId,
          botId,
          callId: i.callId,
          title: i.summary ?? i.name,
          sinceMs: i.requestedMs,
          tool: i
        });
      } else if (
        i.kind === "question" &&
        i.answer === undefined &&
        !i.closed
      ) {
        out.push({
          kind: "question",
          runId: run.id,
          chatId,
          botId,
          callId: i.callId,
          title: i.question,
          sinceMs: i.requestedMs,
          question: i
        });
      } else if (i.kind === "secret" && i.outcome === undefined && !i.closed && i.expiresMs > nowMs) {
        out.push({
          kind: "secret",
          runId: run.id,
          chatId,
          botId,
          callId: i.callId,
          title: i.name,
          sinceMs: i.requestedMs,
          secret: i
        });
      }
    }
  }
  return out.sort((a, b) => a.sinceMs - b.sinceMs);
}

/**
 * Bring back the cards the daemon still holds but the events did not leave on screen (the stream
 * missed the `approval_needed`, or a later event closed the card while the daemon still waits).
 * The daemon's pending list wins over the local picture, for runs that are not finished.
 * Only cards older than `asOfMs` are touched (guards against racing a fresh event).
 */
export function restorePending(
  state: EventsState,
  pending: readonly PendingApprovalFact[],
  runs: readonly RunFact[],
  asOfMs: number
): EventsState {
  let next = state;
  for (const a of pending) {
    if (a.decision != null || a.requested_at_ms >= asOfMs) {
      continue;
    }
    const known = next.runs[a.run_id];
    if (known?.finishedMs !== undefined) {
      continue;
    }
    const fact = runs.find((r) => r.id === a.run_id);
    if (!known && !fact) {
      continue;
    }
    const existing = known?.items.find(
      (i): i is ToolItem => i.kind === "tool" && i.callId === a.call_id
    );
    if (
      existing &&
      ((existing.status !== "requested" && existing.status !== "expired") ||
        // closed by an event newer than the list: the list is the stale one
        (existing.completedMs ?? 0) >= asOfMs)
    ) {
      continue; // already shown, or really decided / finished
    }
    const revive = (t: ToolItem): ToolItem => ({
      ...t,
      status: "awaiting_approval",
      summary: t.summary ?? a.summary,
      details: t.details ?? a.details,
      input: t.input ?? a.input ?? null,
      gated: true,
      completedMs: undefined
    });
    next = withRun(next, a.run_id, a.requested_at_ms, (r) => {
      const base: RunState = {
        ...r,
        chatId: r.chatId || fact?.chat_id || "",
        botId: r.botId || fact?.bot_id || "",
        parentRunId: r.parentRunId ?? fact?.parent_run_id ?? undefined,
        startedMs: r.items.length === 0 && fact ? fact.created_at_ms : r.startedMs
      };
      if (existing) {
        return updateTool(base, a.call_id, revive);
      }
      const item: ToolItem = revive({
        kind: "tool",
        key: `tool-${a.call_id}`,
        callId: a.call_id,
        name: a.tool_name,
        input: null,
        status: "requested",
        requestedMs: a.requested_at_ms,
        gated: true
      });
      return { ...base, items: [...base.items, item] };
    });
  }
  return next;
}

/** Whether `name` looks like an outbound send ("Send on your behalf?"). */
export function isOutboundSend(name: string): boolean {
  return /(^|_)(send|post|reply|publish|email|mail|message|dm|tweet)(_|$)/.test(
    name
  );
}

/** The text the assistant produced in `run` (its top-level text items). */
export function runText(run: RunState): string {
  return run.items
    .filter((i): i is TextItem => i.kind === "text")
    .map((i) => i.text)
    .join("\n");
}
