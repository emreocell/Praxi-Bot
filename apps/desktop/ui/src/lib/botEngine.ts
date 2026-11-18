/** Engine settings of a bot (who runs its files and shell), as stored in `config.engine`. */
export type EngineMode = "praxi_governed" | "native_tools";
export type ApprovalLevel = "ask" | "accept_edits" | "auto" | "full";

export const APPROVAL_LEVELS: readonly ApprovalLevel[] = ["ask", "accept_edits", "auto", "full"];

export interface EngineState {
  mode: EngineMode;
  approvalLevel: ApprovalLevel;
  workspace: string;
}

export const DEFAULT_ENGINE: EngineState = { mode: "praxi_governed", approvalLevel: "ask", workspace: "" };

/** Read the stored `engine` table of a bot config; anything unexpected is the default. */
export function engineOf(config: unknown): EngineState {
  const raw = (config as { engine?: Record<string, unknown> } | null | undefined)?.engine;
  if (!raw || typeof raw !== "object") { return DEFAULT_ENGINE; }
  const mode: EngineMode = raw.mode === "native_tools" ? "native_tools" : "praxi_governed";
  const level = APPROVAL_LEVELS.find((l) => l === raw.approval_level) ?? "ask";
  return { mode, approvalLevel: level, workspace: typeof raw.workspace === "string" ? raw.workspace : "" };
}

/** Registry id of the CLI behind the bot's executor model (`cli:claude/sonnet` -> `claude`). */
export function executorCli(config: unknown): string | null {
  const executor = (config as { models?: { executor?: unknown } } | null | undefined)?.models?.executor;
  if (typeof executor !== "string" || !executor.startsWith("cli:")) { return null; }
  return executor.slice(4).split("/")[0] || null;
}

/** Body of `PUT /v1/bots/{id}/engine`. Consent is only sent for the widening switches. */
export function engineRequest(
  next: EngineState,
  current: EngineState,
  consent: { native: boolean; full: boolean }
) {
  if (next.mode === "praxi_governed") { return { mode: next.mode }; }
  return {
    mode: next.mode,
    approval_level: next.approvalLevel,
    // Left empty, the daemon makes a folder for the bot.
    ...(next.workspace.trim() ? { workspace: next.workspace.trim() } : {}),
    consent_native_tools: current.mode !== "native_tools" && consent.native,
    consent_full_access:
      next.approvalLevel === "full" &&
      !(current.mode === "native_tools" && current.approvalLevel === "full") &&
      consent.full
  };
}

/** Whether Apply must stay disabled until the owner ticks the consent boxes. */
export function consentMissing(
  next: EngineState,
  current: EngineState,
  consent: { native: boolean; full: boolean }
): boolean {
  if (next.mode !== "native_tools") { return false; }
  const needNative = current.mode !== "native_tools";
  const needFull = next.approvalLevel === "full" && !(current.mode === "native_tools" && current.approvalLevel === "full");
  return (needNative && !consent.native) || (needFull && !consent.full);
}

const CLI_NAMES: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  cursor: "Cursor",
  opencode: "OpenCode",
  gemini: "Gemini CLI"
};

/** Name of the CLI that runs this bot's files and shell, or `null` when Praxi does. */
export function nativeEngineName(config: unknown): string | null {
  if (engineOf(config).mode !== "native_tools") { return null; }
  const id = executorCli(config);
  return id ? (CLI_NAMES[id] ?? id) : null;
}
