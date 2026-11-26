import type { AdminCliEngine, AdminCliLogin } from "../api/types.ts";

/** What the sign-in modal shows. `idle` = nothing started yet (or the engine cannot be driven). */
export type SignInPhase =
  | "idle"
  | "starting"
  | "needs_browser"
  | "needs_code"
  | "verifying"
  | "done"
  | "failed"
  | "timed_out"
  | "cancelled";

const PHASES: readonly SignInPhase[] = [
  "idle",
  "starting",
  "needs_browser",
  "needs_code",
  "verifying",
  "done",
  "failed",
  "timed_out",
  "cancelled"
];

export function signInPhase(job: Pick<AdminCliLogin, "state"> | null): SignInPhase {
  const s = job?.state as SignInPhase | undefined;
  return s && PHASES.includes(s) ? s : "idle";
}

/**
 * The model provider that a pasted API key signs in for, per engine: the one-click alternative to
 * the engine's own sign-in. Engines without a matching provider offer none.
 */
export function apiKeyProvider(engineId: string): string | null {
  switch (engineId) {
    case "claude":
      return "anthropic";
    case "codex":
      return "openai";
    default:
      return null;
  }
}

/** Failures that mean the CLI never showed its sign-in page: the plain command is the way on. */
export function needsManualFallback(job: Pick<AdminCliLogin, "state" | "error"> | null): boolean {
  return job?.state === "failed" && (job.error === "login_start_timeout" || job.error === "login_cli_outdated");
}

/** What a sign-in waits for while the person works in the browser: a typed code, or nothing. */
export function waitingFor(job: Pick<AdminCliLogin, "code"> | null): "device" | "browser" {
  return job?.code ? "device" : "browser";
}

/** The status line of one engine in the server's engine list (an i18n key plus its values). */
export function engineStatus(
  engine: Pick<AdminCliEngine, "installed" | "signed_in" | "account">
): { key: string; values?: { account: string } } {
  if (!engine.installed) {
    return { key: "home.engines.notInstalled" };
  }
  if (engine.signed_in === true) {
    return engine.account ? { key: "home.engines.readyAs", values: { account: engine.account } } : { key: "home.engines.ready" };
  }
  return { key: engine.signed_in === false ? "home.engines.needsSignIn" : "home.engines.installedUnknown" };
}
