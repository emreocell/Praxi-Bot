/**
 * What the single run bar shows (244-D). The chat has ONE run-control surface:
 * this model decides whether it exists and which controls it offers. It only
 * chooses the presentation; Stop itself stays in state/{appStop,runLedger,...}.
 */
import type { ControlMode } from "../overlay/events.ts";

export type ChatRunPhase = "none" | "live" | "waiting_approval" | "paused";

export type RunBarKind = "running" | "waiting" | "paused" | "human";

export interface RunBarModel {
  kind: RunBarKind;
  /** The run is being driven on the desktop (native control is active). */
  native: boolean;
  /** Pause the desktop run (native only). */
  canPause: boolean;
  /** Resume: the native run, or a paused daemon run. */
  canResume: boolean;
  /** "Take the wheel" / "Give back" (native only). */
  canWheel: boolean;
  /** "Open big view" (the live screen exists only for desktop control). */
  canBigView: boolean;
}

export function runBarModel(input: {
  chat: ChatRunPhase;
  native: ControlMode;
}): RunBarModel | null {
  const native = input.native !== "idle";
  if (!native && input.chat === "none") {
    return null;
  }
  const kind: RunBarKind =
    input.native === "human_driving"
      ? "human"
      : input.native === "paused" || (!native && input.chat === "paused")
        ? "paused"
        : input.chat === "waiting_approval"
          ? "waiting"
          : "running";
  return {
    kind,
    native,
    canPause: input.native === "bot_driving",
    canResume: input.native === "paused" || (!native && input.chat === "paused"),
    canWheel: native,
    canBigView: native
  };
}

/** The bar exists while a run is active or a Stop still has something to say. */
export function runBarVisible(
  model: RunBarModel | null,
  hasStopStatus: boolean
): boolean {
  return model !== null || hasStopStatus;
}

/** "0:07", "12:30", "1:02:03". */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/**
 * How the run ledger's own state for the chat's run reads in the bar. A Stop of one
 * chat run (the bar's Stop) is a "single" decision, which the app-wide Stop status
 * does not describe, so the bar reads the run's phase directly (read-only).
 */
export function singleRunStop(
  phase: "tracked" | "held" | "cancel_requested" | "failed" | "stopped" | undefined
): "stopping" | "failed" | null {
  if (phase === "held" || phase === "cancel_requested") {
    return "stopping";
  }
  return phase === "failed" ? "failed" : null;
}

/** The daemon has not confirmed a Stop after this long: say so and offer the hard stop. */
export const STOP_CONFIRM_MS = 5000;

export interface RunStopUi {
  /** `unconfirmed`: Stop was sent, the daemon has not said the run ended for `STOP_CONFIRM_MS`. */
  phase: "stopping" | "unconfirmed" | "failed";
  error: string | null;
  /** The last attempt did not reach the daemon (link down): the Stop is being retried. */
  unreachable: boolean;
}

/**
 * What the chat shows about the Stop of its run: nothing, "Stopping…", "not confirmed
 * by the daemon yet" (after 5 s, with the Force stop action) or a failed cancel. The
 * end state itself is never assumed here: the run leaves this state only when the
 * daemon reports it terminal (ledger phase `stopped`, `run_finished` or the run list).
 */
export function runStopUi(p: {
  ledgerPhase: Parameters<typeof singleRunStop>[0];
  /** Stop pressed before the daemon named the run (waiting for the send to be acknowledged). */
  pending: boolean;
  /** Monotonic time of the Stop decision, when the ledger holds one. */
  decidedMono: number | null;
  /** Same clock as `decidedMono`. */
  now: number;
  error: string | null;
  /** The Stop's last request failed in transit (no daemon answer). */
  unreachable?: boolean;
}): RunStopUi | null {
  const unreachable = p.unreachable === true;
  const phase = singleRunStop(p.ledgerPhase);
  if (phase === "failed") {
    return { phase: "failed", error: p.error, unreachable };
  }
  if (phase === "stopping") {
    const waited = p.decidedMono === null ? 0 : p.now - p.decidedMono;
    // A link that is down is said at once; a slow daemon after STOP_CONFIRM_MS.
    const late = unreachable || waited >= STOP_CONFIRM_MS;
    return { phase: late ? "unconfirmed" : "stopping", error: null, unreachable };
  }
  return p.pending ? { phase: "stopping", error: null, unreachable: false } : null;
}
