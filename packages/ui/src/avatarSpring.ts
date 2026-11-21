/**
 * Smooth motion for the avatar pose: critically damped springs advanced by ONE shared
 * requestAnimationFrame loop for every avatar on the page.
 *
 * - `springStep` is the exact analytic solution of a critically damped spring, so the result does
 *   not depend on the frame rate (two 8 ms steps land exactly where one 16 ms step does).
 * - A retarget mid-flight can never overshoot: if a step would cross the target, it lands on it.
 * - `createAvatarLoop` runs frames only while some subject is unsettled and the document is
 *   visible; once everything has settled it stops requesting frames (zero idle cost).
 * No DOM access except through the injected scheduler, so it is unit-testable in node.
 */

export interface SpringState {
  /** Current value. */
  x: number;
  /** Current velocity (units per second). */
  v: number;
}

/** Settle time (to ~2 % of the distance) -> natural frequency of a critically damped spring. */
export function omegaForSettle(settleMs: number): number {
  return 5.8 / Math.max(0.016, settleMs / 1000);
}

/**
 * Advance a critically damped spring toward `target` by `dtSec` (exact, frame-rate independent).
 * Mutates and returns `s`.
 */
export function springStep(s: SpringState, target: number, omega: number, dtSec: number): SpringState {
  if (!(dtSec > 0)) return s;
  const c1 = s.x - target;
  const c2 = s.v + omega * c1;
  const e = Math.exp(-omega * dtSec);
  const nx = target + (c1 + c2 * dtSec) * e;
  const nv = (c2 - omega * (c1 + c2 * dtSec)) * e;
  // A retarget while moving fast toward the old target could carry the value past the new
  // one: clamp onto the target instead (the owner asked for "no overshoot").
  if (c1 !== 0 && Math.sign(nx - target) !== Math.sign(c1) && nx !== target) {
    s.x = target;
    s.v = 0;
    return s;
  }
  s.x = nx;
  s.v = nv;
  return s;
}

/** Close enough to stop animating (values are degrees / unit-less openness). */
export function springSettled(s: SpringState, target: number, eps = 0.02): boolean {
  return Math.abs(s.x - target) < eps && Math.abs(s.v) < eps * 10;
}

/* ---------------- the shared loop ---------------- */

export interface LoopSubject {
  /** Advance by `dtSec`; return `true` once at rest (the loop then forgets the subject). */
  step(dtSec: number): boolean;
}

export interface LoopScheduler {
  request(cb: (time: number) => void): number;
  cancel(id: number): void;
  now(): number;
  /** False while the window is hidden: the loop sleeps and resumes on `resume()`. */
  visible(): boolean;
}

export interface AvatarLoop {
  /** Ensure `subject` is stepped on the next frames until it reports settled. */
  wake(subject: LoopSubject): void;
  /** Stop stepping `subject` (destroyed avatar). */
  remove(subject: LoopSubject): void;
  /** Re-arm after the document became visible again. */
  resume(): void;
  /** Number of unsettled subjects (tests, diagnostics). */
  readonly activeCount: number;
  /** Whether a frame is currently requested (tests, diagnostics). */
  readonly running: boolean;
}

/** Longest step one frame may take (a stalled tab must not jump a whole second at once). */
const MAX_DT = 0.064;

export function createAvatarLoop(scheduler: LoopScheduler): AvatarLoop {
  const active = new Set<LoopSubject>();
  let frame: number | null = null;
  let last = 0;

  const tick = (time: number) => {
    frame = null;
    if (!scheduler.visible()) return;
    const t = Number.isFinite(time) ? time : scheduler.now();
    const dt = Math.min(MAX_DT, Math.max(0, (t - last) / 1000));
    last = t;
    for (const s of [...active]) {
      if (s.step(dt)) active.delete(s);
    }
    arm();
  };

  const arm = () => {
    if (frame !== null || active.size === 0 || !scheduler.visible()) return;
    frame = scheduler.request(tick);
  };

  return {
    wake(subject) {
      if (active.size === 0 || frame === null) last = scheduler.now();
      active.add(subject);
      arm();
    },
    remove(subject) {
      active.delete(subject);
      if (active.size === 0 && frame !== null) {
        scheduler.cancel(frame);
        frame = null;
      }
    },
    resume() {
      last = scheduler.now();
      arm();
    },
    get activeCount() {
      return active.size;
    },
    get running() {
      return frame !== null;
    }
  };
}

let shared: AvatarLoop | null = null;

/** The page-wide loop (lazily created; browser only). */
export function sharedAvatarLoop(): AvatarLoop {
  if (shared) return shared;
  const hasWindow = typeof window !== 'undefined' && typeof window.requestAnimationFrame === 'function';
  const now = () => (typeof performance !== 'undefined' ? performance.now() : Date.now());
  const loop = createAvatarLoop({
    request: (cb) => (hasWindow ? window.requestAnimationFrame(cb) : (setTimeout(() => cb(now()), 16) as unknown as number)),
    cancel: (id) => (hasWindow ? window.cancelAnimationFrame(id) : clearTimeout(id)),
    now,
    visible: () => typeof document === 'undefined' || !document.hidden
  });
  if (typeof document !== 'undefined') {
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) loop.resume();
    });
  }
  shared = loop;
  return loop;
}
