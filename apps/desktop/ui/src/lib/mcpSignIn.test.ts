import assert from "node:assert/strict";
import { test } from "node:test";
import type { McpAuthStart, McpAuthState } from "../api/types.ts";
import {
  runSignIn,
  signInReason,
  signInRetryable,
  stateOf,
  type RelayApi,
  type SignInControl,
  type SignInDeps,
  type SignInState
} from "./mcpSignIn.ts";

const URL_OK = "https://accounts.example/auth?client_id=c&state=st-1&redirect_uri=x";

function started(over: Partial<McpAuthStart> = {}): McpAuthStart {
  return {
    authorization_url: URL_OK,
    expires_in: 300,
    redirect_uri: "http://127.0.0.1:41000/callback",
    relayed: false,
    ...over
  };
}

function apiError(code: string, message: string, status = 422): Error {
  return Object.assign(new Error(message), { code, status });
}

interface Calls {
  start: (number | undefined)[];
  complete: string[];
  cancel: number;
  opened: string[];
  relayCancelled: number[];
  relayWaitedFor: [number, string][];
}

/** A scripted world: a controllable clock, instant sleeps and a relay the test can answer. */
function world(over: Partial<SignInDeps> & { relay?: RelayApi | null } = {}) {
  const calls: Calls = { start: [], complete: [], cancel: 0, opened: [], relayCancelled: [], relayWaitedFor: [] };
  let now = 0;
  let answer: ((url: string) => void) | null = null;
  let fail: ((e: Error) => void) | null = null;
  const relay: RelayApi = {
    listen: async () => ({ id: 7, port: 53123 }),
    wait: (id, state) => {
      calls.relayWaitedFor.push([id, state]);
      return new Promise<string>((resolve, reject) => {
        answer = resolve;
        fail = reject;
      });
    },
    cancel: async (id) => {
      calls.relayCancelled.push(id);
    }
  };
  const deps: SignInDeps = {
    remote: true,
    start: async (port) => {
      calls.start.push(port);
      return started({ relayed: port !== undefined, redirect_uri: `http://127.0.0.1:${port ?? 41000}/callback` });
    },
    status: async () => "signing_in" as McpAuthState,
    complete: async (url) => {
      calls.complete.push(url);
      return "signed_in";
    },
    cancel: async () => {
      calls.cancel += 1;
      return "signed_out";
    },
    open: async (url) => {
      calls.opened.push(url);
      return true;
    },
    relay: over.relay === undefined ? relay : over.relay,
    // Time is fake, but every wait still yields to the event loop so the test can act meanwhile.
    sleep: async (ms) => {
      now += ms;
      await new Promise((resolve) => setTimeout(resolve, 0));
    },
    now: () => now,
    ...over
  };
  return {
    deps,
    calls,
    answer: (url: string) => answer?.(url),
    fail: (e: Error) => fail?.(e)
  };
}

function steps(): { seen: SignInState[]; report: (s: SignInState) => void } {
  const seen: SignInState[] = [];
  return { seen, report: (s) => seen.push(s) };
}

const control = (): SignInControl => ({ cancelled: false, pasted: null });
const names = (seen: SignInState[]) => seen.map((s) => s.step);

test("a server's sign-in is caught on this computer and forwarded: no copying between machines", async () => {
  const w = world();
  const { seen, report } = steps();
  const run = runSignIn(w.deps, "google-gmail", report, control(), { pollMs: 10 });
  // The listener's port is what the daemon is told; the page opens in the browser here.
  await new Promise((r) => setTimeout(r, 0));
  w.answer("http://127.0.0.1:53123/callback?code=c&state=st-1");
  const last = await run;
  assert.deepEqual(w.calls.start, [53123]);
  assert.deepEqual(w.calls.relayWaitedFor, [[7, "st-1"]]);
  assert.deepEqual(w.calls.opened, [URL_OK]);
  assert.deepEqual(w.calls.complete, ["http://127.0.0.1:53123/callback?code=c&state=st-1"]);
  assert.deepEqual(names(seen), ["starting", "browser", "finishing", "done"]);
  assert.equal(last.step, "done");
  const browser = seen[1];
  assert.equal(browser.step === "browser" && browser.mode, "relay");
  // The listener is closed once the redirect was used.
  assert.deepEqual(w.calls.relayCancelled, [7]);
});

test("a server without a listener here asks for the address, and says so", async () => {
  const w = world({ relay: null });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "linear", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(w.calls.start, [undefined]);
  const browser = seen.find((s) => s.step === "browser");
  assert.equal(browser?.step === "browser" && browser.mode, "paste");
  ctl.pasted = "http://127.0.0.1:41000/callback?code=c&state=st-1";
  const last = await run;
  assert.equal(last.step, "done");
  assert.deepEqual(w.calls.complete, ["http://127.0.0.1:41000/callback?code=c&state=st-1"]);
});

test("a listener that will not open falls back to the paste step instead of failing", async () => {
  const w = world({
    relay: {
      listen: async () => {
        throw new Error("bind_failed");
      },
      wait: async () => "",
      cancel: async () => undefined
    }
  });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "linear", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(w.calls.start, [undefined]);
  const browser = seen.find((s) => s.step === "browser");
  assert.equal(browser?.step === "browser" && browser.mode, "paste");
  ctl.cancelled = true;
  assert.equal((await run).step, "cancelled");
});

test("an old daemon that ignores the port is not treated as relayed", async () => {
  const w = world({
    start: async () => started({ relayed: false })
  });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "x", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  const browser = seen.find((s) => s.step === "browser");
  assert.equal(browser?.step === "browser" && browser.mode, "paste");
  // Its listener here would never be used: it is closed.
  assert.deepEqual(w.calls.relayCancelled, [7]);
  ctl.cancelled = true;
  await run;
});

