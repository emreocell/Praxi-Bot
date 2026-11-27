import assert from "node:assert/strict";
import { test } from "node:test";
import type { ComposioApp } from "../api/types.ts";
import { type AppTestState, appsToTestNow, isTestable, runAppTest } from "./appTest.ts";

const app = (slug: string, state: ComposioApp["state"], enabled = true): ComposioApp => ({
  slug,
  name: slug,
  state,
  enabled,
  native_first: false,
  bots: null,
  tools: 0
});

const describe = (e: unknown) => ({ title: "Could not test", hint: "Try again", details: String((e as Error).message) });

test("a test reports running, then the account and at most three samples", async () => {
  const seen: AppTestState["status"][] = [];
  const end = await runAppTest(
    { test: async () => ({ account: " me@example.com ", items: ["a", "", "b", "c", "d"] }), describe },
    "googledrive",
    (_slug, s) => seen.push(s.status)
  );
  assert.deepEqual(seen, ["running", "ok"]);
  assert.deepEqual(end, { status: "ok", account: " me@example.com ", items: ["a", "b", "c"] });
});

test("an empty account name is no account, and an empty list is still a pass", async () => {
  const end = await runAppTest({ test: async () => ({ account: "  ", items: [] }), describe }, "gmail", () => {});
  assert.deepEqual(end, { status: "ok", account: null, items: [] });
});

test("an authorization-only check is identified separately from an action probe", async () => {
  const end = await runAppTest({
    test: async () => ({ account: "Team inbox", items: [], authorization_only: true }),
    describe
  }, "slack", () => {});
  assert.deepEqual(end, { status: "ok", account: "Team inbox", items: [], authorizationOnly: true });
});

test("a failed test says why in plain words and never throws", async () => {
  const end = await runAppTest(
    {
      test: async () => {
        throw new Error("HTTP 502");
      },
      describe
    },
    "github",
    () => {}
  );
  assert.deepEqual(end, { status: "failed", title: "Could not test", hint: "Try again", details: "HTTP 502" });
});

test("only an app that just finished signing in is tested by itself", () => {
  const before = [app("googledrive", "pending"), app("gmail", "active"), app("slack", "pending")];
  const after = [app("googledrive", "active"), app("gmail", "active"), app("slack", "active")];
  // Drive just became active; Gmail was already active; Slack has no action probe.
  assert.deepEqual(appsToTestNow(before, after, {}), ["googledrive"]);
  // Never twice, never for a switched-off or still-pending app.
  assert.deepEqual(appsToTestNow(before, after, { googledrive: { status: "running" } }), []);
  assert.deepEqual(appsToTestNow(before, [app("googledrive", "active", false)], {}), []);
  assert.deepEqual(appsToTestNow(before, [app("googledrive", "pending")], {}), []);
  assert.deepEqual(appsToTestNow([], after, {}), []);
});

test("the quick test exists for Drive, Gmail, Calendar and GitHub only", () => {
  for (const slug of ["googledrive", "gmail", "googlecalendar", "github"]) {
    assert.equal(isTestable(slug), true, slug);
  }
  assert.equal(isTestable("slack"), false);
});
