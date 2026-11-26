import type { HostUiState } from "./hostState.ts";

/**
 * "Home" is where the bots live: the paired host the owner adopted (`GET /v1/hosts/home`), or this
 * computer when there is none. The user never picks it; the app follows it. The only way to look at
 * this computer's own bots while a home exists is the explicit, temporary "work on this computer
 * for now" mode.
 */
export type Home = { kind: "local" } | { kind: "host"; hostId: string; name: string };

export const LOCAL_HOME: Home = { kind: "local" };

/** The adopted host from the daemon's answer; an id it cannot name yet still counts as a host. */
export function homeFromInfo(
  hostId: string | null | undefined,
  hosts: ReadonlyArray<{ id: string; name: string }>
): Home {
  if (!hostId) {
    return LOCAL_HOME;
  }
  return { kind: "host", hostId, name: hosts.find((h) => h.id === hostId)?.name ?? "" };
}

/** The computer whose data the app shows: the home, unless the user chose to work here for now. */
export function wantedComputer(home: Home, workLocalNow: boolean, localId: string): string {
  return home.kind === "host" && !workLocalNow ? home.hostId : localId;
}

/** What the status chip says about the bots' computer. */
export type HomeStatus = "local" | "online" | "starting" | "degraded" | "connecting" | "offline" | "temporary" | "version_mismatch";

export function homeStatus(home: Home, workLocalNow: boolean, host: HostUiState | null): HomeStatus {
  if (home.kind === "local") {
    return "local";
  }
  if (workLocalNow) {
    return "temporary";
  }
  switch (host?.kind) {
    case "connected":
    case "no_model_key":
      return "online";
    case "starting":
      return "starting";
    case "degraded":
      return "degraded";
    case "version_mismatch":
      return host.blocking ? "version_mismatch" : "online";
    case "offline":
    case "not_paired":
      return "offline";
    default:
      return "connecting";
  }
}
