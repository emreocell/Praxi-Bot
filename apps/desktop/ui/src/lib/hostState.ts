/**
 * Connection state of a paired always-on host as the app shows it.
 * Pure: the inputs are what the local daemon reports about the link
 * (`GET /v1/hosts`) and what the host answered on its public health route.
 */

import { isProcessingRunState } from "./runState.ts";
import { reportedRelease, updateState } from "./serverUpdate.ts";

export type LinkState =
  | "idle"
  | "connecting"
  | "connected"
  | "offline"
  | "unreachable";

export interface HostLinkLike {
  id: string;
  name: string;
  state: LinkState;
}

export interface HostHealthLike {
  status: string;
  version: string;
  api_version: string;
  release_tag?: string | null;
  build_id?: string | null;
  /** Background startup of the host's own components; absent on older hosts. */
  startup?: "starting" | "ready" | "degraded";
}

export type HostUiState =
  | { kind: "connecting" }
  | { kind: "connected" }
  | { kind: "offline" }
  /** Revoked by the host, removed from the list, or never paired. */
  | { kind: "not_paired" }
  /** The two daemons speak different API versions (blocking) or builds. */
  | { kind: "version_mismatch"; local: string; host: string; blocking: boolean; update: "app" | "host" | "unknown"; unidentified?: true }
  /** Connected and compatible, but the host cannot run a model yet. */
  | { kind: "no_model_key" }
  /** Reachable, but still verifying leadership and starting its components. */
  | { kind: "starting" }
  /** Reachable, but a component (for example the scheduler) failed to start. */
  | { kind: "degraded" };

/** Compare `major.minor` only: patch differences never block. */
function sameMinor(a: string, b: string): boolean {
  const key = (v: string) => v.replace(/^v/, "").split(".").slice(0, 2).join(".");
  return key(a) === key(b);
}

/**
 * Which side is behind, including prerelease ordering.
 */
export function olderSide(local: string, host: string): "app" | "host" | "unknown" {
  const state = updateState({ version: host }, { version: local });
  return state === "older" ? "host" : state === "newer" ? "app" : "unknown";
}

export interface HostProtocol {
  localVersion: string | null;
  localApiVersion: string | null;
  localRelease?: string | null;
}

export function hostMismatch(health: HostHealthLike, protocol: HostProtocol): Extract<HostUiState, { kind: "version_mismatch" }> | null {
  const { localVersion, localApiVersion, localRelease } = protocol;
  if (!localVersion) return null;
  const hostRelease = reportedRelease(health.release_tag, health.build_id);
  const local = localRelease || localVersion;
  const host = hostRelease || health.version;
  const builds = updateState({ version: health.version, releaseTag: hostRelease }, { version: localVersion, releaseTag: localRelease });
  const incompatible = (localApiVersion !== null && health.api_version !== localApiVersion) || !sameMinor(host, local);
  const preview = Boolean(localRelease && (localRelease.includes("-") || hostRelease?.includes("-")));
  // Every published build reports the same cargo version, so "same version" says nothing about which
  // release a server is. Once this app knows its own release, a server that names none (an older
  // build, or a pre-release answering as 0.1.0 to a stable app) is not accepted as the same.
  const unidentified = Boolean(localRelease) && !hostRelease;
  const previewSkew = (preview && builds !== "current") || unidentified;
  const sameVersion = builds === "current" || (builds === "unknown" && health.version === localVersion);
  if (!incompatible && !previewSkew && sameVersion) return null;
  return {
    kind: "version_mismatch",
    local,
    host,
    blocking: incompatible || previewSkew,
    ...(previewSkew && builds === "unknown" ? { unidentified: true } : {}),
    update: builds === "older" || (previewSkew && builds === "unknown") ? "host" : builds === "newer" ? "app" : "unknown"
  };
}

