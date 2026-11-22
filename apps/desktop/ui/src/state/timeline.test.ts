import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import {
  applyEvent,
  assignChildRuns,
  childRunsOf,
  isSubagentTool,
  subagentStatus,
  topLevelRuns,
  EMPTY_EVENTS,
  type EventsState,
  needsYou,
  reconcile,
  RUN_INTERRUPTED,
  restorePending,
  runText,
  type ToolItem
} from "./timeline.ts";

const RUN = "run-1";
let gseq = 0;
function feed(state: EventsState, event: DaemonEvent, ts = 1000): EventsState {
  gseq += 1;
  const env: GlobalEnvelope = { v: 1, seq: gseq, ts_ms: ts, gseq, event };
  return applyEvent(state, env);
}

function started(): EventsState {
  gseq = 0;
  return feed(EMPTY_EVENTS, {
    type: "run_started",
    run_id: RUN,
    chat_id: "chat-1",
    bot_id: "bot-1"
  });
}

test("message deltas accumulate into one text item", () => {
  let s = started();
  s = feed(s, { type: "message_delta", run_id: RUN, text: "Hel" });
  s = feed(s, { type: "message_delta", run_id: RUN, text: "lo" });
  const items = s.runs[RUN].items;
  assert.equal(items.length, 1);
  assert.equal(items[0].kind === "text" && items[0].text, "Hello");
});

test("a tool call goes through approval to done", () => {
  let s = started();
  s = feed(s, {
    type: "tool_call_requested",
    run_id: RUN,
    call_id: "c1",
    tool_name: "fs_write",
    input: { path: "a" }
  });
  s = feed(s, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "c1",
    tool_name: "fs_write",
    input: { path: "a" },
    summary: "write a"
  });
  assert.equal(needsYou(s).length, 1);
  assert.equal(needsYou(s)[0].kind, "approval");
  s = feed(s, {
    type: "tool_call_approved",
    run_id: RUN,
    call_id: "c1",
    approved_by: "u"
  });
  s = feed(
    s,
    { type: "tool_call_completed", run_id: RUN, call_id: "c1", error: null },
    1500
  );
  const tool = s.runs[RUN].items[0] as ToolItem;
  assert.equal(tool.status, "done");
  assert.equal(tool.gated, true);
  assert.equal(tool.completedMs, 1500);
  assert.equal(needsYou(s).length, 0);
});

test("a denied call stays denied after completion", () => {
  let s = started();
  s = feed(s, {
    type: "tool_call_requested",
    run_id: RUN,
    call_id: "c1",
    tool_name: "shell_run",
    input: {}
  });
  s = feed(s, {
    type: "tool_call_denied",
    run_id: RUN,
    call_id: "c1",
    denied_by: "u",
    reason: "no"
  });
  s = feed(s, {
    type: "tool_call_completed",
    run_id: RUN,
    call_id: "c1",
    error: "Permission denied"
  });
  assert.equal((s.runs[RUN].items[0] as ToolItem).status, "denied");
});

test("a question replaces the ask_user tool card and closes with the run", () => {
  let s = started();
  s = feed(s, {
    type: "tool_call_requested",
    run_id: RUN,
    call_id: "q1",
    tool_name: "ask_user",
    input: {}
  });
  s = feed(s, {
    type: "question_asked",
    run_id: RUN,
    call_id: "q1",
    question: "Which?",
    options: ["a", "b"]
  });
  assert.equal(s.runs[RUN].items.length, 1);
  assert.equal(s.runs[RUN].items[0].kind, "question");
  assert.equal(needsYou(s)[0].kind, "question");
  s = feed(s, { type: "run_finished", run_id: RUN, error: null });
  assert.equal(needsYou(s).length, 0);
});

test("events replayed after a reconnect are ignored", () => {
  let s = started();
  const env: GlobalEnvelope = {
    v: 1,
    seq: 5,
    ts_ms: 1,
    gseq: 1,
    event: { type: "message_delta", run_id: RUN, text: "dup" }
  };
  s = applyEvent(s, env);
  assert.equal(s.runs[RUN].items.length, 0);
});

