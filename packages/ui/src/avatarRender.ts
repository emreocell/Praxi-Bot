/**
 * Photoreal clay "render" skin: which pre-rendered eyeless clay body each of the 16 persisted
 * shape ids uses, where that body sits in the avatar's 64x64 box, where its face is, and how a
 * swatch colour is turned into the multiply / lift tints that recolour the neutral render.
 * Pure data and maths (no asset imports) so node tests can check it; the webp URLs live in
 * `avatarRenderAssets.ts`.
 */
import type { BotShapeId } from './botShape.ts';
import { EYE_SPACING } from './botShape.ts';

/** File names (without `.webp`) of the 16 renders in `assets/renders/`. */
export const RENDER_NAMES = [
  'cloud',
  'clover',
  'diamond',
  'drop',
  'egg',
  'flower',
  'ghost',
  'gumdrop',
  'heart',
  'hexgem',
  'pillow',
  'sphere',
  'squircle',
  'star',
  'tiltcube',
  'tri'
] as const;

export type RenderName = (typeof RENDER_NAMES)[number];

/** Persisted shape id -> render. Ids never change; only the art behind them does. */
export const RENDER_FOR_SHAPE: Readonly<Record<BotShapeId, RenderName>> = {
  pebble: 'sphere',
  cube: 'squircle',
  diamond: 'diamond',
  cloud: 'cloud',
  tri: 'tri',
  flower: 'flower',
  gem: 'hexgem',
  drop: 'drop',
  star: 'star',
  ghost: 'ghost',
  capsule: 'pillow',
  ears: 'tiltcube',
  gumdrop: 'gumdrop',
  heart: 'heart',
  clover: 'clover',
  boulder: 'egg'
};

/** Pixel size of every render (square, transparent). */
export const RENDER_PX = 512;

interface RenderMeta {
  /** Alpha bounding box in render pixels: [left, top, right, bottom]. */
  readonly box: readonly [number, number, number, number];
  /** Opaque area in render pixels (alpha-weighted). */
  readonly area: number;
  /** Eye-pair centre as fractions of the bounding box. */
  readonly fx: number;
  readonly fy: number;
  /** Eye lean (degrees, + clockwise) following the body's own lean. */
  readonly eyeTilt: number;
  /** Eye size / spacing multiplier. */
  readonly eyeScale: number;
  /** Brightness gain so all bodies read equally light (sphere is rendered darker). */
  readonly gain: number;
  /** Opaque span of the body on the eye row (fractions of the box width), measured from the alpha. */
  readonly span: readonly [number, number];
}

/**
 * Measured from the webp files (alpha bbox and area) and face anchors tuned by eye on
 * screenshots so both eyes sit on the visible front surface.
 */
const META: Readonly<Record<RenderName, RenderMeta>> = {
  cloud: { box: [12, 77, 500, 435], area: 122071, fx: 0.5, fy: 0.6, eyeTilt: 0, eyeScale: 0.96, gain: 1.02, span: [0.012, 0.986] },
  clover: { box: [12, 16, 500, 496], area: 192700, fx: 0.5, fy: 0.5, eyeTilt: 0, eyeScale: 0.94, gain: 1, span: [0.113, 0.883] },
  diamond: { box: [64, 12, 447, 500], area: 108759, fx: 0.5, fy: 0.5, eyeTilt: 0, eyeScale: 0.88, gain: 1, span: [0.008, 0.99] },
  drop: { box: [60, 12, 451, 500], area: 118302, fx: 0.42, fy: 0.68, eyeTilt: 6, eyeScale: 1.04, gain: 1, span: [0.008, 0.99] },
  egg: { box: [77, 12, 435, 500], area: 130744, fx: 0.5, fy: 0.5, eyeTilt: 0, eyeScale: 1.02, gain: 1, span: [0.034, 0.969] },
  flower: { box: [27, 12, 485, 500], area: 155643, fx: 0.5, fy: 0.5, eyeTilt: 0, eyeScale: 0.92, gain: 1.01, span: [0.092, 0.906] },
  ghost: { box: [19, 12, 493, 500], area: 166216, fx: 0.5, fy: 0.44, eyeTilt: 0, eyeScale: 0.98, gain: 1.02, span: [0.093, 0.884] },
  gumdrop: { box: [12, 21, 500, 490], area: 172276, fx: 0.55, fy: 0.5, eyeTilt: 4, eyeScale: 1.08, gain: 1.01, span: [0.041, 0.98] },
  heart: { box: [12, 50, 500, 461], area: 141567, fx: 0.5, fy: 0.44, eyeTilt: 0, eyeScale: 0.96, gain: 1.01, span: [0.012, 0.984] },
  hexgem: { box: [12, 27, 500, 485], area: 166511, fx: 0.5, fy: 0.5, eyeTilt: 0, eyeScale: 0.98, gain: 1.02, span: [0.004, 0.992] },
  pillow: { box: [12, 51, 500, 461], area: 163369, fx: 0.5, fy: 0.5, eyeTilt: 4, eyeScale: 1.08, gain: 1, span: [0.01, 0.992] },
  sphere: { box: [15, 12, 497, 500], area: 177497, fx: 0.52, fy: 0.48, eyeTilt: 0, eyeScale: 1.08, gain: 1.1, span: [0.006, 0.992] },
  squircle: { box: [12, 13, 500, 499], area: 188633, fx: 0.5, fy: 0.46, eyeTilt: -2, eyeScale: 1.08, gain: 1, span: [0.006, 0.957] },
  star: { box: [12, 24, 500, 488], area: 135198, fx: 0.5, fy: 0.55, eyeTilt: 0, eyeScale: 0.9, gain: 1.01, span: [0.068, 0.932] },
  tiltcube: { box: [18, 12, 494, 500], area: 149786, fx: 0.54, fy: 0.48, eyeTilt: -10, eyeScale: 1.06, gain: 1.02, span: [0.006, 0.966] },
  tri: { box: [12, 31, 500, 481], area: 138045, fx: 0.5, fy: 0.64, eyeTilt: 0, eyeScale: 0.92, gain: 1.02, span: [0.08, 0.914] }
};

