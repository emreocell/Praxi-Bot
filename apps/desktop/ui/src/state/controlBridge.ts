import type {
  ControlModeName,
  ControlVerdict,
  DaemonEvent,
  GlobalEnvelope,
  Run
} from "../api/types.ts";
import { isCancellableRunState, isProcessingRunState } from "../lib/runState.ts";

/**
 * The app side of the daemon <-> app control bridge.
 *
 * Bot desktop actions run inside the daemon, the on-screen overlay and the
 * "human has the wheel" state live here in the app.  This module is pure (all
 * effects go through `BridgeDeps`) so it can be unit tested:
 *
 * - `control_requested` events: show the target (`native.beginAction`, ~300 ms
 *   highlight, refuses while the human drives) and post the decision;
 * - run lifecycle: a run starts -> `bot_driving`; the last run ends -> idle;
 * - every control state change (and a 5 s heartbeat) is reported with
 *   `PUT /v1/control/state`, which also registers the app as controller.
 *   The daemon latches `paused` / `human_driving`; only the user's explicit
 *   resume (native state leaves one of them) sends `release: true`.  Heartbeats
 *   and app (re)opens never do, so a restarted UI cannot un-pause the bot;
 * - Stop (widget button, hotkey): the state machine goes idle while runs are
 *   live -> `POST /v1/runs/{id}/cancel` for each.
 */

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BotMeta {
  botId: string;
  botName: string;
  botShape?: string;
  botAvatar?: string;
  botColor?: string;
  botInitials?: string;
}

export interface BridgeDeps {
  client: {
    listRuns(): Promise<Pick<Run, "id" | "bot_id" | "state">[]>;
    putControlState(mode: ControlModeName, release?: boolean): Promise<unknown>;
    decideControl(
      requestId: string,
      decision: ControlVerdict,
      reason?: string
    ): Promise<unknown>;
    cancelRun(runId: string): Promise<unknown>;
  };
  native: {
    /** `rect: null` = no target to highlight (key press, launch). */
    beginAction(
      rect: Rect | null,
      label: string,
      action: string
    ): Promise<boolean>;
    startRun(bot: BotMeta, step: string): Promise<void>;
    /** Returns the generation emitted by this programmatic native idle transition. */
    stopRun(): Promise<number>;
    click(x: number, y: number): Promise<void>;
  };
  botMeta(botId: string): BotMeta;
  /** Text of the widget while a run starts (localised). */
  startingStep: string;
  now(): number;
}

/** Heartbeat period; the daemon expires a registration after 15 s. */
export const HEARTBEAT_MS = 5000;
/** A `control_requested` older than this was already given up on by the daemon. */
export const STALE_REQUEST_MS = 15_000;
/** Events older than the bridge (a replay of history) are ignored, minus skew. */
const REPLAY_SLACK_MS = 2000;

export function refusalReason(mode: ControlModeName): string {
  switch (mode) {
    case "human_driving":
      return "The user has taken control of the desktop, so this action was not performed. Do not retry it; ask the user to give control back.";
    case "paused":
      return "The user has paused desktop control for this bot, so this action was not performed. Do not retry it; ask the user to resume.";
    default:
      return "The user stopped this desktop action, so it was not performed. Do not retry it; ask the user how to continue.";
  }
}

function centre(r: Rect): { x: number; y: number } {
  return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
}

export class ControlBridge {
  mode: ControlModeName = "idle";
  private readonly active = new Set<string>();
  private readonly handled = new Set<string>();
  private readonly startedAt: number;
  private readonly deps: BridgeDeps;
  private chain: Promise<void> = Promise.resolve();
  private disposed = false;
  private restored = false;
  private restoring: Promise<void> | null = null;
  private snapshot: { finished: Set<string> } | null = null;
  private stopRequested = false;
  private automaticIdleGeneration: number | null = null;

  constructor(deps: BridgeDeps) {
    this.deps = deps;
    this.startedAt = deps.now();
  }

  /** Run ids the bridge believes are live (for tests / diagnostics). */
  liveRuns(): string[] {
    return [...this.active];
  }

  /** Retire the old daemon connection before a replacement bridge can act. */
  dispose(): void {
    this.disposed = true;
    this.active.clear();
    this.handled.clear();
  }

  /** Initial native state is a snapshot, never an explicit Stop or resume. */
  initializeNativeState(mode: ControlModeName): Promise<void> {
    this.chain = this.chain.then(async () => {
      if (this.disposed) return;
      if (mode !== "idle" || this.mode === "idle") this.mode = mode;
      await this.heartbeat();
    }).catch(() => undefined);
    return this.chain;
  }

  /** Subscribe to events first, then restore live runs that predate this UI. */
  restoreRuns(): Promise<void> {
    if (this.disposed || this.restored) return Promise.resolve();
    if (this.restoring) return this.restoring;
    const snapshot = { finished: new Set<string>() };
    this.snapshot = snapshot;
    this.restoring = (async () => {
      try {
        const rows = await this.deps.client.listRuns();
        this.chain = this.chain.then(async () => {
          if (this.disposed) return;
          const live = rows.filter((run) =>
            isCancellableRunState(run.state) && !snapshot.finished.has(run.id)
          );
          if (this.stopRequested) {
            await Promise.all(live.map((run) => this.deps.client.cancelRun(run.id).catch(() => undefined)));
          } else {
            for (const run of live) this.active.add(run.id);
            const processing = live.find((run) => isProcessingRunState(run.state));
            if (processing) {
              await this.ensureDriving(processing.bot_id);
            } else if (this.active.size === 0 && this.mode === "bot_driving") {
              await this.stopNativeRun();
            }
          }
          this.restored = true;
          this.stopRequested = false;
        }).catch(() => undefined);
        await this.chain;
      } catch {
        // A failed initial read is retried by the next heartbeat.
      } finally {
        this.snapshot = null;
        this.restoring = null;
      }
    })();
    return this.restoring;
  }