export function deriveHostState(input: HostProtocol & {
  link: HostLinkLike | null;
  health: HostHealthLike | null;
}): HostUiState {
  const { link, health } = input;
  if (!link || link.state === "unreachable") {
    return { kind: "not_paired" };
  }
  if (link.state === "offline") {
    return { kind: "offline" };
  }
  if (link.state !== "connected" || !health) {
    return { kind: "connecting" };
  }
  const mismatch = hostMismatch(health, input);
  if (mismatch) return mismatch;
  if (health.status === "setup_required") {
    return { kind: "no_model_key" };
  }
  if (health.startup === "starting") {
    return { kind: "starting" };
  }
  if (health.startup === "degraded") {
    return { kind: "degraded" };
  }
  return { kind: "connected" };
}

/** Whether the app may send work to the host in this state. */
export function hostAcceptsWork(state: HostUiState): boolean {
  return (
    state.kind === "connected" ||
    state.kind === "starting" ||
    state.kind === "degraded" ||
    (state.kind === "version_mismatch" && !state.blocking)
  );
}

/** Whether chats can run a model there (the host holds a usable key). */
export function hostCanRunModel(state: HostUiState): boolean {
  return hostAcceptsWork(state);
}

/** Runs in these states are live and are what "stop on host" lists. */
export function liveRunIds(runs: ReadonlyArray<{ id: string; state: string }>): string[] {
  return runs.filter((r) => isProcessingRunState(r.state)).map((r) => r.id);
}

/**
 * What is mounted for a given active computer. The local safety machinery (the
 * control bridge: heartbeat, native Stop / tray, local events, approval overlays)
 * is ALWAYS connected to this computer's daemon; a host only adds a second
 * session for the views.
 */
export function computerScopePlan(
  activeComputerId: string,
  localId = "local"
): { localBridge: true; hostSession: boolean } {
  return { localBridge: true, hostSession: activeComputerId !== localId };
}

/** How long a good connection may look bad before the banner says so (health ticks every 3 s). */
export const HOST_STATE_HOLD_MS = 10_000;
/** How long "connecting" may last before it becomes "offline" with a retry. */
export const HOST_CONNECT_GIVEUP_MS = 25_000;

export interface HostStateMemory {
  shown: HostUiState;
  /** When `shown` was first shown. */
  since: number;
  /** A worse raw state seen since this time, not yet shown. */
  worseSince: number | null;
}

const WORSE = new Set(["connecting", "offline"]);
const GOOD = new Set(["connected", "no_model_key", "starting", "degraded"]);

function sameState(a: HostUiState, b: HostUiState): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * One stable state per connection. The raw state is recomputed on every poll and
 * flips whenever one health read is missed; the banner only follows it when that lasts:
 * connected stays connected for `HOST_STATE_HOLD_MS` of bad polls (then it is "offline", never
 * "connecting" again), connecting becomes offline after `HOST_CONNECT_GIVEUP_MS`, and any
 * improvement or hard state (not paired, version) shows at once.
 */
export function settleHostState(
  mem: HostStateMemory | null,
  raw: HostUiState,
  now: number
): HostStateMemory {
  if (!mem) {
    return { shown: raw, since: now, worseSince: null };
  }
  const { shown } = mem;
  if (sameState(shown, raw)) {
    return { ...mem, worseSince: null };
  }
  if (GOOD.has(shown.kind) && WORSE.has(raw.kind)) {
    const worseSince = mem.worseSince ?? now;
    if (now - worseSince >= HOST_STATE_HOLD_MS) {
      return { shown: { kind: "offline" }, since: now, worseSince: null };
    }
    return { ...mem, worseSince };
  }
  if (shown.kind === "connecting" && raw.kind === "connecting") {
    return mem;
  }
  if (shown.kind === "offline" && raw.kind === "connecting") {
    // Stay "offline" while a retry is under way; no flip back to "connecting".
    return { ...mem, worseSince: null };
  }
  return { shown: raw, since: now, worseSince: null };
}

/** The shown state, turning a connect that never completes into "offline". */
export function giveUpConnecting(mem: HostStateMemory, now: number): HostUiState {
  return mem.shown.kind === "connecting" && now - mem.since >= HOST_CONNECT_GIVEUP_MS
    ? { kind: "offline" }
    : mem.shown;
}
