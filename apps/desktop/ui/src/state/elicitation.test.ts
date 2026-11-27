import { strict as assert } from "node:assert";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types";
import { EMPTY_EVENTS, type TimelineItem, applyEvent, awaitsOwner, needsYou } from "./timeline.ts";

test("an MCP form is pending once and closes for all views when the daemon settles it", () => {
  const run_id = "run";
  const call_id = "form";
  const wrap = (seq: number, event: DaemonEvent) => ({
    v: 1, seq, gseq: seq + 1, ts_ms: 1000, event
  }) as GlobalEnvelope;
  const started = applyEvent(EMPTY_EVENTS, wrap(0, { type: "run_started", run_id, chat_id: "chat", bot_id: "bot" }));
  const event = {
    type: "elicitation_requested", run_id, call_id, server: "calendar",
    message: "<b>Plain text</b>", fields: [], expires_in_secs: 60
  } as const;
  const opened = applyEvent(started, wrap(1, { ...event, fields: [] }));
  const duplicate = applyEvent(opened, wrap(2, { ...event, fields: [] }));
  assert.equal(needsYou(duplicate, [], 1000).length, 1);
  assert.equal(needsYou(duplicate, [], 1000)[0].elicitation?.message, "<b>Plain text</b>");
  assert.equal(needsYou(duplicate, [], 61_000).length, 0);
  const settled = applyEvent(duplicate, wrap(3, { type: "elicitation_closed", run_id, call_id }));
  assert.equal(needsYou(settled, [], 1000).length, 0);
});

test("a form, a credential request and a hand-over each await the owner exactly until they close", () => {
  const base = { key: "k", callId: "c", requestedMs: 0, expiresMs: 10 };
  const secret = { ...base, kind: "secret" } as TimelineItem;
  const handover = { ...base, kind: "handover" } as TimelineItem;
  const form = { ...base, kind: "elicitation" } as TimelineItem;
  for (const item of [secret, handover, form]) {
    assert.equal(awaitsOwner(item), true, item.kind);
    assert.equal(awaitsOwner({ ...item, closed: true } as TimelineItem), false, `${item.kind} closed`);
  }
  // Only a credential request or hand-over has an outcome; an answered one no longer waits.
  assert.equal(awaitsOwner({ ...secret, outcome: "done" } as TimelineItem), false);
  assert.equal(awaitsOwner({ ...handover, outcome: "cancelled" } as TimelineItem), false);
  // An answered credential request is not "open" just because a form would be.
  assert.equal(awaitsOwner({ ...secret, outcome: "done", closed: false } as TimelineItem), false);
  for (const kind of ["tool", "question", "todo", "text", "notice"] as const) {
    assert.equal(awaitsOwner({ ...base, kind } as unknown as TimelineItem), false, kind);
  }
});