/** Same visual-mass rule as the procedural silhouettes (sqrt(area) ~ 37, capped to the box). */
const TARGET_MASS = 37;
const MAX_W = 50;
const MAX_H = 48;
/** Ground line the bodies rest on (64x64 viewBox units). */
export const RENDER_GROUND = 55.5;

export interface RenderLayout {
  readonly render: RenderName;
  /** Image square placement in viewBox units (64 = full avatar box). */
  readonly imgX: number;
  readonly imgY: number;
  readonly imgSize: number;
  /** Body bounds in viewBox units. */
  readonly minX: number;
  readonly maxX: number;
  readonly minY: number;
  readonly maxY: number;
  readonly leftEyeX: number;
  readonly rightEyeX: number;
  readonly eyeY: number;
  readonly eyeTilt: number;
  readonly eyeScale: number;
  /** Sphere the face is projected on for the head turn. */
  readonly faceCx: number;
  readonly faceCy: number;
  readonly faceR: number;
  readonly shadowRx: number;
  readonly gain: number;
}

const r2 = (v: number) => Math.round(v * 100) / 100;

/** Where the render for `shape` sits in the 64x64 avatar box, and where its eyes go. */
export function renderLayout(shape: BotShapeId): RenderLayout {
  const render = RENDER_FOR_SHAPE[shape];
  const m = META[render];
  const [x0, y0, x1, y1] = m.box;
  const wPx = x1 - x0;
  const hPx = y1 - y0;
  const k = Math.min(TARGET_MASS / Math.sqrt(m.area), MAX_W / wPx, MAX_H / hPx);
  const imgSize = RENDER_PX * k;
  const imgX = 32 - ((x0 + x1) / 2) * k;
  const imgY = RENDER_GROUND - y1 * k;
  const minX = imgX + x0 * k;
  const maxX = imgX + x1 * k;
  const minY = imgY + y0 * k;
  const maxY = imgY + y1 * k;
  const w = maxX - minX;
  const h = maxY - minY;
  const ex = minX + w * m.fx;
  const ey = minY + h * m.fy;
  const half = (EYE_SPACING * m.eyeScale) / 2;
  const t = (m.eyeTilt * Math.PI) / 180;
  return {
    render,
    imgX: r2(imgX),
    imgY: r2(imgY),
    imgSize: r2(imgSize),
    minX: r2(minX),
    maxX: r2(maxX),
    minY: r2(minY),
    maxY: r2(maxY),
    leftEyeX: r2(ex - half * Math.cos(t)),
    rightEyeX: r2(ex + half * Math.cos(t)),
    eyeY: r2(ey),
    eyeTilt: m.eyeTilt,
    eyeScale: m.eyeScale,
    // The head turn projects the eyes on a sphere as wide as the body at eye height, so even a
    // full turn keeps the far eye on the visible surface instead of sliding off the silhouette.
    faceCx: r2(minX + w * ((m.span[0] + m.span[1]) / 2)),
    faceCy: r2(ey),
    faceR: r2(Math.max(8, w * ((m.span[1] - m.span[0]) / 2) * 0.88)),
    shadowRx: r2(Math.min(24, Math.max(10, w * 0.4))),
    gain: m.gain
  };
}

