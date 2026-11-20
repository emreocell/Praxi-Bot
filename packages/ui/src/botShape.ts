/** All 16 character shapes (eye coordinates in a 64x64 viewBox). */
export const BOT_SHAPES = [
  { id: 'pebble', alias: 'daire', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 32.5 },
  { id: 'cube', alias: 'kare', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 31.5 },
  { id: 'diamond', alias: 'elmas', leftEyeX: 26, rightEyeX: 38, eyeY: 32 },
  { id: 'cloud', alias: 'bulut', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 37 },
  { id: 'tri', alias: 'ucgen', leftEyeX: 26, rightEyeX: 38, eyeY: 39.5 },
  { id: 'flower', alias: 'cicek', leftEyeX: 26.5, rightEyeX: 37.5, eyeY: 32 },
  { id: 'gem', alias: 'altigen', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 32 },
  { id: 'drop', alias: 'damla', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 39 },
  { id: 'star', alias: 'yildiz', leftEyeX: 26.5, rightEyeX: 37.5, eyeY: 34 },
  { id: 'ghost', alias: 'hayalet', leftEyeX: 26, rightEyeX: 38, eyeY: 29.5 },
  { id: 'capsule', alias: 'kapsul', leftEyeX: 27.5, rightEyeX: 36.5, eyeY: 27.5 },
  { id: 'ears', alias: 'kulakli', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 36 },
  { id: 'gumdrop', alias: 'jole', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 34 },
  { id: 'heart', alias: 'kalp', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 29.5 },
  { id: 'clover', alias: 'yonca', leftEyeX: 26.5, rightEyeX: 37.5, eyeY: 32 },
  { id: 'boulder', alias: 'kaya', leftEyeX: 25.5, rightEyeX: 38.5, eyeY: 35.5 }
] as const;

export type BotShapeDef = (typeof BOT_SHAPES)[number];
export type BotShapeId = BotShapeDef['id'];

export const BOT_SHAPE_IDS: readonly BotShapeId[] = BOT_SHAPES.map((s) => s.id);

export type BotAvatarState = 'idle' | 'thinking' | 'working' | 'waiting-approval' | 'error';

export const BOT_AVATAR_STATES: readonly BotAvatarState[] = [
  'idle',
  'thinking',
  'working',
  'waiting-approval',
  'error'
] as const;

export interface BotShapeGeometry {
  /** Smooth sculptural SVG silhouette path in a 64x64 viewBox. */
  readonly path: string;
  /** Gentle organic cant (degrees) applied around (32, 32) while keeping studio lighting fixed. */
  readonly tilt: number;
  /** Subtle harmonious tilt for the eyes (degrees). */
  readonly eyeTilt: number;
  /** Relative eye scale multiplier for narrow or roomy silhouettes. */
  readonly eyeScale: number;
  /** Center of the soft top-left studio satin sheen. */
  readonly sheenCx: number;
  readonly sheenCy: number;
  readonly sheenRx: number;
  readonly sheenRy: number;
  /** Ground contact shadow horizontal radius (viewBox units). */
  readonly shadowRx: number;
}

/**
 * Handcrafted procedural SVG silhouettes for all 16 character shapes (64x64 viewBox).
 * Each silhouette stays centered with balanced visual mass from 24px (`xs`) to 104px (`hero`).
 */
