import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type FlowCancel,
  type FlowDeps,
  type FlowState,
  canRetry,
  classifyLinkFailure,
  runConnectFlow
} from "./connectFlow.ts";

function rig(opts: {
  configured: boolean;
  expired?: boolean;
  /** Errors `appConnect` throws, one per call, before it succeeds. */
  connectErrors?: unknown[];
}) {
  const log: string[] = [];
  const errors = [...(opts.connectErrors ?? [])];
  const deps: FlowDeps = {
    configured: () => opts.configured,
    loginExpired: () => opts.expired === true,
    appConnect: async (slug) => {
      log.push(`appConnect:${slug}`);
      const err = errors.shift();
      if (err !== undefined) {
        throw err;
      }
      return { redirect_url: `https://connect.composio.dev/link/${slug}` };
    },
    open: async (url) => {
      log.push(`open:${url}`);
      return true;
    }
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
  assert.deepEqual(log, ["appConnect:gmail", "open:https://connect.composio.dev/link/gmail"]);
  assert.equal(end.step, "opened");
  assert.equal(end.hasLink, true);
  assert.deepEqual(seen.map((s) => s.step), ["app", "opened"]);
});

test("missing and expired Composio credentials stop before requesting app consent", async () => {
  for (const options of [{ configured: false }, { configured: true, expired: true }]) {
    const { deps, log } = rig(options);
    const end = await runConnectFlow(deps, "gmail", "Gmail", () => {}, { cancelled: false });
    assert.deepEqual([end.step, end.reason, end.hasLink], ["failed", "login_expired", false]);
    assert.deepEqual(log, []);
    assert.equal(canRetry(end.reason), false);
  }
});

test("cancelling an in-flight request reports the created link for cleanup without opening it", async () => {
  const { deps, log } = rig({ configured: true });
  const cancel: FlowCancel = { cancelled: false };
  const request = deps.appConnect;
  deps.appConnect = async (slug) => {
    cancel.cancelled = true;
    return request(slug);
  };
  const result = await runConnectFlow(deps, "gmail", "Gmail", () => {}, cancel);
  assert.deepEqual([result.step, result.hasLink], ["cancelled", true]);
  assert.deepEqual(log, ["appConnect:gmail"]);
});

test("a browser opener failure retains the pending link for cleanup", async () => {
  const { deps } = rig({ configured: true });
  deps.open = async () => false;
  const end = await runConnectFlow(deps, "gmail", "Gmail", () => {}, { cancelled: false });
  assert.deepEqual([end.step, end.reason, end.hasLink], ["failed", "app_open", true]);
});

const loginExpired = Object.assign(new Error("Composio no longer accepts the saved login"), { code: "composio_login_expired" });

test("a rejected credential during connect returns to account setup without starting a second flow", async () => {
  const { deps, log } = rig({ configured: true, connectErrors: [loginExpired] });
  const end = await runConnectFlow(deps, "gmail", "Gmail", () => {}, { cancelled: false });
  assert.deepEqual([end.step, end.reason], ["failed", "login_expired"]);
  assert.deepEqual(log, ["appConnect:gmail"]);
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
  assert.deepEqual(log, ["appConnect:googledrive", "appConnect:googledrive", "open:https://connect.composio.dev/link/googledrive"]);
});
