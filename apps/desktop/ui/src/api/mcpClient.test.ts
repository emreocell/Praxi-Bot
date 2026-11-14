import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { ApiClient, ApiError, LIVE_VIEW_MEDIA_TYPE } from "./client.ts";
import { MockApiClient } from "./mock.ts";

interface Call {
  method: string;
  url: string;
  auth: string | null;
  accept: string | null;
  cache: RequestCache | undefined;
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
      accept: h.get("accept"),
      cache: init?.cache,
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

test("live-view and control routes send Bearer header, Accept media type, no query tokens", async () => {
  const calls = stubFetch(200, {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: null,
    mode: "idle",
    release: null,
    takeover: null
  });
  const c = client();
  await c.listViewTargets();
  await c.createViewSession({
    client_id: "c1",
    target_id: "t1",
    profile: "watch",
    codecs: ["jpeg"],
    max_width: 960,
    max_height: 600
  });
  await c.openViewFrameStream("s 1");
  await c.heartbeatViewSession("s 1", { takeover_id: "to-1", control_epoch: 2 });
  await c.updateViewSession("s 1", { last_rendered_frame_seq: 5, decode_ms: 8 });
  await c.requestTakeover({ session_id: "s 1", request_id: "r-1" });
  await c.sendViewInput("s 1", {
    v: 1,
    takeover_id: "to-1",
    control_epoch: 2,
    generation: 1,
    geometry_revision: 1,
    first_seq: 1,
    events: [{ type: "reset" }]
  });
  await c.releaseControl({
    takeover_id: "to-1",
    control_epoch: 2,
    request_id: "r-2",
    outcome: "completed"
  });
  await c.overrideControl();
  await c.recoverControl();
  await c.deleteViewSession("s 1");

  assert.deepEqual(
    calls.map((x) => `${x.method} ${new URL(x.url).pathname}`),
    [
      "GET /v1/view/targets",
      "POST /v1/view/sessions",
      "GET /v1/view/sessions/s%201/frames",
      "PUT /v1/view/sessions/s%201/heartbeat",
      "PATCH /v1/view/sessions/s%201",
      "POST /v1/control/takeover",
      "POST /v1/view/sessions/s%201/input",
      "POST /v1/control/release",
      "POST /v1/control/override",
      "POST /v1/control/recover",
      "DELETE /v1/view/sessions/s%201"
    ]
  );
  assert.ok(calls.every((x) => x.auth === "Bearer tok"));
  assert.ok(calls.every((x) => new URL(x.url).search === ""));
  const streamCall = calls[2];
  assert.equal(streamCall.accept, LIVE_VIEW_MEDIA_TYPE);
  assert.equal(streamCall.cache, "no-store");
});

test("MockApiClient executes takeover -> input -> Bitirdim release and override -> recover", async () => {
  const m = new MockApiClient();
  const targets = await m.listViewTargets();
  const avail = targets.find((t) => !t.unavailable_reason);
  assert.ok(avail);
  const sess = await m.createViewSession({
    client_id: "c1",
    target_id: avail.target_id,
    profile: "watch",
    codecs: ["jpeg"],
    max_width: 800,
    max_height: 500
  });
  const acc = await m.requestTakeover({
    session_id: sess.session_id,
    request_id: "req-to-1"
  });
  await new Promise((r) => setTimeout(r, 420));
  const inputRes = await m.sendViewInput(sess.session_id, {
    v: 1,
    takeover_id: acc.takeover_id,
    control_epoch: acc.control_epoch,
    generation: avail.generation,
    geometry_revision: 1,
    first_seq: 1,
    events: [{ type: "text", text: "ok" }]
  });
  assert.equal(inputRes.accepted_through, 1);
  assert.equal(inputRes.state, "active");

  const rel = await m.releaseControl({
    takeover_id: acc.takeover_id,
    control_epoch: acc.control_epoch,
    request_id: "req-rel-1",
    outcome: "completed",
    human_action: "login_completed"
  });
  assert.equal(rel.takeover, null);
  assert.equal(rel.release?.request_id, "req-rel-1");

  const paused = await m.overrideControl();
  assert.equal(paused.latched, "paused");
  const recovered = await m.recoverControl();
  assert.equal(recovered.latched, null);
  await m.deleteViewSession(sess.session_id);
});