export const BOT_SHAPE_GEOMETRY: Readonly<Record<BotShapeId, BotShapeGeometry>> = {
  pebble: {
    path: 'M 31.2 9.5 C 43.8 9.5, 54.8 20.8, 54.4 35.2 C 54.0 47.6, 45.2 54.8, 32.0 54.8 C 18.6 54.8, 9.6 47.2, 9.6 34.4 C 9.6 21.4, 19.4 9.5, 31.2 9.5 Z',
    tilt: 0,
    eyeTilt: 2,
    eyeScale: 1,
    sheenCx: 24,
    sheenCy: 21,
    sheenRx: 15,
    sheenRy: 11,
    shadowRx: 17
  },
  cube: {
    path: 'M 22 11.5 L 42 11.5 C 48.8 11.5, 52.5 15.2, 52.5 22 L 52.5 42 C 52.5 48.8, 48.8 52.5, 42 52.5 L 22 52.5 C 15.2 52.5, 11.5 48.8, 11.5 42 L 11.5 22 C 11.5 15.2, 15.2 11.5, 22 11.5 Z',
    tilt: -8,
    eyeTilt: -6,
    eyeScale: 1,
    sheenCx: 23,
    sheenCy: 20,
    sheenRx: 15,
    sheenRy: 11,
    shadowRx: 17
  },
  diamond: {
    path: 'M 32 7.2 C 35.8 7.2, 39.2 11.4, 45.4 18.2 C 51.4 24.8, 55.6 28.4, 55.6 32 C 55.6 35.6, 51.4 39.2, 45.4 45.8 C 39.2 52.6, 35.8 56.8, 32 56.8 C 28.2 56.8, 24.8 52.6, 18.6 45.8 C 12.6 39.2, 8.4 35.6, 8.4 32 C 8.4 28.4, 12.6 24.8, 18.6 18.2 C 24.8 11.4, 28.2 7.2, 32 7.2 Z',
    tilt: -3,
    eyeTilt: -2,
    eyeScale: 0.94,
    sheenCx: 25,
    sheenCy: 21,
    sheenRx: 13,
    sheenRy: 10,
    shadowRx: 13
  },
  cloud: {
    path: 'M 20.5 28.0 C 21.8 17.8, 29.8 13.0, 37.8 16.0 C 42.6 17.8, 45.4 21.6, 46.0 25.4 C 52.8 25.8, 57.6 31.8, 56.8 39.2 C 56.0 46.8, 49.4 51.6, 41.2 51.2 C 38.6 51.1, 36.6 50.4, 34.4 51.2 C 29.2 53.2, 22.8 52.8, 18.2 49.4 C 11.2 49.0, 6.8 43.2, 7.6 36.2 C 8.2 30.2, 13.8 26.8, 20.5 28.0 Z',
    tilt: 0,
    eyeTilt: 0,
    eyeScale: 0.96,
    sheenCx: 27,
    sheenCy: 24,
    sheenRx: 16,
    sheenRy: 10,
    shadowRx: 19
  },
  tri: {
    path: 'M 32 9.5 C 37.2 9.5, 42.2 16.5, 48.0 28.4 C 53.6 39.8, 56.2 46.2, 53.4 50.6 C 50.8 54.6, 43.4 55.2, 32 55.2 C 20.6 55.2, 13.2 54.6, 10.6 50.6 C 7.8 46.2, 10.4 39.8, 16.0 28.4 C 21.8 16.5, 26.8 9.5, 32 9.5 Z',
    tilt: 0,
    eyeTilt: 0,
    eyeScale: 0.94,
    sheenCx: 28,
    sheenCy: 24,
    sheenRx: 11,
    sheenRy: 11,
    shadowRx: 18
  },
  flower: {
    path: 'M 32 8.2 C 37.8 8.2, 41.0 13.0, 40.4 18.6 C 45.4 15.8, 51.6 17.6, 54.0 22.8 C 56.4 27.8, 53.8 32.8, 49.0 34.8 C 53.8 36.8, 56.2 42.2, 53.4 47.0 C 50.4 52.0, 44.2 53.2, 39.6 49.8 C 39.8 54.6, 36.6 57.6, 32 57.6 C 27.4 57.6, 24.2 54.6, 24.4 49.8 C 19.8 53.2, 13.6 52.0, 10.6 47.0 C 7.8 42.2, 10.2 36.8, 15.0 34.8 C 10.2 32.8, 7.6 27.8, 10.0 22.8 C 12.4 17.6, 18.6 15.8, 23.6 18.6 C 23.0 13.0, 26.2 8.2, 32 8.2 Z',
    tilt: 0,
    eyeTilt: 0,
    eyeScale: 0.92,
    sheenCx: 25,
    sheenCy: 22,
    sheenRx: 14,
    sheenRy: 11,
    shadowRx: 15
  },
  gem: {
    path: 'M 23.2 10.8 L 40.8 10.8 C 44.2 10.8, 46.2 12.2, 48.0 15.2 L 54.4 26.8 C 56.2 30.0, 56.2 34.0, 54.4 37.2 L 48.0 48.8 C 46.2 51.8, 44.2 53.2, 40.8 53.2 L 23.2 53.2 C 19.8 53.2, 17.8 51.8, 16.0 48.8 L 9.6 37.2 C 7.8 34.0, 7.8 30.0, 9.6 26.8 L 16.0 15.2 C 17.8 12.2, 19.8 10.8, 23.2 10.8 Z',
    tilt: -6,
    eyeTilt: -4,
    eyeScale: 0.98,
    sheenCx: 24,
    sheenCy: 20,
    sheenRx: 14,
    sheenRy: 11,
    shadowRx: 16
  },
  drop: {
    path: 'M 38.2 7.6 C 42.2 8.0, 43.2 12.4, 40.2 14.2 C 38.0 15.4, 37.6 17.2, 40.0 21.0 C 46.8 29.0, 53.8 35.8, 53.6 43.4 C 53.4 51.4, 44.6 55.6, 32.0 55.6 C 19.4 55.6, 10.6 51.2, 10.4 42.8 C 10.2 33.4, 18.2 24.4, 26.2 16.0 C 31.0 10.8, 34.8 7.2, 38.2 7.6 Z',
    tilt: 0,
    eyeTilt: 2,
    eyeScale: 0.98,
    sheenCx: 25,
    sheenCy: 27,
    sheenRx: 12,
    sheenRy: 11,
    shadowRx: 17
  },
  star: {
    path: 'M 32 8.2 C 35.8 8.2, 38.0 15.6, 40.8 20.4 C 46.2 21.2, 54.8 22.0, 56.2 25.8 C 57.6 29.6, 51.4 34.6, 47.6 38.2 C 48.6 44.0, 51.2 52.0, 47.6 54.6 C 44.0 57.2, 37.6 52.0, 32 49.8 C 26.4 52.0, 20.0 57.2, 16.4 54.6 C 12.8 52.0, 15.4 44.0, 16.4 38.2 C 12.6 34.6, 6.4 29.6, 7.8 25.8 C 9.2 22.0, 17.8 21.2, 23.2 20.4 C 26.0 15.6, 28.2 8.2, 32 8.2 Z',
    tilt: -5,
    eyeTilt: -2,
    eyeScale: 0.9,
    sheenCx: 26,
    sheenCy: 24,
    sheenRx: 13,
    sheenRy: 10,
    shadowRx: 16
  },
  ghost: {
    path: 'M 32 8.8 C 44.6 8.8, 50.2 18.8, 51.4 31.8 C 52.4 41.6, 56.2 46.8, 53.8 51.6 C 51.6 55.6, 46.2 54.2, 42.6 51.0 C 41.0 49.6, 39.4 50.2, 38.0 52.0 C 35.6 55.2, 33.8 56.2, 32.0 56.2 C 30.2 56.2, 28.4 55.2, 26.0 52.0 C 24.6 50.2, 23.0 49.6, 21.4 51.0 C 17.8 54.2, 12.4 55.6, 10.2 51.6 C 7.8 46.8, 11.6 41.6, 12.6 31.8 C 13.8 18.8, 19.4 8.8, 32 8.8 Z',
    tilt: 0,
    eyeTilt: 0,
    eyeScale: 0.98,
    sheenCx: 25,
    sheenCy: 20,
    sheenRx: 12,
    sheenRy: 11,
    shadowRx: 17
  },
  capsule: {
    path: 'M 32 8.2 C 41.8 8.2, 47.6 15.0, 47.6 25.0 L 47.6 39.0 C 47.6 49.0, 41.8 55.8, 32 55.8 C 22.2 55.8, 16.4 49.0, 16.4 39.0 L 16.4 25.0 C 16.4 15.0, 22.2 8.2, 32 8.2 Z',
    tilt: 13,
    eyeTilt: 9,
    eyeScale: 0.9,
    sheenCx: 26,
    sheenCy: 20,
    sheenRx: 10,
    sheenRy: 12,
    shadowRx: 13
  },
  ears: {
    path: 'M 20.8 10.6 C 25.2 10.2, 28.0 13.8, 29.2 17.0 C 31.0 16.6, 33.0 16.6, 34.8 17.0 C 36.0 13.8, 38.8 10.2, 43.2 10.6 C 48.4 11.2, 50.6 16.8, 48.6 22.2 C 53.6 26.4, 55.8 32.6, 54.8 39.6 C 53.6 49.0, 45.2 55.2, 32.0 55.2 C 18.8 55.2, 10.4 49.0, 9.2 39.6 C 8.2 32.6, 10.4 26.4, 15.4 22.2 C 13.4 16.8, 15.6 11.2, 20.8 10.6 Z',
    tilt: 0,
    eyeTilt: 0,
    eyeScale: 0.98,
    sheenCx: 24,
    sheenCy: 24,
    sheenRx: 15,
    sheenRy: 11,
    shadowRx: 18
  },
  gumdrop: {
    path: 'M 32 9.4 C 42.8 9.4, 48.6 17.8, 51.6 30.8 C 53.8 40.4, 54.4 47.4, 50.4 51.6 C 46.8 55.0, 40.2 55.6, 32.0 55.6 C 23.8 55.6, 17.2 55.0, 13.6 51.6 C 9.6 47.4, 10.2 40.4, 12.4 30.8 C 15.4 17.8, 21.2 9.4, 32 9.4 Z',
    tilt: 0,
    eyeTilt: 0,
    eyeScale: 0.98,
    sheenCx: 25,
    sheenCy: 21,
    sheenRx: 13,
    sheenRy: 11,
    shadowRx: 17
  },
  heart: {
    path: 'M 32 19.6 C 36.2 11.4, 46.4 9.6, 52.6 16.4 C 57.6 22.0, 57.2 31.2, 51.2 38.8 C 46.0 45.4, 38.2 52.4, 32.0 55.6 C 25.8 52.4, 18.0 45.4, 12.8 38.8 C 6.8 31.2, 6.4 22.0, 11.4 16.4 C 17.6 9.6, 27.8 11.4, 32 19.6 Z',
    tilt: -4,
    eyeTilt: -2,
    eyeScale: 0.96,
    sheenCx: 22,
    sheenCy: 22,
    sheenRx: 14,
    sheenRy: 10,
    shadowRx: 14
  },
  clover: {
    path: 'M 32 18.8 C 35.6 10.8, 45.8 9.8, 51.4 15.4 C 57.0 21.0, 56.0 31.2, 48.0 34.8 C 56.0 38.4, 57.0 48.6, 51.4 54.2 C 45.8 59.8, 35.6 58.8, 32 50.8 C 28.4 58.8, 18.2 59.8, 12.6 54.2 C 7.0 48.6, 8.0 38.4, 16.0 34.8 C 8.0 31.2, 7.0 21.0, 12.6 15.4 C 18.2 9.8, 28.4 10.8, 32 18.8 Z',
    tilt: -8,
    eyeTilt: -3,
    eyeScale: 0.92,
    sheenCx: 23,
    sheenCy: 22,
    sheenRx: 15,
    sheenRy: 11,
    shadowRx: 16
  },
  boulder: {
    path: 'M 24.0 10.0 C 31.6 9.8, 36.2 16.8, 42.6 19.6 C 50.6 23.0, 55.4 28.6, 55.0 39.2 C 54.6 49.4, 46.8 55.0, 31.8 55.0 C 16.4 55.0, 9.2 49.0, 9.2 38.0 C 9.2 25.0, 15.0 10.2, 24.0 10.0 Z',
    tilt: 0,
    eyeTilt: 2,
    eyeScale: 1,
    sheenCx: 23,
    sheenCy: 21,
    sheenRx: 14,
    sheenRy: 11,
    shadowRx: 18
  }
};

