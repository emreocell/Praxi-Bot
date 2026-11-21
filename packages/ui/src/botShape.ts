import {
  SHAPE_SPECS,
  groundPoints,
  pointBounds,
  smoothClosedPath,
  type Point
} from './avatarShapes.ts';

/**
 * The 16 persisted character ids (+ Turkish prototype aliases). Order matters: the id fallback
 * is `hash % 16`, so never reorder, rename or remove an entry. Eye anchors are in a 64x64 viewBox.
 */
const SHAPE_IDS = [
  { id: 'pebble', alias: 'daire' },
  { id: 'cube', alias: 'kare' },
  { id: 'diamond', alias: 'elmas' },
  { id: 'cloud', alias: 'bulut' },
  { id: 'tri', alias: 'ucgen' },
  { id: 'flower', alias: 'cicek' },
  { id: 'gem', alias: 'altigen' },
  { id: 'drop', alias: 'damla' },
  { id: 'star', alias: 'yildiz' },
  { id: 'ghost', alias: 'hayalet' },
  { id: 'capsule', alias: 'kapsul' },
  { id: 'ears', alias: 'kulakli' },
  { id: 'gumdrop', alias: 'jole' },
  { id: 'heart', alias: 'kalp' },
  { id: 'clover', alias: 'yonca' },
  { id: 'boulder', alias: 'kaya' }
] as const;

type ShapeIdEntry = (typeof SHAPE_IDS)[number];

export type BotAvatarState = 'idle' | 'thinking' | 'working' | 'waiting-approval' | 'error';

export const BOT_AVATAR_STATES: readonly BotAvatarState[] = [
  'idle',
  'thinking',
  'working',
  'waiting-approval',
  'error'
] as const;

export interface BotShapeGeometry {
  /** Soft organic SVG silhouette path in a 64x64 viewBox, resting on y = 55.5. */
  readonly path: string;
  /** Baked lean of the silhouette (degrees); the eyes follow part of it. */
  readonly tilt: number;
  /** Tilt of the pill eyes (degrees). */
  readonly eyeTilt: number;
  /** Relative eye scale multiplier for narrow or roomy silhouettes. */
  readonly eyeScale: number;
  /** Centre and radii of the broad soft top-left specular. */
  readonly sheenCx: number;
  readonly sheenCy: number;
  readonly sheenRx: number;
  readonly sheenRy: number;
  /** Ground contact shadow horizontal radius (viewBox units). */
  readonly shadowRx: number;
  /** Silhouette bounds (viewBox units). */
  readonly minX: number;
  readonly maxX: number;
  readonly minY: number;
  readonly maxY: number;
  /** Centre and radius of the sphere the face sits on (for the head-turn projection). */
  readonly faceCx: number;
  readonly faceCy: number;
  readonly faceR: number;
}

/** Eye placement per shape: centre as fractions of the bounds, spacing and lean. */
interface FaceSpec {
  readonly fx: number;
  readonly fy: number;
  readonly eyeTilt: number;
  readonly eyeScale: number;
  readonly tilt: number;
}

const FACE: Readonly<Record<ShapeIdEntry['id'], FaceSpec>> = {
  pebble: { fx: 0.52, fy: 0.5, eyeTilt: -3, eyeScale: 1, tilt: -6 },
  cube: { fx: 0.52, fy: 0.47, eyeTilt: -4, eyeScale: 1, tilt: -5 },
  diamond: { fx: 0.5, fy: 0.5, eyeTilt: 4, eyeScale: 0.92, tilt: 5 },
  cloud: { fx: 0.52, fy: 0.6, eyeTilt: 0, eyeScale: 0.95, tilt: 0 },
  tri: { fx: 0.5, fy: 0.62, eyeTilt: 3, eyeScale: 0.94, tilt: 4 },
  flower: { fx: 0.5, fy: 0.5, eyeTilt: -3, eyeScale: 0.92, tilt: -4 },
  gem: { fx: 0.52, fy: 0.5, eyeTilt: -4, eyeScale: 0.98, tilt: -6 },
  drop: { fx: 0.4, fy: 0.66, eyeTilt: 6, eyeScale: 0.96, tilt: 8 },
  star: { fx: 0.5, fy: 0.54, eyeTilt: -5, eyeScale: 0.9, tilt: -7 },
  ghost: { fx: 0.52, fy: 0.4, eyeTilt: 2, eyeScale: 0.98, tilt: 0 },
  capsule: { fx: 0.53, fy: 0.46, eyeTilt: 7, eyeScale: 1, tilt: 11 },
  ears: { fx: 0.52, fy: 0.48, eyeTilt: -14, eyeScale: 0.98, tilt: -24 },
  gumdrop: { fx: 0.55, fy: 0.58, eyeTilt: 2, eyeScale: 1, tilt: 0 },
  heart: { fx: 0.5, fy: 0.42, eyeTilt: -3, eyeScale: 0.95, tilt: 0 },
  clover: { fx: 0.5, fy: 0.5, eyeTilt: -6, eyeScale: 0.92, tilt: -12 },
  boulder: { fx: 0.55, fy: 0.52, eyeTilt: 2, eyeScale: 1, tilt: 4 }
};

