import assert from "node:assert/strict";
import { test } from "node:test";
import type { CliEngineDto } from "../api/types.ts";
import { DEFAULT_ENGINE, type EngineState } from "./botEngine.ts";
import { configFromDraft, draftFromBot, parseModelRef } from "./botProfile.ts";
import { botEngineStatus, engineChoiceRequest, engineStatus, isFullBot, needsEngineCheck, normalizePreset } from "./engineConsent.ts";

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

test("switching a native bot to an engine that lacks its level asks again, never widening it", () => {
  const native = (approvalLevel: EngineState["approvalLevel"]): EngineState => ({ ...DEFAULT_ENGINE, mode: "native_tools", approvalLevel });
  const engines = [
    engine("agy", { levels: ["ask", "accept_edits", "auto", "full"] }),
    engine("cursor", { levels: ["ask", "auto", "full"] }),
    engine("opencode", { levels: ["full"] })
  ];
  // Offered as it is: nothing to ask.
  assert.deepEqual(engineStatus("cli:agy/default", native("accept_edits"), engines), { kind: "ready" });
  // Cursor has no Accept edits: the dialog starts on the widest level that is not wider (Ask).
  const cursor = engineStatus("cli:cursor/auto", native("accept_edits"), engines);
  assert.deepEqual(cursor, { kind: "consent", cliId: "cursor", name: "cursor", levels: ["ask", "auto", "full"], defaultLevel: "ask" });
  // OpenCode only offers Full: the only choice, and it is the owner's to confirm.
  const open = engineStatus("cli:opencode/x", native("ask"), engines);
  assert.deepEqual(open, { kind: "consent", cliId: "opencode", name: "opencode", levels: ["full"], defaultLevel: "full" });
});

test("a freshly added engine with default settings ends up runnable: ready, or one consent that the daemon accepts", () => {
  // Levels each engine offers on Linux (mirrors `level_plan` in crates/cli-provider/src/levels.rs).
  const offered: Record<string, string[]> = {
    claude: ["ask", "accept_edits", "auto", "full"],
    codex: ["ask", "auto", "full"],
    cursor: ["ask", "auto", "full"],
    opencode: ["full"],
    agy: ["ask", "accept_edits", "auto", "full"],
    qwen: ["ask", "accept_edits", "auto", "full"],
    kiro: ["ask", "full"],
    copilot: ["ask", "accept_edits", "full"],
    kimi: ["auto", "full"],
    qoder: ["ask", "accept_edits", "auto", "full"],
    vibe: ["ask", "accept_edits", "auto", "full"],
    // Goose has no edits-only mode: Accept edits would run as Ask, so it is not offered.
    goose: ["ask", "auto", "full"]
  };
  for (const [id, levels] of Object.entries(offered)) {
    const list = [engine(id, { runnable: id === "claude", levels })];
    const status = engineStatus(`cli:${id}/default`, DEFAULT_ENGINE, list);
    if (id === "claude") {
      assert.deepEqual(status, { kind: "ready" }, id);
      continue;
    }
    assert.equal(status.kind, "consent", id);
    if (status.kind !== "consent") continue;
    assert.ok(levels.includes(status.defaultLevel), `${id}: the dialog starts on an offered level`);
    const request = engineChoiceRequest({ cliId: id, level: status.defaultLevel });
    assert.equal(request.mode, "native_tools");
    assert.equal(request.consent_native_tools, true);
    assert.equal(request.consent_full_access, status.defaultLevel === "full", id);
    // Applied: the bot is native, so the next check (send time) has nothing left to ask.
    const applied: EngineState = { mode: "native_tools", approvalLevel: status.defaultLevel, workspace: "" };
    assert.deepEqual(engineStatus(`cli:${id}/default`, applied, list), { kind: "ready" }, id);
  }
});

test("a Full-access bot gets its engine's own tools switched on at Full without a question", () => {
  const engines = [engine("agy", { levels: ["ask", "accept_edits", "auto", "full"] }), engine("claude", { runnable: true, levels: ["ask", "full"] }), engine("kimi", { levels: ["auto"] })];
  // The owner's rule: on Full a bot asks nothing except before spending money.
  assert.deepEqual(engineStatus("cli:agy/gemini-4-hydrogen-medium", DEFAULT_ENGINE, engines, true), { kind: "auto", cliId: "agy", name: "agy", level: "full" });
  // What the daemon is sent is the owner-consented Full request.
  assert.deepEqual(engineChoiceRequest({ cliId: "agy", level: "full" }), { mode: "native_tools", approval_level: "full", consent_native_tools: true, consent_full_access: true });
  // Any other bot still gets the one consent dialog.
  assert.equal(engineStatus("cli:agy/gemini-4-hydrogen-medium", DEFAULT_ENGINE, engines, false).kind, "consent");
  // An engine that runs confined (Claude Code) needs nothing; one without a Full level cannot be auto-enabled.
  assert.deepEqual(engineStatus("cli:claude/sonnet", DEFAULT_ENGINE, engines, true), { kind: "ready" });
  assert.equal(engineStatus("cli:kimi/x", DEFAULT_ENGINE, engines, true).kind, "consent");
  // The stored permission preset decides: only `full` is Full access.
  const bot = (preset?: string) => ({ models: { executor: "cli:agy/gemini-4-hydrogen-medium" }, permissions: preset ? { preset } : {} });
  assert.equal(isFullBot(bot("full")), true);
  assert.equal(isFullBot(bot("approve-destructive")), false);
  assert.equal(isFullBot(bot()), false);
  assert.equal(isFullBot(null), false);
  assert.equal(botEngineStatus(bot("full"), engines).kind, "auto");
  assert.equal(botEngineStatus(bot("read-only"), engines).kind, "consent");
});

