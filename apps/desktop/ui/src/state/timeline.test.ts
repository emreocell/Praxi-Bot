import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import {
  applyEvent,
  EMPTY_EVENTS,
  type EventsState,
  needsYou,
  reconcile,
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