test("reconcile expires approvals the daemon no longer knows", () => {
  let s = started();
  s = feed(
    s,
    {
      type: "approval_needed",
      run_id: RUN,
      call_id: "c9",
      tool_name: "fs_write",
      input: {},
      summary: "x"
    },
    1000
  );
  // still pending on the daemon: untouched
  assert.equal(
    (reconcile(s, new Set(["c9"]), new Set([RUN]), 5000).runs[RUN]
      .items[0] as ToolItem).status,
    "awaiting_approval"
  );
  // unknown to the daemon and old enough: expired
  assert.equal(
    (reconcile(s, new Set(), new Set([RUN]), 5000).runs[RUN]
      .items[0] as ToolItem).status,
    "expired"
  );
  // too fresh to judge: untouched
  assert.equal(
    (reconcile(s, new Set(), new Set([RUN]), 500).runs[RUN]
      .items[0] as ToolItem).status,
    "awaiting_approval"
  );
});

const PENDING = {
  call_id: "c7",
  run_id: RUN,
  tool_name: "browser_click",
  summary: "browser_click {}",
  input: { selector: "#buy" },
  details: { why: "ask" },
  decision: null,
  requested_at_ms: 1000
};
const RUN_FACT = {
  id: RUN,
  chat_id: "chat-1",
  bot_id: "bot-1",
  state: "waiting_approval",
  created_at_ms: 900
};

test("an approval the daemon still holds comes back as a card the events never delivered", () => {
  // the stream missed approval_needed: the run is known, the card is not
  const s = restorePending(started(), [PENDING], [RUN_FACT], 5000);
  const card = needsYou(s);
  assert.equal(card.length, 1);
  assert.equal(card[0].callId, "c7");
  assert.equal(card[0].chatId, "chat-1");
  assert.equal(card[0].tool?.name, "browser_click");
  assert.deepEqual(card[0].tool?.input, { selector: "#buy" });
  assert.deepEqual(card[0].tool?.details, { why: "ask" });
});

test("a card that the events or reconcile closed while the daemon still waits is revived", () => {
  let s = started();
  s = feed(s, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "c7",
    tool_name: "browser_click",
    input: {},
    summary: "browser_click {}"
  });
  s = reconcile(s, new Set(), new Set([RUN]), 5000); // the list had missed it: expired
  assert.equal(needsYou(s).length, 0);
  s = restorePending(s, [PENDING], [RUN_FACT], 5000);
  assert.equal(needsYou(s).length, 1);
  assert.equal((s.runs[RUN].items[0] as ToolItem).status, "awaiting_approval");
});

test("restore does not undo a decision, a finished run or a newer event", () => {
  let s = started();
  s = feed(s, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "c7",
    tool_name: "browser_click",
    input: {},
    summary: "x"
  });
  s = feed(s, { type: "tool_call_approved", run_id: RUN, call_id: "c7", approved_by: "u" });
  assert.equal(needsYou(restorePending(s, [PENDING], [RUN_FACT], 5000)).length, 0, "approved");
  const done = feed(started(), { type: "run_finished", run_id: RUN, error: null });
  assert.equal(needsYou(restorePending(done, [PENDING], [RUN_FACT], 5000)).length, 0, "finished");
  // the daemon expired it after the list was read: the newer event wins
  let late = started();
  late = feed(late, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "c7",
    tool_name: "browser_click",
    input: {},
    summary: "x"
  });
  late = feed(late, {
    type: "tool_call_denied",
    run_id: RUN,
    call_id: "c7",
    denied_by: "u",
    reason: "expired: nobody answered in time"
  }, 4800);
  assert.equal((late.runs[RUN].items[0] as ToolItem).status, "expired");
  assert.equal(needsYou(restorePending(late, [PENDING], [RUN_FACT], 4000)).length, 0, "newer event");
});

