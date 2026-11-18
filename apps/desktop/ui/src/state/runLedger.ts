import type {
  StopOutcome,
  StopRunsInput,
  StopRunsResponse,
  StopScope
} from "../api/types.ts";

/**
 * The app's single owner of run lifecycle and Stop decisions.
 *
 * Pure TypeScript: no React, no globals. Time, ids, timers, native commands, the
 * daemon transport and storage are injected, so every rule below is unit tested.
 *
 * - A Stop is ONE user decision with ONE `stop_id`. The same id is the daemon
 *   `stop_id` and the native `stop_bot_run` token, and every retry reuses it.
 *   Different decisions always get different ids.
 * - The daemon decides which runs a Stop targets (`POST /v1/runs/stop` fixes the
 *   target set T at first receipt). Once the app knows T, every retry is `listed`
 *   with exactly T, so a restarted daemon can never re-sweep. No clock is ever
 *   compared with the daemon's.
 * - A run a Stop holds never acts again (`mayAct` is false) until it is terminal.
 *   A failed cancel is retried, never re-tracked. Every Stop also bumps a
 *   synchronous `stopEpoch` the bridge re-checks right before each local effect.
 * - Native tells the ledger a Stop happened (`stopSeq`). A sequence is attributed
 *   to a decision only by its token (event or command reply); untokened ones are
 *   presses of their own. `ack_stop` is sent only once every run a Stop targeted is
 *   POSITIVELY known to be terminal; unknown is never "stopped".
 * - Run lists are ordered by an observation counter: a list never reconciles a run
 *   the ledger learned about after the list was issued.
 * - Daemon clients come and go (`attachClient` / `detachClient`); Stop records
 *   outlive them and are sent again to the next client.
 */

/** A first attempt that has not reached the daemon by then is narrowed to the runs the app knew. */
export const STOP_SWEEP_WINDOW_MS = 2000;
/** A UI Stop and a native Stop this close together are the same key press. */
export const STOP_COALESCE_MS = 250;
/** One attempt of `POST /v1/runs/stop` is abandoned (and retried) after this. */
export const STOP_ATTEMPT_TIMEOUT_MS = 3000;
/** Retry delays; the last one repeats for as long as a targeted run is not terminal. */
export const STOP_RETRY_MS: readonly number[] = [500, 1000, 2000, 5000];
/** Request ids remembered so a replayed `control_requested` is never answered twice. */
const HANDLED_REQUESTS_MAX = 512;
/** Stop records kept after they settled (for dedupe across a webview reload). */
const SETTLED_RECORDS_MAX = 16;
/** Terminal observations remembered for list ordering. */
const FINISHED_MAX = 4096;
export const STOP_STORAGE_KEY = "praxi.stopLedger.v2";

/** Run states that are not terminal (daemon `RunState` minus `is_terminal`). */
export const OPEN_RUN_STATES: ReadonlySet<string> = new Set([
  "queued",
  "running",
  "waiting_approval",
  "paused"
]);
/** Open states in which the bot may act (`paused` stays distinct). */
export const PROCESSING_RUN_STATES: ReadonlySet<string> = new Set([
  "queued",
  "running",
  "waiting_approval"
]);

export function isOpenRunState(state: string | null | undefined): boolean {
  return state != null && OPEN_RUN_STATES.has(state);
}

export type StopOrigin = "native" | "hotkey" | "banner" | "viewer" | "chat";

/**
 * - `tracked`: live, may act (unless quarantined);
 * - `held`: a Stop targets it and no daemon answer has confirmed anything yet;
 * - `cancel_requested`: the daemon fired its cancel token, effects may still run;
 * - `failed`: the daemon could not cancel it (retried; still never acts);
 * - `stopped`: terminal (confirmed by the daemon, `run_finished` or the run list).
 */
export type RunPhase = "tracked" | "held" | "cancel_requested" | "failed" | "stopped";

export interface RunEntry {
  readonly id: string;
  botId: string | null;
  phase: RunPhase;
  /** Latest Stop that holds this run. */
  stopId: string | null;
  /**
   * The Stop whose target set this run may belong to but which the app could not
   * confirm (a sweep not answered yet, or one the daemon forgot). It cannot act
   * until that Stop answers or the user decides.
   */
  quarantinedBy: string | null;
  /** Observation counter value when the ledger first learned about the run. */
  seenObs: number;
  /** Daemon `processing` state (queued/running/waiting_approval), when known. */
  processing: boolean;
  error: string | null;
}

/**
 * - `queued`: no daemon client yet;
 * - `in_flight`: an attempt is outstanding;
 * - `retrying`: waiting for the next attempt (transport error, `failed` or still-live members);
 * - `settled`: every run it targeted is terminal;
 * - `expired`: the daemon forgot this id after it had told the app T; a successor
 *   (new id, `listed`, only the runs still held) carries on;
 * - `unconfirmed`: the daemon forgot this id before the app ever learned T. Runs that
 *   may be in T stay quarantined until a list proves them terminal or the user decides;
 *   it is never acknowledged on its own;
 * - `released`: the user let the quarantined runs of an unconfirmed Stop continue;
 * - `unconfirmable`: restored from a previous app run before the daemon ever answered and
 *   it knew no runs: the app cannot tell what it targeted, so it asks the user to press Stop again.
 */