/** Default character colour swatches matching the v6 palette. */
export const CHARACTER_COLORS = [
  '#f4f4f5',
  '#b98a5e',
  '#f2545b',
  '#f5803e',
  '#f2b233',
  '#34c77b',
  '#2fc4b2',
  '#4c8dff',
  '#9b7bff',
  '#f25fa8',
  '#8e9099'
] as const;

const DEFAULT_TINT_COLOR = '#8e9099';

const SHAPE_BY_NAME = new Map<string, BotShapeDef>();
for (const shape of BOT_SHAPES) {
  SHAPE_BY_NAME.set(shape.id, shape);
  SHAPE_BY_NAME.set(shape.alias, shape);
}

/** Deterministic FNV-1a 32-bit hash for stable identity fallbacks. */
export function fnv1a32(input: string): number {
  let hash = 2166136261;
  for (const char of input) {
    hash = Math.imul(hash ^ (char.codePointAt(0) ?? 0), 16777619) >>> 0;
  }
  return hash;
}

/** Normalize an explicit shape id or prototype alias (`daire` -> `pebble`). */
export function normalizeBotShape(shape: string | null | undefined): BotShapeId | null {
  if (typeof shape !== 'string') {
    return null;
  }
  const key = shape.trim().toLowerCase();
  return SHAPE_BY_NAME.get(key)?.id ?? null;
}

