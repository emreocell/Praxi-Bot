import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiClient } from "./client.ts";
import { createSseParser, subscribeEvents, type SseMessage } from "./events.ts";

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

test("UI SSE sends management and resume headers without a query token", { timeout: 5000 }, async (t) => {
  const client = new ApiClient({ base_url: "http://127.0.0.1:9876", token: "test-management-capability" });
  let requested: { url: string; headers: Headers } | undefined;
  t.mock.method(globalThis, "fetch", async (url: string, init: RequestInit) => {
    requested = { url, headers: new Headers(init.headers) };
    return new Response('id: 8\ndata: {"v":1,"seq":2,"ts_ms":3,"event":{"type":"run_finished"}}\n\n', {
      headers: { "Content-Type": "text/event-stream" }
    });
  });
  let received!: () => void;
  const event = new Promise<void>((resolve) => { received = resolve; });
  const stop = subscribeEvents(client, {
    lastEventId: 7,
    onConnection: () => {},
    onEvent: (envelope) => {
      assert.equal(envelope.gseq, 8);
      received();
    }
  });
  try {
    await event;
    assert.ok(requested);
    assert.equal(requested.url, "http://127.0.0.1:9876/v1/events");
    assert.equal(requested.headers.get("Authorization"), "Bearer test-management-capability");
    assert.equal(requested.headers.get("Last-Event-ID"), "7");
    assert.equal(requested.headers.get("Accept"), "text/event-stream");
  } finally {
    stop();
  }
});
