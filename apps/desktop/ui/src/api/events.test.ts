import assert from "node:assert/strict";
import { test } from "node:test";
import { createSseParser, parseSseEnvelope, type SseMessage } from "./events.ts";

test("parses events split across chunks, with ids and comments", () => {
  const got: SseMessage[] = [];
  const feed = createSseParser((m) => got.push(m));
  feed(": keep-alive\n\nid: 7\nevent: message_delta\nda");
  feed('ta: {"a":1}\n\nid: 8\r\nevent: run_finished\r\ndata: x\r\ndata: y\r\n\r\n');
  assert.deepEqual(got, [
    { id: "7", event: "message_delta", data: '{"a":1}' },
    { id: "8", event: "run_finished", data: "x\ny" }
  ]);
});

test("an event without data is not dispatched", () => {
  const got: SseMessage[] = [];
  const feed = createSseParser((m) => got.push(m));
  feed("id: 1\nevent: ping\n\n");
  assert.equal(got.length, 0);
});

test("parseSseEnvelope handles control_state_changed without advancing cursor", () => {
  const parsed = parseSseEnvelope(
    {
      id: "42",
      event: "control_state_changed",
      data: JSON.stringify({
        type: "control_state_changed",
        state: "active",
        owner: "local-ui:1",
        target_id: "00000000-0000-4000-8000-000000000001",
        reason: null
      })
    },
    42,
    1000
  );
  assert.ok(parsed);
  assert.equal(parsed.nextCursor, 42);
  assert.equal(parsed.envelope.event.type, "control_state_changed");
  assert.equal(parsed.envelope.gseq, 42);
});
