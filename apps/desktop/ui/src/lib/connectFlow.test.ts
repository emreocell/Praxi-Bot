import assert from "node:assert/strict";
import { test } from "node:test";
import { settledApps } from "./appTiles.ts";
import {
  type FlowCancel,
  type FlowDeps,
  type FlowState,
  canRetry,
  classifyLinkFailure,
  flowActive,
  progressDot,
  runConnectFlow
} from "./connectFlow.ts";

function rig(opts: {
  configured: boolean;
  expired?: boolean;
  /** Errors `appConnect` throws, one per call, before it succeeds. */
  connectErrors?: unknown[];
  polls?: Array<"pending" | "linked" | "failed">;
}) {
  const log: string[] = [];
  const polls = [...(opts.polls ?? ["linked"])];
  let clock = 0;
  const errors = [...(opts.connectErrors ?? [])];
  const deps: FlowDeps = {
    configured: () => opts.configured,
    loginExpired: () => opts.expired === true,
    classify: classifyLinkFailure,
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
      const err = errors.shift();
      if (err !== undefined) {
        throw err;
      }
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

const loginExpired = Object.assign(new Error("Composio no longer accepts the saved login"), { code: "composio_login_expired" });

test("a saved login Composio refuses sends the flow through the sign-in first, never to a dead retry", async () => {
  const { deps, log } = rig({ configured: true, expired: true, polls: ["linked"] });
  const { seen, report } = states();
  const end = await runConnectFlow(deps, "gmail", "Gmail", report, { cancelled: false });
  assert.deepEqual(log, ["loginStart", "open:login", "loginStatus", "appConnect:gmail", "open:app"]);
  assert.equal(end.step, "opened");
  assert.equal(end.firstTime, true);
  assert.deepEqual(seen.map((s) => s.step), ["login", "app", "opened"]);
});

test("when the link request itself says the login expired, the same flow signs in again and retries once", async () => {
  const { deps, log } = rig({ configured: true, connectErrors: [loginExpired], polls: ["linked"] });
  const { seen, report } = states();
  const end = await runConnectFlow(deps, "gmail", "Gmail", report, { cancelled: false });
  assert.deepEqual(log, [
    "appConnect:gmail",
    "loginStart",
    "open:login",
    "loginStatus",
    "appConnect:gmail",
    "open:app"
  ]);
  assert.equal(end.step, "opened");
  assert.deepEqual(seen.map((s) => s.step), ["app", "login", "app", "opened"]);
});

test("a login that is refused again right after signing in ends with a plain login_expired message, not a loop", async () => {
  const { deps, log } = rig({ configured: true, expired: true, connectErrors: [loginExpired], polls: ["linked"] });
  const { report } = states();
  const end = await runConnectFlow(deps, "gmail", "Gmail", report, { cancelled: false });
  assert.equal(end.step, "failed");
  assert.equal(end.reason, "login_expired");
  assert.equal(log.filter((l) => l === "loginStart").length, 1);
});

test("link failures are told apart by the daemon code, in plain reasons", async () => {
  assert.equal(classifyLinkFailure({ code: "composio_login_expired" }), "login");
  assert.equal(classifyLinkFailure({ code: "composio_not_configured" }), "login");
  assert.equal(classifyLinkFailure({ code: "composio_rate_limited" }), "rate_limited");
  assert.equal(classifyLinkFailure({ code: "composio_error" }), "upstream");
  assert.equal(classifyLinkFailure(new Error("boom")), "other");
  const { deps } = rig({ configured: true, connectErrors: [{ code: "composio_error" }] });
  const { report } = states();
  const end = await runConnectFlow(deps, "gmail", "Gmail", report, { cancelled: false });
  assert.equal(end.reason, "upstream");
});

test("every code the connect route can answer with maps to one reason, and a retry is offered only where it can help", async () => {
  const table: Array<[string, string, boolean]> = [
    ["owner_key_required", "owner_key", false],
    ["not_found", "unknown_app", false],
    ["bad_composio_request", "app_refused", false],
    ["unavailable", "unavailable", true],
    ["composio_rate_limited", "rate_limited", true],
    ["network", "upstream", true],
    ["timeout", "upstream", true],
    ["internal", "app_link", true],
    ["something_new", "app_link", true]
  ];
  for (const [code, reason, retry] of table) {
    const { deps } = rig({ configured: true, connectErrors: [Object.assign(new Error("x"), { code })] });
    const end = await runConnectFlow(deps, "googledrive", "Google Drive", () => {}, { cancelled: false });
    assert.equal(end.step, "failed", code);
    assert.equal(end.reason, reason, code);
    assert.equal(canRetry(end.reason), retry, code);
  }
});

test("a failure carries the daemon's own words (translated, redacted) for the Details, not as the main message", async () => {
  const boom = Object.assign(new Error("unknown or unsupported app toolkit; connection refused"), { code: "bad_composio_request" });
  const { deps } = rig({ configured: true, connectErrors: [boom] });
  deps.describe = (e) => ({ title: "T", hint: "H", details: `HTTP 422 · ${(e as Error).message}` });
  const end = await runConnectFlow(deps, "googledrive", "Google Drive", () => {}, { cancelled: false });
  assert.equal(end.reason, "app_refused");
  assert.deepEqual(end.failure, { title: "T", hint: "H", details: "HTTP 422 · unknown or unsupported app toolkit; connection refused" });
  // A flow without a describer (tests, other callers) still ends with the reason.
  const bare = rig({ configured: true, connectErrors: [boom] });
  const e2 = await runConnectFlow(bare.deps, "googledrive", "Google Drive", () => {}, { cancelled: false });
  assert.equal(e2.failure, undefined);
});

test("pressing retry after a fixable failure runs the whole flow again from the app step", async () => {
  const { deps, log } = rig({ configured: true, connectErrors: [{ code: "composio_error" }] });
  const first = await runConnectFlow(deps, "googledrive", "Google Drive", () => {}, { cancelled: false });
  assert.equal(first.reason, "upstream");
  const second = await runConnectFlow(deps, "googledrive", "Google Drive", () => {}, { cancelled: false });
  assert.equal(second.step, "opened");
  assert.deepEqual(log, ["appConnect:googledrive", "appConnect:googledrive", "open:app"]);
});
