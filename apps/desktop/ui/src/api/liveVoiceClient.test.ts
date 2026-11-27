import assert from "node:assert/strict";
import { test } from "node:test";
import { markRemoteBaseUrl } from "../lib/liveViewState.ts";
import { ApiClient } from "./client.ts";
import type { LiveVoiceEvent } from "./types.ts";

const LOCAL = "http://127.0.0.1:47821";

/** A fake live voice endpoint: the daemon's event stream, split at awkward places. */
function fakeStream(chunks: string[]): Response {
  const enc = new TextEncoder();
  return new Response(
    new ReadableStream({
      start(c) {
        for (const chunk of chunks) c.enqueue(enc.encode(chunk));
        c.close();
      }
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } }
  );
}

async function withFetch<T>(fake: typeof fetch, run: () => Promise<T>): Promise<T> {
  const real = globalThis.fetch;
  globalThis.fetch = fake;
  ApiClient.ownerKeyProvider = async () => "owner-key-for-test";
  try {
    return await run();
  } finally {
    globalThis.fetch = real;
    ApiClient.ownerKeyProvider = null;
  }
}

test("liveVoiceOpen posts to the local daemon with the owner key and parses events split across chunks", async () => {
  const seen: { url: string; method: string; headers: Record<string, string>; body: unknown }[] = [];
  const events: LiveVoiceEvent[] = [];
  const ready = `data: {"type":"ready","session_id":"s1","model":"m","input_rate":16000,"output_rate":24000,"max_seconds":900}\n\n`;
  await withFetch(
    (async (url, init) => {
      seen.push({ url: String(url), method: String(init?.method), headers: (init?.headers ?? {}) as Record<string, string>, body: JSON.parse(String(init?.body)) });
      return fakeStream([ready.slice(0, 20), ready.slice(20), `data: {"type":"you","text":"hi"}\n\ndata: {"type":"ended","reason":"closed","message":""}\n\n`]);
    }) as typeof fetch,
    async () => {
      const client = new ApiClient({ base_url: LOCAL, token: "t" });
      await client.liveVoiceOpen({ language: "tr", assistant_name: "Atlas" }, (e) => events.push(e), new AbortController().signal);
    }
  );
  assert.equal(seen[0]?.url, `${LOCAL}/v1/voice/live`);
  assert.equal(seen[0]?.method, "POST");
  assert.equal(seen[0]?.headers["x-praxi-owner-key"], "owner-key-for-test");
  assert.deepEqual(seen[0]?.body, { language: "tr", assistant_name: "Atlas" });
  assert.deepEqual(events.map((e) => e.type), ["ready", "you", "ended"]);
});

test("live voice never goes through a paired host's proxy", async () => {
  const base = "http://127.0.0.1:50999";
  const unmark = markRemoteBaseUrl(base);
  try {
    let called = false;
    await withFetch(
      (async () => {
        called = true;
        return fakeStream([]);
      }) as typeof fetch,
      async () => {
        const client = new ApiClient({ base_url: base, token: "t" });
        await assert.rejects(client.liveVoiceOpen({}, () => undefined, new AbortController().signal), { code: "owner_key_required" });
        await assert.rejects(client.liveVoiceAudio("s1", new Uint8Array(2)), { code: "owner_key_required" });
      }
    );
    assert.equal(called, false);
  } finally {
    unmark();
  }
});

test("a refused open carries the daemon's code", async () => {
  await withFetch(
    (async () => new Response(JSON.stringify({ error: { code: "live_voice_not_configured", message: "no key" } }), { status: 409, headers: { "content-type": "application/json" } })) as typeof fetch,
    async () => {
      const client = new ApiClient({ base_url: LOCAL, token: "t" });
      await assert.rejects(client.liveVoiceOpen({}, () => undefined, new AbortController().signal), { status: 409, code: "live_voice_not_configured" });
    }
  );
});

test("audio goes up as raw bytes; results, notes and the transcript as JSON", async () => {
  const calls: { url: string; method: string; type: string | undefined; body: unknown }[] = [];
  await withFetch(
    (async (url, init) => {
      const headers = (init?.headers ?? {}) as Record<string, string>;
      const body = init?.body instanceof Blob ? `blob:${init.body.size}` : init?.body ? JSON.parse(String(init.body)) : undefined;
      calls.push({ url: String(url), method: String(init?.method), type: headers["Content-Type"], body });
      return new Response(url.toString().endsWith("voice-transcript") ? `{"saved":1}` : null, { status: url.toString().endsWith("voice-transcript") ? 201 : 204 });
    }) as typeof fetch,
    async () => {
      const client = new ApiClient({ base_url: LOCAL, token: "t" });
      await client.liveVoiceAudio("s 1", new Uint8Array(3200));
      await client.liveVoiceToolResult("s1", "c1", "done");
      await client.liveVoiceNote("s1", "waiting");
      await client.liveVoiceClose("s1");
      assert.deepEqual(await client.saveVoiceTranscript("chat1", [{ role: "user", text: "hi" }]), { saved: 1 });
    }
  );
  assert.deepEqual(calls.map((c) => `${c.method} ${c.url.replace(`${LOCAL}/v1`, "")}`), [
    "POST /voice/live/s%201/audio",
    "POST /voice/live/s1/tool",
    "POST /voice/live/s1/note",
    "DELETE /voice/live/s1",
    "POST /chats/chat1/voice-transcript"
  ]);
  assert.equal(calls[0]?.type, "application/octet-stream");
  assert.equal(calls[0]?.body, "blob:3200");
  assert.deepEqual(calls[1]?.body, { call_id: "c1", text: "done" });
  assert.deepEqual(calls[4]?.body, { turns: [{ role: "user", text: "hi" }] });
});
