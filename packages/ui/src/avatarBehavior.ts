/**
 * Pure behaviour model for the bot avatars: seeded idle scheduling, state expressions and the
 * "head turn" projection that slides and foreshortens the eyes as if they sat on a rounded body.
 * No DOM access here; `avatarLife.ts` applies the results.
 */
import type { BotAvatarState, BotShapeId } from './botShape.ts';

/** Small fast deterministic PRNG (mulberry32). Returns floats in [0, 1). */
export function seededRandom(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

/** Maximum head turn the face projection allows (degrees). */
export const MAX_YAW = 38;
export const MAX_PITCH = 26;

export type IdleActionKind = 'glance' | 'glance-back' | 'tilt' | 'settle' | 'shake-off' | 'look-around';

export interface IdleAction {
  readonly kind: IdleActionKind;
  /** Wait before the action starts (ms, unscaled). */
  readonly delayMs: number;
  /** Target head turn for glances (degrees). */
  readonly yaw: number;
  readonly pitch: number;
  /** Body tilt for curious tilts (degrees, positive = clockwise). */
  readonly tilt: number;
  /** How long the pose is held before returning (ms). */
  readonly holdMs: number;
}

/** Per-shape personality: how the body idles between actions. */
export type ShapeSway = 'tip' | 'float' | 'sway' | 'beat' | 'wobble' | 'none';

export function shapeSway(shape: BotShapeId): ShapeSway {
  switch (shape) {
    case 'drop':
      return 'tip';
    case 'ghost':
      return 'float';
    case 'cloud':
    case 'flower':
    case 'clover':
    case 'star':
      return 'sway';
    case 'heart':
      return 'beat';
    case 'gumdrop':
    case 'boulder':
    case 'pebble':
      return 'wobble';
    default:
      return 'none';
  }
}

/**
 * Pick the next idle action. Glances dominate (they read as "alive"), tilts and settles are rarer,
 * a shake-off is a treat. Delays are 2.4 - 7.5 s so a crowd never acts together.
 */
export function nextIdleAction(rand: () => number): IdleAction {
  const r = rand();
  const delayMs = Math.round(2400 + rand() * 5100);
  const side = rand() < 0.5 ? -1 : 1;
  if (r < 0.5) {
    return {
      kind: 'glance',
      delayMs,
      yaw: Math.round(side * (14 + rand() * 20)),
      pitch: Math.round(-8 + rand() * 14),
      tilt: 0,
      holdMs: Math.round(700 + rand() * 1600)
    };
  }
  if (r < 0.64) {
    return {
      kind: 'look-around',
      delayMs,
      yaw: Math.round(side * (18 + rand() * 14)),
      pitch: Math.round(-4 + rand() * 6),
      tilt: 0,
      holdMs: Math.round(500 + rand() * 500)
    };
  }
  if (r < 0.8) {
    return {
      kind: 'tilt',
      delayMs,
      yaw: Math.round(side * (4 + rand() * 6)),
      pitch: -4,
      tilt: Number((side * (5 + rand() * 4)).toFixed(1)),
      holdMs: Math.round(900 + rand() * 1200)
    };
  }
  if (r < 0.93) {
    return { kind: 'settle', delayMs, yaw: 0, pitch: 0, tilt: 0, holdMs: 0 };
  }
  return { kind: 'shake-off', delayMs, yaw: 0, pitch: 0, tilt: 0, holdMs: 0 };
}

/** The first `count` idle actions for a seed (deterministic; used by tests and the gallery). */
export function idleSchedule(seed: number, count: number): IdleAction[] {
  const rand = seededRandom(seed);
  return Array.from({ length: count }, () => nextIdleAction(rand));
}

export interface BlinkPlan {
  readonly delayMs: number;
  /** A quick double blink now and then. */
  readonly double: boolean;
}

/** Natural blink spacing: 2 - 6 s, slower when sleepy, ~15 % doubles. */
export function nextBlink(rand: () => number, sleepy = false): BlinkPlan {
  const base = 2000 + rand() * 4000;
  return { delayMs: Math.round(sleepy ? base * 1.8 : base), double: rand() < 0.15 };
}

/** Inactivity before an idle avatar dozes off (45 - 80 s, per seed). */
export function sleepAfterMs(seed: number): number {
  return Math.round(45000 + seededRandom(seed ^ 0x5bd1e995)() * 35000);
}

export interface AvatarExpression {
  /** Resting head turn for the state (degrees). */
  readonly yaw: number;
  readonly pitch: number;
  /** Eye openness (1 = normal, < 1 squint, > 1 wide). */
  readonly open: number;
  /** Eye droop: each eye rotates outwards by this many degrees (sad / tired). */
  readonly droop: number;
  /** Resting body tilt (degrees). */
  readonly tilt: number;
  /** Small eye saccades while the state lasts (thinking drifts, working scans). */
  readonly saccade: 'none' | 'drift-up' | 'scan';
}

/** Pure state -> expression mapping used by the controller and the reduced-motion pose. */
export function stateExpression(state: BotAvatarState, sleepy = false): AvatarExpression {
  switch (state) {
    case 'thinking':
      return { yaw: 16, pitch: -16, open: 0.94, droop: 0, tilt: 3, saccade: 'drift-up' };
    case 'working':
      return { yaw: 0, pitch: 7, open: 0.78, droop: 0, tilt: 0, saccade: 'scan' };
    case 'waiting-approval':
      return { yaw: 0, pitch: -3, open: 1.1, droop: 0, tilt: -4, saccade: 'none' };
    case 'error':
      return { yaw: -6, pitch: 10, open: 0.58, droop: 9, tilt: -3, saccade: 'none' };
    default:
      return sleepy
        ? { yaw: 0, pitch: 10, open: 0.3, droop: 4, tilt: 2, saccade: 'none' }
        : { yaw: 0, pitch: 0, open: 1, droop: 0, tilt: 0, saccade: 'none' };
  }
}

export interface GazeRect {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

export interface PointerGaze {
  readonly yaw: number;
  readonly pitch: number;
  /** Pointer distance from the avatar centre in avatar widths. */
  readonly distance: number;
}

/**
 * Head turn toward a pointer: eased, clamped to the face limits and calmer near the centre
 * so the eyes do not jitter when the pointer rests on the avatar.
 */
export function gazeFromPointer(
  pointerX: number,
  pointerY: number,
  rect: GazeRect | null | undefined,
  maxYaw = 32,
  maxPitch = 20
): PointerGaze {
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    return { yaw: 0, pitch: 0, distance: Infinity };
  }
  const cx = rect.left + rect.width / 2;
  const cy = rect.top + rect.height / 2;
  const nx = (pointerX - cx) / rect.width;
  const ny = (pointerY - cy) / rect.height;
  const ease = (v: number) => Math.sign(v) * (1 - Math.exp(-Math.abs(v) * 1.6));
  return {
    yaw: Number((clamp(ease(nx), -1, 1) * maxYaw).toFixed(2)),
    pitch: Number((clamp(ease(ny), -1, 1) * maxPitch).toFixed(2)),
    distance: Math.hypot(nx, ny)
  };
}

export interface EyeAnchor {
  /** Eye centre in the 64x64 viewBox. */
  readonly x: number;
  readonly y: number;
}

export interface EyeProjection {
  /** Translation in % of the avatar box. */
  readonly tx: number;
  readonly ty: number;
  /** Foreshortening (1 = facing the viewer). */
  readonly sx: number;
  readonly sy: number;
}

export interface FaceProjection {
  readonly eyes: readonly EyeProjection[];
  /** Subtle body yaw: horizontal shift (%), narrowing and skew (degrees). */
  readonly bodyTx: number;
  readonly bodySx: number;
  readonly bodySkew: number;
  /** Lean of the whole body toward the look direction (degrees). */
  readonly lean: number;
}

/**
 * Project the eyes onto a sphere of radius `radius` (viewBox units) centred at `center` and
 * turn it by yaw / pitch: eyes slide along the surface, foreshorten, and the far eye shrinks
 * more than the near one, so the face turns in space instead of sliding like a sticker.
 */
export function projectFace(
  yawDeg: number,
  pitchDeg: number,
  eyes: readonly EyeAnchor[],
  center: EyeAnchor,
  radius: number
): FaceProjection {
  const yaw = (clamp(yawDeg, -MAX_YAW, MAX_YAW) * Math.PI) / 180;
  const pitch = (clamp(pitchDeg, -MAX_PITCH, MAX_PITCH) * Math.PI) / 180;
  const r = Math.max(8, radius);
  const projected = eyes.map((eye) => {
    const ax = clamp((eye.x - center.x) / r, -0.95, 0.95);
    const ay = clamp((eye.y - center.y) / r, -0.95, 0.95);
    const theta = Math.asin(ax) + yaw;
    const phi = Math.asin(ay) + pitch;
    const nx = Math.sin(clamp(theta, -1.45, 1.45)) * r;
    const ny = Math.sin(clamp(phi, -1.45, 1.45)) * r;
    const sx = clamp(Math.cos(theta) / Math.max(0.3, Math.cos(Math.asin(ax))), 0.35, 1.05);
    const sy = clamp(Math.cos(phi) / Math.max(0.3, Math.cos(Math.asin(ay))), 0.5, 1.05);
    return {
      tx: Number((((nx - (eye.x - center.x)) / 64) * 100).toFixed(2)),
      ty: Number((((ny - (eye.y - center.y)) / 64) * 100).toFixed(2)),
      sx: Number(sx.toFixed(3)),
      sy: Number(sy.toFixed(3))
    };
  });
  const s = Math.sin(yaw);
  return {
    eyes: projected,
    bodyTx: Number((s * 2.2).toFixed(2)),
    bodySx: Number((1 - Math.abs(s) * 0.045).toFixed(3)),
    bodySkew: Number((-s * 3).toFixed(2)),
    lean: Number((lerp(0, 4.5, Math.abs(s)) * Math.sign(s)).toFixed(2))
  };
}