test("a daemon on this computer listens itself and the flow only polls", async () => {
  let polls = 0;
  const w = world({
    remote: false,
    status: async () => (++polls < 3 ? "signing_in" : "signed_in")
  });
  const { seen, report } = steps();
  const last = await runSignIn(w.deps, "linear", report, control(), { pollMs: 10 });
  assert.equal(last.step, "done");
  assert.deepEqual(w.calls.start, [undefined]);
  assert.equal(polls, 3);
  assert.deepEqual(w.calls.complete, []);
  const browser = seen.find((s) => s.step === "browser");
  assert.equal(browser?.step === "browser" && browser.mode, "local");
});

test("cancelling stops the daemon's flow and closes the listener", async () => {
  const w = world();
  const { report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "x", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  ctl.cancelled = true;
  assert.equal((await run).step, "cancelled");
  assert.equal(w.calls.cancel, 1);
  assert.deepEqual(w.calls.relayCancelled, [7]);
});

test("a sign-in nobody finishes expires and is cancelled", async () => {
  const w = world({ start: async (port) => started({ relayed: port !== undefined, expires_in: 1 }) });
  const { report } = steps();
  const last = await runSignIn(w.deps, "x", report, control(), { pollMs: 400 });
  assert.equal(last.step, "expired");
  assert.equal(w.calls.cancel, 1);
  assert.deepEqual(w.calls.relayCancelled, [7]);
  assert.deepEqual(w.calls.complete, []);
});

test("the page that could not be opened is reported, the flow keeps waiting", async () => {
  const w = world({ open: async () => false });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "x", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  const browser = seen.find((s) => s.step === "browser");
  assert.equal(browser?.step === "browser" && browser.opened, false);
  ctl.cancelled = true;
  assert.equal((await run).step, "cancelled");
});

test("a start that fails names the reason and leaves nothing listening", async () => {
  const w = world({
    start: async () => {
      throw apiError("host_admin_required", "the owner must confirm this desktop on the host");
    }
  });
  const { report } = steps();
  const last = await runSignIn(w.deps, "x", report, control());
  assert.equal(last.step, "failed");
  assert.equal(last.step === "failed" && last.reason, "admin_needed");
  assert.deepEqual(w.calls.relayCancelled, [7]);
});

test("the daemon refusing the forwarded redirect ends the flow with its reason", async () => {
  const w = world({
    complete: async () => {
      throw apiError("bad_mcp_config", "sign-in failed: OAuth sign-in failed: the redirect carried a wrong `state`");
    }
  });
  const { seen, report } = steps();
  const run = runSignIn(w.deps, "x", report, control(), { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  w.answer("http://127.0.0.1:53123/callback?code=c&state=other");
  const last = await run;
  assert.equal(last.step === "failed" && last.reason, "wrong_address");
  assert.deepEqual(names(seen), ["starting", "browser", "finishing", "failed"]);
  assert.ok(last.step === "failed" && last.detail?.includes("state"));
});

test("a listener that is cancelled or runs out of time ends the flow, a dead one falls back to paste", async () => {
  for (const [why, step] of [["cancelled", "cancelled"], ["timeout", "expired"]] as const) {
    const w = world();
    const { report } = steps();
    const run = runSignIn(w.deps, "x", report, control(), { pollMs: 10 });
    await new Promise((r) => setTimeout(r, 0));
    w.fail(new Error(why));
    assert.equal((await run).step, step);
  }
  const w = world();
  const { report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "x", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  w.fail(new Error("not_listening"));
  await new Promise((r) => setTimeout(r, 0));
  ctl.pasted = "http://127.0.0.1:53123/callback?code=c&state=st-1";
  assert.equal((await run).step, "done");
});

test("the daemon's fixed sentences and codes map to stable reasons", () => {
  const cases: [Error, string][] = [
    [apiError("host_admin_required", "x"), "admin_needed"],
    [apiError("owner_key_required", "x"), "owner_needed"],
    [apiError("network", "x", 0), "unreachable"],
    [apiError("conflict", "a sign-in is already in progress", 409), "already_running"],
    [apiError("bad_mcp_config", "no sign-in is waiting for a redirect"), "not_waiting"],
    [apiError("bad_mcp_config", "sign-in failed: OAuth sign-in failed: the pasted address is not this sign-in's redirect"), "wrong_address"],
    [apiError("bad_mcp_config", "sign-in failed: OAuth sign-in failed: the redirect carried a wrong `state`"), "wrong_address"],
    [apiError("bad_mcp_config", "the authorization server refused the request"), "denied"],
    [apiError("bad_mcp_config", "the authorization code was not accepted"), "code_rejected"],
    [apiError("bad_mcp_config", "the requested scope was refused"), "scope_refused"],
    [apiError("bad_mcp_config", "authorization server discovery failed"), "provider_unreachable"],
    [apiError("bad_mcp_config", "the authorization server's issuer does not match"), "issuer_mismatch"],
    [apiError("bad_mcp_config", "sign-in did not finish"), "timeout"],
    [new Error("something new"), "unknown"]
  ];
  for (const [error, reason] of cases) {
    assert.equal(signInReason(error), reason, error.message);
  }
  // Retrying cannot help when someone has to act somewhere else first.
  assert.equal(signInRetryable("admin_needed"), false);
  assert.equal(signInRetryable("owner_needed"), false);
  assert.equal(signInRetryable("issuer_mismatch"), false);
  assert.equal(signInRetryable("denied"), true);
});

test("the state is read from the authorization address", () => {
  assert.equal(stateOf(URL_OK), "st-1");
  assert.equal(stateOf("https://x.example/auth"), null);
  assert.equal(stateOf("not a url"), null);
});
