import type {
  ControlModeName,
  ControlStateDto,
  ControlVerdict,
  DaemonEvent,
  GlobalEnvelope,
  ReleaseRequest,
  TakeoverInfo,
  TargetInfo,
  ViewReason
} from "../api/types.ts";

/**
 * The app side of the daemon <-> app control bridge.
 *
 * Bot desktop actions run inside the daemon; the native overlay (local desktop
 * targets only) and panel overlay (browser / remote targets) mirror and gate
 * them here:
 *
 * - `control_requested` events:
 *   - known local desktop target -> `native.beginAction` + `native.click` + verdict;
 *   - browser / remote target -> panel-local overlay highlight + verdict
 *     (remote screen coordinates NEVER touch local desktop monitors or mouse);
 * - run lifecycle: a local-desktop run starts -> `bot_driving`; the last run ends -> `idle`;
 * - legacy local control changes report via `PUT /v1/control/state`; plain
 *   heartbeats and reopens NEVER send `release: true`;
 * - while a daemon-owned takeover holds the latch, only `POST /v1/control/release`
 *   ("Bitirdim"), `POST /v1/control/override` (Stop) or `POST /v1/control/recover`
 *   touch the latch — heartbeats/reopen never clear it;
 * - transitioning out of `human_driving -> idle` ("Bitirdim" hand-back) NEVER
 *   cancels live runs; only explicit Stop from `bot_driving` / `paused` or
 *   `emergencyStop()` cancels live runs.
 */

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type TargetScope = "local_desktop" | "browser" | "remote";

export type ComputerTargetSetting = "this-pc" | "browser" | "remote" | "off";

/**
 * Determine whether a run / control_requested action targets the local PC
 * monitor (`local_desktop`) vs a browser or remote host (`browser` / `remote`).
 * Remote and browser coordinates MUST NEVER drive the local desktop overlay.
 */
export function resolveTargetScope(params: {
  runId?: string;
  botId?: string;
  targets: readonly TargetInfo[];
  selectedTarget: TargetInfo | null;
  computerTarget: ComputerTargetSetting;
}): TargetScope {
  const { runId, botId, targets, selectedTarget, computerTarget } = params;
  const matched =
    targets.find((t) => runId && t.run_id === runId) ??
    targets.find((t) => botId && t.bot_id === botId) ??
    (selectedTarget?.run_id && selectedTarget.run_id === runId
      ? selectedTarget
      : null);

  if (matched) {
    if (matched.kind === "browser") {
      return "browser";
    }
    if (
      matched.unavailable_reason === "remote_node_live_view" ||
      (matched.host_id !== "local" && computerTarget === "remote") ||
      matched.host_id.startsWith("peer:")
    ) {
      return "remote";
    }
    return "local_desktop";
  }

  if (computerTarget === "browser" || computerTarget === "off") {
    return "browser";
  }
  if (computerTarget === "remote") {
    return "remote";
  }
  return "local_desktop";
}

export interface PanelActionHighlight {
  runId: string;
  botId: string;
  requestId: string;
  rect: Rect | null;
  label: string;
  action: string;
  timestampMs: number;
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
    putControlState(mode: ControlModeName, release?: boolean): Promise<unknown>;
    decideControl(
      requestId: string,
      decision: ControlVerdict,
      reason?: string
    ): Promise<unknown>;
    cancelRun(runId: string): Promise<unknown>;
    getControlState?(): Promise<ControlStateDto>;
    releaseControl?(req?: ReleaseRequest): Promise<ControlStateDto>;
    overrideControl?(): Promise<ControlStateDto>;
    recoverControl?(): Promise<ControlStateDto>;
  };
  native: {
    beginAction(rect: Rect | null, label: string, action: string): Promise<boolean>;
    startRun(bot: BotMeta, step: string): Promise<void>;
    stopRun(): Promise<void>;
    click(x: number, y: number): Promise<void>;
  };
  /** Resolve whether a run / bot currently targets the local PC or a browser/remote screen. */
  resolveTargetScope?: (info: { runId?: string; botId?: string }) => TargetScope;
  /** Optional hook to render non-local action highlights inside the live-view panel. */
  panelOverlay?: {
    showAction(highlight: PanelActionHighlight): void;
  };
  botMeta(botId: string): BotMeta;
  startingStep: string;
  now(): number;
}

export interface TrackedTakeover {
  id?: string;
  state: string;
  owner: string;
  targetId: string;
  reason?: ViewReason | null;
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
      return "The user stopped this desktop action, so this action was not performed. Do not retry it; ask the user how to continue.";
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
  private activeTakeover: TrackedTakeover | null = null;
  private releasingTakeover = false;
  /** Id of the takeover THIS app owns (set by the live-view controller). */
  private ownedTakeoverId: string | null = null;

