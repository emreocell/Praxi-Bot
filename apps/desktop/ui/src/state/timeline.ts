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
  | TodoListItem
  | NoticeItem;

export interface RunState {
  id: string;
  chatId: string;
  botId: string;
  startedMs: number;
  finishedMs?: number;
  /** `undefined` while running; `null` = finished fine. */
  error?: string | null;
  items: TimelineItem[];
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
  return i.kind === "tool" || i.kind === "question" ? i.requestedMs : i.ts;
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
      : i.kind === "tool" &&
          (i.status === "awaiting_approval" || i.status === "requested")
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
        startedMs: ts
      }));
    case "run_finished":
      return withRun(state, e.run_id, ts, (r) => ({
        ...r,
        finishedMs: ts,
        error: e.error,
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
      return withRun(state, e.run_id, ts, (r) =>
        updateTool(r, e.call_id, (t) => ({
          ...t,
          status: "denied",
          denyReason: e.reason ?? undefined,
          completedMs: ts
        }))
      );
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
      return state;
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
      return i;
    });
    if (runChanged) {
      changed = true;
      runs[id] = { ...run, items };
    } else {
      runs[id] = run;
    }
  }
  return changed ? { ...state, runs } : state;
}

export interface NeedsYouEntry {
  kind: "approval" | "question";
  runId: string;
  chatId: string;
  botId: string;
  callId: string;
  title: string;
  sinceMs: number;
}

/** Everything waiting for a human decision, oldest first. */
export function needsYou(state: EventsState): NeedsYouEntry[] {
  const out: NeedsYouEntry[] = [];
  for (const run of Object.values(state.runs)) {
    if (run.finishedMs !== undefined) {
      continue;
    }
    for (const i of run.items) {
      if (i.kind === "tool" && i.status === "awaiting_approval") {
        out.push({
          kind: "approval",
          runId: run.id,
          chatId: run.chatId,
          botId: run.botId,
          callId: i.callId,
          title: i.summary ?? i.name,
          sinceMs: i.requestedMs
        });
      } else if (
        i.kind === "question" &&
        i.answer === undefined &&
        !i.closed
      ) {
        out.push({
          kind: "question",
          runId: run.id,
          chatId: run.chatId,
          botId: run.botId,
          callId: i.callId,
          title: i.question,
          sinceMs: i.requestedMs
        });
      }
    }
  }
  return out.sort((a, b) => a.sinceMs - b.sinceMs);
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
