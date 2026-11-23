import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import { createStreamBatch } from "./streamBatch.ts";
import { applyEvent, EMPTY_EVENTS } from "./timeline.ts";

test("stream frames batch tokens, preserve replay/tool order and flush approvals immediately; disposal drops queued work", () => {
  const frames = new Map<number, () => void>();
  let nextFrame = 0;
  let seq = 0;
  let state = EMPTY_EVENTS;
  const publications: number[] = [];
  const batch = createStreamBatch((events) => {
    publications.push(events.length);
    state = events.reduce(applyEvent, state);
  }, (callback) => { frames.set(++nextFrame, callback); return nextFrame; }, (id) => { frames.delete(id); });
  const env = (event: DaemonEvent): GlobalEnvelope => ({ v: 1, gseq: ++seq, seq, ts_ms: seq, event });
  batch.push(env({ type: "run_started", run_id: "r", chat_id: "c", bot_id: "b" }));
  const first = env({ type: "message_delta", run_id: "r", text: "Hello" });
  batch.push(first);
  batch.push(first); // reconnect duplicate must still be deduplicated by applyEvent
  batch.push(env({ type: "message_delta", run_id: "r", text: " world" }));
  assert.deepEqual(publications, [1]);
  assert.equal(frames.size, 1);
  frames.values().next().value!();
  assert.deepEqual(publications, [1, 3]);
  assert.equal(state.runs.r.items[0].key, `text-${first.seq}`);
  batch.push(env({ type: "message_delta", run_id: "r", text: "!" }));
  batch.push(env({ type: "approval_needed", run_id: "r", call_id: "a", tool_name: "fs_read", input: {}, summary: "Read" }));
  assert.deepEqual(publications, [1, 3, 1, 1]);
  assert.equal(frames.size, 0);
  batch.push(env({ type: "message_delta", run_id: "r", text: "After tool" }));
  batch.push(env({ type: "run_finished", run_id: "r", error: null }));
  assert.deepEqual(state.runs.r.items.filter((i) => i.kind === "text").map((i) => i.text), ["Hello world!", "After tool"]);
  assert.ok(state.runs.r.finishedMs);
  batch.push(env({ type: "message_delta", run_id: "other", text: "discard" }));
  batch.dispose();
  assert.equal(frames.size, 0);
  assert.equal(state.runs.other, undefined);
});
