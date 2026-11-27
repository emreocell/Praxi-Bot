import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import type { ComposioApp, ComposioToolkit } from "../api/types.ts";
import {
  appendPage,
  isComposioLink,
  pendingSlugs,
  upsertApp,
  withoutConnected
} from "./composioApps.ts";

const app = (slug: string, state: ComposioApp["state"]): ComposioApp => ({
  slug,
  name: slug,
  state,
  enabled: true,
  native_first: false,
  bots: null,
  tools: 0
});
const kit = (slug: string): ComposioToolkit => ({
  slug,
  name: slug,
  description: "",
  categories: [],
  tools_count: null,
  group: "other",
  native_first: false,
  state: "not_connected",
  enabled: false
});

test("only https links on composio.dev may be opened", () => {
  assert.equal(isComposioLink("https://connect.composio.dev/link/x"), true);
  assert.equal(isComposioLink("https://app.composio.dev/link/lt_example"), true);
  assert.equal(isComposioLink("https://app.composio.dev/link/lt_example", true), false);
  assert.equal(isComposioLink("https://platform.composio.dev/", true), true);
  assert.equal(isComposioLink("https://dashboard.composio.dev/?cliKey=abc", true), true);
  assert.equal(isComposioLink("https://platform.composio.dev/"), false);
  assert.equal(isComposioLink("https://platform.composio.dev:8443/", true), false);
  assert.equal(isComposioLink("https://u:p@platform.composio.dev/", true), false);
});

// The daemon (is_composio_https_url) and the desktop shell (browser.rs) run these same cases.
test("a hosted connect link follows the one rule the daemon and the desktop shell share", () => {
  const vectors = JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../../../../../crates/composio/src/link_vectors.json", import.meta.url)),
      "utf8"
    )
  ) as { accept: string[]; reject: string[]; longest_token: number };
  assert.ok(vectors.accept.length > 0 && vectors.reject.length > 0);
  for (const good of vectors.accept) {
    assert.equal(isComposioLink(good), true, JSON.stringify(good));
  }
  for (const bad of vectors.reject) {
    assert.equal(isComposioLink(bad), false, JSON.stringify(bad));
  }
  const link = (n: number) => `https://connect.composio.dev/link/${"a".repeat(n)}`;
  assert.equal(isComposioLink(link(vectors.longest_token)), true);
  assert.equal(isComposioLink(link(vectors.longest_token + 1)), false);
});

test("pending apps are polled and the catalog never repeats entries shown under Your apps", () => {
  const apps = [app("a", "active"), app("b", "pending")];
  assert.deepEqual(pendingSlugs(apps), ["b"]);
  assert.equal(upsertApp(apps, app("b", "active"))[1]?.state, "active");
  assert.equal(upsertApp(apps, app("c", "pending")).length, 3);
  assert.deepEqual(
    withoutConnected([kit("a"), kit("z")], apps).map((t) => t.slug),
    ["z"]
  );
  assert.deepEqual(
    appendPage([kit("a")], [kit("a"), kit("b"), kit("b")]).map((t) => t.slug),
    ["a", "b"]
  );
});
