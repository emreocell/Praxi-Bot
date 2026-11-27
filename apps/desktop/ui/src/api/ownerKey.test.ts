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
