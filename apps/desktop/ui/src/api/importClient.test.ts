import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { ApiClient, ApiError } from "./client.ts";
import { MockApiClient } from "./mock.ts";

interface Call {
  method: string;
  url: string;
  auth: string | null;
  body: unknown;
}

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(status: number, payload: unknown): Call[] {
  const calls: Call[] = [];
  globalThis.fetch = (async (url: URL | string, init?: RequestInit) => {
    const h = new Headers(init?.headers);
    calls.push({
      method: init?.method ?? "GET",
      url: String(url),
      auth: h.get("authorization"),
      body: init?.body ? JSON.parse(String(init.body)) : undefined
    });
    return new Response(JSON.stringify(payload), { status });
  }) as typeof fetch;
  return calls;
}

const client = () => new ApiClient({ base_url: "http://127.0.0.1:9/", token: "tok" });

test("scanImport posts the scan request to /v1/import/scan with the owner token", async () => {
  const calls = stubFetch(200, { items: [] });
  const c = client();
  assert.deepEqual(await c.scanImport(), { items: [] });
  await c.scanImport({ folder: "/srv/prompts" });
  await c.scanImport({ only: "cursor" });
  assert.deepEqual(
    calls.map((x) => `${x.method} ${new URL(x.url).pathname}`),
    ["POST /v1/import/scan", "POST /v1/import/scan", "POST /v1/import/scan"]
  );
  assert.ok(calls.every((x) => x.auth === "Bearer tok"));
  assert.deepEqual(calls.map((x) => x.body), [{}, { folder: "/srv/prompts" }, { only: "cursor" }]);
});

test("runImport posts ids and confirmed secret names, and returns the results", async () => {
  const results = [{ id: "a", status: "imported", message: "ok", secrets_stored: [], secrets_left_out: [] }];
  const calls = stubFetch(200, { results });
  const out = await client().runImport({ folder: "/srv/prompts", ids: ["a", "b"], confirmed_secrets: ["mcp_x_TOKEN"] });
  assert.deepEqual(out.results, results);
  assert.equal(calls[0].method, "POST");
  assert.equal(new URL(calls[0].url).pathname, "/v1/import/run");
  assert.deepEqual(calls[0].body, { folder: "/srv/prompts", ids: ["a", "b"], confirmed_secrets: ["mcp_x_TOKEN"] });
  assert.equal(calls[0].auth, "Bearer tok");
});

test("import errors surface as ApiError", async () => {
  stubFetch(403, { error: { code: "forbidden", message: "owner only" } });
  await assert.rejects(client().scanImport(), (e: unknown) => e instanceof ApiError && e.status === 403 && e.code === "forbidden");
});

test("mock mode scans and runs without a daemon", async () => {
  const mock = new MockApiClient();
  const { items } = await mock.scanImport();
  assert.ok(items.some((i) => i.kind === "mcp_server" && i.import));
  assert.ok(items.some((i) => i.kind === "skill"));
  assert.ok(items.some((i) => i.kind === "instructions" && !i.default_on));
  assert.ok(items.some((i) => !i.import && i.reason));
  assert.ok(!items.some((i) => i.app === "folder"));
  const withFolder = await mock.scanImport({ folder: "/x" });
  assert.ok(withFolder.items.some((i) => i.app === "folder"));
  const { results } = await mock.runImport({ ids: [items[0].id], confirmed_secrets: [] });
  assert.equal(results.length, 1);
  assert.equal(results[0].status, "imported");
  assert.deepEqual(results[0].secrets_stored, []);
  assert.deepEqual(results[0].secrets_left_out, ["GITHUB_TOKEN"]);
});