test("a cli that is not installed blocks the first message with a reason, also for a bot that already works natively", () => {
  const missing = [engine("agy", { installed: false })];
  const native = { models: { executor: "cli:agy/x" }, permissions: { preset: "full" }, engine: { mode: "native_tools", approval_level: "full" } };
  const governed = { models: { executor: "cli:agy/x" }, permissions: { preset: "full" } };
  for (const bot of [native, governed]) {
    assert.equal(needsEngineCheck(bot), true);
    assert.deepEqual(botEngineStatus(bot, missing), { kind: "blocked", cliId: "agy", name: "agy", reason: "not_installed" });
  }
});

test("an API model never waits for the engine check, whatever an earlier CLI left in the bot", () => {
  // Switching the executor from a CLI to an API model leaves a stale native engine table behind
  // until the daemon resets it: the composer must not stay blocked by it.
  const stale = { models: { executor: "openai/gpt-5" }, permissions: { preset: "full" }, engine: { mode: "native_tools", approval_level: "full", workspace: "/var/lib/praxi-work/main" } };
  assert.equal(needsEngineCheck(stale), false);
  assert.deepEqual(botEngineStatus(stale, [engine("agy", { installed: false })]), { kind: "ready" });
  assert.equal(needsEngineCheck({ models: { executor: "cli:agy/x" } }), true);
  assert.equal(needsEngineCheck({}), false);
  assert.equal(needsEngineCheck(undefined), false);
});

test("Full access never switches on an engine that cannot start: not installed, out of date and signed out stay blocked", () => {
  const levels = ["ask", "accept_edits", "auto", "full"];
  const fullBot = { models: { executor: "cli:agy/x" }, permissions: { preset: "full" } };
  const cases: Array<[string, CliEngineDto, string]> = [
    ["not installed", engine("agy", { installed: false, levels }), "not_installed"],
    ["out of date", { ...engine("agy", { levels }), outdated: true } as CliEngineDto, "outdated"],
    ["signed out", engine("agy", { auth: "signed_out", levels }), "sign_in"]
  ];
  for (const [name, cli, reason] of cases) {
    assert.deepEqual(botEngineStatus(fullBot, [cli]), { kind: "blocked", cliId: "agy", name: "agy", reason }, name);
    assert.deepEqual(engineStatus("cli:agy/x", DEFAULT_ENGINE, [cli], true), { kind: "blocked", cliId: "agy", name: "agy", reason }, name);
  }
  // Only a working engine is switched on.
  assert.equal(botEngineStatus(fullBot, [engine("agy", { levels })]).kind, "auto");
});

test("the preset spelling rule is the daemon's: case, edge spaces and underscores do not matter", () => {
  const bot = (preset: string) => ({ permissions: { preset } });
  for (const spelled of ["full", "Full", " FULL ", "\tfull\n"]) {
    assert.equal(isFullBot(bot(spelled)), true, JSON.stringify(spelled));
  }
  for (const spelled of ["fu_ll", "full access", "approve_destructive", "", "read-only"]) {
    assert.equal(isFullBot(bot(spelled)), false, JSON.stringify(spelled));
  }
  assert.equal(normalizePreset(" Approve_Destructive "), "approve-destructive");
});

test("only the no-dialog Full access pick tells the daemon so; the dialog's own answer never does", () => {
  assert.deepEqual(engineChoiceRequest({ cliId: "agy", level: "full", fullAccessBot: true }), {
    mode: "native_tools",
    approval_level: "full",
    consent_native_tools: true,
    consent_full_access: true,
    full_access_bot: true
  });
  assert.equal("full_access_bot" in engineChoiceRequest({ cliId: "agy", level: "full" }), false);
  assert.equal("full_access_bot" in engineChoiceRequest({ cliId: "agy", level: "ask" }), false);
});
