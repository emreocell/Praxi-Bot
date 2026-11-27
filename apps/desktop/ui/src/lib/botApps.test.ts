import assert from "node:assert/strict";
import { test } from "node:test";
import { appGroupOf, appGroups } from "./botApps.ts";

test("apps are grouped by what they let the bot do, in a fixed order", () => {
  const servers = [
    { name: "github", category: "developer" },
    { name: "gmail", category: "communication" },
    { name: "weird", category: "something-new" },
    { name: "slack", category: "communication" },
    { name: "fs", category: "files" }
  ];
  assert.deepEqual(
    appGroups(servers).map((g) => [g.group, g.servers.map((s) => s.name)]),
    [
      ["communication", ["gmail", "slack"]],
      ["files", ["fs"]],
      ["developer", ["github"]],
      ["other", ["weird"]]
    ]
  );
  assert.deepEqual(appGroups([]), []);
  assert.equal(appGroupOf(undefined), "other");
  assert.equal(appGroupOf("home"), "home");
});
