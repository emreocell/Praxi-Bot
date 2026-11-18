import type { ControlModeName, GlobalEnvelope } from "../api/types.ts";
import type {
  ClientHandle,
  ListedRun,
  RunLedger,
  StopOrigin,
  StopRecord,
  StopTransport
} from "./runLedger.ts";
import {
  type NativeControlSnapshot,
  overrideOptionsFor,
  stopTransport
} from "./stopHost.ts";

/**
 * Everything `ControlBridgeHost` does, without React (card 237 PR C, review r-94 #13).
 * The React host only forwards subscriptions, effects and timers to these methods,
 * so the tests drive the same wiring the app installs: native snapshots, daemon
 * clients, run restore, bridge lifecycle and every Stop entry point.
 */

/** A list request older than this no longer blocks a new one (ordering stays safe: tickets). */
export const LIST_STALE_MS = 10_000;

export interface RuntimeBridge {
  onNativeState(mode: ControlModeName, generation?: number): Promise<void>;
  restoreDriving(): Promise<void>;
  heartbeat(release?: boolean): Promise<void>;
  refreshControlState(): Promise<void>;
  handleEvent(env: GlobalEnvelope): Promise<void>;
  revoke(): void;
}

export interface RuntimeClient {
  stopRuns: StopTransport["stopRuns"];
  listRuns(): Promise<readonly ListedRun[]>;
}

export interface StopRuntimeDeps {
  ledger: RunLedger;
  /** Read the native control snapshot (`get_control_state`; a failure has no `stopSeq`). */
  readNative(): Promise<NativeControlSnapshot>;
  /** Monotonic clock (ms). */
  now(): number;
}

interface Session {
  readonly client: RuntimeClient;
  readonly handle: ClientHandle;
  readonly bridge: RuntimeBridge | null;
  listSince: number | null;
}

export class StopRuntime {
  readonly ledger: RunLedger;
  private readonly deps: StopRuntimeDeps;
  private session: Session | null = null;
  private override: (opts: { onlyIfControlled: boolean }) => void = () => {};

  constructor(deps: StopRuntimeDeps) {
    this.deps = deps;
    this.ledger = deps.ledger;
    // One decision, one takeover override (the viewer's always; others only if controlled).
    this.ledger.onStop((rec) => this.override(overrideOptionsFor(rec)));
    // The ledger is the single owner of the accepted native snapshot: whoever routed it
    // (an event, a read, the ledger's own Stop reply), the bridge hears every accepted one.
    this.ledger.onNativeAccepted((snap) => {
      const s = snap as NativeControlSnapshot;
      void this.session?.bridge?.onNativeState(s.mode, s.runGeneration);
    });
    // An unconfirmed Stop needs a run list issued after its expiry: ask for one now.
    this.ledger.onReconcileNeeded(() => this.resync());
  }

  /** Where the takeover half of a Stop goes (LiveViewContext's `overrideStop`). */
  setOverride(fn: (opts: { onlyIfControlled: boolean }) => void): void {
    this.override = fn;
  }

  // ── entry points ───────────────────────────────────────────────────────────

  /** Hotkey, run banner and viewer Stop (main window). */
  requestStop(origin: Exclude<StopOrigin, "native" | "chat">): StopRecord {
    return this.ledger.stopAll(origin);
  }

  /** Chat Stop / subagent cancel: one run, its own decision. */
  cancelOne(runId: string, opts?: { force?: boolean }): StopRecord {
    return this.ledger.cancelOne(runId, opts);
  }

  // ── native ─────────────────────────────────────────────────────────────────

  /**
   * Every native snapshot (event, command reply, read). The ledger sees it first
   * (Stops, acks); only an accepted newer revision is cached and given to the bridge.
   */
  onNativeSnapshot(snap: NativeControlSnapshot): boolean {
    return this.ledger.onNativeSnapshot(snap);
  }

  /** Initial / explicit native read. A failed read changes nothing. */
  async readNative(): Promise<void> {
    this.onNativeSnapshot(await this.deps.readNative());
  }

