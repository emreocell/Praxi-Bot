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
    calls.push({
      method: init?.method ?? "GET",
      url: String(url),
      auth: new Headers(init?.headers).get("authorization"),
      body: init?.body ? JSON.parse(String(init.body)) : undefined
    });
    return new Response(status === 204 ? null : JSON.stringify(payload), {
      status
    });
  }) as typeof fetch;
  return calls;
}

const client = () =>
  new ApiClient({ base_url: "http://127.0.0.1:9/", token: "tok" });

test("MCP client functions hit the contract routes", async () => {
  const calls = stubFetch(200, []);
  const c = client();
  await c.listMcpServers();
  await c.mcpCatalog();
  await c.listMcpTools("my server");
  await c.reconnectMcpServer("gh");
  const cfg = { name: "gh", transport: "stdio" as const, command: "npx" };
  await c.createMcpServer(cfg);
  await c.updateMcpServer("gh", cfg);
  assert.deepEqual(
    calls.map((x) => `${x.method} ${new URL(x.url).pathname}`),
    [
      "GET /v1/mcp/servers",
      "GET /v1/mcp/catalog",
      "GET /v1/mcp/servers/my%20server/tools",
      "POST /v1/mcp/servers/gh/reconnect",
      "POST /v1/mcp/servers",
      "PUT /v1/mcp/servers/gh"
    ]
  );
  assert.ok(calls.every((x) => x.auth === "Bearer tok"));
  assert.deepEqual(calls[4].body, cfg);
});

test("deleteMcpServer resolves on 204 and setSecret is write-only", async () => {
  let calls = stubFetch(204, null);
  const c = client();
  assert.equal(await c.deleteMcpServer("gh"), undefined);
  assert.equal(calls[0].method, "DELETE");

  calls = stubFetch(204, null);
  assert.equal(await c.setSecret("mcp_gh_token", "s3cret"), undefined);
  assert.equal(new URL(calls[0].url).pathname, "/v1/secrets/mcp_gh_token");
  assert.deepEqual(calls[0].body, { value: "s3cret" });
});

test("daemon errors surface as ApiError", async () => {
  stubFetch(409, { error: { code: "conflict", message: "exists" } });
  await assert.rejects(
    client().createMcpServer({ name: "x", transport: "http", url: "http://x" }),
    (e: unknown) =>
      e instanceof ApiError && e.status === 409 && e.code === "conflict"
  );
});

test("the mock client serves the same typed calls in memory", async () => {
  const m = new MockApiClient();
  const before = (await m.listMcpServers()).length;
  const s = await m.createMcpServer({
    name: "demo",
    transport: "http",
    url: "https://x.io"
  });
  assert.equal(s.status, "connected");
  assert.equal((await m.listMcpServers()).length, before + 1);
  assert.equal((await m.mcpCatalog()).length, 21);
  assert.ok((await m.listMcpTools("demo")).length > 0);
  await m.deleteMcpServer("demo");
  assert.equal((await m.listMcpServers()).length, before);
  await assert.rejects(m.reconnectMcpServer("demo"), ApiError);
  await m.setSecret("a", "b");
  assert.equal(m.secrets.size, 1);
});
