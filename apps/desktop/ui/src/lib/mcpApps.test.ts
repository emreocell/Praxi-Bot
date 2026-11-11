import assert from "node:assert/strict";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import { MOCK_MCP_CATALOG } from "../api/mockCatalog.ts";
import type { McpCatalogEntry, McpServer } from "../api/types.ts";
import {
  ALL_CATEGORIES,
  applyServerEvent,
  botAccess,
  botHasAccess,
  appsPatch,
  scopePatch,
  applyAppsSelection,
  botAccessLabels,
  changedApps,
  botScope,
  botSelection,
  botsFromScope,
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
  statusTone,
  toggleBotInScope
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

const BOTS = [
  { id: "11111111-aaaa", name: "Atlas" },
  { id: "22222222-bbbb", name: "Borealis" },
  { id: "33333333-cccc", name: "Cirrus" }
];

test("bot scope: null is all bots, an empty list is NO bot, never all", () => {
  assert.deepEqual(botScope({ bots: null }), { mode: "all" });
  assert.deepEqual(botScope({ bots: undefined }), { mode: "all" });
  assert.deepEqual(botScope({ bots: [] }), { mode: "some", ids: [] });
  assert.deepEqual(botsFromScope({ mode: "all" }), null);
  // An emptied selection is written as [], not null.
  assert.deepEqual(botsFromScope({ mode: "some", ids: [] }), []);
  assert.deepEqual(botsFromScope({ mode: "some", ids: ["a"] }), ["a"]);
  assert.deepEqual(botAccess({ bots: [] }, BOTS), { all: false, names: [] });
});

test("bot scope: switching to 'only these bots' starts from nobody, ids are UUIDs", () => {
  let scope = botScope({ bots: null });
  scope = toggleBotInScope(scope, BOTS[0].id, true);
  // Ticking a bot while on "all" narrows to exactly that bot.
  assert.deepEqual(scope, { mode: "some", ids: [BOTS[0].id] });
  scope = toggleBotInScope(scope, BOTS[1].id, true);
  scope = toggleBotInScope(scope, BOTS[1].id, true);
  assert.deepEqual(botsFromScope(scope), [BOTS[0].id, BOTS[1].id]);
  scope = toggleBotInScope(scope, BOTS[0].id, false);
  scope = toggleBotInScope(scope, BOTS[1].id, false);
  assert.deepEqual(botsFromScope(scope), []);
});

test("botSelection drops ids of bots that no longer exist without widening", () => {
  assert.deepEqual(
    botSelection({ mode: "some", ids: ["gone", BOTS[2].id] }, BOTS),
    { mode: "some", ids: [BOTS[2].id] }
  );
  assert.deepEqual(botSelection({ mode: "some", ids: ["gone"] }, BOTS), {
    mode: "some",
    ids: []
  });
  assert.deepEqual(botSelection({ mode: "all" }, BOTS), { mode: "all" });
});

const srv = (name: string, bots: string[] | null) =>
  ({ name, bots }) as unknown as McpServer;

test("bot side: access of a bot to each server, and the list to write back", () => {
  const open = srv("open", null);
  const mine = srv("mine", [BOTS[0].id]);
  const theirs = srv("theirs", [BOTS[1].id]);
  const nobody = srv("nobody", []);
  assert.equal(botHasAccess(open, BOTS[0].id), true);
  assert.equal(botHasAccess(mine, BOTS[0].id), true);
  assert.equal(botHasAccess(theirs, BOTS[0].id), false);
  assert.equal(botHasAccess(nobody, BOTS[0].id), false);
  // A bot that does not exist yet only gets the open servers.
  assert.equal(botHasAccess(open, undefined), true);
  assert.equal(botHasAccess(mine, undefined), false);

  const all = BOTS.map((b) => b.id);
  // Unticking on an open server narrows it to every OTHER bot (explicit list).
  assert.deepEqual(appsPatch(open, BOTS[0].id, false, all), {
    bots: [BOTS[1].id, BOTS[2].id]
  });
  // Ticking on an open server changes nothing (it stays open).
  assert.equal(appsPatch(open, BOTS[0].id, true, all), null);
  // On a list, ticking / unticking are atomic add / remove of ONE id.
  assert.deepEqual(appsPatch(theirs, BOTS[0].id, true, all), {
    bots_add: [BOTS[0].id]
  });
  assert.deepEqual(appsPatch(mine, BOTS[0].id, false, all), {
    bots_remove: [BOTS[0].id]
  });
});

test("saving a server's bots goes through PUT with UUIDs and keeps other fields", async () => {
  const api = new MockApiClient();
  const before = api.servers.find((s) => s.name === "github");
  assert.ok(before);
  const updated = await api.updateMcpServer("github", { bots: [BOTS[0].id] });
  assert.deepEqual(updated.bots, [BOTS[0].id]);
  assert.equal(updated.command, before.command);
  assert.equal(updated.tool_count, before.tool_count);
  const none = await api.updateMcpServer("github", { bots: [] });
  assert.deepEqual(none.bots, []);
  const all = await api.updateMcpServer("github", { bots: null });
  assert.equal(all.bots, null);
  // Omitting bots keeps the stored list.
  await api.updateMcpServer("github", { bots: [BOTS[1].id] });
  const kept = await api.updateMcpServer("github", { enabled: true });
  assert.deepEqual(kept.bots, [BOTS[1].id]);
});

test("a custom server can be created for chosen bots", () => {
  const r = customServerConfig(
    {
      name: "mine",
      transport: "stdio",
      command: "npx -y x",
      url: "",
      env: ""
    },
    [],
    { mode: "some", ids: [BOTS[0].id] }
  );
  assert.ok(r.ok);
  assert.deepEqual(r.config.bots, [BOTS[0].id]);
  const none = customServerConfig(
    { name: "n", transport: "stdio", command: "x", url: "", env: "" },
    [],
    { mode: "some", ids: [] }
  );
  assert.ok(none.ok);
  assert.deepEqual(none.config.bots, []);
  const dflt = customServerConfig(
    { name: "d", transport: "stdio", command: "x", url: "", env: "" },
    []
  );
  assert.ok(dflt.ok);
  assert.equal("bots" in dflt.config, false);
});

test("bot form only applies the ticks the user changed in this session", async () => {
  const api = new MockApiClient();
  const Y = BOTS[0].id;
  const all = BOTS.map((b) => b.id);
  await api.updateMcpServer("github", { bots: [Y, BOTS[1].id] });
  // The form opened with Y ticked for github and (open) linear.
  const initial = { github: true, linear: true, filesystem: false };
  // Meanwhile the user removes Y from github in the Apps modal.
  await api.updateMcpServer("github", { bots_remove: [Y] });
  // Saving an unrelated edit of the bot form: no tick was changed.
  assert.deepEqual(changedApps(initial, { ...initial }), []);
  const failed = await applyAppsSelection(api, Y, initial, { ...initial }, all);
  assert.deepEqual(failed, []);
  const github = api.servers.find((s) => s.name === "github");
  assert.deepEqual(github?.bots, [BOTS[1].id], "Y must not be re-added");

  // A tick the user did change is applied, on the fresh server state.
  const failed2 = await applyAppsSelection(
    api,
    Y,
    initial,
    { ...initial, filesystem: true, linear: false },
    all
  );
  assert.deepEqual(failed2, []);
  assert.ok(api.servers.find((s) => s.name === "filesystem")?.bots?.includes(Y));
  assert.deepEqual(api.servers.find((s) => s.name === "linear")?.bots, [
    BOTS[1].id,
    BOTS[2].id
  ]);
});

test("a failing server is reported, the others are still applied", async () => {
  const api = new MockApiClient();
  const Y = BOTS[0].id;
  const real = api.updateMcpServer.bind(api);
  api.updateMcpServer = (name, patch) =>
    name === "github" ? Promise.reject(new Error("boom")) : real(name, patch);
  await real("github", { bots: [BOTS[1].id] });
  const failed = await applyAppsSelection(
    api,
    Y,
    { github: false, filesystem: false },
    { github: true, filesystem: true },
    BOTS.map((b) => b.id)
  );
  assert.deepEqual(failed, ["github"]);
  assert.ok(api.servers.find((s) => s.name === "filesystem")?.bots?.includes(Y));
});

test("stale bot ids are labelled as deleted, never shown as raw ids", () => {
  const a = botAccessLabels({ bots: [BOTS[0].id, "gone-uuid"] }, BOTS, "(deleted bot)");
  assert.deepEqual(a, { all: false, names: ["Atlas", "(deleted bot)"] });
  assert.deepEqual(botAccessLabels({ bots: null }, BOTS, "x"), { all: true });
  assert.deepEqual(botAccessLabels({ bots: [] }, BOTS, "x"), { all: false, names: [] });
});

test("the server editor sends a diff for list edits, absolute only across all-bots", () => {
  const some = (...ids: string[]) => ({ mode: "some" as const, ids });
  assert.deepEqual(scopePatch(some("a", "b"), some("b", "c")), {
    bots_add: ["c"],
    bots_remove: ["a"]
  });
  assert.deepEqual(scopePatch({ mode: "all" }, some("a")), { bots: ["a"] });
  assert.deepEqual(scopePatch({ mode: "all" }, some()), { bots: [] });
  assert.deepEqual(scopePatch(some("a"), { mode: "all" }), { bots: null });
  assert.deepEqual(scopePatch(some("a"), some()), {
    bots_add: [],
    bots_remove: ["a"]
  });
});