  /**
   * Last-moment check before a local `proceed`: a fresh read (routed like any other
   * snapshot, so a Stop it reveals is recorded) and native still bot driving.
   */
  async confirmDriving(): Promise<boolean> {
    const snap = await this.deps.readNative();
    if (snap.stopSeq === undefined) {
      return false; // could not confirm
    }
    this.onNativeSnapshot(snap);
    const newest = this.nativeSnapshot();
    return newest !== null && newest.mode === "bot_driving" && snap.mode === "bot_driving";
  }

  /** The newest native snapshot the ledger accepted (never a failed read, never older). */
  nativeSnapshot(): NativeControlSnapshot | null {
    return this.ledger.nativeSnapshot() as NativeControlSnapshot | null;
  }

  // ── daemon clients ─────────────────────────────────────────────────────────

  /**
   * A daemon client became current: pending Stops go first (ledger), the bridge
   * starts from the newest accepted native mode, runs are restored. Returns the
   * cleanup, which revokes the bridge synchronously.
   */
  attach(client: RuntimeClient, bridge: RuntimeBridge | null): () => void {
    const handle = this.ledger.attachClient(stopTransport(client));
    const session: Session = { client, handle, bridge, listSince: null };
    this.session = session;
    if (bridge) {
      const known = this.nativeSnapshot();
      if (known) {
        void bridge.onNativeState(known.mode, known.runGeneration);
      } else {
        void bridge.heartbeat();
      }
    }
    this.resync();
    return () => {
      bridge?.revoke();
      this.ledger.detachClient(handle);
      if (this.session === session) {
        this.session = null;
      }
    };
  }

  /** A daemon event (SSE). Without a bridge the ledger still follows runs. */
  onDaemonEvent(env: GlobalEnvelope): void {
    const session = this.session;
    if (session?.bridge) {
      void session.bridge.handleEvent(env);
      return;
    }
    if (env.event.type === "run_started") {
      this.ledger.observeRun(env.event.run_id, env.event.bot_id);
    } else if (env.event.type === "run_finished") {
      this.ledger.observeRunFinished(env.event.run_id);
    }
  }

  /** Re-read `GET /v1/runs` on the current client (one request at a time; ordered by ticket). */
  resync(): void {
    const session = this.session;
    if (!session || !session.handle.live) {
      return;
    }
    const now = this.deps.now();
    if (session.listSince !== null && now - session.listSince < LIST_STALE_MS) {
      return;
    }
    const ticket = this.ledger.beginList(session.handle);
    if (!ticket) {
      return;
    }
    session.listSince = now;
    session.client.listRuns().then(
      (list) => {
        if (session.listSince === now) {
          session.listSince = null;
        }
        this.ledger.applyList(ticket, list);
        if (session.bridge && session.handle.live && this.ledger.client() === session.handle) {
          void session.bridge.restoreDriving();
        }
      },
      () => {
        if (session.listSince === now) {
          session.listSince = null;
        }
      }
    );
  }

  /** The heartbeat tick (every `HEARTBEAT_MS`). */
  tick(): void {
    const session = this.session;
    if (!session) {
      return;
    }
    if (session.bridge) {
      void session.bridge.heartbeat();
      // events are notifications only and never replayed: re-read the snapshot
      void session.bridge.refreshControlState();
    }
    if (!session.handle.synced || this.ledger.needsReconcile()) {
      this.resync();
    }
  }

  /** The event stream connected (re-list) or dropped (wait for the next list). */
  eventsConnected(up: boolean): void {
    const session = this.session;
    if (!session) {
      return;
    }
    if (up) {
      this.resync();
    } else {
      this.ledger.markUnsynced(session.handle);
    }
  }

  /**
   * The target scope may have become known (settings loaded, this client's targets
   * listed): drive restored local runs that were waiting for it (review r-94 #10).
   */
  scopeChanged(): void {
    const session = this.session;
    if (session?.bridge && session.handle.live) {
      void session.bridge.restoreDriving();
    }
  }
}