/** Eye centre spacing (viewBox units) before `eyeScale`. */
export const EYE_SPACING = 10.4;
/** Pill eye size (viewBox units) before `eyeScale`. */
export const EYE_WIDTH = 3.5;
export const EYE_HEIGHT = 7.6;

function polygonArea(points: readonly Point[]): number {
  let a = 0;
  for (let i = 0; i < points.length; i += 1) {
    const [x1, y1] = points[i];
    const [x2, y2] = points[(i + 1) % points.length];
    a += x1 * y2 - x2 * y1;
  }
  return Math.abs(a) / 2;
}

/** Scale every silhouette to a similar visual mass (sqrt(area) ~ 37), capped to the box. */
function normalizePoints(points: readonly Point[]): Point[] {
  const b = pointBounds(points);
  const target = 37 / Math.sqrt(polygonArea(points));
  const cap = Math.min(50 / (b.maxX - b.minX), 48 / (b.maxY - b.minY));
  const k = Math.min(target, cap);
  const cx = (b.minX + b.maxX) / 2;
  const cy = (b.minY + b.maxY) / 2;
  return groundPoints(points.map(([x, y]) => [cx + (x - cx) * k, cy + (y - cy) * k] as Point));
}

interface BuiltShape {
  readonly geometry: BotShapeGeometry;
  readonly leftEyeX: number;
  readonly rightEyeX: number;
  readonly eyeY: number;
}

const round1 = (v: number) => Math.round(v * 10) / 10;

function buildShape(id: ShapeIdEntry['id']): BuiltShape {
  const spec = SHAPE_SPECS[id];
  const points = normalizePoints(spec.points());
  const b = pointBounds(points);
  const face = FACE[id];
  const w = b.maxX - b.minX;
  const h = b.maxY - b.minY;
  const ex = b.minX + w * face.fx;
  const ey = b.minY + h * face.fy;
  const half = (EYE_SPACING * face.eyeScale) / 2;
  const t = (face.eyeTilt * Math.PI) / 180;
  return {
    leftEyeX: round1(ex - half * Math.cos(t)),
    rightEyeX: round1(ex + half * Math.cos(t)),
    eyeY: round1(ey),
    geometry: {
      path: smoothClosedPath(points, spec.tension ?? 1),
      tilt: face.tilt,
      eyeTilt: face.eyeTilt,
      eyeScale: face.eyeScale,
      sheenCx: round1(b.minX + w * 0.34),
      sheenCy: round1(b.minY + h * 0.27),
      sheenRx: round1(w * 0.3),
      sheenRy: round1(h * 0.22),
      shadowRx: round1(Math.min(24, Math.max(10, w * 0.4))),
      minX: round1(b.minX),
      maxX: round1(b.maxX),
      minY: round1(b.minY),
      maxY: round1(b.maxY),
      faceCx: round1((b.minX + b.maxX) / 2),
      faceCy: round1(b.minY + h * 0.52),
      faceR: round1(Math.max(w, h) * 0.56)
    }
  };
}

const BUILT = Object.fromEntries(SHAPE_IDS.map((s) => [s.id, buildShape(s.id)])) as Record<
  ShapeIdEntry['id'],
  BuiltShape
>;

/** All 16 character shapes (eye coordinates in a 64x64 viewBox). */
export const BOT_SHAPES = SHAPE_IDS.map((s) => ({
  id: s.id,
  alias: s.alias,
  leftEyeX: BUILT[s.id].leftEyeX,
  rightEyeX: BUILT[s.id].rightEyeX,
  eyeY: BUILT[s.id].eyeY
}));

export type BotShapeDef = (typeof BOT_SHAPES)[number];
export type BotShapeId = BotShapeDef['id'];

export const BOT_SHAPE_IDS: readonly BotShapeId[] = BOT_SHAPES.map((s) => s.id);

