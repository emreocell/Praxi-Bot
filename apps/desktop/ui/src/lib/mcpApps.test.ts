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
  saveBotAndApps,
  saveServerScope,
  scopeApplied,
  botAccessLabels,
  changedApps,
  botScope,
  botSelection,
  botsFromScope,
  catalogCategories,
  configFromCatalog,
  connectCatalogEntry,
  customServerConfig,
  eventNeedsRefetch,
  filterCatalog,
  filterServers,
  isConnected,
  MissingFolderError,
  MissingSecretError,
  McpBrowserOpenError,
  secretStoreName,
  signInMcpServer,
  splitArgs,
  statusTone,
  storeHeaderTemplate,
  toggleBotInScope,
  withFolder
} from "./mcpApps.ts";

const entry = (id: string): McpCatalogEntry => {
  const e = MOCK_MCP_CATALOG.find((c) => c.id === id);
  assert.ok(e, `catalog has ${id}`);
  return e;
};

test("catalog fixture carries the ~20 curated servers", () => {
  assert.equal(MOCK_MCP_CATALOG.length, 24);
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

test("configFromCatalog maps the entry's secret_env and secret_headers", () => {
  // The catalog says which variable each secret belongs to. Before this fix the
  // variable was named after the secret, so the server never saw the key.
  const brave = configFromCatalog({
    ...entry("brave-search"),
    install: {
      transport: "stdio",
      command: "npx",
      args: ["-y", "@brave/brave-search-mcp-server@2.2.0"],
      secret_env: { BRAVE_API_KEY: "mcp/brave-token" },
      required_secrets: [
        {
          name: "mcp/brave-token",
          label: "Brave Search API key",
          help_url: "https://api-dashboard.search.brave.com/"
        }
      ]
    }
  });
  assert.deepEqual(brave.secret_env, {
    BRAVE_API_KEY: "mcp_brave_search_mcp_brave_token"
  });

  // An http entry names its header and may use a template.
  const gh = configFromCatalog({
    ...entry("github"),
    install: {
      transport: "http",
      url: "https://api.githubcopilot.com/mcp/",
      secret_headers: { Authorization: "Bearer {secret:mcp/github-token}" },
      required_secrets: [
        {
          name: "mcp/github-token",
          label: "GitHub fine-grained personal access token",
          help_url: "https://github.com/settings/personal-access-tokens/new"
        }
      ]
    }
  });
  assert.deepEqual(gh.secret_headers, {
    Authorization: "Bearer {secret:mcp_github_mcp_github_token}"
  });
  // A template references the secret; it never carries a value.
  assert.equal(JSON.stringify(gh).includes("Bearer mcp_github_"), false);
});

test("storeHeaderTemplate rewrites only what the placeholder names", () => {
  const upper = (name: string) => name.toUpperCase();
  assert.equal(
    storeHeaderTemplate("token {secret:a/b} and {secret:c}", upper),
    "token {secret:A/B} and {secret:C}"
  );
  assert.equal(storeHeaderTemplate("Basic abc", upper), "Basic abc");
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
  // Linear signs in with OAuth now: no secret to reference.
  assert.equal(linear.secret_headers, undefined);

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

test("a server is added without trust unless the owner chose it", async () => {
  // Catalog app: no trust_hints key at all by default (the stored default is "no").
  assert.equal("trust_hints" in configFromCatalog(entry("fetch")), false);
  assert.equal(configFromCatalog(entry("fetch"), undefined, true).trust_hints, true);
  const api = new MockApiClient();
  const safe = await connectCatalogEntry(api, entry("fetch"), {});
  assert.equal(safe.trust_hints, false);
  const trusted = await connectCatalogEntry(api, entry("brave-search"), { BRAVE_API_KEY: "k" }, undefined, true);
  assert.equal(trusted.trust_hints, true);
  // Own server: same rule.
  const form = { name: "mine", transport: "http" as const, command: "", url: "https://x.example/mcp", env: "" };
  const plain = customServerConfig(form);
  assert.ok(plain.ok && !("trust_hints" in plain.config));
  const yes = customServerConfig(form, [], { mode: "all" }, true);
  assert.ok(yes.ok && yes.config.trust_hints === true);
  // Changing it later goes through the normal update.
  const back = await api.updateMcpServer("fetch", { trust_hints: true });
  assert.equal(back.trust_hints, true);
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
  const none = await applyAppsSelection(api, Y, initial, { ...initial }, all);
  assert.deepEqual(none, { failed: [], notApplied: [] });
  const github = api.servers.find((s) => s.name === "github");
  assert.deepEqual(github?.bots, [BOTS[1].id], "Y must not be re-added");

  // A tick the user did change is applied, on the fresh server state.
  const second = await applyAppsSelection(
    api,
    Y,
    initial,
    { ...initial, filesystem: true, linear: false },
    all
  );
  assert.deepEqual(second, { failed: [], notApplied: [] });
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
  assert.deepEqual(failed, { failed: ["github"], notApplied: [] });
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

test("a removal that did not take effect (server now open to all) is reported, not 'saved'", async () => {
  const api = new MockApiClient();
  const Y = BOTS[0].id;
  // The form saw github as a list containing Y; another window set it to all bots.
  await api.updateMcpServer("github", { bots: null });
  const real = api.listMcpServers.bind(api);
  // The fresh list the save reads still shows the stale list state: a remove on
  // an open server is the no-op the server reports back as `bots: null`.
  api.listMcpServers = async () => {
    const list = await real();
    return list.map((s) => (s.name === "github" ? { ...s, bots: [Y] } : s));
  };
  const r = await applyAppsSelection(api, Y, { github: true }, { github: false }, BOTS.map((b) => b.id));
  assert.deepEqual(r, { failed: [], notApplied: ["github"] });
  assert.equal(api.servers.find((s) => s.name === "github")?.bots, null);
});

test("saving a bot: a failing server list after create reports failure and a retry updates the SAME bot", async () => {
  const api = new MockApiClient();
  let creates = 0;
  const updates: string[] = [];
  const made = { id: "new-bot-uuid", name: "N", version: 7 } as unknown as import("../api/types.ts").Bot;
  api.createBot = async () => {
    creates += 1;
    return made;
  };
  api.updateBot = async (id, payload) => {
    assert.equal(payload.expected_version, 7);
    updates.push(id);
    return made;
  };
  const real = api.listMcpServers.bind(api);
  let listFails = true;
  api.listMcpServers = async () => {
    if (listFails) {
      throw new Error("daemon hiccup");
    }
    return real();
  };
  const input = { name: "N", config: {} } as unknown as import("../api/types.ts").BotInput;
  const initial = { github: true, linear: true };
  const allowed = { github: true, linear: false };
  const ids = BOTS.map((b) => b.id);

  const first = await saveBotAndApps(api, undefined, input, initial, allowed, ids);
  assert.equal(first.bot.id, made.id);
  assert.deepEqual(first.failed, ["linear"]);
  assert.equal(creates, 1);

  // Retry: the form holds `first.bot`, so it updates it, never creates again.
  listFails = false;
  const retry = await saveBotAndApps(api, first.bot, input, initial, allowed, ids);
  assert.equal(creates, 1);
  assert.deepEqual(updates, [made.id]);
  assert.deepEqual(retry.failed, []);
  assert.deepEqual(api.servers.find((s) => s.name === "linear")?.bots?.includes(made.id), false);
});

test("the server editor diffs against the scope it opened with, not the live one", async () => {
  const api = new MockApiClient();
  const [a, b, c] = [BOTS[0].id, BOTS[1].id, BOTS[2].id];
  await api.updateMcpServer("github", { bots: [a, b] });
  const opened = { mode: "some" as const, ids: [a, b] };
  // Someone else adds `c` while the editor is open (and a reload changes the live server).
  await api.updateMcpServer("github", { bots_add: [c] });
  // The user unticks `b` only.
  const { applied, server } = await saveServerScope(api, "github", opened, {
    mode: "some",
    ids: [a]
  });
  assert.equal(applied, true);
  assert.deepEqual(server.bots, [a, c], "the other window's change to c must survive");
});

test("the server editor reports a list edit that hit an open server", async () => {
  const api = new MockApiClient();
  const [a, b] = [BOTS[0].id, BOTS[1].id];
  await api.updateMcpServer("github", { bots: [a, b] });
  const opened = { mode: "some" as const, ids: [a, b] };
  await api.updateMcpServer("github", { bots: null }); // set to all meanwhile
  const { applied, server } = await saveServerScope(api, "github", opened, {
    mode: "some",
    ids: [a]
  });
  assert.equal(applied, false);
  assert.equal(server.bots, null, "the server is not silently converted to a list");
  assert.equal(scopeApplied(opened, { mode: "all" }, server), true);
});

test("filesystem catalog entry needs a folder, and it becomes the only root", async () => {
  const api = new MockApiClient();
  const before = api.servers.length;
  await assert.rejects(
    connectCatalogEntry(api, entry("filesystem"), {}),
    (e: unknown) => e instanceof MissingFolderError
  );
  await assert.rejects(
    connectCatalogEntry(api, entry("filesystem"), {}, "   "),
    (e: unknown) => e instanceof MissingFolderError
  );
  assert.equal(api.servers.length, before, "nothing was created");
  const cfg = configFromCatalog(entry("filesystem"), " /home/me/Docs ");
  assert.deepEqual(cfg.args, [
    "-y",
    "@modelcontextprotocol/server-filesystem@2.2.0",
    "/home/me/Docs"
  ]);
  assert.ok(!cfg.args?.includes("."));
});

test("withFolder replaces the old roots and keeps package and flags", () => {
  const pkg = "@modelcontextprotocol/server-filesystem@2.2.0";
  assert.deepEqual(withFolder(["-y", pkg, "."], "/d/Docs"), ["-y", pkg, "/d/Docs"]);
  assert.deepEqual(withFolder(["-y", pkg], "/x"), ["-y", pkg, "/x"]);
  assert.deepEqual(withFolder(["-y", pkg, "--ro", "/a", "/b"], "/c"), ["-y", pkg, "--ro", "/c"]);
  assert.deepEqual(withFolder(["serve"], "/c"), ["serve", "/c"]);
});

test("a background error event refetches the server so the error details appear", () => {
  const base = { name: "sqlite" } as McpServer;
  const connecting = [{ ...base, status: "connecting" as const }] as McpServer[];
  // Known server, still connecting: nothing to fetch yet.
  assert.equal(eventNeedsRefetch(connecting, { name: "sqlite", status: "connecting" }), false);
  // The same server settles into error / quarantined / connected: refetch.
  for (const status of ["error", "quarantined", "connected", "disconnected"] as const) {
    assert.equal(eventNeedsRefetch(connecting, { name: "sqlite", status }), true, status);
  }
  // Unknown server: refetch.
  assert.equal(eventNeedsRefetch(connecting, { name: "new", status: "connecting" }), true);
  // Success clears the old diagnostics so a stale hint cannot linger.
  const failed = [
    {
      ...base,
      status: "error" as const,
      error: "boom",
      error_kind: "runtime_missing" as const,
      stderr_tail: ["x"]
    }
  ] as McpServer[];
  const ok = applyServerEvent(failed, { name: "sqlite", status: "connected" })![0];
  assert.equal(ok.error, null);
  assert.equal(ok.error_kind, undefined);
  assert.equal(ok.stderr_tail, undefined);
  // Quarantine also drops the stale error text until the refetch lands.
  const q = applyServerEvent(failed, { name: "sqlite", status: "quarantined" })![0];
  assert.equal(q.error, null);
});

test("sign-in opens the daemon's URL and polls until the callback finished", async () => {
  const calls: string[] = [];
  const states = ["signing_in", "signing_in", "signed_in"] as const;
  let polled = 0;
  const api = {
    startMcpAuth: async (name: string) => {
      calls.push(`start ${name}`);
      return { authorization_url: "https://auth.example/authorize?state=s", expires_in: 300 };
    },
    mcpAuthStatus: async () => states[polled++],
    cancelMcpAuth: async () => {
      calls.push("cancel");
      return "signed_out" as const;
    }
  };
  const opened: string[] = [];
  const result = await signInMcpServer(api, "linear", (u) => opened.push(u) > 0, {
    sleep: async () => {}
  });
  assert.equal(result, "signed_in");
  assert.deepEqual(opened, ["https://auth.example/authorize?state=s"]);
  assert.deepEqual(calls, ["start linear"]);
  assert.equal(polled, 3);
});

test("sign-in cancels the daemon flow when the browser cannot open or the user stops", async () => {
  const calls: string[] = [];
  const api = {
    startMcpAuth: async () => ({ authorization_url: "https://auth.example/a", expires_in: 300 }),
    mcpAuthStatus: async () => "signing_in" as const,
    cancelMcpAuth: async () => {
      calls.push("cancel");
      return "auth_required" as const;
    }
  };
  await assert.rejects(
    signInMcpServer(api, "x", () => false, { sleep: async () => {} }),
    (e: unknown) => e instanceof McpBrowserOpenError
  );
  assert.deepEqual(calls, ["cancel"]);
  const signal = { aborted: false };
  let polls = 0;
  const result = await signInMcpServer(api, "x", () => true, {
    signal,
    sleep: async () => {
      polls += 1;
      if (polls === 3) {
        signal.aborted = true;
      }
    }
  });
  assert.equal(result, "auth_required");
  assert.deepEqual(calls, ["cancel", "cancel"]);
});
