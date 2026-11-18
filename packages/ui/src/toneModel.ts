/**
 * Backdrop tone: the lightest and darkest colours that sit behind the panels, so the
 * surface model can make them exactly as clear as legibility allows.  Pure functions (no DOM).
 *
 * "Lightest / darkest" always means WCAG relative luminance of the colour *as the panel sees it*:
 *  - `peak`: the dimmed backdrop pixel by pixel (what an unblurred layer sits on).  Every pixel is
 *    read; nothing is skipped, striped or percentile-trimmed, so one small bright patch counts.
 *  - `soft`: what a glass layer sits on: the dimmed backdrop, blurred, then `saturate()`d, exactly
 *    the order the CSS filter runs.  Measured with a real Gaussian-equivalent blur (three box
 *    passes) with the smallest radius any layer uses (`SOFT_SIGMA_PX`), which bounds every slider
 *    value because blurring more only lowers the maximum.
 * The dim layers are theme-dependent, so a `ToneSource` keeps the raw pixels and the tone is
 * evaluated per (dim, theme background); the blur of the raw pixels is computed once per source.
 */
import {
  GLASS_SATURATE,
  SOFT_SIGMA_PX,
  saturateHex,
  WORST_TONE,
  type BackdropTone,
  type ToneExtremes
} from "./surfaceModel.ts";
import { isLightColor, mix } from "./themeModel.ts";

/** Window px a busy-texture cell spans (what the eye reads as "one patch of texture"). */
export const DETAIL_CELL_PX = 32;

export interface ToneSource {
  readonly id: number;
  readonly w: number;
  readonly h: number;
  /** RGBA, `w * h * 4`. */
  readonly data: ArrayLike<number>;
  /** Window px per source px (a canvas drawn at 1/3 size has scale 3). */
  readonly scale: number;
  /** Pixels with alpha < 255 show whatever is behind them: not modelled, so the worst case applies. */
  readonly translucent: boolean;
  /** Texture: light / dark pixel of the cell at the 90th percentile of within-cell contrast (raw colours). */
  readonly detail?: ToneExtremes;
}

let nextId = 1;

const hex = (r: number, g: number, b: number) =>
  `#${[r, g, b].map((v) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, "0")).join("")}`;

const channels = (c: string): [number, number, number] => {
  const n = parseInt(c.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
};

/** sRGB channel (0..255, may be fractional) -> linear light, 1/8-step table (error < 0.01 %). */
const LIN = new Float64Array(255 * 8 + 1).map((_, i) => {
  const c = i / 8 / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
});
const lin = (v: number) => LIN[Math.round(Math.max(0, Math.min(255, v)) * 8)];
const lum = (r: number, g: number, b: number) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);

/** Read a pixel buffer into a source; computes the texture measure once. */
export function makeToneSource(data: ArrayLike<number>, w: number, h: number, scale: number): ToneSource {
  let translucent = false;
  for (let i = 3; i < w * h * 4; i += 4) {
    if (data[i] !== 255) {
      translucent = true;
      break;
    }
  }
  const size = Math.max(2, Math.round(DETAIL_CELL_PX / Math.max(0.01, scale)));
  const cols = Math.ceil(w / size);
  const rows = Math.ceil(h / size);
  const loL = new Int16Array(cols * rows).fill(256);
  const hiL = new Int16Array(cols * rows).fill(-1);
  const lo = new Int32Array(cols * rows).fill(-1);
  const hi = new Int32Array(cols * rows).fill(-1);
  for (let y = 0; y < h; y++) {
    const row = Math.floor(y / size) * cols;
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      const l = (data[i] * 54 + data[i + 1] * 183 + data[i + 2] * 19) >> 8;
      const c = row + Math.floor(x / size);
      if (l < loL[c]) {
        loL[c] = l;
        lo[c] = i;
      }
      if (l > hiL[c]) {
        hiL[c] = l;
        hi[c] = i;
      }
    }
  }
  const order = [...Array(cols * rows).keys()].filter((c) => hi[c] >= 0).sort((a, b) => hiL[a] - loL[a] - (hiL[b] - loL[b]));
  const p90 = order[Math.min(order.length - 1, Math.floor(order.length * 0.9))];
  const at = (i: number) => hex(data[i], data[i + 1], data[i + 2]);
  return {
    id: nextId++,
    w,
    h,
    data,
    scale,
    translucent,
    ...(p90 !== undefined ? { detail: { light: at(hi[p90]), dark: at(lo[p90]) } } : {})
  };
}

