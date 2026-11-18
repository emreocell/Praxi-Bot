/**
 * Which computer the right panel's "Computer" card shows.
 *
 * Bots live at their home: the adopted server, or this computer. The card shows the computer the
 * bot RUNS ON. While a server is home the user can also look at "my computer" (the PC the app runs
 * on), which a bot only touches when asked.
 */
import type { HostUiState } from "./hostState.ts";
import type { Home } from "./home.ts";

export type ComputerSide = "bot" | "mine";

/** The server whose screen the card shows, or `null` when it shows this computer. */
export function shownServer(home: Home, workLocalNow: boolean, side: ComputerSide): string | null {
  return home.kind === "host" && !workLocalNow && side === "bot" ? home.name : null;
}

/** The switcher only exists while both a server home and this computer are in play. */
export function hasBothComputers(home: Home, workLocalNow: boolean): boolean {
  return home.kind === "host" && !workLocalNow;
}

/** Why the server's screen cannot be shown at all, before asking it for targets. */
export type ServerScreenIssue = "connecting" | "offline" | "notPaired" | "version" | null;

export function serverScreenIssue(host: HostUiState | null): ServerScreenIssue {
  switch (host?.kind) {
    case undefined:
    case "connecting":
      return "connecting";
    case "offline":
      return "offline";
    case "not_paired":
      return "notPaired";
    case "version_mismatch":
      return host.blocking ? "version" : null;
    default:
      return null;
  }
}

/**
 * Reason codes (`noScreenCode`) after which the fix is a desktop session on the server, and the
 * command that adds one from this computer (`bot host repair` re-runs the install with a desktop).
 */
const NEEDS_DESKTOP_SESSION = new Set(["no_display", "desktop_off", "screen_unavailable"]);

export function serverDesktopCommand(
  noScreen: string | null,
  serverName: string
): string | null {
  if (!noScreen || !NEEDS_DESKTOP_SESSION.has(noScreen) || !serverName.trim()) {
    return null;
  }
  const name = serverName.trim();
  return /^[A-Za-z0-9._@:-]+$/.test(name)
    ? `bot host repair ${name}`
    : `bot host repair "${name.replace(/"/g, "")}"`;
}