/** Normalize any raw bot/run status string to a canonical `BotAvatarState`. */
export function normalizeBotAvatarState(state: string | null | undefined): BotAvatarState {
  if (typeof state !== 'string') {
    return 'idle';
  }
  const normalized = state.trim().toLowerCase().replace(/_/g, '-');
  if (
    normalized === 'thinking' ||
    normalized === 'working' ||
    normalized === 'waiting-approval' ||
    normalized === 'error'
  ) {
    return normalized;
  }
  return 'idle';
}

/** Deterministic fallback mapping from bot ID to one of the 16 shapes. */
export function fallbackBotShape(id: string): BotShapeId {
  return BOT_SHAPES[fnv1a32(id) % BOT_SHAPES.length].id;
}

/** Deterministic chromatic fallback tint when a caller omits `bot.color`. */
export function fallbackBotColor(id: string): string {
  const chromatic = CHARACTER_COLORS.slice(1, -1);
  return chromatic[fnv1a32(id) % chromatic.length];
}

/** Resolve character geometry (explicit shape when valid, else ID fallback). */
export function botShape(id: string, explicitShape?: string | null) {
  const hash = fnv1a32(id);
  const resolvedId = normalizeBotShape(explicitShape) ?? BOT_SHAPES[hash % BOT_SHAPES.length].id;
  const def = SHAPE_BY_NAME.get(resolvedId) ?? BOT_SHAPES[0];
  return {
    hash,
    id: def.id,
    alias: def.alias,
    leftEyeX: def.leftEyeX,
    rightEyeX: def.rightEyeX,
    eyeY: def.eyeY
  };
}