/* ---------- blur ---------- */

/** Box sizes whose three successive passes approximate a Gaussian of std deviation `sigma`. */
function boxSizes(sigma: number, n = 3): number[] {
  const ideal = Math.sqrt((12 * sigma * sigma) / n + 1);
  let wl = Math.floor(ideal);
  if (wl % 2 === 0) wl--;
  const wu = wl + 2;
  const m = Math.round((12 * sigma * sigma - n * wl * wl - 4 * n * wl - 3 * n) / (-4 * wl - 4));
  return Array.from({ length: n }, (_, i) => (i < m ? wl : wu));
}

/** One box pass along rows or columns; the line is padded by repeating its edge pixels. */
function boxPass(src: Float32Array, dst: Float32Array, w: number, h: number, r: number, vertical: boolean): void {
  const lines = vertical ? w : h;
  const len = vertical ? h : w;
  const step = vertical ? w : 1;
  const inv = 1 / (2 * r + 1);
  const pad = new Float32Array(len + 2 * r);
  for (let line = 0; line < lines; line++) {
    const base = vertical ? line : line * w;
    for (let k = 0; k < pad.length; k++) pad[k] = src[base + Math.max(0, Math.min(len - 1, k - r)) * step];
    let acc = 0;
    for (let k = 0; k <= 2 * r; k++) acc += pad[k];
    for (let k = 0; k < len; k++) {
      dst[base + k * step] = acc * inv;
      if (k < len - 1) acc += pad[k + 2 * r + 1] - pad[k];
    }
  }
}

function gaussianBlur(plane: Float32Array, w: number, h: number, sigma: number): Float32Array {
  let cur = plane;
  const tmp = new Float32Array(plane.length);
  for (const size of boxSizes(sigma)) {
    const r = (size - 1) / 2;
    const out = new Float32Array(plane.length);
    boxPass(cur, tmp, w, h, r, false);
    boxPass(tmp, out, w, h, r, true);
    cur = out;
  }
  return cur;
}

const blurCache = new WeakMap<ToneSource, { sigma: number; planes: Float32Array[] }>();

/** The source's colour planes blurred with `sigma` source px (computed once per source). */
function blurredPlanes(src: ToneSource, sigma: number): Float32Array[] {
  const cached = blurCache.get(src);
  if (cached && cached.sigma === sigma) return cached.planes;
  const n = src.w * src.h;
  const raw = [new Float32Array(n), new Float32Array(n), new Float32Array(n)];
  for (let i = 0; i < n; i++) {
    raw[0][i] = src.data[i * 4];
    raw[1][i] = src.data[i * 4 + 1];
    raw[2][i] = src.data[i * 4 + 2];
  }
  const planes = sigma >= 0.5 ? raw.map((p) => gaussianBlur(p, src.w, src.h, sigma)) : raw;
  blurCache.set(src, { sigma, planes });
  return planes;
}

/* ---------- evaluation ---------- */

export interface ToneEnv {
  /** Theme background: the colour of both dim layers. */
  bg: string;
  /** Combined strength of the dim layers over the backdrop, 0..1. */
  dim: number;
  /** `saturate()` of the glass layers (default `GLASS_SATURATE`). */
  saturate?: number;
}

/** One dim layer over another: remaining backdrop share is the product of what each leaves. */
export const combineDim = (...amounts: number[]): number =>
  1 - amounts.reduce((rest, a) => rest * (1 - Math.min(1, Math.max(0, Number.isFinite(a) ? a : 0))), 1);

/** Opacities of the translucent layers the worst case is searched at (see `ToneExtremes.lightAlt`). */
const ALT_ALPHAS = [0.4, 0.7, 0.9];

