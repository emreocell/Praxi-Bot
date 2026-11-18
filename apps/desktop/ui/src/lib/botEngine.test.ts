import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_ENGINE, consentMissing, engineOf, engineRequest, executorCli } from "./botEngine.ts";

test("engine settings: default Ask, consent per widening switch, CLI from the executor", () => {
  assert.deepEqual(engineOf({ models: { executor: "x" } }), DEFAULT_ENGINE);
  assert.equal(executorCli({ models: { executor: "cli:opencode/some-model" } }), "opencode");
  assert.equal(executorCli({ models: { executor: "openai/gpt" } }), null);
  const native = { mode: "native_tools" as const, approvalLevel: "ask" as const, workspace: " C:/work " };
  const none = { native: false, full: false };
  assert.equal(consentMissing(native, DEFAULT_ENGINE, none), true);
  assert.equal(consentMissing(native, DEFAULT_ENGINE, { native: true, full: false }), false);
  const full = { ...native, approvalLevel: "full" as const };
  // Already native at Ask: only the Full consent is needed, and it is sent.
  assert.equal(consentMissing(full, native, { native: true, full: false }), true);
  assert.deepEqual(engineRequest(full, native, { native: true, full: true }), {
    mode: "native_tools", approval_level: "full", workspace: "C:/work",
    consent_native_tools: false, consent_full_access: true
  });
  assert.deepEqual(engineRequest(DEFAULT_ENGINE, full, none), { mode: "praxi_governed" });
  // No folder typed: none is sent and the daemon makes one for the bot.
  assert.equal("workspace" in engineRequest({ ...native, workspace: "  " }, DEFAULT_ENGINE, { native: true, full: false }), false);
});