/** Normalize a CSS hex color (`#rgb`, `#rrggbb`, `#rrggbbaa`) or fall back safely. */
export function normalizeHexColor(color: string | null | undefined): string {
  if (typeof color !== 'string') {
    return DEFAULT_TINT_COLOR;
  }
  const trimmed = color.trim().toLowerCase();
  const short = /^#([0-9a-f]{3})$/.exec(trimmed);
  if (short) {
    const [r, g, b] = short[1];
    return `#${r}${r}${g}${g}${b}${b}`;
  }
  const full = /^#([0-9a-f]{6})(?:[0-9a-f]{2})?$/.exec(trimmed);
  if (full) {
    return `#${full[1]}`;
  }
  return DEFAULT_TINT_COLOR;
}

function parseHexRgb(hex: string): [number, number, number] {
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16)
  ];
}

function formatHexRgb(r: number, g: number, b: number): string {
  const clamp = (n: number) => Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, '0');
  return `#${clamp(r)}${clamp(g)}${clamp(b)}`;
}

function mixHex(a: string, b: string, t: number): string {
  const [ar, ag, ab] = parseHexRgb(a);
  const [br, bg, bb] = parseHexRgb(b);
  return formatHexRgb(ar + (br - ar) * t, ag + (bg - ag) * t, ab + (bb - ab) * t);
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  if (max === min) {
    return [0, 0, l];
  }
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = 0;
  if (max === rn) h = ((gn - bn) / d + (gn < bn ? 6 : 0)) / 6;
  else if (max === gn) h = ((bn - rn) / d + 2) / 6;
  else h = ((rn - gn) / d + 4) / 6;
  return [h * 360, s, l];
}

