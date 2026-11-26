import assert from "node:assert/strict";
import { test } from "node:test";
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
  assert.equal(isComposioLink("https://platform.composio.dev/"), false);
  for (const bad of [
    "https://app.composio.dev/",
    "https://app.composio.dev/link/../settings",
    "https://app.composio.dev/link/lt_example/other",
    "http://connect.composio.dev/x",
    "https://evilcomposio.dev/x",
    "https://composio.dev.evil.com/x",
    "https://u:p@connect.composio.dev/x",
    "https://connect.composio.dev:8443/x",
    "http://169.254.169.254/latest",
    "javascript:alert(1)",
    "https://logos.composio.dev/consent",
    "https://unexpected.composio.dev/consent",
    "https://%63onnect.composio.dev/x",
    "https://connect.composio.dev:443/x",
    "https://xn--composo-9za.dev/x",
    "not a url",
    ""
  ]) {
    assert.equal(isComposioLink(bad), false, bad);
  }
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