test("a run the events never introduced is placed by the daemon's run list", () => {
  gseq = 0;
  const s = restorePending(EMPTY_EVENTS, [PENDING], [RUN_FACT], 5000);
  assert.equal(needsYou(s)[0].chatId, "chat-1");
  assert.equal(topLevelRuns(s, "chat-1").length, 1);
  // events only: no chat, but the run list still says where it belongs
  let ghost = feed(EMPTY_EVENTS, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "c8",
    tool_name: "fs_write",
    input: {},
    summary: "x"
  });
  assert.equal(needsYou(ghost)[0].chatId, "");
  assert.equal(needsYou(ghost, [RUN_FACT])[0].chatId, "chat-1");
  ghost = restorePending(ghost, [], [], 5000);
  assert.equal(needsYou(ghost).length, 1);
});

test("an expired denial closes the card as expired, and a question with it", () => {
  let s = started();
  s = feed(s, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "c1",
    tool_name: "fs_write",
    input: {},
    summary: "x"
  });
  s = feed(s, {
    type: "question_asked",
    run_id: RUN,
    call_id: "q1",
    question: "Which?",
    options: []
  });
  assert.equal(needsYou(s).length, 2);
  s = feed(s, { type: "tool_call_denied", run_id: RUN, call_id: "c1", denied_by: "u", reason: "expired: nobody answered in time" });
  s = feed(s, { type: "tool_call_denied", run_id: RUN, call_id: "q1", denied_by: "u", reason: "expired: nobody answered in time" });
  assert.equal(needsYou(s).length, 0);
  assert.equal((s.runs[RUN].items[0] as ToolItem).status, "expired");
});

// ------------------------------------------------------------- subagents

function feedChild(
  state: EventsState,
  parent: string,
  event: DaemonEvent,
  ts = 1000
): EventsState {
  gseq += 1;
  const env: GlobalEnvelope = {
    v: 1,
    seq: gseq,
    ts_ms: ts,
    gseq,
    parent_run_id: parent,
    event
  };
  return applyEvent(state, env);
}

test("isSubagentTool matches spawn_subagent", () => {
  assert.equal(isSubagentTool("spawn_subagent"), true);
  assert.equal(isSubagentTool("fs_write"), false);
});

test("subagent_started / finished fold into the child run", () => {
  let s = started();
  s = feed(s, {
    type: "subagent_started",
    run_id: "child-1",
    parent_run_id: RUN,
    task: "research",
    call_id: "c1"
  });
  const child = s.runs["child-1"];
  assert.equal(child.parentRunId, RUN);
  assert.equal(child.chatId, "chat-1");
  assert.equal(child.botId, "bot-1");
  assert.equal(child.task, "research");
  assert.equal(child.callId, "c1");
  assert.equal(child.subagentState, "running");
  assert.equal(subagentStatus(child), "running");
  assert.equal(childRunsOf(s, RUN, "c1").length, 1);
  assert.equal(childRunsOf(s, RUN, "other").length, 0);

  s = feed(s, {
    type: "subagent_finished",
    run_id: "child-1",
    parent_run_id: RUN,
    state: "failed",
    summary: "boom",
    call_id: "c1"
  });
  const done = s.runs["child-1"];
  assert.equal(done.subagentState, "failed");
  assert.equal(done.summary, "boom");
  assert.notEqual(done.finishedMs, undefined);
  assert.equal(subagentStatus(done), "failed");
});

test("a restart re-announces a paused subagent: it runs again", () => {
  let s = started();
  const base = { run_id: "child-1", parent_run_id: RUN, call_id: "c1" };
  s = feed(s, { type: "subagent_started", task: "research", ...base });
  s = feed(s, {
    type: "subagent_finished",
    state: "paused",
    summary: "paused: the run was interrupted",
    ...base
  });
  assert.equal(subagentStatus(s.runs["child-1"]), "paused");
  s = feed(s, { type: "subagent_started", task: "research", ...base });
  assert.equal(s.runs["child-1"].finishedMs, undefined);
  assert.equal(subagentStatus(s.runs["child-1"]), "running");
});

