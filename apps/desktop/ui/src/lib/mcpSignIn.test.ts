import assert from "node:assert/strict";
import { test } from "node:test";
import type { McpAuthStart, McpAuthState } from "../api/types.ts";
import {
  expectedSignInHosts,
  isExpectedSignInPage,
  looksLikeRedirect,
  runSignIn,
  sameSite,
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
    expectedHosts: ["https://accounts.example"],
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

test("a sign-in page on another site than the app's own is shown, not opened, until the person confirms", async () => {
  const w = world({
    start: async (port) =>
      started({
        authorization_url: "https://login.evil.example/auth?state=st-1",
        relayed: port !== undefined,
        redirect_uri: `http://127.0.0.1:${port}/callback`
      })
  });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "google-gmail", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  // Nothing was handed to the opener; the panel is told where the page is and that it needs a confirm.
  assert.deepEqual(w.calls.opened, []);
  const browser = seen.find((s) => s.step === "browser");
  assert.ok(browser?.step === "browser");
  assert.equal(browser.origin, "https://login.evil.example");
  assert.equal(browser.needsConfirm, true);
  assert.equal(browser.opened, false);
  ctl.cancelled = true;
  assert.equal((await run).step, "cancelled");
});

test("a page on the app's own site opens by itself and carries its origin", async () => {
  for (const expectedHosts of [["https://accounts.example"], ["accounts.example"], ["https://api.accounts.example/mcp"]]) {
    const w = world({ expectedHosts });
    const { seen, report } = steps();
    const ctl = control();
    const run = runSignIn(w.deps, "linear", report, ctl, { pollMs: 10 });
    await new Promise((r) => setTimeout(r, 0));
    assert.deepEqual(w.calls.opened, [URL_OK], expectedHosts.join());
    const browser = seen.find((s) => s.step === "browser");
    assert.ok(browser?.step === "browser");
    assert.equal(browser.origin, "https://accounts.example");
    assert.equal(browser.needsConfirm, false);
    ctl.cancelled = true;
    await run;
  }
  // No known site to compare with: the page is confirmed, never trusted by default.
  const w = world({ expectedHosts: [] });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "linear", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(w.calls.opened, []);
  const browser = seen.find((s) => s.step === "browser");
  assert.equal(browser?.step === "browser" && browser.needsConfirm, true);
  ctl.cancelled = true;
  await run;
});

test("look-alike hosts are not the same site", () => {
  const google = ["https://accounts.google.com"];
  assert.ok(isExpectedSignInPage("https://accounts.google.com/o/oauth2/v2/auth?state=s", google));
  for (const bad of [
    "https://accounts.google.com.evil.example/auth",
    "https://evil-accounts.google.com.example/",
    "https://accountsxgoogle.com/",
    "https://google.com.evil.example/",
    "https://evilaccounts.google.com/",
    "http://",
    "not a url",
    ""
  ]) {
    // `evilaccounts.google.com` is a different host under google.com: it has to be confirmed too.
    assert.equal(isExpectedSignInPage(bad, google), false, bad);
  }
  // A sub-site of the expected host is the same site; a bare top-level name matches nothing.
  assert.ok(sameSite("login.example.com", "example.com"));
  assert.ok(sameSite("example.com", "login.example.com"));
  assert.equal(sameSite("evil.com", "com"), false);
  assert.equal(sameSite("badexample.com", "example.com"), false);
  assert.equal(isExpectedSignInPage("https://evil.com/", ["https://com"]), false);
});

test("the expected host is the issuer the client belongs to, else the app's own address", () => {
  const entry = {
    install: { url: "https://gmailmcp.googleapis.com/mcp/v1", oauth_client: { issuer: "https://accounts.google.com" } }
  } as unknown as Parameters<typeof expectedSignInHosts>[1];
  const plain = { install: { url: "https://mcp.linear.app/mcp" } } as unknown as Parameters<typeof expectedSignInHosts>[1];
  assert.deepEqual(expectedSignInHosts(undefined, entry), ["https://accounts.google.com"]);
  assert.deepEqual(
    expectedSignInHosts({ url: "https://evil.example/mcp", oauth: { issuer: "https://accounts.google.com" } } as never, entry),
    ["https://accounts.google.com"]
  );
  assert.deepEqual(expectedSignInHosts({ url: "https://mcp.linear.app/mcp" } as never, plain), [
    "https://mcp.linear.app/mcp",
    "https://mcp.linear.app/mcp"
  ]);
  assert.deepEqual(expectedSignInHosts(undefined, undefined), []);
});

test("an address that is not this sign-in's redirect fails at once and is never sent on", async () => {
  const w = world({ relay: null });
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "linear", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  ctl.pasted = "https://evil.example/callback?code=c&state=st-1";
  await new Promise((r) => setTimeout(r, 5));
  assert.deepEqual(w.calls.complete, []);
  const wrong = seen.filter((s) => s.step === "browser" && s.wrongPaste);
  assert.equal(wrong.length, 1);
  assert.equal(ctl.pasted, null);
  // The flow is still waiting: the right address goes through.
  ctl.pasted = "http://127.0.0.1:41000/callback?code=c&state=st-1";
  assert.equal((await run).step, "done");
  assert.deepEqual(w.calls.complete, ["http://127.0.0.1:41000/callback?code=c&state=st-1"]);
});

test("the redirect check compares up to the query and nothing looser", () => {
  const redirect = "http://127.0.0.1:41000/callback";
  assert.ok(looksLikeRedirect("http://127.0.0.1:41000/callback?code=c&state=s", redirect));
  assert.ok(looksLikeRedirect("  http://127.0.0.1:41000/callback?code=c&state=s \n", `${redirect}?x=1`));
  for (const bad of [
    "http://127.0.0.1:41001/callback?code=c",
    "http://127.0.0.1:41000/callback",
    "http://127.0.0.1:41000/callbackx?code=c",
    "http://127.0.0.1.evil.example:41000/callback?code=c",
    "https://127.0.0.1:41000/callback?code=c",
    ""
  ]) {
    assert.equal(looksLikeRedirect(bad, redirect), false, bad);
  }
  assert.equal(looksLikeRedirect("anything?x", ""), false);
});

test("a listener that gave up on stray requests hands over to the paste step, which is then shown", async () => {
  const w = world();
  const { seen, report } = steps();
  const ctl = control();
  const run = runSignIn(w.deps, "x", report, ctl, { pollMs: 10 });
  await new Promise((r) => setTimeout(r, 0));
  w.fail(new Error("too_many_requests"));
  await new Promise((r) => setTimeout(r, 5));
  const last = [...seen].reverse().find((s) => s.step === "browser");
  assert.equal(last?.step === "browser" && last.mode, "paste");
  ctl.pasted = "http://127.0.0.1:53123/callback?code=c&state=st-1";
  assert.equal((await run).step, "done");
});
