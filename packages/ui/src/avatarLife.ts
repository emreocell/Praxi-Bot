/**
 * DOM side of the avatar behaviour model. One controller per avatar applies poses as inline
 * transforms with CSS transitions (no per-frame JS): a seeded schedule of blinks and idle actions
 * runs on timers, one shared rAF-throttled pointer listener serves every avatar that is hovered,
 * featured (tracks a near pointer) or asleep (wakes when the pointer comes near).
 */
import {
  gazeFromPointer,
  nextBlink,
  nextIdleAction,
  projectFace,
  seededRandom,
  sleepAfterMs,
  stateExpression,
  type AvatarExpression,
  type EyeAnchor,
  type IdleAction
} from './avatarBehavior.ts';
import type { BotAvatarState } from './botShape.ts';

export interface AvatarElements {
  readonly root: HTMLElement;
  readonly stage: HTMLElement;
  readonly act: HTMLElement;
  readonly body: HTMLElement;
  readonly shadow: HTMLElement;
  /** Per-eye transform layer (turn + openness) and the blink layer inside it. */
  readonly eyes: readonly HTMLElement[];
  readonly lids: readonly HTMLElement[];
}

export interface AvatarFace {
  readonly eyes: readonly EyeAnchor[];
  readonly center: EyeAnchor;
  readonly radius: number;
  readonly eyeTilt: number;
}

export interface AvatarLifeOptions {
  readonly seed: number;
  readonly state: BotAvatarState;
  /** Timers (blink, idle actions, sleep) run. */
  readonly alive: boolean;
  /** Track the pointer whenever it is near (featured avatars), not only on hover. */
  readonly trackNear: boolean;
  readonly reducedMotion: boolean;
  /** Fixed look direction (gallery / hero use); disables pointer and idle gaze. */
  readonly lookAt?: { readonly yaw: number; readonly pitch: number } | null;
}

/* ---------------- shared, document-wide state ---------------- */

let timeScale = 1;
let lastActivity = typeof performance !== 'undefined' ? performance.now() : 0;
const controllers = new Set<AvatarLife>();
let pointer: { x: number; y: number } | null = null;
let rafId: number | null = null;
let listening = false;

/** Speed up every avatar's schedule (gallery "watch it idle" mode). 1 = real time. */
export function setAvatarTimeScale(scale: number): void {
  timeScale = Math.max(0.25, Math.min(20, scale));
  for (const c of controllers) c.reschedule();
}

export function getAvatarTimeScale(): number {
  return timeScale;
}

