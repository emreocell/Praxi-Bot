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