test("child events before subagent_started land via parent_run_id", () => {
  let s = started();
  s = feedChild(s, RUN, {
    type: "message_delta",
    run_id: "child-1",
    text: "hi"
  });
  assert.equal(s.runs["child-1"].parentRunId, RUN);
  assert.equal(s.runs["child-1"].chatId, "chat-1");
  assert.equal(s.runs["child-1"].items.length, 1);
  s = feed(s, {
    type: "subagent_started",
    run_id: "child-1",
    parent_run_id: RUN,
    task: "t"
  });
  assert.equal(s.runs["child-1"].items.length, 1);
  assert.equal(s.runs["child-1"].task, "t");
});

test("a child approval is listed against the parent chat, finished ones not", () => {
  let s = started();
  s = feed(s, {
    type: "subagent_started",
    run_id: "child-1",
    parent_run_id: RUN,
    task: "t",
    call_id: "c1"
  });
  s = feedChild(s, RUN, {
    type: "approval_needed",
    run_id: "child-1",
    call_id: "k1",
    tool_name: "shell_run",
    input: {},
    summary: "run it"
  });
  const list = needsYou(s);
  assert.equal(list.length, 1);
  assert.equal(list[0].runId, "child-1");
  assert.equal(list[0].chatId, "chat-1");
  assert.equal(subagentStatus(s.runs["child-1"]), "waiting_approval");
  s = feed(s, {
    type: "subagent_finished",
    run_id: "child-1",
    parent_run_id: RUN,
    state: "cancelled",
    summary: ""
  });
  assert.equal(needsYou(s).length, 0);
  assert.equal(
    (s.runs["child-1"].items[0] as ToolItem).status,
    "expired"
  );
});

test("topLevelRuns excludes children; grandchildren nest", () => {
  let s = started();
  s = feed(s, {
    type: "tool_call_requested",
    run_id: RUN,
    call_id: "c1",
    tool_name: "spawn_subagent",
    input: {}
  });
  s = feed(s, {
    type: "subagent_started",
    run_id: "child-1",
    parent_run_id: RUN,
    task: "a",
    call_id: "c1"
  });
  s = feedChild(s, RUN, {
    type: "tool_call_requested",
    run_id: "child-1",
    call_id: "g1",
    tool_name: "spawn_subagent",
    input: {}
  });
  s = feedChild(s, "child-1", {
    type: "subagent_started",
    run_id: "grand-1",
    parent_run_id: "child-1",
    task: "b",
    call_id: "g1"
  });
  assert.deepEqual(
    topLevelRuns(s, "chat-1").map((r) => r.id),
    [RUN]
  );
  assert.equal(s.runs["grand-1"].chatId, "chat-1");
  assert.equal(assignChildRuns(s, s.runs[RUN])["c1"].id, "child-1");
  assert.equal(assignChildRuns(s, s.runs["child-1"])["g1"].id, "grand-1");
});

test("assignChildRuns falls back to unmatched children in order", () => {
  let s = started();
  s = feed(s, {
    type: "tool_call_requested",
    run_id: RUN,
    call_id: "c1",
    tool_name: "spawn_subagent",
    input: {}
  });
  s = feed(s, {
    type: "subagent_started",
    run_id: "child-1",
    parent_run_id: RUN,
    task: "a"
  });
  assert.equal(assignChildRuns(s, s.runs[RUN])["c1"].id, "child-1");
});

