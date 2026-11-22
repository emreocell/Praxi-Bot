/**
 * DOM side of the avatar behaviour model. One controller per avatar composes a target pose and
 * critically damped springs carry the eyes and the head toward it (eyes lead, the body follows a
 * little later), advanced by ONE shared requestAnimationFrame loop that only runs while some
 * avatar is moving and the window is visible. A seeded schedule of blinks and idle actions runs
 * on timers; one shared rAF-throttled pointer listener serves every avatar that is hovered,
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
import {
  omegaForSettle,
  sharedAvatarLoop,
  springSettled,
  springStep,
  type LoopSubject,
  type SpringState
} from './avatarSpring.ts';

export interface AvatarElements {
  readonly root: HTMLElement;
  readonly stage: HTMLElement;
  readonly act: HTMLElement;
  readonly body: HTMLElement;
  readonly shadow: HTMLElement;
  /** Per-eye transform layer (turn + openness) and the blink layer inside it. */
  readonly eyes: readonly HTMLElement[];
  readonly lids: readonly HTMLElement[];
  /** Socket depth shading inside each eye (slides opposite to the turn). Optional. */
  readonly depths?: readonly HTMLElement[];
  /** The face layer (reduced motion fades it on pose changes). Optional. */
  readonly face?: HTMLElement | null;
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

const EASE_SOFT = 'cubic-bezier(0.37, 0, 0.2, 1)';
/** Legacy easing name kept so call sites read the same; motion is spring-driven now. */
const EASE_LOOK = 'look';

interface Pose {
  yaw: number;
  pitch: number;
  open: number;
  droop: number;
  tilt: number;
  lean: number;
}

/** Spring channels: the eyes and the head turn separately so the eyes can lead. */
type Channel = 'eyeYaw' | 'eyePitch' | 'headYaw' | 'headPitch' | 'open' | 'droop' | 'tilt';
const CHANNELS: readonly Channel[] = ['eyeYaw', 'eyePitch', 'headYaw', 'headPitch', 'open', 'droop', 'tilt'];

/** Head lags the eyes: a bit longer settle, capped so long returns do not drag. */
const headSettle = (ms: number) => Math.min(ms * 1.4, ms + 130);

