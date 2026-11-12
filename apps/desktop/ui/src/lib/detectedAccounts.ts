import type { DetectedProviderDto, ProvidersDto } from "../api/types.ts";

export type DetectedRowAction =
  | { kind: "enabled" }
  | { kind: "enable" }
  | { kind: "not_installed" }
  | { kind: "sign_in_hint" }
  | { kind: "codex_unconfinable" }
  | { kind: "not_runnable" };

/** Set of enabled `cli:*` provider ids currently configured on the daemon. */
export function configuredCliIds(dto: ProvidersDto | null): Set<string> {
  const out = new Set<string>();
  for (const p of dto?.configured ?? []) {
    if (p.enabled && p.id.startsWith("cli:")) {
      out.add(p.id);
    }
  }
  return out;
}

/**
 * Determine which control/status a Detected accounts row renders.
 * Uses `row.runnable` from the daemon as the single source of truth.
 */
export function detectedRowAction(
  row: DetectedProviderDto,
  enabledCliIds: ReadonlySet<string>
): DetectedRowAction {
  if (!row.installed) {
    return { kind: "not_installed" };
  }
  if (!row.runnable) {
    if (
      row.detail === "codex_unconfinable" ||
      row.id === "codex" ||
      row.id === "chatgpt-plan"
    ) {
      return { kind: "codex_unconfinable" };
    }
    return { kind: "not_runnable" };
  }
  if (enabledCliIds.has(`cli:${row.id}`)) {
    return { kind: "enabled" };
  }
  if (row.auth === "signed_out") {
    return { kind: "sign_in_hint" };
  }
  return { kind: "enable" };
}

/** Terminal command to sign in to a detected CLI when `auth === "signed_out"`. */
export function signInCommand(id: string): string {
  switch (id) {
    case "codex":
    case "chatgpt-plan":
      return "codex login";
    case "claude":
      return "claude";
    default:
      return id;
  }
}
