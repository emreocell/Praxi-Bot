import assert from "node:assert/strict";
import { test } from "node:test";
import { markRemoteBaseUrl } from "../lib/liveViewState.ts";
import { ApiClient } from "./client.ts";

/** Records the headers of the one request a client makes. */
async function headersOf(client: ApiClient): Promise<Record<string, string>> {
  const real = globalThis.fetch;
  let seen: Record<string, string> = {};
  globalThis.fetch = (async (_url: unknown, init?: { headers?: Record<string, string> }) => {
    seen = init?.headers ?? {};
    return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
  try {
    await client.health();
  } finally {
    globalThis.fetch = real;
  }
  return seen;
}

test("the owner key goes to this computer's daemon only, never through a host proxy", async () => {
  ApiClient.ownerKeyProvider = async () => "owner-key-for-test";
  try {
    const local = new ApiClient({ base_url: "http://127.0.0.1:47821", token: "t" });
    const proxy = new ApiClient({ base_url: "http://127.0.0.1:50999", token: "p" });
    const unmark = markRemoteBaseUrl("http://127.0.0.1:50999");
    try {
      // `health` is unauthenticated; use a call that sends the bearer path.
      const localHeaders = await headersOf(local);
      const proxyHeaders = await headersOf(proxy);
      assert.equal(localHeaders["x-praxi-owner-key"], "owner-key-for-test");
      assert.equal(proxyHeaders["x-praxi-owner-key"], undefined);
    } finally {
      unmark();
    }
  } finally {
    ApiClient.ownerKeyProvider = null;
  }
});

test("offline read-aloud sends reply text only to the local owner daemon", async () => {
  const local = new ApiClient({ base_url: "http://127.0.0.1:47821", token: "t" });
  const proxy = new ApiClient({ base_url: "http://127.0.0.1:50999", token: "p" });
  const unmark = markRemoteBaseUrl(proxy.baseUrl);
  const realFetch = globalThis.fetch;
  let fetches = 0;
  globalThis.fetch = (async (url, init) => {
    fetches += 1;
    assert.equal(String(url), `${local.baseUrl}/v1/voice/synthesize`);
    assert.equal(init?.redirect, "error");
    assert.equal(new Headers(init?.headers).get("x-praxi-owner-key"), "owner-key-for-test");
    assert.deepEqual(JSON.parse(String(init?.body)), { text: "Günaydın.", language: "tr-TR" });
    return new Response("WAV", { headers: { "content-type": "audio/wav" } });
  }) as typeof fetch;
  const signal = new AbortController().signal;
  try {
    await assert.rejects(local.synthesizeVoice("Günaydın.", "tr-TR", signal), { status: 403 });
    ApiClient.ownerKeyProvider = async () => "owner-key-for-test";
    await assert.rejects(proxy.synthesizeVoice("Günaydın.", "tr-TR", signal), { status: 403 });
    assert.equal(fetches, 0);
    assert.equal((await local.synthesizeVoice("Günaydın.", "tr-TR", signal)).type, "audio/wav");
    assert.equal(fetches, 1);
  } finally {
    ApiClient.ownerKeyProvider = null;
    globalThis.fetch = realFetch;
    unmark();
    local.dispose();
    proxy.dispose();
  }
});

test("a managed host update sends the exact tag and accepts an empty 202 response", async () => {
  const real = globalThis.fetch;
  const base = "http://127.0.0.1:50999";
  const unmark = markRemoteBaseUrl(base);
  const client = new ApiClient({ base_url: base, token: "proxy-fixture" });
  globalThis.fetch = (async (url, init) => {
    assert.equal(String(url), `${base}/v1/admin/host-update`);
    assert.equal(init?.method, "POST");
    assert.deepEqual(JSON.parse(String(init?.body)), { tag: "v0.1.0-alpha.6" });
    return new Response(null, { status: 202 });
  }) as typeof fetch;
  try {
    await client.startManagedHostUpdate("v0.1.0-alpha.6");
  } finally {
    globalThis.fetch = real;
    unmark();
    client.dispose();
  }
});

test("a secure form only sends its body to the verified local owner daemon", async () => {
  const local = new ApiClient({ base_url: "http://127.0.0.1:47821", token: "t" });
  const proxy = new ApiClient({ base_url: "http://127.0.0.1:50999", token: "p" });
  const unmark = markRemoteBaseUrl(proxy.baseUrl);
  const fetch = globalThis.fetch;
  const ownerProvider = ApiClient.ownerKeyProvider;
  const requests: RequestInit[] = [];
  globalThis.fetch = async (_url, init) => {
    requests.push(init ?? {});
    return Response.json({ outcome: "stored" });
  };
  const body = { value: "canary-input-142" };
  try {
    ApiClient.ownerKeyProvider = async () => null;
    await assert.rejects(local.submitSecretRequest("test-id", body), { code: "owner_key_required" });
    ApiClient.ownerKeyProvider = async () => "owner-key-for-test";
    await assert.rejects(proxy.submitSecretRequest("test-id", body), { code: "owner_key_required" });
    assert.equal(requests.length, 0);

    await local.submitSecretRequest("test-id", body);
    assert.equal(requests.length, 1);
    assert.equal(requests[0].redirect, "error");
    assert.deepEqual(JSON.parse(String(requests[0].body)), body);
  } finally {
    unmark();
    globalThis.fetch = fetch;
    ApiClient.ownerKeyProvider = ownerProvider;
  }
});

test("an MCP form answer and its cancel only go to the verified local owner daemon, like a secret request", async () => {
  const local = new ApiClient({ base_url: "http://127.0.0.1:47821", token: "t" });
  const proxy = new ApiClient({ base_url: "http://127.0.0.1:50999", token: "p" });
  const unmark = markRemoteBaseUrl(proxy.baseUrl);
  const fetch = globalThis.fetch;
  const ownerProvider = ApiClient.ownerKeyProvider;
  const requests: { url: string; init: RequestInit }[] = [];
  globalThis.fetch = async (url, init) => {
    requests.push({ url: String(url), init: init ?? {} });
    return Response.json({ call_id: "c1", outcome: "accepted" });
  };
  const answer = { action: "accept" as const, content: { email: "a@b.c" } };
  try {
    // No owner key, or a remote/proxied daemon: refused before anything is sent.
    ApiClient.ownerKeyProvider = async () => null;
    await assert.rejects(local.answerElicitation("c1", answer), { code: "owner_key_required" });
    await assert.rejects(local.cancelElicitation("c1"), { code: "owner_key_required" });
    ApiClient.ownerKeyProvider = async () => "owner-key-for-test";
    await assert.rejects(proxy.answerElicitation("c1", answer), { code: "owner_key_required" });
    await assert.rejects(proxy.cancelElicitation("c1"), { code: "owner_key_required" });
    assert.equal(requests.length, 0);

    await local.answerElicitation("c1", answer);
    await local.cancelElicitation("c1");
    assert.equal(requests.length, 2);
    assert.match(requests[0].url, /\/v1\/elicitations\/c1$/);
    assert.match(requests[1].url, /\/v1\/elicitations\/c1\/cancel$/);
    for (const r of requests) {
      assert.equal(r.init.redirect, "error");
      assert.equal((r.init.headers as Record<string, string>)["x-praxi-owner-key"], "owner-key-for-test");
    }
    assert.deepEqual(JSON.parse(String(requests[0].init.body)), answer);
  } finally {
    unmark();
    globalThis.fetch = fetch;
    ApiClient.ownerKeyProvider = ownerProvider;
  }
});
