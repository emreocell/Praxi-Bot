import assert from "node:assert/strict";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import { MOCK_MCP_CATALOG } from "../api/mockCatalog.ts";
import type { McpCatalogEntry, McpServer } from "../api/types.ts";
import {
  ALL_CATEGORIES,
  applyServerEvent,
  botAccess,
  catalogCategories,
  configFromCatalog,
  connectCatalogEntry,
  customServerConfig,
  filterCatalog,
  filterServers,
  isConnected,
  MissingSecretError,
  secretStoreName,
  splitArgs,
  statusTone
} from "./mcpApps.ts";

const entry = (id: string): McpCatalogEntry => {
  const e = MOCK_MCP_CATALOG.find((c) => c.id === id);
  assert.ok(e, `catalog has ${id}`);
  return e;
};

test("catalog fixture carries the ~20 curated servers", () => {
  assert.equal(MOCK_MCP_CATALOG.length, 21);
  for (const id of ["filesystem", "github", "slack", "context7", "home-assistant"]) {
    entry(id);
  }
});

test("filterCatalog by query, category and Turkish-insensitive text", () => {
  assert.equal(
    filterCatalog(MOCK_MCP_CATALOG, "", ALL_CATEGORIES).length,
    MOCK_MCP_CATALOG.length
  );
  const hits = filterCatalog(MOCK_MCP_CATALOG, "git", ALL_CATEGORIES).map(
    (c) => c.id
  );
  assert.deepEqual(hits.sort(), ["github", "gitlab"]);
  const search = filterCatalog(MOCK_MCP_CATALOG, "", "search");
  assert.ok(search.length >= 4);
  assert.ok(search.every((c) => c.category === "search"));
  assert.deepEqual(filterCatalog(MOCK_MCP_CATALOG, "zzz", ALL_CATEGORIES), []);
  assert.ok(catalogCategories(MOCK_MCP_CATALOG).includes("developer"));
});

test("configFromCatalog references secrets by name, never by value", () => {
  const gh = configFromCatalog(entry("github"));
  assert.equal(gh.transport, "stdio");
  assert.equal(gh.command, "npx");
  assert.deepEqual(gh.secret_env, {
    GITHUB_PERSONAL_ACCESS_TOKEN: "mcp_github_github_personal_access_token"
  });
  assert.equal(JSON.stringify(gh).includes("ghp_"), false);

  const linear = configFromCatalog(entry("linear"));
  assert.equal(linear.transport, "http");
  assert.equal(linear.url, "https://mcp.linear.app/mcp");
  assert.deepEqual(linear.secret_headers, {
    Authorization: "mcp_linear_linear_api_key"
  });

  const fs = configFromCatalog(entry("filesystem"));
  assert.equal(fs.secret_env, undefined);
  assert.equal(secretStoreName("a-b", "X Y"), "mcp_a_b_x_y");
});

test("connectCatalogEntry asks only for required secrets", async () => {
  const api = new MockApiClient();
  // no secrets needed: connects straight away
  const fs = await connectCatalogEntry(api, entry("fetch"), {});
  assert.equal(fs.name, "fetch");
  assert.equal(api.secrets.size, 0);

  await assert.rejects(
    connectCatalogEntry(api, entry("brave-search"), { BRAVE_API_KEY: "  " }),
    (e: unknown) =>
      e instanceof MissingSecretError && e.secrets[0] === "BRAVE_API_KEY"
  );
  assert.equal(api.servers.some((s) => s.name === "brave-search"), false);

  const ok = await connectCatalogEntry(api, entry("brave-search"), {
    BRAVE_API_KEY: "k"
  });
  assert.equal(ok.name, "brave-search");
  assert.ok(api.secrets.has("mcp_brave_search_brave_api_key"));
  assert.equal(api.secrets.size, 1);
  assert.ok(isConnected(entry("brave-search"), api.servers));
});

test("splitArgs honours quotes", () => {
  assert.deepEqual(splitArgs(`npx -y "my pkg" 'a b' c`), [
    "npx",
    "-y",
    "my pkg",
    "a b",
    "c"
  ]);
  assert.deepEqual(splitArgs("   "), []);
  assert.deepEqual(splitArgs('x ""'), ["x", ""]);
});

test("customServerConfig validates stdio and http forms", () => {
  const base = { name: "mine", transport: "stdio" as const, command: "", url: "", env: "" };
  assert.deepEqual(customServerConfig(base), { ok: false, error: "command" });
  assert.deepEqual(customServerConfig({ ...base, name: "bad name", command: "x" }), {
    ok: false,
    error: "name"
  });
  assert.deepEqual(customServerConfig({ ...base, command: "x" }, ["mine"]), {
    ok: false,
    error: "name"
  });
  const stdio = customServerConfig({
    ...base,
    command: 'node "a b.js" --flag',
    env: "A=1\nB = two\nnoeq"
  });
  assert.ok(stdio.ok);
  assert.equal(stdio.config.command, "node");
  assert.deepEqual(stdio.config.args, ["a b.js", "--flag"]);
  assert.deepEqual(stdio.config.env, { A: "1", B: "two" });

  assert.deepEqual(
    customServerConfig({ ...base, transport: "http", url: "ftp://x" }),
    { ok: false, error: "url" }
  );
  const http = customServerConfig({ ...base, transport: "http", url: "https://x.io/mcp" });
  assert.ok(http.ok);
  assert.equal(http.config.url, "https://x.io/mcp");
});

test("status tone, bot access and event application", () => {
  assert.equal(statusTone("connected"), "ok");
  assert.equal(statusTone("connecting"), "busy");
  assert.equal(statusTone("quarantined"), "bad");
  assert.equal(statusTone("disconnected"), "muted");

  const bots = [{ id: "b1", name: "Atlas" }];
  assert.deepEqual(botAccess({ bots: null }, bots), { all: true });
  assert.deepEqual(botAccess({ bots: ["b1", "zz"] }, bots), {
    all: false,
    names: ["Atlas", "zz"]
  });

  const servers = [{ name: "a", status: "error", error: "x" }] as McpServer[];
  const next = applyServerEvent(servers, { name: "a", status: "connected" });
  assert.equal(next?.[0].status, "connected");
  assert.equal(next?.[0].error, null);
  assert.equal(applyServerEvent(servers, { name: "zz", status: "connected" }), null);
  assert.equal(filterServers(servers, "A").length, 1);
});
