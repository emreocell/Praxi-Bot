/**
 * The computer card's view states (design v7). The card never opens a live session by itself:
 * the view is idle until the owner asks for it, runs while they look, and pauses again after a
 * quiet spell. These are the states the card is built against; `state/useComputerView.ts` is the
 * one place that decides them from the watch session.
 */
import type { TargetsStatus } from "./computerLabel.ts";
import type { ServerScreenIssue } from "./botComputer.ts";

export type ComputerViewKind = "idle" | "starting" | "live" | "paused" | "unavailable" | "timeout";

/** Why a view is `paused` (design: "3 dakikadır bakılmadığı için duraklatıldı"). */
export type PausedReason = "idle";

/** Why a view is `unavailable`: a fixed set the card words in the user's language. */
export type UnavailableReason =
  | "connecting"
  | "offline"
  | "notPaired"
  | "version"
  | "permission"
  | "failed"
  | "timeout"
  | { streamError: string }
  | { targetReason: string }
  | { noScreen: string };

export interface ComputerViewModel {
  kind: ComputerViewKind;
  paused: PausedReason | null;
  unavailable: UnavailableReason | null;
  /** The current frame (blob URL) while `live`. */
  frameUrl: string | null;
  previewUrl: string | null;
  /** `1280×800` from `source_rect` once a frame is known; stable across view sizes. */
  resolution: string | null;
  /** Frames per second, measured; `null` until measured. */
  fps: number | null;
  /** When the view went live (epoch ms) for the overlay timer. */
  liveSinceMs: number | null;
}

export interface ComputerViewActions {
  /** "Ekranı göster" / "Devam et" / "Yeniden dene". */
  start: () => void;
  /** "Kapat": ends the view. */
  stop: () => void;
  /** "Büyüt": the large viewer. */
  bigView: () => void;
  /** "Kontrolü al": the large viewer, then the takeover request. */
  takeControl: () => void;
  /** The "Tarayıcı" link: the large viewer on the bot's browser. */
  openBrowser: () => void;
  /** Any pointer / key use of the card: resets the quiet-time counter. */
  touch: () => void;
}

export type ComputerView = ComputerViewModel & ComputerViewActions;

/** No interaction for this long ends a view (the design says three minutes). */
export const IDLE_PAUSE_MS = 3 * 60 * 1000;
export const SCREEN_START_TIMEOUT_MS = 6_000;

export interface ViewInputs {
  /** The owner pressed start and has not stopped it. */
  wanted: boolean;
  /** The last view ended because nobody interacted. */
  pausedIdle: boolean;
  /** The computer cannot be asked at all (server connecting / offline ...). */
  issue: ServerScreenIssue;
  targetsStatus: TargetsStatus;
  hasTarget: boolean;
  /** The target exists but cannot be viewed. */
  targetUnavailable: string | null;
  startFailure: UnavailableReason | null;
  /** Code of the host's "no screen" note, if any. */
  noScreen: string | null;
  /** The screen list error, when discovery failed. */
  targetsError: string | null;
  hasFrame: boolean;
}

export interface DerivedView {
  kind: ComputerViewKind;
  unavailable: UnavailableReason | null;
  paused: PausedReason | null;
}

/** What the card shows, from the facts. Pure. */
export function deriveView(i: ViewInputs): DerivedView {
  const out = (
    kind: ComputerViewKind,
    unavailable: UnavailableReason | null = null,
    paused: PausedReason | null = null
  ): DerivedView => ({ kind, unavailable, paused });
  if (i.issue && i.issue !== "connecting") {
    return out("unavailable", i.issue);
  }
  if (i.issue === "connecting" && i.wanted) {
    return out("unavailable", "connecting");
  }
  if (i.wanted) {
    if (i.startFailure) {
      return out(i.startFailure === "timeout" ? "timeout" : "unavailable", i.startFailure);
    }
    if (i.targetsStatus === "failed") {
      const code = i.targetsError;
      return out("unavailable", code === "forbidden" ? "permission" : code ? { streamError: code } : "failed");
    }
    if (i.targetUnavailable) {
      return out("unavailable", { targetReason: i.targetUnavailable });
    }
    if (i.targetsStatus === "ok" && !i.hasTarget) {
      return out("unavailable", i.noScreen ? { noScreen: i.noScreen } : "failed");
    }
    return i.hasFrame ? out("live") : out("starting");
  }
  return i.pausedIdle ? out("paused", null, "idle") : out("idle");
}

/** "0:42" for the overlay timer. */
export function formatLive(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/** Frames per second over the last `windowMs`, from frame arrival times. */
export function measureFps(arrivals: readonly number[], now: number, windowMs = 3000): number | null {
  const recent = arrivals.filter((t) => now - t <= windowMs);
  if (recent.length < 2) {
    return null;
  }
  return Math.round(recent.length / (windowMs / 1000));
}

/** Whether the card's status line shows the computer as reachable. */
export function reachable(unavailable: UnavailableReason | null): boolean {
  return unavailable !== "offline" && unavailable !== "notPaired" && unavailable !== "version";
}