/** The tone of a source under the given dim layers and theme (see the file comment). */
export function evaluateTone(src: ToneSource, env: ToneEnv): BackdropTone {
  if (src.translucent || src.w * src.h === 0) return WORST_TONE;
  const dim = Math.min(1, Math.max(0, Number.isFinite(env.dim) ? env.dim : 0));
  const rest = 1 - dim;
  const bgc = channels(env.bg);
  const sat = env.saturate ?? GLASS_SATURATE;
  const n = src.w * src.h;
  // The layers sit on this tint; the worst case is the lightest backdrop for a dark theme, the darkest for a light one.
  const lightTheme = isLightColor(env.bg);
  const sign = lightTheme ? -1 : 1;
  const tint = channels(lightTheme ? mix(env.bg, "#ffffff", 0.45) : env.bg);

  // peak: every pixel, exact luminance via per-channel tables of the dimmed value (plain, and mixed
  // toward the tint at each ALT_ALPHA).
  const plain = [new Float64Array(256), new Float64Array(256), new Float64Array(256)];
  const alts = ALT_ALPHAS.map((a) => [0, 1, 2].map((ch) => {
    const t = new Float64Array(256);
    for (let c = 0; c < 256; c++) t[c] = lin((1 - a) * (rest * c + dim * bgc[ch]) + a * tint[ch]);
    return t;
  }));
  for (let c = 0; c < 256; c++) for (let ch = 0; ch < 3; ch++) plain[ch][c] = lin(rest * c + dim * bgc[ch]);
  let maxL = -1;
  let minL = 2;
  let maxI = 0;
  let minI = 0;
  // Three alternative opacities, unrolled: composite tables per channel, best score and pixel each.
  const [a0r, a0g, a0b] = alts[0];
  const [a1r, a1g, a1b] = alts[1];
  const [a2r, a2g, a2b] = alts[2];
  const [pR, pG, pB] = plain;
  let best0 = -Infinity;
  let best1 = -Infinity;
  let best2 = -Infinity;
  let idx0 = 0;
  let idx1 = 0;
  let idx2 = 0;
  const d = src.data;
  for (let i = 0; i < n * 4; i += 4) {
    const r = d[i];
    const g = d[i + 1];
    const b = d[i + 2];
    const l = 0.2126 * pR[r] + 0.7152 * pG[g] + 0.0722 * pB[b];
    if (l > maxL) {
      maxL = l;
      maxI = i;
    }
    if (l < minL) {
      minL = l;
      minI = i;
    }
    const v0 = sign * (0.2126 * a0r[r] + 0.7152 * a0g[g] + 0.0722 * a0b[b]);
    if (v0 > best0) {
      best0 = v0;
      idx0 = i;
    }
    const v1 = sign * (0.2126 * a1r[r] + 0.7152 * a1g[g] + 0.0722 * a1b[b]);
    if (v1 > best1) {
      best1 = v1;
      idx1 = i;
    }
    const v2 = sign * (0.2126 * a2r[r] + 0.7152 * a2g[g] + 0.0722 * a2b[b]);
    if (v2 > best2) {
      best2 = v2;
      idx2 = i;
    }
  }
  const dimmed = (i: number) => hex(rest * d[i] + dim * bgc[0], rest * d[i + 1] + dim * bgc[1], rest * d[i + 2] + dim * bgc[2]);
  const peak = withAlts(
    { light: dimmed(maxI), dark: dimmed(minI) },
    [idx0, idx1, idx2].map(dimmed),
    lightTheme
  );

  // soft: blur (of the raw pixels; the dim layers are affine, so they commute with it), dim, saturate.
  const planes = blurredPlanes(src, SOFT_SIGMA_PX / Math.max(0.01, src.scale));
  let sMax = -1;
  let sMin = 2;
  let sMaxC: [number, number, number] = [255, 255, 255];
  let sMinC: [number, number, number] = [0, 0, 0];
  // `saturate()` inlined (same matrix as `saturateRgb`); no allocation, no clamping in the hot loop beyond the matrix's own.
  const m00 = 0.213 + 0.787 * sat;
  const m01 = 0.715 - 0.715 * sat;
  const m02 = 0.072 - 0.072 * sat;
  const m10 = 0.213 - 0.213 * sat;
  const m11 = 0.715 + 0.285 * sat;
  const m22 = 0.072 + 0.928 * sat;
  const pr = planes[0];
  const pg = planes[1];
  const pb = planes[2];
  const k0 = dim * bgc[0];
  const k1 = dim * bgc[1];
  const k2 = dim * bgc[2];
  const [t0, t1, t2] = tint;
  const [al0, al1, al2] = ALT_ALPHAS;
  const tb0 = [al0 * t0, al0 * t1, al0 * t2];
  const tb1 = [al1 * t0, al1 * t1, al1 * t2];
  const tb2 = [al2 * t0, al2 * t1, al2 * t2];
  let sb0 = -Infinity;
  let sb1 = -Infinity;
  let sb2 = -Infinity;
  let sc0: [number, number, number] = [0, 0, 0];
  let sc1: [number, number, number] = [0, 0, 0];
  let sc2: [number, number, number] = [0, 0, 0];
  const L = LIN;
  const at = (v: number) => L[(v * 8 + 0.5) | 0];
  for (let i = 0; i < n; i++) {
    const r0 = rest * pr[i] + k0;
    const g0 = rest * pg[i] + k1;
    const b0 = rest * pb[i] + k2;
    let r = m00 * r0 + m01 * g0 + m02 * b0;
    let g = m10 * r0 + m11 * g0 + m02 * b0;
    let b = m10 * r0 + m01 * g0 + m22 * b0;
    r = r < 0 ? 0 : r > 255 ? 255 : r;
    g = g < 0 ? 0 : g > 255 ? 255 : g;
    b = b < 0 ? 0 : b > 255 ? 255 : b;
    const l = 0.2126 * at(r) + 0.7152 * at(g) + 0.0722 * at(b);
    if (l > sMax) {
      sMax = l;
      sMaxC = [r, g, b];
    }
    if (l < sMin) {
      sMin = l;
      sMinC = [r, g, b];
    }
    const v0 = sign * (0.2126 * at((1 - al0) * r + tb0[0]) + 0.7152 * at((1 - al0) * g + tb0[1]) + 0.0722 * at((1 - al0) * b + tb0[2]));
    if (v0 > sb0) {
      sb0 = v0;
      sc0 = [r, g, b];
    }
    const v1 = sign * (0.2126 * at((1 - al1) * r + tb1[0]) + 0.7152 * at((1 - al1) * g + tb1[1]) + 0.0722 * at((1 - al1) * b + tb1[2]));
    if (v1 > sb1) {
      sb1 = v1;
      sc1 = [r, g, b];
    }
    const v2 = sign * (0.2126 * at((1 - al2) * r + tb2[0]) + 0.7152 * at((1 - al2) * g + tb2[1]) + 0.0722 * at((1 - al2) * b + tb2[2]));
    if (v2 > sb2) {
      sb2 = v2;
      sc2 = [r, g, b];
    }
  }
  const soft = withAlts({ light: hex(...sMaxC), dark: hex(...sMinC) }, [sc0, sc1, sc2].map((c) => hex(...c)), lightTheme);

  const detail = src.detail
    ? {
        light: hex(...(channels(src.detail.light).map((v, k) => rest * v + dim * bgc[k]) as [number, number, number])),
        dark: hex(...(channels(src.detail.dark).map((v, k) => rest * v + dim * bgc[k]) as [number, number, number]))
      }
    : undefined;
  return { peak, soft, ...(detail ? { detail } : {}) };
}