  constructor(deps: BridgeDeps) {
    this.deps = deps;
    this.startedAt = deps.now();
  }

  /** Run ids the bridge believes are live (for tests / diagnostics). */
  liveRuns(): string[] {
    return [...this.active];
  }

  /** Currently tracked daemon takeover (owned or peer), if any. */
  takeover(): TrackedTakeover | null {
    return this.activeTakeover;
  }

  /** Tell the bridge which takeover this app owns; only that one blocks legacy release. */
  setOwnedTakeoverId(id: string | null): void {
    this.ownedTakeoverId = id;
  }

  /** Re-read `GET /v1/control/state` (events are notifications only and are not replayed). */
  refreshControlState(): Promise<void> {
    const get = this.deps.client.getControlState;
    if (!get) {
      return this.chain;
    }
    this.chain = this.chain
      .then(async () => {
        this.applyControlDto(await get.call(this.deps.client));
      })
      .catch(() => undefined);
    return this.chain;
  }

  /** Sync authoritative daemon control snapshot into the bridge. */
  syncControlState(dto: ControlStateDto): Promise<void> {
    this.chain = this.chain
      .then(() => {
        this.applyControlDto(dto);
      })
      .catch(() => undefined);
    return this.chain;
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

  /**
   * Explicitly hand back an owned takeover ("Bitirdim") without cancelling live
   * runs. A failed release rejects (the caller decides what the daemon state
   * means); the internal serialisation chain stays usable.
   */
  releaseOwnedTakeover(req: ReleaseRequest): Promise<ControlStateDto | null> {
    const run = this.chain.then(async () => {
      if (!this.deps.client.releaseControl) {
        return null;
      }
      this.releasingTakeover = true;
      try {
        const result = await this.deps.client.releaseControl(req);
        this.applyControlDto(result);
        return result;
      } finally {
        this.releasingTakeover = false;
      }
    });
    this.chain = run.then(
      () => undefined,
      () => undefined
    );
    return run;
  }

  /** Explicit user Stop override: pauses any active takeover and cancels live runs. */
  emergencyStop(): Promise<ControlStateDto | null> {
    let result: ControlStateDto | null = null;
    this.chain = this.chain
      .then(async () => {
        if (this.deps.client.overrideControl) {
          try {
            result = await this.deps.client.overrideControl();
            this.applyControlDto(result);
          } catch {
            // fall through to local stop / cancel
          }
        }
        await this.deps.native.stopRun().catch(() => undefined);
        this.mode = "idle";
        await this.cancelActiveRuns();
      })
      .catch(() => undefined);
    return this.chain.then(() => result);
  }

  /** Send the current mode (call every `HEARTBEAT_MS` and on connect). */
  async heartbeat(release = false): Promise<void> {
    // Heartbeats and reopens never release an owned takeover latch.
    const effectiveRelease = release && !this.hasOwnedTakeover();
    try {
      await this.deps.client.putControlState(
        this.mode,
        effectiveRelease || undefined
      );
    } catch {
      // daemon unreachable or 409 because owned takeover holds the latch
    }
  }

  private applyTakeoverInfo(t: TakeoverInfo | null | undefined): void {
    if (!t) {
      this.activeTakeover = null;
      return;
    }
    this.activeTakeover = {
      id: t.id,
      state: t.state,
      owner: t.owner,
      targetId: t.target_id,
      reason: t.reason ?? null
    };
  }

  private applyControlDto(dto: ControlStateDto): void {
    this.applyTakeoverInfo(dto.takeover);
  }

  /** True only for this app's own live takeover, never a peer's or a stale one. */
  private hasOwnedTakeover(): boolean {
    if (this.releasingTakeover) {
      return true;
    }
    const t = this.activeTakeover;
    return (
      t !== null &&
      this.ownedTakeoverId !== null &&
      t.id === this.ownedTakeoverId &&
      (t.state === "active" || t.state === "acquiring")
    );
  }

  private scopeFor(info: { runId?: string; botId?: string }): TargetScope {
    return this.deps.resolveTargetScope?.(info) ?? "local_desktop";
  }

  private async cancelActiveRuns(): Promise<void> {
    const runs = [...this.active];
    this.active.clear();
    await Promise.all(
      runs.map((id) => this.deps.client.cancelRun(id).catch(() => undefined))
    );
  }

  private async nativeState(mode: ControlModeName): Promise<void> {
    const previous = this.mode;
    this.mode = mode;

    // While an owned takeover controls the daemon latch, a local Pause overrides
    // instead of fighting the latch, and leaving human_driving never sends legacy release:true.
    if (mode === "paused" && this.hasOwnedTakeover() && this.deps.client.overrideControl) {
      try {
        const dto = await this.deps.client.overrideControl();
        this.applyControlDto(dto);
      } catch {
        // next beat / state check retries
      }
    } else {
      const resumed =
        !this.hasOwnedTakeover() &&
        (previous === "human_driving" || previous === "paused") &&
        (mode === "idle" || mode === "bot_driving");
      await this.heartbeat(resumed);
    }

    // Leaving `human_driving -> idle` is a hand-back ("Bitirdim"), never Stop.
    // Only Stop (from `bot_driving` or `paused` when not ending an owned takeover) cancels runs.
    if (
      mode === "idle" &&
      !this.releasingTakeover &&
      (previous === "bot_driving" || previous === "paused")
    ) {
      await this.cancelActiveRuns();
    }
  }

  private fresh(env: GlobalEnvelope): boolean {
    if (env.event.type === "control_state_changed") {
      return true;
    }
    return env.ts_ms >= this.startedAt - REPLAY_SLACK_MS;
  }

  private async ensureDriving(botId: string, runId?: string): Promise<void> {
    if (this.scopeFor({ botId, runId }) !== "local_desktop") {
      return;
    }
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
      case "control_state_changed":
        if (e.state === "released" || e.state === "recovered") {
          this.activeTakeover = null;
        } else {
          // The event never carries a takeover id: keep a known id for the same
          // actor/target and let the snapshot below fill in the rest.
          const prev = this.activeTakeover;
          const same =
            prev !== null && prev.owner === e.owner && prev.targetId === e.target_id;
          this.activeTakeover = {
            id: same ? prev.id : undefined,
            state: e.state,
            owner: e.owner,
            targetId: e.target_id,
            reason: e.reason ?? null
          };
        }
        if (this.deps.client.getControlState) {
          try {
            this.applyControlDto(await this.deps.client.getControlState());
          } catch {
            // the next refresh / heartbeat re-syncs
          }
        }
        return;
      case "run_started":
        this.active.add(e.run_id);
        await this.ensureDriving(e.bot_id, e.run_id);
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
        await this.answer(e);
        return;
      default:
        return;
    }
  }

