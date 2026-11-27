import assert from "node:assert/strict";
import { test } from "node:test";
import type { McpCatalogEntry } from "../api/types.ts";
import { directGroup, filterDirect, tileAction } from "./appTiles.ts";

test("a direct MCP tile offers the action for its current state", () => {
  assert.equal(tileAction("idle"), "connect");
  assert.equal(tileAction("failed"), "retry");
  assert.equal(tileAction("expired"), "reconnect");
  for (const busy of ["linking", "waiting", "connected"] as const) {
    assert.equal(tileAction(busy), null);
  }
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

test("the second wave of direct apps is grouped, and an unknown app falls into other", () => {
  const want: Record<string, string> = {
    supabase: "dev",
    neon: "dev",
    cloudflare: "dev",
    netlify: "dev",
    webflow: "dev",
    square: "finance",
    monday: "productivity",
    dropbox: "productivity",
    airtable: "productivity",
    calendly: "productivity",
    clickup: "productivity",
    miro: "productivity"
  };
  for (const [id, group] of Object.entries(want)) assert.equal(directGroup(id), group, id);
  assert.equal(directGroup("not-in-the-catalog"), "other");
});