/** Attach the composite-aware candidates (deduplicated, without the plain extreme itself). */
function withAlts(ext: ToneExtremes, alts: string[], lightTheme: boolean): ToneExtremes {
  const plainColor = lightTheme ? ext.dark : ext.light;
  const unique = [...new Set(alts)].filter((c) => c !== plainColor);
  if (!unique.length) return ext;
  return lightTheme ? { ...ext, darkAlt: unique } : { ...ext, lightAlt: unique };
}

/** Tone of the plain layout background (no backdrop): the two gradient stops, saturated for the glass layers. */
export function plainTone(bg: string, bg2: string, saturate: number = GLASS_SATURATE): BackdropTone {
  const l = (c: string) => lum(...channels(c));
  const [dark, light] = l(bg) <= l(bg2) ? [bg, bg2] : [bg2, bg];
  const s = [saturateHex(dark, saturate), saturateHex(light, saturate)];
  const [sd, sl] = l(s[0]) <= l(s[1]) ? [s[0], s[1]] : [s[1], s[0]];
  return { peak: { light, dark }, soft: { light: sl, dark: sd } };
}

/** Animated formats show one frame per sample, so an animated image cannot be bounded: worst case. */
export function mayBeAnimated(url: string): boolean {
  const head = /^data:([^;,]+)[^,]*,/i.exec(url);
  const mime = head?.[1].toLowerCase() ?? "";
  if (mime === "image/gif" || /\.gif(\?|#|$)/i.test(url)) return true;
  if (mime === "image/webp" || mime === "image/png" || mime === "image/apng") {
    // Look at the first bytes: WebP VP8X animation flag, PNG acTL chunk.
    const b64 = url.slice((head?.[0].length ?? 0), (head?.[0].length ?? 0) + 400);
    try {
      const bin = typeof atob === "function" ? atob(b64.slice(0, b64.length - (b64.length % 4))) : "";
      if (mime === "image/webp") return bin.slice(12, 16) === "VP8X" && (bin.charCodeAt(20) & 0x02) !== 0;
      return bin.includes("acTL");
    } catch {
      return true;
    }
  }
  return false;
}
