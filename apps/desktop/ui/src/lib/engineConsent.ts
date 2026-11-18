import type { CliEngineDto } from "../api/types.ts";
import { type ApprovalLevel, type EngineState, engineOf, executorCli } from "./botEngine.ts";
import { cliStatus } from "./cliCards.ts";

/** Why a CLI model cannot be used for a bot right now (the picker row is disabled with this reason). */
export type EngineBlock = "not_installed" | "outdated" | "unsupported" | "sign_in";

/** What picking a CLI model for a bot requires. */
export type EngineStatus =
  | { kind: "ready" }
  | { kind: "blocked"; cliId: string; name: string; reason: EngineBlock }
  | { kind: "consent"; cliId: string; name: string; levels: ApprovalLevel[]; defaultLevel: ApprovalLevel };

/** The owner's answer to the one consent step; applied with `PUT /v1/bots/{id}/engine`. */
export interface EngineChoice {
  cliId: string;
  level: ApprovalLevel;
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
  engines: readonly CliEngineDto[]
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
  if (engine.detected.runnable || current.mode === "native_tools") {
    return { kind: "ready" };
  }
  const levels = (engine.native?.levels ?? []).filter((l) => l.offered).map((l) => l.level as ApprovalLevel);
  if (levels.length === 0) {
    return { kind: "blocked", cliId, name, reason: "unsupported" };
  }
  return { kind: "consent", cliId, name, levels, defaultLevel: levels.includes("ask") ? "ask" : (levels[0] as ApprovalLevel) };
}

/** Same as `engineStatus` for a bot config (its stored executor and engine table). */
export function botEngineStatus(config: unknown, engines: readonly CliEngineDto[]): EngineStatus {
  const executor = (config as { models?: { executor?: unknown } } | null | undefined)?.models?.executor;
  return engineStatus(typeof executor === "string" ? executor : "", engineOf(config), engines);
}

/** Body of `PUT /v1/bots/{id}/engine` for the owner's choice (the daemon picks the bot's folder). */
export function engineChoiceRequest(choice: EngineChoice) {
  return {
    mode: "native_tools",
    approval_level: choice.level,
    consent_native_tools: true,
    consent_full_access: choice.level === "full"
  };
}