function hslToHex(h: number, s: number, l: number): string {
  const hn = ((h % 360) + 360) % 360;
  const sc = Math.max(0, Math.min(1, s));
  const lc = Math.max(0, Math.min(1, l));
  const c = (1 - Math.abs(2 * lc - 1)) * sc;
  const x = c * (1 - Math.abs(((hn / 60) % 2) - 1));
  const m = lc - c / 2;
  let rn = 0;
  let gn = 0;
  let bn = 0;
  if (hn < 60) [rn, gn, bn] = [c, x, 0];
  else if (hn < 120) [rn, gn, bn] = [x, c, 0];
  else if (hn < 180) [rn, gn, bn] = [0, c, x];
  else if (hn < 240) [rn, gn, bn] = [0, x, c];
  else if (hn < 300) [rn, gn, bn] = [x, 0, c];
  else [rn, gn, bn] = [c, 0, x];
  return formatHexRgb((rn + m) * 255, (gn + m) * 255, (bn + m) * 255);
}

function relativeLuminance(hex: string): number {
  const [r, g, b] = parseHexRgb(hex).map((v) => {
    const c = v / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export interface BotAvatarMaterial {
  /** Normalized `#rrggbb` tint colour chosen for the bot. */
  readonly base: string;
  /** Soft warm-lit crown highlight. */
  readonly highlight: string;
  /** Mid-body true tint. */
  readonly mid: string;
  /** Rich, hue-shifted lower-body shadow (preserves chroma instead of muddying). */
  readonly shadow: string;
  /** Subtle deep edge occlusion. */
  readonly deepShadow: string;
  /** Soft diffused upper-left studio sheen tint. */
  readonly sheen: string;
  /** Opacity of the diffused sheen layer (lower on white/light tints to avoid glare). */
  readonly sheenOpacity: number;
  /** Top-left inner rim highlight tint. */
  readonly rimLight: string;
  /** Opacity of the inner rim highlight. */
  readonly rimOpacity: number;
  /** Subtle ambient bounce light at the bottom curve. */
  readonly bounceLight: string;
  /** Delicate contour stroke that keeps light bots crisp on Clay light and dark bots crisp on dark themes. */
  readonly contour: string;
  readonly contourOpacity: number;
  /** High-contrast eye ink adapted to the bot's tint luminance. */
  readonly eyeColor: string;
  /** Subtle eye catchlight opacity (0 for light-on-dark eyes). */
  readonly catchlightOpacity: number;
}

/**
 * Derive a cohesive soft satin-matte material palette from any user-chosen hex tint.
 * Avoids cheap glossy-plastic glare and muddy grey shadows across light and dark themes.
 */
export function botAvatarMaterial(color: string | null | undefined): BotAvatarMaterial {
  const base = normalizeHexColor(color);
  const [r, g, b] = parseHexRgb(base);
  const [h, s, l] = rgbToHsl(r, g, b);
  const lum = relativeLuminance(base);

  const isVeryLight = lum > 0.76;
  const isVeryDark = lum < 0.085;
  const isNeutral = s < 0.1;

  // Hue-shift highlights gently warmer and shadows slightly cooler/deeper for lively ceramic depth.
  const highlightHue = isNeutral ? h : h - 4;
  const shadowHue = isNeutral ? h : h + 7;

  const highlight = isVeryLight
    ? mixHex(base, '#ffffff', 0.45)
    : hslToHex(highlightHue, Math.min(1, s * 0.92), Math.min(0.95, l + (1 - l) * 0.24));

  const mid = base;

  const shadow = isVeryLight
    ? mixHex(base, '#b8bdc9', 0.34)
    : hslToHex(
        shadowHue,
        isNeutral ? s : Math.min(1, s * 1.08 + 0.04),
        Math.max(0.08, l * 0.79)
      );

  const deepShadow = isVeryLight
    ? mixHex(base, '#949aa8', 0.42)
    : hslToHex(
        shadowHue + 4,
        isNeutral ? s : Math.min(1, s * 1.12 + 0.05),
        Math.max(0.05, l * 0.64)
      );

  const sheen = mixHex(highlight, '#fffdf7', isVeryLight ? 0.65 : 0.72);
  const sheenOpacity = isVeryLight ? 0.22 : isVeryDark ? 0.2 : 0.28;

  const rimLight = mixHex(highlight, '#ffffff', 0.78);
  const rimOpacity = isVeryLight ? 0.55 : isVeryDark ? 0.32 : 0.44;

  const bounceLight = isVeryLight
    ? '#ffffff'
    : hslToHex(h - 6, Math.min(1, s * 0.7), Math.min(0.92, l + 0.22));

  const contour = isVeryDark
    ? mixHex(base, '#ffffff', 0.26)
    : mixHex(deepShadow, '#0f131a', isVeryLight ? 0.22 : 0.32);
  const contourOpacity = isVeryLight ? 0.2 : isVeryDark ? 0.28 : 0.16;

  // Adaptive high-contrast eye ink: rich deep hue-tinted obsidian on normal/light bots,
  // crisp warm off-white on very dark custom tints.
  const eyeColor = isVeryDark ? '#f4f6fa' : mixHex('#141820', deepShadow, 0.14);
  const catchlightOpacity = isVeryDark ? 0 : 0.42;

  return {
    base,
    highlight,
    mid,
    shadow,
    deepShadow,
    sheen,
    sheenOpacity,
    rimLight,
    rimOpacity,
    bounceLight,
    contour,
    contourOpacity,
    eyeColor,
    catchlightOpacity
  };
}

export interface BotAvatarTiming {
  /** Ambient breathing duration in seconds (3.8s - 5.5s). */
  readonly breathDurationSec: number;
  /** Negative start offset in seconds so a list of bots never moves in lockstep. */
  readonly breathDelaySec: number;
  /** Natural blink cycle duration in seconds (4.3s - 7.1s). */
  readonly blinkDurationSec: number;
  /** Negative blink start offset in seconds so eyes never blink in unison. */
  readonly blinkDelaySec: number;
}

/**
 * Compute deterministic, slightly irregular breathing and blinking timings for a bot ID
 * so a sidebar or grid of bots feels organic rather than synchronized.
 */
export function botAvatarTiming(id: string): BotAvatarTiming {
  const h1 = fnv1a32(id);
  const h2 = fnv1a32(`${id}:blink`);
  const breathDurationSec = Number((3.8 + ((h1 % 18) * 0.1)).toFixed(2));
  const breathPhase = ((h1 >>> 8) % 100) / 100;
  const breathDelaySec = Number((-breathPhase * breathDurationSec).toFixed(2));

  const blinkDurationSec = Number((4.3 + ((h2 % 29) * 0.1)).toFixed(2));
  const blinkPhase = ((h2 >>> 8) % 100) / 100;
  const blinkDelaySec = Number((-blinkPhase * blinkDurationSec).toFixed(2));

  return {
    breathDurationSec,
    breathDelaySec,
    blinkDurationSec,
    blinkDelaySec
  };
}

export type BotAvatarSize = 'xs' | 'sm' | 'md' | 'lg' | 'panel' | 'hero';

export interface AvatarMotionConfig {
  /** Whether subtle idle breathing runs when in `idle` state. */
  readonly breathe: boolean;
  /** Whether natural blinking runs. */
  readonly blink: boolean;
  /** Whether pointer look-at-cursor gaze attaches (featured avatars only, never lists). */
  readonly gaze: boolean;
  /** Effective state rendered (always preserved for static expression under reduced motion). */
  readonly state: BotAvatarState;
  /** True when animations must stay static due to `prefers-reduced-motion`. */
  readonly reducedMotion: boolean;
}

/**
 * Resolve motion flags cleanly from size, state, explicit `live` override and reduced-motion preference.
 * Ensures lists never attach cursor-tracking JS and `prefers-reduced-motion` yields a static pose.
 */
export function resolveAvatarMotion(options: {
  size: BotAvatarSize;
  state: BotAvatarState;
  live?: boolean;
  prefersReducedMotion: boolean;
}): AvatarMotionConfig {
  const { size, state, live, prefersReducedMotion } = options;
  if (prefersReducedMotion) {
    return {
      breathe: false,
      blink: false,
      gaze: false,
      state,
      reducedMotion: true
    };
  }
  const isFeatured = size === 'lg' || size === 'panel' || size === 'hero';
  // Breathing runs on idle avatars by default (including sidebar lists with staggered timing),
  // unless a caller explicitly sets `live={false}` (e.g. the 16-shape picker grid).
  const breathe = state === 'idle' && live !== false;
  // Cursor-tracking gaze runs ONLY on featured avatars (hero/panel/lg) or explicit `live={true}`,
  // never on list items (`xs`/`sm`/`md`) so lists have zero per-frame JS overhead.
  const gaze = state === 'idle' && (live ?? isFeatured);
  const blink = live !== false;

  return {
    breathe,
    blink,
    gaze,
    state,
    reducedMotion: false
  };
}

export interface GradientMapConfig {
  /** Normalized `#rrggbb` tint colour used to build the ramp. */
  color: string;
  /** Luminance-desaturation color matrix values. */
  luminanceMatrix: string;
  /** Contrast stretch slope before gradient lookup. */
  slope: string;
  /** Contrast stretch intercept before gradient lookup. */
  intercept: string;
  /** 5-stop feFuncR tableValues string. */
  tableR: string;
  /** 5-stop feFuncG tableValues string. */
  tableG: string;
  /** 5-stop feFuncB tableValues string. */
  tableB: string;
  /** Raw 5 hex stops from shadow to highlight. */
  stops: readonly [string, string, string, string, string];
}

/**
 * Legacy sRGB 5-stop gradient-map filter helper kept for backwards-compatible exports.
 */
export function gradientMapConfig(color: string | null | undefined): GradientMapConfig {
  const base = normalizeHexColor(color);
  const [r, g, b] = parseHexRgb(base);
  const saturatedShadow = formatHexRgb((r * r) / 255, (g * g) / 255, (b * b) / 255);
  const stops: [string, string, string, string, string] = [
    mixHex(saturatedShadow, '#000000', 0.38),
    mixHex(base, saturatedShadow, 0.8),
    mixHex(base, saturatedShadow, 0.38),
    base,
    mixHex(base, '#ffffff', 0.42)
  ];
  const channelTable = (index: 0 | 1 | 2) =>
    stops.map((stop) => (parseHexRgb(stop)[index] / 255).toFixed(3)).join(' ');
  return {
    color: base,
    luminanceMatrix: '.3 .59 .11 0 0 .3 .59 .11 0 0 .3 .59 .11 0 0 0 0 0 1 0',
    slope: '2.38',
    intercept: '-1.31',
    tableR: channelTable(0),
    tableG: channelTable(1),
    tableB: channelTable(2),
    stops
  };
}