test("route_decided is stored on the run; runText joins the reply", () => {
  let s = started();
  s = feed(s, {
    type: "route_decided",
    run_id: RUN,
    level: "K1",
    route: { kind: "workflow", name: "daily-report", confidence: 0.9 },
    confidence: 0.9,
    latency_ms: 4,
    reason: "intent cache"
  });
  const r = s.runs[RUN].route;
  assert.equal(r?.level, "K1");
  assert.equal(r?.route.kind, "workflow");
  assert.equal(r?.latencyMs, 4);
  s = feed(s, { type: "message_delta", run_id: RUN, text: "Started a new chat: x" });
  assert.equal(runText(s.runs[RUN]), "Started a new chat: x");
});

test("approval details reach the tool item for the card", () => {
  let s = started();
  s = feed(s, {
    type: "tool_call_requested",
    run_id: RUN,
    call_id: "k9",
    tool_name: "schedule_create",
    input: {}
  });
  s = feed(s, {
    type: "approval_needed",
    run_id: RUN,
    call_id: "k9",
    tool_name: "schedule_create",
    input: {},
    summary: "schedule_create {}",
    details: { kind: "schedule", action: "create", prompt: "p" }
  });
  const tool = s.runs[RUN].items.find((i) => i.kind === "tool") as ToolItem;
  assert.deepEqual(tool.details, { kind: "schedule", action: "create", prompt: "p" });
});

test("checkpoint_restored appends an informational notice item", () => {
  let s = started();
  s = feed(s, { type: "run_finished", run_id: RUN, error: null });
  s = feed(s, {
    type: "checkpoint_restored",
    run_id: RUN,
    checkpoint_id: "cp-abc",
    safety_checkpoint_id: "cp-safe",
    restored: ["/tmp/a.txt", "/tmp/b.txt"]
  });
  const notice = s.runs[RUN].items.find((i) => i.kind === "notice");
  assert.equal(notice?.i18nKey, "chat.notice.checkpointRestored");
  assert.deepEqual(notice?.values, { count: "2", checkpointId: "cp-abc", safetyId: "cp-safe" });
});

test("partial restore persists a warning and the safety recovery id", () => {
  const state = feed(started(), { type: "checkpoint_restored", run_id: RUN, checkpoint_id: "all", safety_checkpoint_id: "cp-safe", restored: [], partial: true });
  const notice = state.runs[RUN].items.find((item) => item.kind === "notice");
  assert.equal(notice?.tone, "danger");
  assert.equal(notice?.i18nKey, "chat.notice.checkpointRestorePartial");
  assert.equal(notice?.values?.safetyId, "cp-safe");
});

test("a run that ends closes an approved step still running, so no spinner outlives the run", () => {
  let s = started();
  s = feed(s, { type: "tool_call_requested", run_id: RUN, call_id: "c1", tool_name: "browser_open", input: { url: "https://google.com" } });
  s = feed(s, { type: "tool_call_approved", run_id: RUN, call_id: "c1", approved_by: "u" });
  assert.equal((s.runs[RUN].items[0] as ToolItem).status, "running");
  s = feed(s, { type: "run_finished", run_id: RUN, error: "cancelled" });
  assert.equal((s.runs[RUN].items[0] as ToolItem).status, "expired");
});

test("reconcile closes a run the daemon no longer lists as live, so its step stops showing as running", () => {
  let s = started();
  s = feed(s, { type: "tool_call_requested", run_id: RUN, call_id: "o1", tool_name: "browser_open", input: {} }, 1000);
  // the daemon lists it as live: untouched
  const live = reconcile(s, new Set(), new Set([RUN]), 5000).runs[RUN];
  assert.equal(live.finishedMs, undefined);
  // not listed (its end event was lost): finished, not an error, the step no longer runs
  const gone = reconcile(s, new Set(), new Set(), 5000).runs[RUN];
  assert.equal(gone.finishedMs, 5000);
  assert.equal(gone.stopped, false);
  assert.equal(gone.error, RUN_INTERRUPTED);
  assert.equal((gone.items[0] as ToolItem).status, "expired");
  // a run younger than the poll is never judged
  assert.equal(reconcile(s, new Set(), new Set(), 500).runs[RUN].finishedMs, undefined);
});
