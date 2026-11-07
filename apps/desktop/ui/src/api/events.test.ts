import assert from "node:assert/strict";
import { test } from "node:test";
import { createSseParser, type SseMessage } from "./events.ts";

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