export type StopPhase =
  | "queued"
  | "in_flight"
  | "retrying"
  | "settled"
  | "expired"
  | "unconfirmed"
  | "released"
  | "unconfirmable";

export interface StopRecord {
  readonly id: string;
  origin: StopOrigin;
  /** `all`: a user Stop of everything; `single`: one run cancelled from the chat. */
  readonly kind: "all" | "single";
  /** Hard stop: the daemon also aborts the run's task (a second press of Stop). */
  force?: boolean;
  /** Native stop sequences this decision accounts for (acked once it settles). */
  nativeSeqs: number[];
  /** `stop_bot_run(token = id)` was invoked and its sequence has not been attributed yet. */
  awaitingEcho: boolean;
  /** Runs the request names: K at decision, then exactly T once the daemon told us. */
  known: string[];
  /** Every run this Stop must see terminal: K, then T from the daemon. */
  members: string[];
  syncedAtDecision: boolean;
  decidedMono: number;
  scope: StopScope;
  phase: StopPhase;
  /** The daemon answered at least once (its target set is fixed and known to the app). */
  reached: boolean;
  firstSeen: boolean | null;
  /** `false` when the daemon could not write the audit row of an attempt. */
  audited: boolean;
  attempts: number;
  retryIndex: number;
  lastError: string | null;
  supersedes: string | null;
  successor: string | null;
  /** Unconfirmed only: new runs are quarantined until a list issued after this obs applies. */
  watchFromObs: number | null;
  /** Session that created it (native sequences only mean something within one). */
  session: string;
  /** Mono time it settled, for a short "stopped" confirmation. */
  settledMono: number | null;
}

export interface StopTransport {
  stopRuns(body: StopRunsInput, signal: AbortSignal): Promise<StopRunsResponse>;
}

/** The part of a native control snapshot the ledger reads. */
export interface NativeStopSnapshot {
  stateRev?: number;
  stopSeq?: number;
  lastStopToken?: string | null;
  stopAckedSeq?: number;
}

export interface NativeStopPort {
  stopRun(token: string): Promise<NativeStopSnapshot>;
  ackStop(seq: number): Promise<unknown>;
}

export interface LedgerClock {
  /** Monotonic milliseconds (`performance.now()`), only ever compared with itself. */
  now(): number;
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

export interface LedgerStorage {
  load(): string | null;
  save(value: string): void;
}

export interface LedgerDeps {
  newId(): string;
  clock: LedgerClock;
  native: NativeStopPort;
  /** Identifies this webview session (survives a reload, not an app restart). */
  session: string;
  storage?: LedgerStorage;
}

/** One daemon client as seen by the ledger. Revoked synchronously on detach. */
export interface ClientHandle {
  readonly epoch: number;
  readonly transport: StopTransport;
  live: boolean;
  /** A run list issued by THIS client after its last gap has been applied. */
  synced: boolean;
  /** Event-stream gaps seen on this client (a list issued before one cannot sync it). */
  gaps: number;
  listsIssued: number;
  listsApplied: number;
}

/** One `GET /v1/runs` request: applied only if it is the newest one applied so far. */
export interface ListTicket {
  readonly handle: ClientHandle;
  readonly seq: number;
  /** Observation counter when the request was issued. */
  readonly issuedObs: number;
  readonly gaps: number;
}

export interface ListedRun {
  id: string;
  state: string;
  bot_id?: string | null;
}

interface Persisted {
  v: 2;
  session: string;
  records: StopRecord[];
  runs: RunEntry[];
}

type Listener = () => void;

function outcomePhase(outcome: StopOutcome, state: string | null | undefined): RunPhase {
  switch (outcome) {
    case "cancelled":
    case "already_terminal":
    case "not_found":
      return "stopped";
    case "cancel_requested":
      // Effects have ceased only once the state is terminal.
      return state && !isOpenRunState(state) ? "stopped" : "cancel_requested";
    case "failed":
      return "failed";
  }
}

const STOP_HELD: ReadonlySet<RunPhase> = new Set(["held", "cancel_requested", "failed"]);

export function isStopHeld(phase: RunPhase): boolean {
  return STOP_HELD.has(phase);
}

/** Phases in which a record no longer blocks `ack_stop`. */
const ACKABLE: ReadonlySet<StopPhase> = new Set(["settled", "expired", "released", "unconfirmable"]);

export class RunLedger {
  private readonly deps: LedgerDeps;
  private readonly runs = new Map<string, RunEntry>();
  private readonly records = new Map<string, StopRecord>();
  private readonly retryTimers = new Map<string, unknown>();
  private readonly inFlight = new Map<string, number>();
  private readonly handled: string[] = [];
  private readonly handledSet = new Set<string>();
  private readonly stopListeners = new Set<(rec: StopRecord) => void>();
  private readonly listeners = new Set<Listener>();
  /** Run id -> observation value when the ledger last saw it terminal. */
  private readonly finishedAt = new Map<string, number>();
  private current: ClientHandle | null = null;
  private epochs = 0;
  private obs = 0;
  private epoch = 0;
  /** Highest native `stopSeq` seen; `null` until the first native snapshot that has one. */
  private stopCursor: number | null = null;
  /** Native sequences above the ack base not yet attributed to a decision. */
  private unresolved: number[] = [];
  private ackedSeq = 0;
  private lastRev = -1;
  /** The newest native snapshot accepted (single owner: every route goes through here). */
  private accepted: NativeStopSnapshot | null = null;
  private readonly nativeListeners = new Set<(snap: NativeStopSnapshot) => void>();
  private readonly reconcileListeners = new Set<() => void>();
  private version = 0;
  private saved: string | null = null;

