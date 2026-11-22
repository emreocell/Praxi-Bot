import assert from "node:assert/strict";
import { test } from "node:test";
import type { ComposioApp, ComposioToolkit, McpCatalogEntry } from "../api/types.ts";
import { composioTileState, filterDirect, tileAction, visibleToolkits } from "./appTiles.ts";

const app = (slug: string, state: ComposioApp["state"]): ComposioApp => ({
  slug,
  name: slug,
  state,
  enabled: true,
  native_first: false,
  bots: null,
  tools: 0
});

const kit = (slug: string, name: string): ComposioToolkit => ({
  slug,
  name,
  description: "",
  categories: [],
  tools_count: null,
  group: "other",
  logo: null,
  native_first: false,
  state: "not_connected",
  enabled: false
});

test("a tile walks idle -> linking -> waiting -> connected, and offers one button per state", () => {
  assert.equal(composioTileState(undefined, false), "idle");
  assert.equal(composioTileState(undefined, true), "linking");
  assert.equal(composioTileState(app("gmail", "pending"), false), "waiting");
  assert.equal(composioTileState(app("gmail", "active"), false), "connected");
  assert.equal(composioTileState(app("gmail", "failed"), false), "failed");
  assert.equal(composioTileState(app("gmail", "expired"), false), "expired");
  assert.equal(tileAction("idle"), "connect");
  assert.equal(tileAction("failed"), "retry");
  assert.equal(tileAction("expired"), "reconnect");
  for (const busy of ["linking", "waiting", "connected"] as const) {
    assert.equal(tileAction(busy), null);
  }
});

test("a product connected directly or already in Connected has one tile only", () => {
  const direct = [{ id: "notion-remote", name: "Notion" }] as McpCatalogEntry[];
  const items = [kit("notion", "Notion"), kit("slack", "Slack"), kit("gmail", "Gmail")];
  const shown = visibleToolkits(items, [app("gmail", "active")], direct, []);
  assert.deepEqual(
    shown.map((t) => t.slug),
    ["slack"]
  );
});

test("direct tiles are found without regard to case, accents or the Turkish i (#638)", () => {
  const entries = [
    { id: "notion-remote", name: "Notion", description: "Notlarını ve görevlerini yönet" },
    { id: "linear-remote", name: "Linear", description: "Issue tracking" },
    { id: "ikas-remote", name: "İkas", description: "Online store" }
  ] as McpCatalogEntry[];
  const ids = (query: string): string[] => filterDirect(entries, query, "all").map((e) => e.id);
  assert.deepEqual(ids(""), ["notion-remote", "linear-remote", "ikas-remote"]);
  assert.deepEqual(ids("  "), ["notion-remote", "linear-remote", "ikas-remote"]);
  assert.deepEqual(ids("ikas"), ["ikas-remote"]);
  assert.deepEqual(ids("İKAS"), ["ikas-remote"]);
  assert.deepEqual(ids("notlarini gorevlerini"), ["notion-remote"]);
  assert.deepEqual(ids(" LINEAR "), ["linear-remote"]);
  assert.deepEqual(ids("notion tracking"), []);
});