/* ---------------- recolouring ---------------- */

/** Lit-front colour of the neutral renders (sRGB 0..1): multiplying by tint/REF lands the lit front on the tint. */
const RENDER_REF: readonly [number, number, number] = [0.93, 0.9, 0.88];

const hex2 = (n: number) => Math.round(Math.max(0, Math.min(1, n)) * 255).toString(16).padStart(2, '0');
const toHex = (c: readonly number[]) => `#${hex2(c[0])}${hex2(c[1])}${hex2(c[2])}`;

function parse(hex: string): [number, number, number] {
  const h = /^#?([0-9a-f]{6})/i.exec(hex.trim())?.[1] ?? '8e9099';
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255) as [number, number, number];
}

function hsl([r, g, b]: readonly number[]): [number, number, number] {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  h /= 6;
  return [h, s, l];
}

function rgb([h, s, l]: readonly number[]): [number, number, number] {
  if (s === 0) return [l, l, l];
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const f = (t: number) => {
    const u = ((t % 1) + 1) % 1;
    if (u < 1 / 6) return p + (q - p) * 6 * u;
    if (u < 1 / 2) return q;
    if (u < 2 / 3) return p + (q - p) * (2 / 3 - u) * 6;
    return p;
  };
  return [f(h + 1 / 3), f(h), f(h - 1 / 3)];
}

export interface RenderTint {
  /** Multiply layer colour (white = keep the neutral clay). */
  readonly multiply: string;
  /** Screen-blended highlight restore strength (0..1): keeps lit areas luminous on saturated tints. */
  readonly glow: number;
  /** Shadow lift colour and strength (screen): dark tints keep visible form instead of going black. */
  readonly lift: string;
  readonly liftOpacity: number;
  /** The body's resulting mid colour (for eye lips, contour, rim lights). */
  readonly body: string;
}

/**
 * Turn a swatch into the layers that recolour a neutral clay render while keeping its shading:
 * the tint is softened slightly toward clay (no plastic neon), its lightness is floored so dark
 * picks stay readable clay, and the multiply colour is divided by the render's own mid tone so
 * the lit front lands on the chosen colour. White picks keep the render's own warm white clay.
 */
export function renderTint(hex: string): RenderTint {
  const src = parse(hex);
  const [h, s, l] = hsl(src);
  // Soft clay, not plastic: tame very high saturation a little, and floor the lightness.
  const sat = Math.min(s, 0.86) * (l > 0.85 ? 1 : 0.94);
  const light = Math.max(0.4, Math.min(0.97, l + (l < 0.55 ? (0.55 - l) * 0.35 : 0)));
  const body = rgb([h, sat, light]);
  const multiply = body.map((c, i) => Math.min(1, c / RENDER_REF[i]));
  const chroma = Math.max(...body) - Math.min(...body);
  const darkness = Math.max(0, 0.62 - light);
  return {
    multiply: toHex(multiply),
    glow: Number(Math.min(0.24, chroma * 0.26).toFixed(3)),
    lift: toHex(rgb([h, Math.min(sat, 0.6), Math.min(0.8, light + 0.3)])),
    liftOpacity: Number(Math.min(0.32, darkness * 1.4).toFixed(3)),
    body: toHex(body)
  };
}

/** Mix two `#rrggbb` colours (t = 0 -> a, 1 -> b). */
export function mixColor(a: string, b: string, t: number): string {
  const ca = parse(a);
  const cb = parse(b);
  return toHex(ca.map((v, i) => v + (cb[i] - v) * t));
}

export interface SocketColors {
  /** Thin lighter lip around the socket, strongest on the lower edge (light catches it). */
  readonly lip: string;
  /** Soft shadow on the socket's upper rim (the body overhangs the hole there). */
  readonly shade: string;
}

/** Colours of the inset eye socket for a body colour (8-digit hex, alpha included). */
export function socketColors(body: string): SocketColors {
  return {
    lip: `${mixColor(body, '#ffffff', 0.5)}b3`,
    shade: `${mixColor(body, '#1b1418', 0.5)}80`
  };
}

/** The film's lighting: warm rim from the left, cool violet from the right. */
export const FILM_RIM_WARM = '#ffd2a3';
export const FILM_RIM_COOL = '#a493ff';