function now(): number {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

function onPointerMove(e: PointerEvent): void {
  pointer = { x: e.clientX, y: e.clientY };
  lastActivity = now();
  if (rafId === null && typeof window !== 'undefined') {
    rafId = window.requestAnimationFrame(flushPointer);
  }
}

function onActivity(): void {
  lastActivity = now();
}

function onLeave(): void {
  pointer = null;
  for (const c of controllers) c.pointerGone();
}

function flushPointer(): void {
  rafId = null;
  if (!pointer) return;
  for (const c of controllers) c.pointerMoved(pointer.x, pointer.y);
}

function syncListeners(): void {
  if (typeof window === 'undefined') return;
  const want = controllers.size > 0;
  if (want && !listening) {
    listening = true;
    window.addEventListener('pointermove', onPointerMove, { passive: true });
    window.addEventListener('keydown', onActivity, { passive: true });
    document.documentElement.addEventListener('pointerleave', onLeave, { passive: true });
  } else if (!want && listening) {
    listening = false;
    window.removeEventListener('pointermove', onPointerMove);
    window.removeEventListener('keydown', onActivity);
    document.documentElement.removeEventListener('pointerleave', onLeave);
    if (rafId !== null) {
      window.cancelAnimationFrame(rafId);
      rafId = null;
    }
  }
}

/* ---------------- per-avatar controller ---------------- */

const EASE_LOOK = 'cubic-bezier(0.33, 0.9, 0.3, 1)';
const EASE_SOFT = 'cubic-bezier(0.37, 0, 0.2, 1)';

interface Pose {
  yaw: number;
  pitch: number;
  open: number;
  droop: number;
  tilt: number;
  lean: number;
}

export class AvatarLife {
  private els: AvatarElements;
  private face: AvatarFace;
  private opts: AvatarLifeOptions;
  private rand: () => number;
  private blinkTimer: ReturnType<typeof setTimeout> | null = null;
  private actionTimer: ReturnType<typeof setTimeout> | null = null;
  private returnTimer: ReturnType<typeof setTimeout> | null = null;
  private saccadeTimer: ReturnType<typeof setTimeout> | null = null;
  private hovered = false;
  private tracking = false;
  private sleepy = false;
  private glance: { yaw: number; pitch: number; tilt: number } | null = null;
  private saccade = { yaw: 0, pitch: 0 };
  private hoverTarget: Element | null = null;
  private readonly sleepAfter: number;
  private mounted = false;
  private destroyed = false;

  constructor(els: AvatarElements, face: AvatarFace, opts: AvatarLifeOptions) {
    this.els = els;
    this.face = face;
    this.opts = opts;
    this.rand = seededRandom(opts.seed);
    this.sleepAfter = sleepAfterMs(opts.seed);
    controllers.add(this);
    syncListeners();
    this.hoverTarget = els.root.closest('button, a, [role="button"], label') ?? els.root;
    this.hoverTarget.addEventListener('pointerenter', this.onEnter);
    this.hoverTarget.addEventListener('pointerleave', this.onExit);
    this.wake(true);
    this.reschedule();
  }

  destroy(): void {
    this.destroyed = true;
    this.clearTimers();
    controllers.delete(this);
    syncListeners();
    this.hoverTarget?.removeEventListener('pointerenter', this.onEnter);
    this.hoverTarget?.removeEventListener('pointerleave', this.onExit);
  }

  update(face: AvatarFace, opts: AvatarLifeOptions): void {
    const prev = this.opts;
    this.face = face;
    this.opts = opts;
    if (prev.state !== opts.state) {
      this.onStateChange(prev.state, opts.state);
    }
    if (prev.alive !== opts.alive || prev.reducedMotion !== opts.reducedMotion || prev.state !== opts.state) {
      this.reschedule();
    }
    this.apply(240);
  }

  /** Restart timers (state change, time scale change, visibility). */
  reschedule(): void {
    this.clearTimers();
    if (!this.running()) {
      this.glance = null;
      this.saccade = { yaw: 0, pitch: 0 };
      return;
    }
    this.scheduleBlink();
    this.scheduleAction();
    this.scheduleSaccade();
  }

  pointerMoved(x: number, y: number): void {
    if (this.opts.reducedMotion || this.opts.lookAt) return;
    const rect = this.els.root.getBoundingClientRect();
    const g = gazeFromPointer(x, y, rect);
    if (this.sleepy) {
      if (g.distance < 2.5) this.setSleepy(false);
      return;
    }
    const near = this.hovered || (this.opts.trackNear && this.opts.alive && g.distance < 3.2);
    if (!near) {
      if (this.tracking) {
        this.tracking = false;
        this.apply(520);
      }
      return;
    }
    this.tracking = true;
    // Attenuate with distance so a far pointer only earns a small turn.
    const k = this.hovered ? 1 : Math.max(0.35, 1 - g.distance / 3.6);
    this.pointerGaze = { yaw: g.yaw * k, pitch: g.pitch * k };
    this.apply(200);
  }

  pointerGone(): void {
    if (this.tracking || this.hovered) {
      this.tracking = false;
      this.hovered = false;
      this.apply(520);
    }
  }

  private pointerGaze = { yaw: 0, pitch: 0 };

  private onEnter = (e: Event): void => {
    if (this.opts.reducedMotion) return;
    this.hovered = true;
    if (this.sleepy) this.setSleepy(false);
    const pe = e as PointerEvent;
    if (typeof pe.clientX === 'number') this.pointerMoved(pe.clientX, pe.clientY);
  };

  private onExit = (): void => {
    this.hovered = false;
    this.tracking = false;
    this.apply(560);
  };

  private running(): boolean {
    return this.opts.alive && !this.opts.reducedMotion;
  }

  private inactive(): boolean {
    if (typeof document !== 'undefined' && document.hidden) return true;
    return this.els.root.hasAttribute('data-offscreen');
  }

  private clearTimers(): void {
    for (const t of [this.blinkTimer, this.actionTimer, this.returnTimer, this.saccadeTimer]) {
      if (t !== null) clearTimeout(t);
    }
    this.blinkTimer = this.actionTimer = this.returnTimer = this.saccadeTimer = null;
  }

  private later(ms: number, fn: () => void): ReturnType<typeof setTimeout> {
    return setTimeout(() => {
      if (!this.destroyed) fn();
    }, Math.max(16, ms / timeScale));
  }

  /* ---- pose composition ---- */

  private expression(): AvatarExpression {
    return stateExpression(this.opts.state, this.sleepy);
  }

  private targetPose(): Pose {
    const ex = this.expression();
    let yaw = ex.yaw;
    let pitch = ex.pitch;
    let tilt = ex.tilt;
    let open = ex.open;
    if (this.opts.lookAt) {
      yaw = this.opts.lookAt.yaw;
      pitch = this.opts.lookAt.pitch;
    } else if ((this.tracking || this.hovered) && !this.opts.reducedMotion) {
      yaw = this.pointerGaze.yaw;
      pitch = this.pointerGaze.pitch;
      if (this.hovered && !this.sleepy) open = Math.max(open, 1.1);
    } else if (this.glance) {
      yaw = this.glance.yaw;
      pitch = this.glance.pitch;
      tilt += this.glance.tilt;
    } else {
      yaw += this.saccade.yaw;
      pitch += this.saccade.pitch;
    }
    const lean = this.hovered && !this.opts.reducedMotion ? Math.max(-5, Math.min(5, yaw * 0.14)) : 0;
    return { yaw, pitch, open, droop: ex.droop, tilt, lean };
  }

  /** Write the composed pose as inline transforms; CSS transitions animate between poses. */
  apply(ms: number, ease = EASE_LOOK): void {
    const pose = this.targetPose();
    const duration = this.opts.reducedMotion ? 0 : Math.round(ms);
    const proj = projectFace(pose.yaw, pose.pitch, this.face.eyes, this.face.center, this.face.radius);
    const { stage, body, eyes } = this.els;
    const set = (el: HTMLElement, transform: string, d = duration, e = ease) => {
      el.style.transition = d > 0 ? `transform ${d}ms ${e}` : 'none';
      el.style.transform = transform;
    };
    set(stage, `rotate(${(pose.tilt + pose.lean).toFixed(2)}deg)`, Math.max(duration, this.opts.reducedMotion ? 0 : 380), EASE_SOFT);
    set(
      body,
      `translateX(${proj.bodyTx}%) scaleX(${proj.bodySx}) skewY(${proj.bodySkew}deg)`
    );
    eyes.forEach((el, i) => {
      const p = proj.eyes[i];
      if (!p) return;
      const side = i === 0 ? 1 : -1;
      const droop = pose.droop * side;
      set(
        el,
        `translate(${p.tx}%, ${p.ty}%) rotate(${droop}deg) scale(${p.sx}, ${(p.sy * pose.open).toFixed(3)})`
      );
    });
  }

  /* ---- schedule ---- */

  private scheduleBlink(): void {
    const plan = nextBlink(this.rand, this.sleepy);
    this.blinkTimer = this.later(plan.delayMs, () => {
      if (!this.inactive()) {
        this.blink();
        if (plan.double) this.later(260, () => this.blink());
      }
      this.scheduleBlink();
    });
  }

  private scheduleAction(): void {
    const action = nextIdleAction(this.rand);
    this.actionTimer = this.later(action.delayMs, () => {
      if (!this.inactive() && !this.opts.lookAt) {
        if (this.opts.state === 'idle' && !this.sleepy && now() - lastActivity > this.sleepAfter / timeScale) {
          this.setSleepy(true);
        } else if (!this.sleepy && !this.tracking && !this.hovered) {
          this.perform(action);
        }
      }
      this.scheduleAction();
    });
  }

  private scheduleSaccade(): void {
    const ex = this.expression();
    if (ex.saccade === 'none') return;
    const scan = ex.saccade === 'scan';
    const delay = scan ? 900 + this.rand() * 1100 : 1600 + this.rand() * 1800;
    this.saccadeTimer = this.later(delay, () => {
      const side = this.saccade.yaw > 0 ? -1 : 1;
      this.saccade = scan
        ? { yaw: side * (6 + this.rand() * 8), pitch: this.rand() * 3 }
        : { yaw: side * (6 + this.rand() * 10) - 8, pitch: -this.rand() * 6 };
      if (!this.inactive()) this.apply(scan ? 260 : 900, scan ? EASE_LOOK : EASE_SOFT);
      this.scheduleSaccade();
    });
  }

  private perform(action: IdleAction): void {
    if (this.opts.state !== 'idle' && action.kind !== 'settle') {
      // Busy states keep their own rhythm; only a rare glance at the viewer breaks it.
      if (this.rand() > 0.25) return;
      this.glance = { yaw: 0, pitch: -2, tilt: 0 };
      this.apply(420);
      this.returnTimer = this.later(900, () => {
        this.glance = null;
        this.apply(700, EASE_SOFT);
      });
      return;
    }
    switch (action.kind) {
      case 'glance':
      case 'tilt': {
        this.glance = { yaw: action.yaw, pitch: action.pitch, tilt: action.kind === 'tilt' ? action.tilt : 0 };
        this.apply(action.kind === 'tilt' ? 700 : 460, action.kind === 'tilt' ? EASE_SOFT : EASE_LOOK);
        this.returnTimer = this.later(action.holdMs, () => {
          this.glance = null;
          this.apply(760, EASE_SOFT);
        });
        break;
      }
      case 'look-around': {
        this.glance = { yaw: action.yaw, pitch: action.pitch, tilt: 0 };
        this.apply(420);
        this.returnTimer = this.later(action.holdMs + 420, () => {
          this.glance = { yaw: -action.yaw * 0.9, pitch: action.pitch, tilt: 0 };
          this.apply(620);
          this.blink();
          this.returnTimer = this.later(action.holdMs + 620, () => {
            this.glance = null;
            this.apply(700, EASE_SOFT);
          });
        });
        break;
      }
      case 'settle':
        this.oneShot('settle');
        break;
      case 'shake-off':
        this.oneShot('shake');
        break;
      case 'glance-back':
        break;
    }
  }

  private setSleepy(on: boolean): void {
    if (this.sleepy === on) return;
    this.sleepy = on;
    this.els.root.toggleAttribute('data-sleepy', on);
    if (on) {
      this.glance = null;
      this.apply(1600, EASE_SOFT);
    } else {
      lastActivity = now();
      this.apply(260);
      this.oneShot('wake');
    }
  }

  /* ---- one-shot motions (WAAPI on the act layer; never lift the body off the ground) ---- */

  blink(): void {
    if (this.opts.reducedMotion) return;
    for (const lid of this.els.lids) {
      lid.animate?.(
        [
          { transform: 'scaleY(1)' },
          { transform: 'scaleY(0.08)', offset: 0.45 },
          { transform: 'scaleY(0.08)', offset: 0.55 },
          { transform: 'scaleY(1)' }
        ],
        { duration: 170, easing: 'ease-in-out' }
      );
    }
  }

  oneShot(kind: 'wake' | 'settle' | 'shake' | 'error' | 'relief' | 'attention'): void {
    if (this.opts.reducedMotion) return;
    const { act, shadow } = this.els;
    if (typeof act.animate !== 'function') return;
    const frames: Record<typeof kind, { body: Keyframe[]; shadow: Keyframe[]; ms: number }> = {
      wake: {
        body: [
          { transform: 'scale(1.05, 0.9)' },
          { transform: 'scale(0.97, 1.04)', offset: 0.45 },
          { transform: 'scale(1.01, 0.99)', offset: 0.75 },
          { transform: 'scale(1, 1)' }
        ],
        shadow: [{ transform: 'scaleX(1.08)' }, { transform: 'scaleX(0.96)', offset: 0.45 }, { transform: 'scaleX(1)' }],
        ms: 760
      },
      settle: {
        body: [
          { transform: 'scale(1, 1)' },
          { transform: 'scale(1.045, 0.95)', offset: 0.35 },
          { transform: 'scale(0.99, 1.012)', offset: 0.7 },
          { transform: 'scale(1, 1)' }
        ],
        shadow: [{ transform: 'scaleX(1)' }, { transform: 'scaleX(1.06)', offset: 0.35 }, { transform: 'scaleX(1)' }],
        ms: 900
      },
      relief: {
        body: [
          { transform: 'scale(1, 1)' },
          { transform: 'scale(0.98, 1.03)', offset: 0.3 },
          { transform: 'scale(1.05, 0.94)', offset: 0.62 },
          { transform: 'scale(1, 1)' }
        ],
        shadow: [{ transform: 'scaleX(1)' }, { transform: 'scaleX(1.07)', offset: 0.62 }, { transform: 'scaleX(1)' }],
        ms: 1100
      },
      shake: {
        body: [
          { transform: 'rotate(0deg) scale(1, 1)' },
          { transform: 'rotate(-4deg) scale(1.02, 0.98)', offset: 0.18 },
          { transform: 'rotate(3.5deg) scale(1.02, 0.98)', offset: 0.36 },
          { transform: 'rotate(-2.4deg)', offset: 0.54 },
          { transform: 'rotate(1.2deg)', offset: 0.72 },
          { transform: 'rotate(0deg) scale(1, 1)' }
        ],
        shadow: [{ transform: 'scaleX(1)' }, { transform: 'scaleX(1.04)', offset: 0.36 }, { transform: 'scaleX(1)' }],
        ms: 720
      },
      error: {
        body: [
          { transform: 'translateX(0) rotate(0deg)' },
          { transform: 'translateX(-4%) rotate(-3deg)', offset: 0.16 },
          { transform: 'translateX(3.5%) rotate(2.5deg)', offset: 0.34 },
          { transform: 'translateX(-2%) rotate(-1.2deg)', offset: 0.52 },
          { transform: 'translateX(1%) rotate(0.5deg)', offset: 0.7 },
          { transform: 'translateX(0) rotate(0deg)' }
        ],
        shadow: [{ transform: 'translateX(0)' }, { transform: 'translateX(-3%)', offset: 0.16 }, { transform: 'translateX(2.5%)', offset: 0.34 }, { transform: 'translateX(0)' }],
        ms: 620
      },
      attention: {
        body: [
          { transform: 'scale(1, 1)' },
          { transform: 'scale(0.98, 1.035)', offset: 0.4 },
          { transform: 'scale(1, 1)' }
        ],
        shadow: [{ transform: 'scaleX(1)' }, { transform: 'scaleX(0.95)', offset: 0.4 }, { transform: 'scaleX(1)' }],
        ms: 640
      }
    };
    const f = frames[kind];
    act.animate(f.body, { duration: f.ms, easing: EASE_SOFT });
    shadow.animate?.(f.shadow, { duration: f.ms, easing: EASE_SOFT, composite: 'add' });
  }

  private onStateChange(from: BotAvatarState, to: BotAvatarState): void {
    this.glance = null;
    this.saccade = { yaw: 0, pitch: 0 };
    if (this.sleepy) this.setSleepy(false);
    if (to === 'error') this.oneShot('error');
    else if (to === 'idle' && from !== 'idle') {
      this.oneShot('relief');
      this.blink();
    } else if (to === 'waiting-approval') this.oneShot('attention');
    else this.oneShot('settle');
  }

  /** One-time wake-up on mount: eyes open from closed and the body stretches awake. */
  private wake(first: boolean): void {
    if (!first || this.mounted) return;
    this.mounted = true;
    if (this.opts.reducedMotion || !this.opts.alive) {
      this.apply(0);
      return;
    }
    // Start with closed eyes, then open after a seeded beat so a crowd wakes up unevenly.
    const { eyes } = this.els;
    const proj = projectFace(0, 0, this.face.eyes, this.face.center, this.face.radius);
    eyes.forEach((el, i) => {
      const p = proj.eyes[i];
      el.style.transition = 'none';
      el.style.transform = `translate(${p?.tx ?? 0}%, ${p?.ty ?? 0}%) scale(1, 0.06)`;
    });
    this.later(120 + this.rand() * 520, () => {
      this.apply(320);
      this.oneShot('wake');
    });
  }
}
