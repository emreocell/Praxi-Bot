import assert from "node:assert/strict";
import { test } from "node:test";
import type { CliEngineDto } from "../api/types.ts";
import { DEFAULT_ENGINE } from "./botEngine.ts";
import { configFromDraft, draftFromBot, parseModelRef } from "./botProfile.ts";
import { engineChoiceRequest, engineStatus } from "./engineConsent.ts";

function engine(id: string, over: { runnable?: boolean; installed?: boolean; auth?: string; levels?: string[] }): CliEngineDto {
  return {
    detected: { id, display_name: id, installed: over.installed ?? true, runnable: over.runnable ?? false, auth: over.auth ?? "signed_in" },
    enabled: true,
    outdated: false,
    native: { support: "verified", levels: ["ask", "accept_edits", "auto", "full"].map((level) => ({ level, applied: level, native_setting: "", offered: (over.levels ?? ["ask"]).includes(level) })) }
  } as unknown as CliEngineDto;
}

test("a CLI model needs the one consent step only when the engine works with its own tools", () => {
  const engines = [engine("claude", { runnable: true }), engine("codex", { levels: ["ask", "full"] }), engine("opencode", { levels: ["full"] }), engine("agy", { installed: false })];
  assert.deepEqual(engineStatus("openai/gpt", DEFAULT_ENGINE, engines), { kind: "ready" });
  // Claude Code runs confined: no consent.
  assert.deepEqual(engineStatus("cli:claude/sonnet", DEFAULT_ENGINE, engines), { kind: "ready" });
  // Codex: Ask is preselected. OpenCode cannot hold Ask: only Full is offered.
  assert.deepEqual(engineStatus("cli:codex/default", DEFAULT_ENGINE, engines), { kind: "consent", cliId: "codex", name: "codex", levels: ["ask", "full"], defaultLevel: "ask" });
  const open = engineStatus("cli:opencode/x", DEFAULT_ENGINE, engines);
  assert.equal(open.kind === "consent" && open.defaultLevel, "full");
  // Already native: nothing to ask again. Not installed: blocked with the reason.
  assert.deepEqual(engineStatus("cli:codex/default", { ...DEFAULT_ENGINE, mode: "native_tools" }, engines), { kind: "ready" });
  assert.deepEqual(engineStatus("cli:agy/x", DEFAULT_ENGINE, engines), { kind: "blocked", cliId: "agy", name: "agy", reason: "not_installed" });
  assert.deepEqual(engineChoiceRequest({ cliId: "codex", level: "full" }), { mode: "native_tools", approval_level: "full", consent_native_tools: true, consent_full_access: true });
});

test("\"Same as main model\" keeps planner and classifier unset when the main model is a CLI", () => {
  const draft = { ...draftFromBot(undefined), name: "Cli bot", executor: parseModelRef("cli:opencode/opencode/deepseek-v4.1-flash-free") };
  const models = (configFromDraft(draft) as { models: Record<string, unknown> }).models;
  assert.equal(models.executor, "cli:opencode/opencode/deepseek-v4.1-flash-free");
  assert.equal("planner" in models, false);
  assert.equal("classifier" in models, false);
});
