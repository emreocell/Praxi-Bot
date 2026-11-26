import assert from "node:assert/strict";
import { test } from "node:test";
import { apiKeyProvider, engineStatus, needsManualFallback, signInPhase, waitingFor } from "./serverSignIn.ts";

test("the sign-in phase follows the server's state and unknown states are idle", () => {
  for (const s of ["starting", "needs_browser", "needs_code", "verifying", "done", "failed", "timed_out", "cancelled"]) {
    assert.equal(signInPhase({ state: s }), s);
  }
  assert.equal(signInPhase(null), "idle");
  assert.equal(signInPhase({ state: "something-new" }), "idle");
});

test("only engines with a matching model provider offer the API-key shortcut", () => {
  assert.equal(apiKeyProvider("claude"), "anthropic");
  assert.equal(apiKeyProvider("codex"), "openai");
  for (const id of ["opencode", "cursor", "agy", "gemini", ""]) assert.equal(apiKeyProvider(id), null);
});

test("a browser flow waits for the page, a device flow waits for the typed code", () => {
  assert.equal(waitingFor({ code: "ABCD-1234" }), "device");
  assert.equal(waitingFor({ code: null }), "browser");
  assert.equal(waitingFor(null), "browser");
});

test("the plain command is offered when the CLI never showed its sign-in page", () => {
  assert.equal(needsManualFallback({ state: "failed", error: "login_start_timeout" }), true);
  assert.equal(needsManualFallback({ state: "failed", error: "login_cli_outdated" }), true);
  assert.equal(needsManualFallback({ state: "failed", error: "login_code_rejected" }), false);
  assert.equal(needsManualFallback({ state: "needs_code", error: null }), false);
  assert.equal(needsManualFallback(null), false);
});

test("the engine list says installed-and-signed-in, needs sign-in, or unknown, with the account", () => {
  assert.deepEqual(engineStatus({ installed: false, signed_in: null, account: null }), { key: "home.engines.notInstalled" });
  assert.deepEqual(engineStatus({ installed: true, signed_in: true, account: "ada@example.com" }), {
    key: "home.engines.readyAs",
    values: { account: "ada@example.com" }
  });
  assert.deepEqual(engineStatus({ installed: true, signed_in: true, account: null }), { key: "home.engines.ready" });
  // Installed but not signed in is "needs sign-in", never "install".
  assert.deepEqual(engineStatus({ installed: true, signed_in: false, account: null }), { key: "home.engines.needsSignIn" });
  assert.deepEqual(engineStatus({ installed: true, signed_in: null, account: null }), { key: "home.engines.installedUnknown" });
});
