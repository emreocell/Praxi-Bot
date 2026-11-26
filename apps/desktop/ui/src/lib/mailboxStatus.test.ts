import assert from "node:assert/strict";
import { test } from "node:test";
import type { EmailTestReport } from "../api/types.ts";
import { mailboxStatus } from "./mailboxStatus.ts";

const report = (over: Partial<EmailTestReport>): EmailTestReport => ({ ok: true, imap: "ok", smtp: "ok", ...over });

test("a configured mailbox reads as connected, and as checked after a passing test", () => {
  assert.deepEqual(mailboxStatus({ password_set: true }, {}), { tone: "ok", tested: false });
  assert.deepEqual(mailboxStatus({ password_set: true }, { report: report({}) }), { tone: "ok", tested: true });
});

test("a missing password beats everything else and is fixed by editing", () => {
  const s = mailboxStatus({ password_set: false }, { report: report({ ok: false, imap: "auth_failed" }), boundBotToolsOn: false });
  assert.deepEqual(s, { tone: "attention", reason: "secretMissing", fix: "edit" });
});

test("a failed test asks to edit for a refused login and to try again for a timeout", () => {
  const login = mailboxStatus({ password_set: true }, { report: report({ ok: false, imap: "auth_failed" }) });
  assert.deepEqual(login, { tone: "attention", reason: "test", step: "authFailed", fix: "edit" });
  const slow = mailboxStatus({ password_set: true }, { report: report({ ok: false, smtp: "timeout" }) });
  assert.deepEqual(slow, { tone: "attention", reason: "test", step: "timeout", fix: "test" });
});

test("a bound bot without mail tools needs one click to turn them on", () => {
  assert.deepEqual(mailboxStatus({ password_set: true }, { boundBotToolsOn: false }), {
    tone: "attention",
    reason: "toolsOff",
    fix: "turnOnTools"
  });
  assert.equal(mailboxStatus({ password_set: true }, { boundBotToolsOn: true }).tone, "ok");
});