  private async answer(
    e: Extract<DaemonEvent, { type: "control_requested" }>
  ): Promise<void> {
    const scope = this.scopeFor({ runId: e.run_id, botId: e.bot_id });
    if (scope !== "local_desktop") {
      // Browser and remote targets MUST NEVER highlight or click the local PC desktop.
      const takeoverBlocks =
        this.activeTakeover !== null &&
        (this.activeTakeover.state === "active" ||
          this.activeTakeover.state === "acquiring" ||
          this.activeTakeover.state === "paused");
      const proceed =
        this.mode !== "human_driving" &&
        this.mode !== "paused" &&
        !takeoverBlocks;
      if (proceed) {
        this.deps.panelOverlay?.showAction({
          runId: e.run_id,
          botId: e.bot_id,
          requestId: e.request_id,
          rect: e.rect,
          label: e.label,
          action: e.action,
          timestampMs: this.deps.now()
        });
      }
      const blockedMode: ControlModeName =
        this.activeTakeover?.state === "paused" || this.mode === "paused"
          ? "paused"
          : "human_driving";
      try {
        await this.deps.client.decideControl(
          e.request_id,
          proceed ? "proceed" : "refuse",
          proceed ? undefined : refusalReason(blockedMode)
        );
      } catch {
        // 404: the daemon already timed out; nothing to do
      }
      return;
    }

    await this.ensureDriving(e.bot_id, e.run_id);
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

/**
 * Owner of the single `ControlBridge` for the live-view controller. Its
 * identity never changes, so control-state refreshes only *sync* the existing
 * bridge (its live-run set, `startedAt` and mode must survive them).
 */
export interface BridgeRegistry {
  register(bridge: ControlBridge | null): void;
  current(): ControlBridge | null;
  syncControlState(dto: ControlStateDto): void;
  setOwnedTakeoverId(id: string | null): void;
}

export function createBridgeRegistry(
  getControlState: () => ControlStateDto | null
): BridgeRegistry {
  let bridge: ControlBridge | null = null;
  let ownedId: string | null = null;
  return {
    register(next) {
      bridge = next;
      if (next) {
        next.setOwnedTakeoverId(ownedId);
        const dto = getControlState();
        if (dto) {
          void next.syncControlState(dto);
        }
      }
    },
    current: () => bridge,
    syncControlState(dto) {
      void bridge?.syncControlState(dto);
    },
    setOwnedTakeoverId(id) {
      ownedId = id;
      bridge?.setOwnedTakeoverId(id);
    }
  };
}