  /** Feed one daemon event; work is serialised in arrival order. */
  handleEvent(env: GlobalEnvelope): Promise<void> {
    if (this.disposed) return Promise.resolve();
    if (env.event.type === "run_finished" && this.fresh(env)) {
      this.snapshot?.finished.add(env.event.run_id);
    }
    this.chain = this.chain
      .then(() => this.process(env))
      .catch(() => undefined);
    return this.chain;
  }

  /** The native state machine changed (Tauri `control:state_changed`). */
  onNativeState(mode: ControlModeName, generation?: number): Promise<void> {
    if (this.disposed) return Promise.resolve();
    this.chain = this.chain
      .then(() => this.nativeState(mode, generation))
      .catch(() => undefined);
    return this.chain;
  }

  /** Send the current mode (call every `HEARTBEAT_MS` and on connect). */
  async heartbeat(release = false): Promise<void> {
    if (this.disposed) return;
    try {
      await this.deps.client.putControlState(this.mode, release || undefined);
    } catch {
      // daemon unreachable: the next beat retries
    }
  }

  private async nativeState(mode: ControlModeName, generation?: number): Promise<void> {
    if (this.disposed) return;
    if (mode === "idle" && generation !== undefined && generation === this.automaticIdleGeneration) {
      // Finishing the final known run is not an explicit Stop for runs still
      // being discovered. Native Stop All always emits a new generation.
      await this.heartbeat();
      return;
    }
    const previous = this.mode;
    this.mode = mode;
    // Explicit user resume: the human gave control back / ended the pause.
    const resumed =
      (previous === "human_driving" || previous === "paused") &&
      (mode === "idle" || mode === "bot_driving");
    if (mode === "idle") {
      // This is a native event, not initializeNativeState's initial snapshot.
      // Keep the user's Stop even when already idle or a snapshot read fails.
      this.stopRequested = true;
      for (const id of this.active) this.snapshot?.finished.add(id);
    }
    await this.heartbeat(resumed);
    if (this.disposed) return;
    if (mode === "idle") {
      // Stop (widget button, hotkey, menu): cancel what the bot is running.
      const runs = [...this.active];
      this.active.clear();
      await Promise.all(
        runs.map((id) => this.deps.client.cancelRun(id).catch(() => undefined))
      );
    }
  }

  private fresh(env: GlobalEnvelope): boolean {
    return env.ts_ms >= this.startedAt - REPLAY_SLACK_MS;
  }

  private async stopNativeRun(): Promise<void> {
    this.automaticIdleGeneration = await this.deps.native.stopRun();
    if (!this.disposed) this.mode = "idle";
  }

  private async ensureDriving(botId: string): Promise<void> {
    if (!this.disposed && this.mode === "idle") {
      await this.deps.native.startRun(
        this.deps.botMeta(botId),
        this.deps.startingStep
      );
      // The state machine reports bot_driving through `onNativeState`; do not
      // wait for it to decide this request.
      if (!this.disposed) this.mode = "bot_driving";
    }
  }

  private async process(env: GlobalEnvelope): Promise<void> {
    if (this.disposed) return;
    const e: DaemonEvent = env.event;
    if (!this.fresh(env)) {
      return;
    }
    switch (e.type) {
      case "run_started":
        this.active.add(e.run_id);
        await this.ensureDriving(e.bot_id);
        return;
      case "run_finished":
        this.active.delete(e.run_id);
        if (this.active.size === 0 && this.mode !== "idle") {
          // `active` is empty, so the idle state this causes cancels nothing.
          await this.stopNativeRun();
        }
        return;
      case "control_requested":
        if (
          this.handled.has(e.request_id) ||
          this.deps.now() - env.ts_ms > STALE_REQUEST_MS
        ) {
          return;
        }
        this.handled.add(e.request_id);
        this.active.add(e.run_id);
        await this.ensureDriving(e.bot_id);
        await this.answer(e);
        return;
      default:
        return;
    }
  }

  private async answer(
    e: Extract<DaemonEvent, { type: "control_requested" }>
  ): Promise<void> {
    let proceed = false;
    try {
      proceed = await this.deps.native.beginAction(e.rect, e.label, e.action);
    } catch {
      proceed = false;
    }
    if (this.disposed) return;
    if (proceed && e.rect && e.action === "click") {
      const c = centre(e.rect);
      await this.deps.native.click(c.x, c.y).catch(() => undefined);
    }
    if (this.disposed) return;
    try {
      await this.deps.client.decideControl(
        e.request_id,
        proceed ? "proceed" : "refuse",
        proceed ? undefined : refusalReason(this.mode)
      );
    } catch {
      // 404: the daemon already timed out; nothing to do
    }
  }
}