  constructor(deps: LedgerDeps) {
    this.deps = deps;
    this.restore();
  }

  // ── observation ────────────────────────────────────────────────────────────

  /** Bumped on every change (for `useSyncExternalStore`). */
  getVersion(): number {
    return this.version;
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** Called once for every NEW Stop decision of kind `all` (override / input-halt hooks). */
  /**
   * Called with every native snapshot the ledger ACCEPTS (newer revision, not a failed
   * read), whoever routed it: events, reads and the ledger's own command replies.
   */
  onNativeAccepted(listener: (snap: NativeStopSnapshot) => void): () => void {
    this.nativeListeners.add(listener);
    return () => {
      this.nativeListeners.delete(listener);
    };
  }

  /** The newest accepted native snapshot (null until one). */
  nativeSnapshot(): NativeStopSnapshot | null {
    return this.accepted;
  }

  /** Called when the ledger needs a fresh run list to resolve an unconfirmed Stop. */
  onReconcileNeeded(listener: () => void): () => void {
    this.reconcileListeners.add(listener);
    return () => {
      this.reconcileListeners.delete(listener);
    };
  }

  /** An unconfirmed Stop is waiting for a run list issued after its expiry. */
  needsReconcile(): boolean {
    for (const rec of this.records.values()) {
      if (rec.phase === "unconfirmed" && rec.watchFromObs !== null) {
        return true;
      }
    }
    return false;
  }

  onStop(listener: (rec: StopRecord) => void): () => void {
    this.stopListeners.add(listener);
    return () => {
      this.stopListeners.delete(listener);
    };
  }

  /**
   * Raised synchronously by every new Stop decision. An action that captured an
   * older value must not perform any further local effect.
   */
  stopEpoch(): number {
    return this.epoch;
  }

  run(id: string): Readonly<RunEntry> | undefined {
    return this.runs.get(id);
  }

  runEntries(): readonly Readonly<RunEntry>[] {
    return [...this.runs.values()];
  }

  stop(id: string): Readonly<StopRecord> | undefined {
    return this.records.get(id);
  }

  stopRecords(): readonly Readonly<StopRecord>[] {
    return [...this.records.values()];
  }

  client(): ClientHandle | null {
    return this.current;
  }

  /** Native sequence the ledger has acknowledged (or seen acknowledged). */
  acked(): number {
    return this.ackedSeq;
  }

  /** Runs that are live and allowed to act. */
  liveRunIds(): string[] {
    return [...this.runs.values()]
      .filter((r) => r.phase === "tracked" && r.quarantinedBy === null)
      .map((r) => r.id);
  }

  /** True when a run may still drive native / receive `proceed`. */
  mayAct(runId: string): boolean {
    const r = this.runs.get(runId);
    return (
      r !== undefined &&
      r.phase === "tracked" &&
      r.quarantinedBy === null &&
      !this.hasUnansweredSweep()
    );
  }

  /** First caller for a `control_requested` id gets `true`; replays get `false`. */
  claimRequest(requestId: string): boolean {
    if (this.handledSet.has(requestId)) {
      return false;
    }
    this.handledSet.add(requestId);
    this.handled.push(requestId);
    if (this.handled.length > HANDLED_REQUESTS_MAX) {
      const old = this.handled.shift();
      if (old !== undefined) {
        this.handledSet.delete(old);
      }
    }
    return true;
  }

  // ── daemon clients ─────────────────────────────────────────────────────────

  /**
   * A daemon client became current. The previous one is revoked first (synchronously).
   * Unresolved Stops are sent to the new client before anything else happens on it.
   */
  attachClient(transport: StopTransport): ClientHandle {
    if (this.current) {
      this.current.live = false;
    }
    this.epochs += 1;
    const handle: ClientHandle = {
      epoch: this.epochs,
      transport,
      live: true,
      synced: false,
      gaps: 0,
      listsIssued: 0,
      listsApplied: 0
    };
    this.current = handle;
    for (const rec of this.records.values()) {
      if (this.isOpenRecord(rec)) {
        this.send(rec);
      }
    }
    this.changed();
    return handle;
  }

  detachClient(handle: ClientHandle): void {
    handle.live = false;
    if (this.current === handle) {
      this.current = null;
      this.changed();
    }
  }

  /** The client's event stream had a gap or reconnected: wait for the next list. */
  markUnsynced(handle: ClientHandle): void {
    if (handle !== this.current) {
      return;
    }
    handle.gaps += 1;
    if (handle.synced) {
      handle.synced = false;
      this.changed();
    }
  }

  /** Call right before issuing `GET /v1/runs` on `handle`; pass the ticket to `applyList`. */
  beginList(handle: ClientHandle): ListTicket | null {
    if (handle !== this.current || !handle.live) {
      return null;
    }
    handle.listsIssued += 1;
    return { handle, seq: handle.listsIssued, issuedObs: this.obs, gaps: handle.gaps };
  }

  /**
   * Apply `GET /v1/runs`. Terminal rows always apply (terminal is final). Open rows and
   * absences apply only from the newest list of the current client, and never to a
   * run the ledger learned about (or saw finish) after this list was issued.
   */
  applyList(ticket: ListTicket, list: readonly ListedRun[]): void {
    const handle = ticket.handle;
    for (const row of list) {
      if (!isOpenRunState(row.state)) {
        const entry = this.runs.get(row.id);
        if (entry) {
          this.markTerminal(entry);
        }
      }
    }
    const fresh =
      handle === this.current && handle.live && ticket.seq > handle.listsApplied;
    if (!fresh) {
      this.settleAll();
      this.changed();
      return;
    }
    const listed = new Set<string>();
    for (const row of list) {
      if (!isOpenRunState(row.state)) {
        continue;
      }
      listed.add(row.id);
      if ((this.finishedAt.get(row.id) ?? -1) > ticket.issuedObs) {
        continue; // seen terminal after this list was issued
      }
      const entry = this.runs.get(row.id);
      if (entry) {
        entry.botId = row.bot_id ?? entry.botId;
        if (entry.phase === "tracked") {
          entry.processing = PROCESSING_RUN_STATES.has(row.state);
        }
      } else {
        this.addRun(row.id, row.bot_id ?? null, PROCESSING_RUN_STATES.has(row.state));
      }
    }
    for (const entry of [...this.runs.values()]) {
      if (listed.has(entry.id) || entry.seenObs > ticket.issuedObs) {
        continue;
      }
      // The daemon lists every run of the user: absent from a list issued after we
      // first saw the run means it is gone.
      this.markTerminal(entry);
    }
    handle.listsApplied = ticket.seq;
    handle.synced = ticket.gaps === handle.gaps;
    for (const rec of this.records.values()) {
      if (rec.watchFromObs !== null && ticket.issuedObs >= rec.watchFromObs) {
        rec.watchFromObs = null; // every run open now is known (and quarantined)
      }
    }
    this.settleAll();
    this.changed();
  }

  /** `run_started` (fresh) or a `control_requested` for a run: it is alive. */
  observeRun(runId: string, botId: string | null, processing = true): void {
    const entry = this.runs.get(runId);
    if (entry) {
      if (botId) {
        entry.botId = botId;
      }
      if (entry.phase === "tracked") {
        entry.processing = processing;
      }
      this.changed();
      return;
    }
    this.addRun(runId, botId, processing);
    this.changed();
  }

  /** `run_finished` (any, including replayed history): the run is terminal. */
  observeRunFinished(runId: string): void {
    const entry = this.runs.get(runId);
    if (entry) {
      this.markTerminal(entry);
    } else {
      this.noteFinished(runId);
    }
    this.settleAll();
    this.changed();
  }

  // ── Stop ───────────────────────────────────────────────────────────────────

  /**
   * One user Stop. Synchronous up to and including the native call and the daemon
   * request: nothing here waits for heartbeats, hand-backs, polls or scope.
   */
  stopAll(origin: Exclude<StopOrigin, "native" | "chat">): StopRecord {
    const now = this.deps.clock.now();
    const joined = this.coalesceTarget(now);
    if (joined) {
      // The same key press already made this decision (e.g. the global listener).
      this.epoch += 1;
      this.changed();
      return joined;
    }
    const rec = this.createRecord(origin, now);
    rec.awaitingEcho = true;
    // Local input halts first; the daemon request does not wait for it. There is NO
    // timeout on the echo: until native answers, sequences it may own stay unattributed
    // (blocking acks, never minting a decision); the Stop itself is already being sent
    // with its own id and target set.
    this.deps.native.stopRun(rec.id).then(
      (snap) => {
        // The reply pairs this decision's own sequence with its token.
        this.onNativeSnapshot(snap);
        if (rec.awaitingEcho) {
          rec.awaitingEcho = false;
          this.classify();
        }
      },
      () => {
        // The command failed: it produced no sequence, so it owns none.
        rec.awaitingEcho = false;
        this.classify();
      }
    );
    this.dispatch(rec);
    return rec;
  }

  /** Cancel one run (chat Stop button): its own decision, `listed`, no native effect. */
  cancelOne(runId: string, opts?: { force?: boolean }): StopRecord {
    const now = this.deps.clock.now();
    const rec = this.blankRecord("chat", "single", now);
    rec.scope = "listed";
    if (opts?.force) {
      rec.force = true;
    }
    const entry = this.runs.get(runId) ?? this.addRun(runId, null, false);
    if (entry.phase !== "stopped") {
      this.hold(entry, rec);
    }
    rec.known = [runId];
    rec.members = [runId];
    this.records.set(rec.id, rec);
    this.dispatch(rec);
    return rec;
  }

  /**
   * The user lets the runs an unconfirmed Stop quarantined continue (they were
   * never shown to be in its target set). Nothing is cancelled.
   */
  releaseUnconfirmed(stopId: string): void {
    const rec = this.records.get(stopId);
    if (!rec || rec.phase !== "unconfirmed") {
      return;
    }
    rec.phase = "released";
    rec.watchFromObs = null;
    rec.settledMono = this.deps.clock.now();
    for (const entry of this.runs.values()) {
      if (entry.quarantinedBy === rec.id) {
        entry.quarantinedBy = null;
      }
    }
    this.tryAck();
    this.changed();
  }

  /**
   * Every native control snapshot (event, command reply, initial read) goes here.
   * Returns `true` only for a real snapshot whose revision is newer than any seen
   * (a failed read has no revision and is never "fresh").
   */
  onNativeSnapshot(snap: NativeStopSnapshot): boolean {
    if (snap.stopSeq === undefined) {
      // A failed read: it says nothing about Stops or modes.
      return false;
    }
    const s = snap.stopSeq;
    if (this.stopCursor === null) {
      const base = Math.min(snap.stopAckedSeq ?? s, s);
      this.ackedSeq = Math.max(this.ackedSeq, base);
      this.stopCursor = base;
      // Records restored from this webview session already own their sequences.
      const claimed = new Set<number>();
      for (const rec of this.records.values()) {
        for (const q of rec.nativeSeqs) {
          claimed.add(q);
        }
      }
      for (let q = base + 1; q <= s; q += 1) {
        if (!claimed.has(q)) {
          this.unresolved.push(q);
        }
      }
      this.stopCursor = s;
    } else if (s > this.stopCursor) {
      for (let q = this.stopCursor + 1; q <= s; q += 1) {
        this.unresolved.push(q);
      }
      this.stopCursor = s;
    }
    if (snap.stopAckedSeq !== undefined && snap.stopAckedSeq > this.ackedSeq) {
      this.ackedSeq = snap.stopAckedSeq;
    }
    // The snapshot names who produced its own sequence: exact, never inferred.
    const at = this.unresolved.indexOf(s);
    if (at >= 0) {
      this.unresolved.splice(at, 1);
      const token = snap.lastStopToken ?? null;
      const owner = token ? this.records.get(token) : undefined;
      if (owner) {
        owner.awaitingEcho = false;
        if (!owner.nativeSeqs.includes(s)) {
          owner.nativeSeqs.push(s);
        }
      } else {
        this.nativePress([s]);
      }
    }
    this.classify();
    const fresh = this.freshRev(snap);
    if (fresh) {
      // Accepting and caching are one step: no route can advance the revision without it.
      this.accepted = snap;
      for (const l of this.nativeListeners) {
        l(snap);
      }
    }
    this.tryAck();
    this.changed();
    return fresh;
  }

  // ── internals ──────────────────────────────────────────────────────────────

  /** Native modes are applied only from a snapshot newer than the last applied one. */
  private freshRev(snap: NativeStopSnapshot): boolean {
    if (snap.stateRev === undefined) {
      return false;
    }
    if (snap.stateRev > this.lastRev) {
      this.lastRev = snap.stateRev;
      return true;
    }
    return false;
  }

  /**
   * Sequences whose own snapshot was never seen: they can only be one of our own
   * outstanding invokes (whose reply will say so) or a press of their own. While an
   * invoke is outstanding they wait; then every remaining one is an untokened press.
   */
  private classify(): void {
    if (!this.unresolved.length) {
      return;
    }
    for (const rec of this.records.values()) {
      if (rec.awaitingEcho) {
        return;
      }
    }
    this.nativePress(this.unresolved.splice(0));
  }

  private nativePress(seqs: number[]): void {
    if (!seqs.length) {
      return;
    }
    const now = this.deps.clock.now();
    // The in-window hotkey and the global listener on the same press, or key bounce.
    const joined = this.coalesceTarget(now);
    if (joined) {
      for (const q of seqs) {
        if (!joined.nativeSeqs.includes(q)) {
          joined.nativeSeqs.push(q);
        }
      }
      this.epoch += 1;
      this.tryAck();
      return;
    }
    const rec = this.createRecord("native", now);
    rec.nativeSeqs.push(...seqs);
    this.dispatch(rec);
  }

  /** A Stop decided within the coalesce window (same key press), whatever its daemon state. */
  private coalesceTarget(now: number): StopRecord | null {
    let best: StopRecord | null = null;
    for (const rec of this.records.values()) {
      if (rec.kind !== "all" || rec.supersedes !== null) {
        continue;
      }
      if (now - rec.decidedMono <= STOP_COALESCE_MS) {
        best = rec;
      }
    }
    return best;
  }

  private addRun(id: string, botId: string | null, processing: boolean): RunEntry {
    this.obs += 1;
    const entry: RunEntry = {
      id,
      botId,
      phase: "tracked",
      stopId: null,
      quarantinedBy: this.quarantineOwner(),
      seenObs: this.obs,
      processing,
      error: null
    };
    this.runs.set(id, entry);
    return entry;
  }

  /** The Stop a newly seen run might belong to without the app being able to tell. */
  private quarantineOwner(): string | null {
    for (const rec of this.records.values()) {
      if (rec.scope === "all_existing" && !rec.reached && this.isOpenRecord(rec)) {
        return rec.id;
      }
    }
    for (const rec of this.records.values()) {
      if (rec.phase === "unconfirmed" && rec.watchFromObs !== null) {
        return rec.id;
      }
    }
    return null;
  }

  private blankRecord(origin: StopOrigin, kind: "all" | "single", now: number): StopRecord {
    return {
      id: this.deps.newId(),
      origin,
      kind,
      nativeSeqs: [],
      awaitingEcho: false,
      known: [],
      members: [],
      syncedAtDecision: this.current?.synced ?? false,
      decidedMono: now,
      scope: "all_existing",
      phase: "queued",
      reached: false,
      firstSeen: null,
      audited: true,
      attempts: 0,
      retryIndex: 0,
      lastError: null,
      supersedes: null,
      successor: null,
      watchFromObs: null,
      session: this.deps.session,
      settledMono: null
    };
  }

  private createRecord(origin: StopOrigin, now: number): StopRecord {
    this.epoch += 1;
    const rec = this.blankRecord(origin, "all", now);
    for (const entry of this.runs.values()) {
      if (entry.phase !== "stopped") {
        this.hold(entry, rec);
      }
    }
    rec.known = [...rec.members];
    this.records.set(rec.id, rec);
    return rec;
  }

  private hold(entry: RunEntry, rec: StopRecord): void {
    if (entry.phase === "tracked" || entry.phase === "failed") {
      entry.phase = "held";
    }
    entry.stopId = rec.id;
    if (!rec.members.includes(entry.id)) {
      rec.members.push(entry.id);
    }
  }

  /** Send a new record, retry every other unresolved one, notify. */
  private dispatch(rec: StopRecord): void {
    this.send(rec);
    for (const other of this.records.values()) {
      if (other !== rec && this.isOpenRecord(other)) {
        this.send(other, true);
      }
    }
    if (rec.kind === "all") {
      for (const l of this.stopListeners) {
        l(rec);
      }
    }
    this.changed();
  }

  private isOpenRecord(rec: StopRecord): boolean {
    return (
      rec.phase === "queued" ||
      rec.phase === "in_flight" ||
      rec.phase === "retrying"
    );
  }

  private hasUnansweredSweep(): boolean {
    for (const rec of this.records.values()) {
      if (rec.scope === "all_existing" && !rec.reached && this.isOpenRecord(rec)) {
        return true;
      }
    }
    return false;
  }

  private send(rec: StopRecord, now = false): void {
    const handle = this.current;
    if (!handle || !handle.live || !this.isOpenRecord(rec)) {
      return;
    }
    if (!now && this.inFlight.get(rec.id) === handle.epoch) {
      return; // one attempt per client at a time, unless the user pressed Stop again
    }
    if (now) {
      const timer = this.retryTimers.get(rec.id);
      if (timer !== undefined) {
        this.deps.clock.clear(timer);
        this.retryTimers.delete(rec.id);
      }
    }
    const clock = this.deps.clock;
    if (
      !rec.reached &&
      rec.scope === "all_existing" &&
      rec.syncedAtDecision &&
      clock.now() - rec.decidedMono > STOP_SWEEP_WINDOW_MS
    ) {
      // Late first delivery while the app knew the runs: never sweep a later run.
      rec.scope = "listed";
    }
    const body: StopRunsInput = {
      stop_id: rec.id,
      scope: rec.scope,
      run_ids: rec.known.slice(0, 1024),
      ...(rec.force ? { force: true } : {})
    };
    const controller = new AbortController();
    const timeout = clock.set(() => controller.abort(), STOP_ATTEMPT_TIMEOUT_MS);
    this.inFlight.set(rec.id, handle.epoch);
    rec.phase = "in_flight";
    rec.attempts += 1;
    let reply: Promise<StopRunsResponse>;
    try {
      reply = handle.transport.stopRuns(body, controller.signal);
    } catch (err) {
      reply = Promise.reject(err);
    }
    reply.then(
      (resp) => {
        clock.clear(timeout);
        if (this.inFlight.get(rec.id) === handle.epoch) {
          this.inFlight.delete(rec.id);
        }
        this.applyReply(rec, resp, body.scope);
      },
      (err: unknown) => {
        clock.clear(timeout);
        if (this.inFlight.get(rec.id) === handle.epoch) {
          this.inFlight.delete(rec.id);
        }
        if (!this.isOpenRecord(rec)) {
          return;
        }
        rec.lastError = err instanceof Error ? err.message : String(err);
        this.scheduleRetry(rec);
        this.changed();
      }
    );
  }

  private scheduleRetry(rec: StopRecord): void {
    if (!this.isOpenRecord(rec)) {
      return;
    }
    rec.phase = "retrying";
    if (this.retryTimers.has(rec.id)) {
      return;
    }
    const delay = STOP_RETRY_MS[Math.min(rec.retryIndex, STOP_RETRY_MS.length - 1)] ?? 5000;
    rec.retryIndex += 1;
    const timer = this.deps.clock.set(() => {
      this.retryTimers.delete(rec.id);
      this.send(rec);
    }, delay);
    this.retryTimers.set(rec.id, timer);
  }

  private applyReply(rec: StopRecord, resp: StopRunsResponse, sentScope: StopScope): void {
    if (!this.isOpenRecord(rec)) {
      // A late duplicate: it may still confirm terminal runs, nothing else.
      for (const r of resp.results ?? []) {
        if (outcomePhase(r.outcome, r.state) === "stopped") {
          const entry = this.runs.get(r.run_id);
          if (entry) {
            this.markTerminal(entry);
          }
        }
      }
      this.settleAll();
      this.changed();
      return;
    }
    if (resp.audited === false) {
      rec.audited = false;
    }
    if (resp.expired) {
      this.expire(rec);
      return;
    }
    rec.reached = true;
    rec.lastError = null;
    if (rec.firstSeen === null) {
      rec.firstSeen = resp.first_seen;
    }
    const inT = new Set<string>();
    for (const r of resp.results) {
      inT.add(r.run_id);
      const phase = outcomePhase(r.outcome, r.state);
      if (!rec.members.includes(r.run_id)) {
        rec.members.push(r.run_id);
      }
      const entry = this.runs.get(r.run_id) ?? this.addRun(r.run_id, null, false);
      if (entry.phase === "stopped") {
        continue; // terminal is final
      }
      if (phase === "stopped") {
        entry.stopId = entry.stopId ?? rec.id;
        this.markTerminal(entry);
        continue;
      }
      if (entry.phase === "tracked" || entry.stopId === null) {
        entry.stopId = rec.id;
      }
      entry.phase = phase;
      entry.quarantinedBy = null;
      entry.error = phase === "failed" ? (r.error ?? null) : null;
    }
    if (sentScope === "all_existing" && resp.first_seen) {
      // The daemon just swept every run open now: any run an older unconfirmed Stop
      // might have targeted and that still exists is in this T (and held by it), and
      // the rest are gone. That is the evidence the older uncertainty was waiting for.
      for (const other of this.records.values()) {
        if (other.phase === "unconfirmed") {
          other.watchFromObs = null;
          // A run the older Stop quarantined that this sweep did not list is not open
          // on the daemon any more (every run seen after this Stop was created is
          // quarantined by THIS Stop, never by the older one): positive absence, so it
          // is resolved here instead of staying quarantined with nobody watching it.
          for (const entry of this.runs.values()) {
            if (entry.quarantinedBy === other.id && !inT.has(entry.id)) {
              this.markTerminal(entry);
            }
          }
        }
      }
    }
    // T is known now: every later attempt names exactly T (a restarted daemon that
    // forgot this id must not sweep again).
    rec.scope = "listed";
    rec.known = [...rec.members];
    // Runs this Stop quarantined but the daemon did not target are free.
    for (const entry of this.runs.values()) {
      if (entry.quarantinedBy === rec.id && !inT.has(entry.id)) {
        entry.quarantinedBy = this.quarantineOwner();
      }
    }
    this.settleAll();
    if (this.isOpenRecord(rec)) {
      this.scheduleRetry(rec);
    }
    this.changed();
  }

  /**
   * The daemon forgot this id. Never re-sweep: a successor with a NEW id carries
   * only the runs the app still holds, as `listed`. When the app never learned T,
   * nothing about the unknown targets is certified: the record becomes `unconfirmed`.
   */
  private expire(rec: StopRecord): void {
    const now = this.deps.clock.now();
    rec.settledMono = now;
    const held = rec.members.filter((id) => {
      const entry = this.runs.get(id);
      return entry !== undefined && entry.phase !== "stopped";
    });
    if (rec.reached) {
      rec.phase = "expired";
    } else {
      rec.phase = "unconfirmed";
      // Runs it quarantined stay quarantined; runs first seen before the next list
      // (which could have existed when the daemon fixed T) are quarantined too.
      rec.watchFromObs = this.obs;
      for (const l of this.reconcileListeners) {
        l();
      }
    }
    if (held.length) {
      const next = this.blankRecord(rec.origin, rec.kind, now);
      next.scope = "listed";
      next.known = [...held];
      next.members = [...held];
      next.supersedes = rec.id;
      if (rec.reached) {
        next.nativeSeqs = rec.nativeSeqs;
        rec.nativeSeqs = [];
      }
      rec.successor = next.id;
      for (const id of held) {
        const entry = this.runs.get(id);
        if (entry) {
          entry.stopId = next.id;
        }
      }
      this.records.set(next.id, next);
      this.send(next);
    }
    this.settleAll();
    this.tryAck();
    this.changed();
  }

  private noteFinished(runId: string): void {
    this.obs += 1;
    this.finishedAt.set(runId, this.obs);
    if (this.finishedAt.size > FINISHED_MAX) {
      const oldest = this.finishedAt.keys().next().value;
      if (oldest !== undefined) {
        this.finishedAt.delete(oldest);
      }
    }
  }

  private markTerminal(entry: RunEntry): void {
    this.noteFinished(entry.id);
    if (entry.phase === "tracked" && entry.stopId === null) {
      this.runs.delete(entry.id);
      return;
    }
    entry.phase = "stopped";
    entry.quarantinedBy = null;
    entry.error = null;
  }

  private settleAll(): void {
    let any = false;
    for (const rec of this.records.values()) {
      if (rec.phase === "unconfirmed") {
        // Every candidate of the unknown T is known (watch over) and terminal.
        const successorOpen =
          rec.successor !== null && this.records.get(rec.successor)?.phase !== "settled";
        const quarantined = [...this.runs.values()].some((e) => e.quarantinedBy === rec.id);
        if (rec.watchFromObs === null && !quarantined && !successorOpen) {
          rec.phase = "settled";
          rec.settledMono = this.deps.clock.now();
          any = true;
        }
        continue;
      }
      if (!this.isOpenRecord(rec) || !rec.reached) {
        continue;
      }
      const open = rec.members.some((id) => {
        const entry = this.runs.get(id);
        return entry !== undefined && entry.phase !== "stopped";
      });
      if (!open) {
        rec.phase = "settled";
        rec.settledMono = this.deps.clock.now();
        const timer = this.retryTimers.get(rec.id);
        if (timer !== undefined) {
          this.deps.clock.clear(timer);
          this.retryTimers.delete(rec.id);
        }
        any = true;
      }
    }
    if (any) {
      this.prune();
      this.tryAck();
    }
  }

  /** Drop stopped runs and old records nothing open refers to any more. */
  private prune(): void {
    const referenced = new Set<string>();
    for (const rec of this.records.values()) {
      if (this.isOpenRecord(rec) || rec.phase === "unconfirmed") {
        for (const id of rec.members) {
          referenced.add(id);
        }
      }
    }
    for (const entry of [...this.runs.values()]) {
      if (entry.phase === "stopped" && !referenced.has(entry.id)) {
        this.runs.delete(entry.id);
      }
    }
    const done = [...this.records.values()].filter((r) => ACKABLE.has(r.phase));
    const excess = done.length - SETTLED_RECORDS_MAX;
    for (let i = 0; i < excess; i += 1) {
      const rec = done[i];
      if (rec) {
        this.records.delete(rec.id);
      }
    }
  }

  /**
   * Acknowledge the highest native sequence below every unresolved Stop and below
   * every sequence not yet attributed. A Stop is unresolved until every run it
   * targeted is positively terminal (expired ones pass their sequences on).
   */
  private tryAck(): void {
    if (this.stopCursor === null) {
      return;
    }
    let target = this.stopCursor;
    for (const q of this.unresolved) {
      target = Math.min(target, q - 1);
    }
    for (const rec of this.records.values()) {
      if (ACKABLE.has(rec.phase)) {
        continue;
      }
      for (const q of rec.nativeSeqs) {
        target = Math.min(target, q - 1);
      }
    }
    if (target > this.ackedSeq) {
      this.ackedSeq = target;
      void this.deps.native.ackStop(target).catch(() => undefined);
    }
  }

  private persist(): void {
    const storage = this.deps.storage;
    if (!storage) {
      return;
    }
    const data: Persisted = {
      v: 2,
      session: this.deps.session,
      records: [...this.records.values()],
      runs: [...this.runs.values()].filter((r) => r.stopId !== null || r.quarantinedBy !== null)
    };
    try {
      const json = JSON.stringify(data);
      if (json !== this.saved) {
        this.saved = json;
        storage.save(json);
      }
    } catch {
      // storage unavailable: Stop still works in memory
    }
  }

  /**
   * Stop records survive a webview reload and an app restart. Native sequences are
   * only meaningful within the session that saw them; a Stop from an earlier app run
   * that never reached the daemon is narrowed to the runs it knew (never a sweep).
   */
  private restore(): void {
    const storage = this.deps.storage;
    if (!storage) {
      return;
    }
    let data: Persisted | null = null;
    try {
      const raw = storage.load();
      data = raw ? (JSON.parse(raw) as Persisted) : null;
    } catch {
      data = null;
    }
    if (!data || data.v !== 2 || !Array.isArray(data.records)) {
      return;
    }
    const sameSession = data.session === this.deps.session;
    for (const entry of data.runs ?? []) {
      this.runs.set(entry.id, { ...entry, seenObs: 0 });
    }
    for (const saved of data.records) {
      const rec: StopRecord = {
        ...saved,
        awaitingEcho: false,
        watchFromObs: saved.watchFromObs === null ? null : 0,
        // mono clocks restart with the page
        decidedMono: Number.NEGATIVE_INFINITY,
        settledMono: saved.settledMono === null ? null : Number.NEGATIVE_INFINITY
      };
      if (!sameSession) {
        rec.nativeSeqs = [];
        if (this.isOpenRecord(rec) && !rec.reached && rec.scope === "all_existing") {
          rec.scope = "listed";
          if (rec.known.length === 0) {
            rec.phase = "unconfirmable";
          }
        }
      }
      if (this.isOpenRecord(rec)) {
        rec.phase = "queued";
      }
      this.records.set(rec.id, rec);
    }
  }

  private changed(): void {
    this.version += 1;
    this.persist();
    for (const l of this.listeners) {
      l();
    }
  }
}
