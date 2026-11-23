/**
 * The ONE app-wide state of the local service (the `bot` program the desktop app starts).
 *
 * Input: "did the service answer just now?" (the health poll plus the start-up result). Output: a
 * calm state with debounce, so a restart of a second or two never shows anything, and a longer
 * one shows a single banner instead of errors inside unrelated dialogs.
 *
 * Pure functions only (time is passed in) so the rules are testable without timers.
 */

export type ServiceStatus = "starting" | "ready" | "reconnecting" | "failed";

/** A gap shorter than this is invisible: nothing is shown and dialogs stay usable. */
export const SERVICE_QUIET_MS = 3_000;
/** After this long without an answer the service counts as failed and the owner may restart it. */
export const SERVICE_FAILED_MS = 15_000;

export interface ServiceMemory {
  /** The service answered at least once in this app session. */
  everReady: boolean;
  /** When the current gap began (first failed probe after the last answer), else null. */
  downSince: number | null;
  /** When the app started waiting for the first answer. */
  startedAt: number;
}

export interface ServiceView {
  status: ServiceStatus;
  /** Show the banner / chip (never before the quiet period is over). */
  bannerVisible: boolean;
  /** Dialogs that need the service disable their save / test buttons. */
  blocked: boolean;
  /** Show the owner's "Restart service" button. */
  canRestart: boolean;
}

export function initialServiceMemory(now: number): ServiceMemory {
  // Until the first answer the service is "starting": the gap begins now.
  return { everReady: false, downSince: now, startedAt: now };
}

/** Record one probe result. */
export function observeService(mem: ServiceMemory, up: boolean, now: number): ServiceMemory {
  if (up) {
    return mem.everReady && mem.downSince === null ? mem : { ...mem, everReady: true, downSince: null };
  }
  if (mem.downSince !== null) {
    return mem;
  }
  return { ...mem, downSince: mem.everReady ? now : mem.startedAt };
}

/** What the person sees at `now`. */
export function serviceView(mem: ServiceMemory, now: number): ServiceView {
  if (mem.downSince === null) {
    return { status: "ready", bannerVisible: false, blocked: false, canRestart: false };
  }
  const down = Math.max(0, now - mem.downSince);
  if (down < SERVICE_QUIET_MS) {
    // Within the quiet period: a brief restart is not worth a word.
    return { status: mem.everReady ? "ready" : "starting", bannerVisible: false, blocked: !mem.everReady, canRestart: false };
  }
  if (down >= SERVICE_FAILED_MS) {
    return { status: "failed", bannerVisible: true, blocked: true, canRestart: true };
  }
  return { status: mem.everReady ? "reconnecting" : "starting", bannerVisible: true, blocked: true, canRestart: false };
}
