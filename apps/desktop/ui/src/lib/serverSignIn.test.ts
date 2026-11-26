import assert from "node:assert/strict";
import { test } from "node:test";
import { apiKeyProvider, engineStatus, needsManualFallback, pageAddress, signInPhase, waitingFor } from "./serverSignIn.ts";

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
  // Sign-in unverified: a known credential file is mentioned (never its contents); "signed out" stays signed out.
  assert.deepEqual(engineStatus({ installed: true, signed_in: null, account: null, credential_file_present: true }), {
    key: "serverConnectionStatus.credentialFound"
  });
  assert.deepEqual(engineStatus({ installed: true, signed_in: null, account: null, credential_file_present: false }), {
    key: "home.engines.installedUnknown"
  });
  assert.deepEqual(engineStatus({ installed: true, signed_in: false, account: null, credential_file_present: true }), {
    key: "home.engines.needsSignIn"
  });
});

test("the window shows the real host and path of the https page, without its query", () => {
  assert.deepEqual(pageAddress("https://accounts.google.com/o/oauth2/v2/auth?client_id=x&state=s"), {
    host: "accounts.google.com",
    shown: "accounts.google.com/o/oauth2/v2/auth"
  });
  assert.deepEqual(pageAddress("https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-1234"), {
    host: "device.sso.us-east-1.amazonaws.com",
    shown: "device.sso.us-east-1.amazonaws.com/"
  });
  // The host is the one a browser would go to, whatever the text before the `@` pretends.
  assert.equal(pageAddress("https://claude.com@evil.example/x")?.host, "evil.example");
  assert.equal(pageAddress("https://claude.com.evil.example/x")?.host, "claude.com.evil.example");
});

test("a page that is not https, or not a page, is not offered", () => {
  for (const bad of ["http://claude.com/x", "javascript:alert(1)", "file:///etc/passwd", "claude.com/x", "", "https://", null, undefined]) {
    assert.equal(pageAddress(bad), null, String(bad));
  }
});
