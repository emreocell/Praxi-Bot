import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { ApiClient } from "./client.ts";
import { MockApiClient } from "./mock.ts";

interface Call {
  method: string;
  url: string;
  body: unknown;
}

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(status: number, payload: unknown): Call[] {
  const calls: Call[] = [];
  globalThis.fetch = (async (url: URL | string, init?: RequestInit) => {
    calls.push({
      method: init?.method ?? "GET",
      url: String(url),
      body: init?.body ? JSON.parse(String(init.body)) : undefined
    });
    return new Response(JSON.stringify(payload), { status });
  }) as typeof fetch;
  return calls;
}

test("web search client methods hit the /v1/web/search* contract routes", async () => {
  const calls = stubFetch(200, { ok: true });
  const c = new ApiClient({ base_url: "http://127.0.0.1:9/", token: "tok" });
  await c.getWebSearch();
  await c.updateWebSearch({ bing: false, brave_key: "BSA-123" });
  await c.testWebSearch({ query: "rust" });

  assert.deepEqual(
    calls.map((x) => `${x.method} ${new URL(x.url).pathname}`),
    [
      "GET /v1/web/search",
      "PUT /v1/web/search",
      "POST /v1/web/search/test"
    ]
  );
  assert.deepEqual(calls[1].body, { bing: false, brave_key: "BSA-123" });
  assert.deepEqual(calls[2].body, { query: "rust" });
});

test("mock client updates web search order, keys write-only, and runs test probe", async () => {
  const mock = new MockApiClient();
  const initial = await mock.getWebSearch();
  assert.equal(initial.brave_key_set, false);
  assert.deepEqual(initial.active_backends, ["duckduckgo", "duckduckgo_lite", "bing"]);

  const updated = await mock.updateWebSearch({
    order: ["brave", "bing", "duckduckgo", "duckduckgo_lite", "searxng", "tavily"],
    brave_key: "BSA-secret-value",
    duckduckgo_lite: false
  });
  assert.equal(updated.brave_key_set, true);
  assert.equal("brave_key" in updated, false);
  assert.deepEqual(updated.active_backends, ["brave", "bing", "duckduckgo"]);

  const probe = await mock.testWebSearch({ query: "rust async" });
  assert.equal(probe.ok, true);
  assert.equal(probe.backend, "brave");
  assert.equal(probe.query, "rust async");
});