export class AvatarLife implements LoopSubject {
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
  private readonly spring: Record<Channel, SpringState> = {
    eyeYaw: { x: 0, v: 0 },
    eyePitch: { x: 0, v: 0 },
    headYaw: { x: 0, v: 0 },
    headPitch: { x: 0, v: 0 },
    open: { x: 1, v: 0 },
    droop: { x: 0, v: 0 },
    tilt: { x: 0, v: 0 }
  };
  private readonly target: Record<Channel, number> = {
    eyeYaw: 0,
    eyePitch: 0,
    headYaw: 0,
    headPitch: 0,
    open: 1,
    droop: 0,
    tilt: 0
  };
  private readonly omega: Record<Channel, number> = {
    eyeYaw: 20,
    eyePitch: 20,
    headYaw: 15,
    headPitch: 15,
    open: 20,
    droop: 12,
    tilt: 10
  };
  private written = '';

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
    sharedAvatarLoop().remove(this);
    controllers.delete(this);
    syncListeners();
    this.hoverTarget?.removeEventListener('pointerenter', this.onEnter);
    this.hoverTarget?.removeEventListener('pointerleave', this.onExit);
  }

  update(face: AvatarFace, opts: AvatarLifeOptions): void {
    const prev = this.opts;
    const faceChanged =
      face.radius !== this.face.radius ||
      face.eyes.some((e, i) => e.x !== this.face.eyes[i]?.x || e.y !== this.face.eyes[i]?.y);
    this.face = face;
    this.opts = opts;
    if (prev.state !== opts.state) {
      this.onStateChange(prev.state, opts.state);
    }
    if (prev.alive !== opts.alive || prev.reducedMotion !== opts.reducedMotion || prev.state !== opts.state) {
      this.reschedule();
    }
    if (faceChanged) this.written = '';
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
        this.apply(650, EASE_SOFT);
      }
      return;
    }
    this.tracking = true;
    // Attenuate with distance so a far pointer only earns a small turn.
    const k = this.hovered ? 1 : Math.max(0.35, 1 - g.distance / 3.6);
    this.pointerGaze = { yaw: g.yaw * k, pitch: g.pitch * k };
    this.apply(220);
  }

  pointerGone(): void {
    if (this.tracking || this.hovered) {
      this.tracking = false;
      this.hovered = false;
      this.apply(650, EASE_SOFT);
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
    this.apply(650, EASE_SOFT);
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
      if (this.hovered && !this.sleepy) open = Math.max(open, 1.08);
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

  /**
   * Retarget the springs to the composed pose. `ms` is the settle time of the eyes; the head
   * follows a little later. Nothing ever jumps: a new target mid-flight bends the current motion.
   * Reduced motion (or `ms` 0) snaps, with a short fade of the face instead of a sweep.
   */
  apply(ms: number, ease: string = EASE_LOOK): void {
    const pose = this.targetPose();
    const t = this.target;
    t.eyeYaw = pose.yaw;
    t.eyePitch = pose.pitch;
    t.headYaw = pose.yaw;
    t.headPitch = pose.pitch;
    t.open = pose.open;
    t.droop = pose.droop;
    t.tilt = pose.tilt + pose.lean;
    if (this.opts.reducedMotion || ms <= 0 || this.inactive()) {
      const changed = CHANNELS.some((c) => Math.abs(this.spring[c].x - t[c]) > 0.5);
      for (const c of CHANNELS) {
        this.spring[c].x = t[c];
        this.spring[c].v = 0;
      }
      sharedAvatarLoop().remove(this);
      this.write();
      if (changed && this.opts.reducedMotion && this.mounted && ms > 0) {
        this.els.face?.animate?.([{ opacity: 0.2 }, { opacity: 1 }], { duration: 180, easing: 'ease-out' });
      }
      return;
    }
    // A long jump of the gaze gets a quicker, saccade-like eye move (still continuous).
    const jump = Math.hypot(t.eyeYaw - this.spring.eyeYaw.x, t.eyePitch - this.spring.eyePitch.x);
    const eyeMs = jump > 22 && ease !== EASE_SOFT ? Math.min(ms, 170) : ms;
    this.omega.eyeYaw = this.omega.eyePitch = omegaForSettle(eyeMs);
    this.omega.headYaw = this.omega.headPitch = omegaForSettle(headSettle(ms));
    this.omega.open = omegaForSettle(Math.max(160, Math.min(ms, 420)));
    this.omega.droop = omegaForSettle(Math.max(300, ms));
    this.omega.tilt = omegaForSettle(Math.max(420, headSettle(ms)));
    sharedAvatarLoop().wake(this);
  }

  /** One frame of the shared loop: advance every channel, write, report rest. */
  step(dtSec: number): boolean {
    if (this.destroyed) return true;
    if (this.inactive()) {
      for (const c of CHANNELS) {
        this.spring[c].x = this.target[c];
        this.spring[c].v = 0;
      }
      this.write();
      return true;
    }
    let settled = true;
    for (const c of CHANNELS) {
      const s = springStep(this.spring[c], this.target[c], this.omega[c], dtSec);
      if (springSettled(s, this.target[c], c === 'open' ? 0.002 : 0.02)) {
        s.x = this.target[c];
        s.v = 0;
      } else {
        settled = false;
      }
    }
    this.write();
    return settled;
  }

  /** Write the current spring values as inline transforms (transform only, no transitions). */
  private write(): void {
    const sp = this.spring;
    const key = CHANNELS.map((c) => sp[c].x.toFixed(3)).join(',');
    if (key === this.written) return;
    this.written = key;
    const { stage, body, eyes, depths } = this.els;
    const head = projectFace(sp.headYaw.x, sp.headPitch.x, this.face.eyes, this.face.center, this.face.radius);
    const look = projectFace(sp.eyeYaw.x, sp.eyePitch.x, this.face.eyes, this.face.center, this.face.radius);
    stage.style.transform = `rotate(${sp.tilt.x.toFixed(2)}deg)`;
    body.style.transform = `translateX(${head.bodyTx}%) scaleX(${head.bodySx}) skewY(${head.bodySkew}deg)`;
    const open = Math.max(0.04, sp.open.x);
    eyes.forEach((el, i) => {
      const p = look.eyes[i];
      if (!p) return;
      const side = i === 0 ? 1 : -1;
      const droop = (sp.droop.x * side).toFixed(2);
      el.style.transform = `translate(${p.tx}%, ${p.ty}%) rotate(${droop}deg) scale(${p.sx}, ${(p.sy * open).toFixed(3)})`;
    });
    // Socket depth: the far inner wall shows on the side the face turns away from.
    if (depths?.length) {
      const dx = (-sp.eyeYaw.x / 38) * 18;
      const dy = (-sp.eyePitch.x / 26) * 10;
      for (const d of depths) d.style.transform = `translate(${dx.toFixed(1)}%, ${dy.toFixed(1)}%)`;
    }
  }

  /** Current spring values (tests / gallery diagnostics). */
  poseSnapshot(): Readonly<Record<Channel, number>> {
    return Object.fromEntries(CHANNELS.map((c) => [c, this.spring[c].x])) as Record<Channel, number>;
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
    this.spring.open.x = 0.06;
    this.target.open = 0.06;
    this.write();
    this.later(120 + this.rand() * 520, () => {
      this.apply(320);
      this.oneShot('wake');
    });
  }
}
