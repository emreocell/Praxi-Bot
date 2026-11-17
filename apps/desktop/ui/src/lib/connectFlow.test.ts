import assert from "node:assert/strict";
import { test } from "node:test";
import { settledApps } from "./appTiles.ts";
import { type FlowCancel, type FlowDeps, type FlowState, flowActive, progressDot, runConnectFlow } from "./connectFlow.ts";

function rig(opts: { configured: boolean; polls?: Array<"pending" | "linked" | "failed"> }) {
  const log: string[] = [];
  const polls = [...(opts.polls ?? ["linked"])];
  let clock = 0;
  const deps: FlowDeps = {
    configured: () => opts.configured,
    loginStart: async () => {
      log.push("loginStart");
      return { login_url: "https://dashboard.composio.dev/?cliKey=k" };
    },
    loginStatus: async () => {
      log.push("loginStatus");
      return { state: polls.shift() ?? "pending" };
    },
    appConnect: async (slug) => {
      log.push(`appConnect:${slug}`);
      return { redirect_url: `https://connect.composio.dev/link/${slug}` };
    },
    open: async (url, login) => {
      log.push(`open:${login ? "login" : "app"}`);
      void url;
      return true;
    },
    sleep: async (ms) => {
      clock += ms;
    },
    now: () => clock
  };
  return { deps, log };
}

const states = () => {
  const seen: FlowState[] = [];
  return { seen, report: (s: FlowState) => seen.push(s) };
};

test("with a credential the app's own consent page opens at once, no sign-in", async () => {
  const { deps, log } = rig({ configured: true });
  const { seen, report } = states();
  const end = await runConnectFlow(deps, "gmail", "Gmail", report, { cancelled: false });
  assert.deepEqual(log, ["appConnect:gmail", "open:app"]);
  assert.equal(end.step, "opened");
  assert.equal(end.firstTime, false);
  assert.deepEqual(seen.map((s) => s.step), ["app", "opened"]);
});

test("the first time the Composio sign-in opens and the SAME flow continues to the app without a second click", async () => {
  const { deps, log } = rig({ configured: false, polls: ["pending", "pending", "linked"] });
  const { seen, report } = states();
  const end = await runConnectFlow(deps, "gmail", "Gmail", report, { cancelled: false });
  assert.deepEqual(log, [
    "loginStart",
    "open:login",
    "loginStatus",
    "loginStatus",
    "loginStatus",
    "appConnect:gmail",
    "open:app"
  ]);
  assert.equal(end.step, "opened");
  assert.equal(end.firstTime, true);
  assert.deepEqual(seen.map((s) => [s.step, progressDot(s)]), [
    ["login", 1],
    ["app", 2],
    ["opened", 2]
  ]);
});

test("cancelling during the sign-in stops the flow before any app link is asked for", async () => {
  const { deps, log } = rig({ configured: false, polls: ["pending", "pending", "pending"] });
  const cancel: FlowCancel = { cancelled: false };
  const base = deps.loginStatus;
  deps.loginStatus = async () => {
    cancel.cancelled = true; // the person pressed Cancel while we waited
    return base();
  };
  const end = await runConnectFlow(deps, "gmail", "Gmail", () => {}, cancel);
  assert.equal(end.step, "cancelled");
  assert.equal(log.some((l) => l.startsWith("appConnect")), false);
  assert.equal(flowActive(end), false);
});

test("a failed sign-in, an unopenable page and an unanswered poll are reported, never a half state", async () => {
  const failed = await runConnectFlow(rig({ configured: false, polls: ["failed"] }).deps, "a", "A", () => {}, { cancelled: false });
  assert.deepEqual([failed.step, failed.reason], ["failed", "login_failed"]);

  const noBrowser = rig({ configured: true });
  noBrowser.deps.open = async () => false;
  const e2 = await runConnectFlow(noBrowser.deps, "a", "A", () => {}, { cancelled: false });
  assert.deepEqual([e2.step, e2.reason], ["failed", "app_open"]);

  const never = rig({ configured: false, polls: [] });
  const e3 = await runConnectFlow(never.deps, "a", "A", () => {}, { cancelled: false });
  assert.equal(e3.step, "timed_out");
  assert.equal(never.log.some((l) => l.startsWith("appConnect")), false);
});

test("an app that is still waiting in the browser is never listed as connected", () => {
  const app = (slug: string, state: string) => ({ slug, name: slug, state, enabled: true, native_first: false, bots: null, tools: 0 });
  const kept = settledApps([app("gmail", "active"), app("notion", "pending"), app("slack", "expired"), app("x", "failed")] as never);
  assert.deepEqual(kept.map((a) => a.slug), ["gmail", "slack"]);
});
