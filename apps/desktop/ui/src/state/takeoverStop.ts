import type { ControlStateDto } from "../api/types.ts";

/**
 * The takeover half of a Stop. Pure.
 *
 * - `stop()` SYNCHRONOUSLY revokes this app's takeover input: from that call on no
 *   pointer / key event is queued or sent and the lease heartbeat stops renewing,
 *   whatever the daemon answers, until the user takes control again (`resume()`).
 * - The `POST /v1/control/override` it owes is retained until it is delivered through
 *   the CURRENT client. Every attempt belongs to one client and one decision
 *   generation and is bounded (`OVERRIDE_ATTEMPT_MS`, then aborted). An attempt whose
 *   client was replaced, or whose decision is no longer the newest, is abandoned: its
 *   late answer is ignored and never re-sent. A failed or timed-out attempt stays owed
 *   and is retried by the next pump (a new control snapshot - polled every 5 s -, a
 *   client change or another Stop); there is no retry timer.
 * - `resume()` is the user's NEWER explicit takeover decision: it retires the owed
 *   override and any attempt in flight, so an old Stop can never pause a takeover
 *   the user acquired after it.
 * - "Only if controlled" Stops decide from a control snapshot of the current client;
 *   while there is none the decision waits (unknown is not "not controlled").
 */

/** One override attempt may take this long before it is aborted and counted as failed. */
export const OVERRIDE_ATTEMPT_MS = 5000;

export interface TakeoverStopDeps<C> {
  /** The daemon client that is current right now (or null). */
  client(): C | null;
  /**
   * Whether an acquiring / active takeover exists according to a snapshot read from
   * the CURRENT client; `null` when no such snapshot exists yet.
   */
  controlled(): boolean | null;
  post(client: C, signal: AbortSignal): Promise<ControlStateDto>;
  /** Apply a delivered override reply (only called while `client` is still current). */
  apply(client: C, dto: ControlStateDto): void;
  schedule(fn: () => void, ms: number): unknown;
  cancel(handle: unknown): void;
}

interface Attempt<C> {
  readonly client: C;
  readonly gen: number;
  readonly controller: AbortController;
  readonly timer: unknown;
}

export class TakeoverStop<C> {
  private readonly deps: TakeoverStopDeps<C>;
  private halted = false;
  /** Decision generation: every Stop and every explicit user takeover raises it. */
  private gen = 0;
  private owed: { gen: number; force: boolean } | null = null;
  private attempt: Attempt<C> | null = null;

  constructor(deps: TakeoverStopDeps<C>) {
    this.deps = deps;
  }

  /** A Stop decision. `force`: send the override even if no takeover is held (viewer Stop). */
  stop(force: boolean): void {
    this.halted = true;
    this.gen += 1;
    this.owed = { gen: this.gen, force: (this.owed?.force ?? false) || force };
    this.pump();
  }

  /** The user took control again (a newer decision): input flows, the old override is retired. */
  resume(): void {
    this.halted = false;
    this.gen += 1;
    this.owed = null;
    this.abandon();
  }

  /**
   * The user made a newer explicit decision about the pause itself (Clear pause,
   * release): the owed override and any attempt in flight are retired by the same
   * generation rule as `resume()`. Input stays halted: only taking control again
   * (`resume()`) re-enables it.
   */
  retire(): void {
    this.gen += 1;
    this.owed = null;
    this.abandon();
  }

  /** May this app send takeover input / renew the input lease right now? */
  inputAllowed(): boolean {
    return !this.halted;
  }

  /** An override is still owed (not yet delivered through the current client). */
  owes(): boolean {
    return this.owed !== null;
  }

  /** Try to deliver the owed override (call on Stop, client change and new snapshots). */
  pump(): void {
    const owed = this.owed;
    if (!owed) {
      return;
    }
    const client = this.deps.client();
    if (!client) {
      return; // retained until a client exists
    }
    const running = this.attempt;
    if (running) {
      if (running.client === client && running.gen === owed.gen) {
        return; // the current client is already being asked for this decision
      }
      this.abandon(); // another client or an older decision: never let it block
    }
    if (!owed.force) {
      const controlled = this.deps.controlled();
      if (controlled === null) {
        return; // unknown: wait for this client's snapshot
      }
      if (!controlled) {
        this.owed = null; // nothing to pause
        return;
      }
    }
    const controller = new AbortController();
    const attempt: Attempt<C> = {
      client,
      gen: owed.gen,
      controller,
      timer: this.deps.schedule(() => controller.abort(), OVERRIDE_ATTEMPT_MS)
    };
    this.attempt = attempt;
    let reply: Promise<ControlStateDto>;
    try {
      reply = this.deps.post(client, controller.signal);
    } catch (err) {
      reply = Promise.reject(err);
    }
    reply.then(
      (dto) => {
        if (this.attempt !== attempt) {
          return; // abandoned: a late answer writes nothing and is not re-sent
        }
        this.finish(attempt);
        if (this.deps.client() !== client) {
          this.pump();
          return;
        }
        if (this.owed?.gen === attempt.gen) {
          this.owed = null;
        }
        this.deps.apply(client, dto);
        this.pump();
      },
      () => {
        if (this.attempt === attempt) {
          this.finish(attempt); // failed or timed out: still owed, retried by the next pump
        }
      }
    );
  }

  private finish(attempt: Attempt<C>): void {
    this.deps.cancel(attempt.timer);
    if (this.attempt === attempt) {
      this.attempt = null;
    }
  }

  private abandon(): void {
    const running = this.attempt;
    if (running) {
      this.attempt = null;
      this.deps.cancel(running.timer);
      running.controller.abort();
    }
  }
}