/** Procedural soft-clay silhouettes for all 16 character shapes (64x64 viewBox). */
export const BOT_SHAPE_GEOMETRY: Readonly<Record<BotShapeId, BotShapeGeometry>> = Object.fromEntries(
  SHAPE_IDS.map((s) => [s.id, BUILT[s.id].geometry])
) as Record<BotShapeId, BotShapeGeometry>;

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
  /** Lit top-left side of the body (warm, lighter, slightly less saturated). */
  readonly highlight: string;
  /** Mid-body true tint. */
  readonly mid: string;
  /** Core shadow on the side turned away from the light (keeps chroma, never muddy grey). */
  readonly shadow: string;
  /** Ambient occlusion near the ground and in the deepest edge. */
  readonly deepShadow: string;
  /** Broad soft specular tint (matte clay: wide and faint, never a glossy dot). */
  readonly sheen: string;
  readonly sheenOpacity: number;
  /** Thin top-left rim light just inside the silhouette. */
  readonly rimLight: string;
  readonly rimOpacity: number;
  /** Warm light bounced up from the ground onto the underside. */
  readonly bounceLight: string;
  /** Hairline contour that keeps very light bots crisp on light themes and dark bots on dark ones. */
  readonly contour: string;
  readonly contourOpacity: number;
  /** Felt/clay grain strength (overlay opacity). */
  readonly grainOpacity: number;
  /** Eye ink: near-black on every tint except very dark custom colours. */
  readonly eyeColor: string;
  /** Kept for API compatibility; the pill eyes no longer draw a catchlight. */
  readonly catchlightOpacity: number;
}

const EYE_INK = '#15161b';
const EYE_INK_LIGHT = '#f4f6fa';

function contrastOf(a: string, b: string): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/**
 * Derive a soft clay/felt material from any user-chosen hex tint: the tint fills the whole body,
 * light comes from the top-left, shadows shift hue instead of turning grey, and white / grey
 * tints get warm-grey shading so they still read as 3D on light and dark themes.
 */
export function botAvatarMaterial(color: string | null | undefined): BotAvatarMaterial {
  const base = normalizeHexColor(color);
  const [r, g, b] = parseHexRgb(base);
  const [h, s, l] = rgbToHsl(r, g, b);
  const lum = relativeLuminance(base);
  const isVeryLight = lum > 0.72;
  const isNeutral = s < 0.12;
  const darkEyes = contrastOf(EYE_INK, base) >= 4;

  // Shadows drift a little toward blue-violet (cool) and highlights toward warm, like painted clay.
  const toward = (target: number, amount: number) => {
    const d = ((target - h + 540) % 360) - 180;
    return h + Math.sign(d) * Math.min(Math.abs(d), amount);
  };
  const highlight = isVeryLight
    ? mixHex(base, '#fffdf9', 0.7)
    : hslToHex(isNeutral ? h : toward(55, 6), Math.min(1, s * 0.92), Math.min(0.95, l + (1 - l) * 0.34));
  // Darken by mixing toward a muted, cooler tone of the same hue (HSL darkening alone turns vivid).
  const darkTone = hslToHex(isNeutral ? h : toward(250, 12), isNeutral ? s : s * 0.78, Math.max(0.04, l * 0.42));
  const shadow = isVeryLight ? mixHex(base, '#aaa39f', 0.4) : mixHex(base, darkTone, 0.36);
  const deepShadow = isVeryLight ? mixHex(base, '#837c79', 0.52) : mixHex(base, darkTone, 0.62);
  const bounceLight = isVeryLight
    ? '#fff6ec'
    : hslToHex(isNeutral ? h : toward(40, 8), Math.min(1, s * 0.9), Math.min(0.9, l + (1 - l) * 0.2));
  const isVeryDark = lum < 0.03;

  return {
    base,
    highlight,
    mid: base,
    shadow,
    deepShadow,
    sheen: '#ffffff',
    sheenOpacity: isVeryLight ? 0.45 : isVeryDark ? 0.12 : 0.22,
    rimLight: mixHex(highlight, '#ffffff', 0.6),
    rimOpacity: isVeryLight ? 0.6 : isVeryDark ? 0.3 : 0.42,
    bounceLight,
    contour: isVeryDark ? mixHex(base, '#ffffff', 0.3) : mixHex(deepShadow, '#0f131a', 0.4),
    contourOpacity: isVeryLight ? 0.1 : isVeryDark ? 0.3 : 0.05,
    grainOpacity: isVeryLight ? 0.09 : isVeryDark ? 0.12 : 0.11,
    eyeColor: darkEyes ? EYE_INK : EYE_INK_LIGHT,
    catchlightOpacity: darkEyes ? 0.42 : 0
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
