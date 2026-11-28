import type { CliEngineDto } from "../api/types.ts";
import { brandIcon } from "./brandIcons.ts";

/** The one status an engine card shows. */
export type CliStatus = "ready" | "sign_in" | "outdated" | "not_installed" | "unsupported";

/** An engine Praxi can switch on: the confined runner, or its own-tools mode. */
export function canRunEngine(engine: CliEngineDto): boolean {
  return engine.detected.runnable || Boolean(engine.native);
}

export function cliStatus(engine: CliEngineDto): CliStatus {
  const row = engine.detected;
  if (!row.installed) {
    return "not_installed";
  }
  if (engine.outdated) {
    return "outdated";
  }
  if (!canRunEngine(engine)) {
    return "unsupported";
  }
  return row.auth === "signed_out" ? "sign_in" : "ready";
}

/** The on/off switch is offered only for an engine that is ready to run. */
export function hasSwitch(engine: CliEngineDto): boolean {
  return cliStatus(engine) === "ready";
}

/** Installed engines are cards; the rest go in one collapsed "more engines" list. */
export function splitEngines(engines: readonly CliEngineDto[]): {
  installed: CliEngineDto[];
  available: CliEngineDto[];
} {
  return {
    installed: engines.filter((e) => e.detected.installed),
    available: engines.filter((e) => !e.detected.installed)
  };
}

/** `\?\C:\x` (verbatim) is shown and copied as `C:\x`. */
export function displayPath(path: string): string {
  if (path.startsWith("\\\\?\\UNC\\")) {
    return `\\\\${path.slice(8)}`;
  }
  return /^\\\\\?\\[a-zA-Z]:/.test(path) ? path.slice(4) : path;
}

/**
 * Brand mark of an engine header: the tool's own logo (Claude Code: the Claude starburst, not the
 * Anthropic "A" of the API; Antigravity: its own arch). Model rows use `productBrandKey` (the model's logo).
 */
export function brandOf(id: string): string {
  return id === "agy" ? "antigravity" : id === "claude" ? "claude" : id;
}

/** Prefer the real logo; data-only engines with no licensed mark use the neutral terminal glyph. */
export function cliBrandKey(id: string): string {
  const name = brandOf(id);
  const { known } = brandIcon(name);
  return known ? name : "terminal";
}

/** Version for display: `claude 2.1.325 (Claude Code)` and `v2.3.0` read as the number. */
export function shortVersion(version: string | null | undefined): string | null {
  if (!version) {
    return null;
  }
  const match = /\d+\.\d+\.\d+/.exec(version);
  return match ? match[0] : version.slice(0, 24);
}

/** Auto-enabled by the daemon but not yet in the provider list the page holds. */
export function needsProviderReload(
  engines: readonly CliEngineDto[],
  enabledCliIds: ReadonlySet<string>
): boolean {
  return engines.some((e) => e.enabled && !enabledCliIds.has(`cli:${e.detected.id}`));
}
