import type {
  ControlModeName,
  ControlVerdict,
  DaemonEvent,
  GlobalEnvelope
} from "../api/types.ts";

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
 *   `PUT /v1/control/state`, which also registers the app as controller;
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
  botAvatar?: string;
  botColor?: string;
  botInitials?: string;
}

export interface BridgeDeps {
  client: {
    putControlState(mode: ControlModeName): Promise<unknown>;
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
    stopRun(): Promise<void>;
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

  constructor(deps: BridgeDeps) {
    this.deps = deps;
    this.startedAt = deps.now();
  }

  /** Run ids the bridge believes are live (for tests / diagnostics). */
  liveRuns(): string[] {
    return [...this.active];
  }

  /** Feed one daemon event; work is serialised in arrival order. */
  handleEvent(env: GlobalEnvelope): Promise<void> {
    this.chain = this.chain
      .then(() => this.process(env))
      .catch(() => undefined);
    return this.chain;
  }

  /** The native state machine changed (Tauri `control:state_changed`). */
  onNativeState(mode: ControlModeName): Promise<void> {
    this.chain = this.chain
      .then(() => this.nativeState(mode))
      .catch(() => undefined);
    return this.chain;
  }

  /** Send the current mode (call every `HEARTBEAT_MS` and on connect). */
  async heartbeat(): Promise<void> {
    try {
      await this.deps.client.putControlState(this.mode);
    } catch {
      // daemon unreachable: the next beat retries
    }
  }

  private async nativeState(mode: ControlModeName): Promise<void> {
    const previous = this.mode;
    this.mode = mode;
    await this.heartbeat();
    if (mode === "idle" && previous !== "idle") {
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

  private async ensureDriving(botId: string): Promise<void> {
    if (this.mode === "idle") {
      await this.deps.native.startRun(
        this.deps.botMeta(botId),
        this.deps.startingStep
      );
      // The state machine reports bot_driving through `onNativeState`; do not
      // wait for it to decide this request.
      this.mode = "bot_driving";
    }
  }

  private async process(env: GlobalEnvelope): Promise<void> {
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
          await this.deps.native.stopRun();
          this.mode = "idle";
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
    if (proceed && e.rect && e.action === "click") {
      const c = centre(e.rect);
      await this.deps.native.click(c.x, c.y).catch(() => undefined);
    }
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
