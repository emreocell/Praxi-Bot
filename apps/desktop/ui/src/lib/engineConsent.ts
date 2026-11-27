import type { CliEngineDto } from "../api/types.ts";
import { APPROVAL_LEVELS, type ApprovalLevel, type EngineState, engineOf, executorCli } from "./botEngine.ts";
import { cliStatus } from "./cliCards.ts";

/** Why a CLI model cannot be used for a bot right now (the picker row is disabled with this reason). */
export type EngineBlock = "not_installed" | "outdated" | "unsupported" | "sign_in";

/**
 * What picking a CLI model for a bot requires. `auto`: the bot is on Full access (it asks nothing
 * but money), so the engine's own tools are switched on at Full without a question.
 */
export type EngineStatus =
  | { kind: "ready" }
  | { kind: "blocked"; cliId: string; name: string; reason: EngineBlock }
  | { kind: "auto"; cliId: string; name: string; level: ApprovalLevel }
  | { kind: "consent"; cliId: string; name: string; levels: ApprovalLevel[]; defaultLevel: ApprovalLevel };

/**
 * One spelling rule for a stored permission preset, the same as the daemon's `normalize_preset`
 * (`permission-engine`): edge spaces, case and `_` versus `-` do not matter.
 */
export function normalizePreset(preset: string): string {
  return preset.trim().toLowerCase().replace(/_/g, "-");
}

/** Whether the bot's permission preset is Full access (the daemon decides the same way, and checks it again). */
export function isFullBot(config: unknown): boolean {
  const preset = (config as { permissions?: { preset?: unknown } } | null | undefined)?.permissions?.preset;
  return typeof preset === "string" && normalizePreset(preset) === "full";
}

/**
 * Whether a message must pass the engine check first: only a bot whose executor is a coding CLI.
 * An API model never waits for it, whatever an earlier CLI left in the bot's engine table.
 */
export function needsEngineCheck(config: unknown): boolean {
  return executorCli(config) !== null;
}

/** The owner's answer to the one consent step; applied with `PUT /v1/bots/{id}/engine`. */
export interface EngineChoice {
  cliId: string;
  level: ApprovalLevel;
  /** Applied without a dialog because the bot is on Full access; the daemon checks the preset again. */
  fullAccessBot?: boolean;
}

/** Why an engine cannot start at all, or `null`. A signed-out CLI is blocked as well: it would fail. */
export function engineBlock(engine: CliEngineDto): EngineBlock | null {
  const status = cliStatus(engine);
  if (status === "ready") {
    return engine.detected.auth === "signed_out" ? "sign_in" : null;
  }
  return status === "sign_in" ? "sign_in" : status;
}

/**
 * What a bot needs before the CLI behind `executor` can answer: nothing (the confined runner works,
 * or the bot already runs it with its own tools), one consent step (the engine only works with its
 * own tools), or it cannot start. API models and unknown engines need nothing.
 */
export function engineStatus(
  executor: string,
  current: EngineState,
  engines: readonly CliEngineDto[],
  fullBot = false
): EngineStatus {
  const cliId = executorCli({ models: { executor } });
  const engine = cliId ? engines.find((e) => e.detected.id === cliId) : undefined;
  if (!cliId || !engine) {
    return { kind: "ready" };
  }
  const name = engine.detected.display_name;
  const reason = engineBlock(engine);
  if (reason) {
    return { kind: "blocked", cliId, name, reason };
  }
  const native = current.mode === "native_tools";
  if (engine.detected.runnable && !native) {
    return { kind: "ready" };
  }
  const levels = (engine.native?.levels ?? []).filter((l) => l.offered).map((l) => l.level as ApprovalLevel);
  // A bot that already works with its own tools keeps its level when the new engine offers it.
  if (native && levels.includes(current.approvalLevel)) {
    return { kind: "ready" };
  }
  if (levels.length === 0) {
    return { kind: "blocked", cliId, name, reason: "unsupported" };
  }
  if (fullBot && levels.includes("full")) {
    return { kind: "auto", cliId, name, level: "full" };
  }
  return { kind: "consent", cliId, name, levels, defaultLevel: defaultLevel(levels, native ? current.approvalLevel : "ask") };
}

/**
 * The level the consent step starts on: `wanted` when offered, else the widest offered level that
 * is not wider than it (a change of engine never silently widens), else the narrowest offered one.
 */
function defaultLevel(levels: readonly ApprovalLevel[], wanted: ApprovalLevel): ApprovalLevel {
  const rank = (l: ApprovalLevel) => APPROVAL_LEVELS.indexOf(l);
  const within = levels.filter((l) => rank(l) <= rank(wanted));
  return within.length > 0 ? within.reduce((a, b) => (rank(b) > rank(a) ? b : a)) : levels.reduce((a, b) => (rank(b) < rank(a) ? b : a));
}

/** Same as `engineStatus` for a bot config (its stored executor and engine table). */
export function botEngineStatus(config: unknown, engines: readonly CliEngineDto[]): EngineStatus {
  const executor = (config as { models?: { executor?: unknown } } | null | undefined)?.models?.executor;
  return engineStatus(typeof executor === "string" ? executor : "", engineOf(config), engines, isFullBot(config));
}

/** Body of `PUT /v1/bots/{id}/engine` for the owner's choice (the daemon picks the bot's folder). */
export function engineChoiceRequest(choice: EngineChoice) {
  return {
    mode: "native_tools",
    approval_level: choice.level,
    consent_native_tools: true,
    consent_full_access: choice.level === "full",
    ...(choice.fullAccessBot ? { full_access_bot: true } : {})
  };
}
