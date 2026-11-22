/**
 * Animated, procedural wallpaper: dithered cumulus clouds.
 *
 * Nothing here ships as an image.  In `subtle` mode (default), a cloud density field is computed
 * once per size (periodic in x) and a slow drift scrolls through it by whole columns using ordered
 * dither (Bayer / diagonal line screen).  In `live` mode, a small WebGL fragment shader renders
 * layered, slowly drifting fbm clouds at low internal resolution with the same ordered dither
 * post-effect, capped at 30 fps, pausing when hidden/unfocused/minimised, on battery saver, or
 * under reduced motion, and falling back to the 2D static frame when WebGL is unavailable.
 */
import { makeToneSource, type ToneSource } from "ui/tone-model";
import { createLiveCloudRenderer, type LiveCloudRenderer } from "./wallpaperLive.ts";

export const WALLPAPER_KINDS = ["sky", "dusk", "night", "warm"] as const;
export type WallpaperKind = (typeof WALLPAPER_KINDS)[number];

/** Stored value: a kind, `off` (default) or `theme` (follow the active theme). */
export const WALLPAPER_SETTINGS = ["off", "sky", "dusk", "night", "warm", "theme"] as const;
export type WallpaperSetting = (typeof WALLPAPER_SETTINGS)[number];
export const DEFAULT_WALLPAPER: WallpaperSetting = "off";

export function parseWallpaper(value: unknown): WallpaperSetting {
  return WALLPAPER_SETTINGS.find((w) => w === value) ?? DEFAULT_WALLPAPER;
}

/** Motion mode for the cloud wallpaper (`off` = static frame, `subtle` = 1 col / 4s, `live` = 30 fps WebGL). */
export const WALLPAPER_MOTIONS = ["off", "subtle", "live"] as const;
export type WallpaperMotion = (typeof WALLPAPER_MOTIONS)[number];
export const DEFAULT_WALLPAPER_MOTION: WallpaperMotion = "subtle";

export function parseWallpaperMotion(value: unknown): WallpaperMotion {
  return WALLPAPER_MOTIONS.find((m) => m === value) ?? DEFAULT_WALLPAPER_MOTION;
}

export const LIVE_FPS_CAP = 30;
export const LIVE_FRAME_INTERVAL_MS = 1000 / LIVE_FPS_CAP;
export const LIVE_MAX_PIXELS = 360_000;
export const BATTERY_SAVER_LEVEL = 0.2;

export interface BatteryLike {
  charging: boolean;
  level: number;
  addEventListener?(type: string, listener: () => void): void;
  removeEventListener?(type: string, listener: () => void): void;
}

/** True when running on battery (not charging) at or below 20% charge. */
export function isBatterySaver(battery: Pick<BatteryLike, "charging" | "level"> | null | undefined): boolean {
  return Boolean(battery && !battery.charging && battery.level <= BATTERY_SAVER_LEVEL);
}

export interface WallpaperMotionState {
  motion: WallpaperMotion;
  hidden: boolean;
  focused: boolean;
  reducedMotion: boolean;
  batterySaver: boolean;
}

/** Whether the wallpaper loop may run right now (pauses on off, hidden, unfocused, reduced-motion, battery saver). */
export function shouldAnimateWallpaper(state: WallpaperMotionState): boolean {
  return state.motion !== "off" && !state.hidden && state.focused && !state.reducedMotion && !state.batterySaver;
}

/** Shipped themes with a hand-picked wallpaper for "Follow theme". */
const THEME_WALLPAPER: Readonly<Record<string, WallpaperKind>> = {
  obsidian: "night",
  cobalt: "sky",
  raven: "dusk",
  clay: "warm"
};

/** The wallpaper to draw, or null when it is off. Unknown themes get Night (dark) or Sky (light). */
export function resolveWallpaper(setting: WallpaperSetting, themeId: string, themeIsDark: boolean): WallpaperKind | null {
  if (setting === "off") return null;
  if (setting !== "theme") return setting;
  return THEME_WALLPAPER[themeId] ?? (themeIsDark ? "night" : "sky");
}

/** Id the shell's backdrop plumbing (tone sampling, float layout) knows the wallpaper by. */
export const wallpaperSceneId = (kind: WallpaperKind): string => `wallpaper:${kind}`;
export const isWallpaperScene = (id: string): boolean => id.startsWith("wallpaper:");

/** The share of the theme background laid over the wallpaper (same meaning as `backdropDim`). */
export const WALLPAPER_DIM = 22;

/* ---------- palettes (the only place wallpaper colours live), darkest-sky first ---------- */

export interface WallpaperStyle {
  /** Colours from open sky to the brightest cloud top. */
  palette: readonly string[];
  /** `bayer`: 8x8 ordered dither; `diagonal`: 45-degree line screen like a halftone print. */
  screen: "bayer" | "diagonal";
  /** Divisor of the viewport for the drawn canvas (bigger = chunkier pixels). */
  scale: number;
  /** Cloud coverage 0..1 (more = more sky covered). */
  cover: number;
  /** Extra haze toward the bottom edge, in density units. */
  haze: number;
}

export const WALLPAPERS: Readonly<Record<WallpaperKind, WallpaperStyle>> = {
  sky: { palette: ["#2455e4", "#5f86ee", "#a9c0f6", "#f6f9ff"], screen: "diagonal", scale: 2, cover: 0.5, haze: 0.1 },
  dusk: { palette: ["#6a8a93", "#8fa8a8", "#bdbb9c", "#eadcb0"], screen: "bayer", scale: 3, cover: 0.46, haze: 0.2 },
  night: { palette: ["#060a1c", "#0f1a42", "#243580", "#5d73bd"], screen: "diagonal", scale: 2, cover: 0.46, haze: 0.08 },
  warm: { palette: ["#f1c8a6", "#f6d8bd", "#fbe9d3", "#fff8ee"], screen: "bayer", scale: 3, cover: 0.46, haze: 0.14 }
};

/* ---------- ordered dither ---------- */

/** 8x8 Bayer matrix, values 0..63. */
export const BAYER8: readonly number[] = [
  0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30, 54, 22,
  3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21
];

/** Bayer threshold for a pixel, strictly inside (0, 1). */
export function bayerThreshold(x: number, y: number): number {
  return (BAYER8[((y & 7) << 3) | (x & 7)] + 0.5) / 64;
}

/** Diagonal line-screen threshold in (0, 1): lines run at 45 degrees, 4 px apart, with a Bayer jitter. */
export function diagonalThreshold(x: number, y: number): number {
  const line = (((x + y) % 4) + 4) % 4; // 0..3 across a line period
  return (line * 16 + (BAYER8[((y & 7) << 3) | (x & 7)] >> 2) + 0.5) / 64;
}

/**
 * Pick a palette index for a value `v` in [0, 1] at threshold `t`: the value spans `levels - 1`
 * bands and inside a band the threshold decides between the band's two colours.
 */
export function quantize(v: number, t: number, levels: number): number {
  const s = Math.min(1, Math.max(0, v)) * (levels - 1);
  const base = Math.min(levels - 2, Math.floor(s));
  return base + (s - base > t ? 1 : 0);
}

export function hexToRgb(hex: string): [number, number, number] {
  return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)) as [number, number, number];
}

/** Palette as packed little-endian RGBA words for `Uint32Array` image writes. */
export function packPalette(palette: readonly string[]): Uint32Array {
  return Uint32Array.from(palette, (c) => {
    const [r, g, b] = hexToRgb(c);
    return ((0xff << 24) | (b << 16) | (g << 8) | r) >>> 0;
  });
}

/** Palette as packed RGB floats in [0, 1] for WebGL uniforms. */
export function packPaletteRgb(palette: readonly string[]): Float32Array {
  const out = new Float32Array(palette.length * 3);
  for (let i = 0; i < palette.length; i++) {
    const [r, g, b] = hexToRgb(palette[i]);
    out[i * 3] = r / 255;
    out[i * 3 + 1] = g / 255;
    out[i * 3 + 2] = b / 255;
  }
  return out;
}

/** 8x8 dither threshold table quantized to 0..255 for the WebGL ordered-dither texture. */
export function buildDitherBytes(screen: WallpaperStyle["screen"]): Uint8Array {
  const thr = screen === "diagonal" ? diagonalThreshold : bayerThreshold;
  const out = new Uint8Array(64);
  for (let y = 0; y < 8; y++) {
    for (let x = 0; x < 8; x++) {
      out[y * 8 + x] = Math.round(thr(x, y) * 255);
    }
  }
  return out;
}

/* ---------- drift ---------- */

/** How long a canvas column takes to scroll by one pixel. Calm: about one pixel per four seconds. */
export const DRIFT_MS_PER_COLUMN = 4000;

/** Whole columns the field has moved after `elapsedMs`. */
export function driftOffset(elapsedMs: number, width: number, msPerColumn: number = DRIFT_MS_PER_COLUMN): number {
  if (width <= 0 || elapsedMs <= 0) return 0;
  return Math.floor(elapsedMs / msPerColumn) % width;
}

/** Milliseconds until the next whole-column step (at least 1): the timer never wakes in between. */
export function nextDriftDelay(elapsedMs: number, msPerColumn: number = DRIFT_MS_PER_COLUMN): number {
  const into = ((elapsedMs % msPerColumn) + msPerColumn) % msPerColumn;
  return Math.max(1, Math.ceil(msPerColumn - into));
}

/** Canvas size for a window; the area is capped so huge windows draw coarser, not slower. */
export function wallpaperSize(innerWidth: number, innerHeight: number, scale: number, maxPixels = 700_000): { w: number; h: number } {
  const k = Math.max(scale, Math.sqrt((innerWidth * innerHeight) / maxPixels));
  return { w: Math.max(2, Math.ceil(innerWidth / k)), h: Math.max(2, Math.ceil(innerHeight / k)) };
}

/* ---------- the cloud field ---------- */

function hash(ix: number, iy: number, seed: number): number {
  let h = Math.imul(ix, 374761393) ^ Math.imul(iy, 668265263) ^ Math.imul(seed, 1274126177);
  h = Math.imul(h ^ (h >>> 13), 1274126177);
  return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
}

const smooth = (t: number): number => t * t * (3 - 2 * t);

/** Value noise that repeats every `period` cells in x (so the field wraps seamlessly). */
function valueNoise(x: number, y: number, period: number, seed: number): number {
  const ix = Math.floor(x);
  const iy = Math.floor(y);
  const fx = smooth(x - ix);
  const fy = smooth(y - iy);
  const x0 = ((ix % period) + period) % period;
  const x1 = (x0 + 1) % period;
  const a = hash(x0, iy, seed);
  const b = hash(x1, iy, seed);
  const c = hash(x0, iy + 1, seed);
  const d = hash(x1, iy + 1, seed);
  return a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy;
}

const smoothstep = (a: number, b: number, v: number): number => {
  const t = Math.min(1, Math.max(0, (v - a) / (b - a)));
  return t * t * (3 - 2 * t);
};

function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface Puff {
  x: number;
  y: number;
  r: number;
  /** Row of the cluster's flat base: nothing is drawn below it. */
  base: number;
  /** Cluster height, for the darker underside. */
  hc: number;
}

interface Cluster {
  /** Centre, base row and size as fractions of the canvas (width / height). */
  cx: number;
  base: number;
  wc: number;
  hc: number;
  puffs: number;
}

/** The composition: one big cumulus, a lower companion and a small distant one; open sky on top. */
const CLUSTERS: readonly Cluster[] = [
  { cx: 0.3, base: 1.04, wc: 0.74, hc: 0.74, puffs: 54 },
  { cx: 0.86, base: 1.0, wc: 0.5, hc: 0.5, puffs: 30 },
  { cx: 0.6, base: 1.1, wc: 0.44, hc: 0.36, puffs: 14 },
  { cx: 0.07, base: 0.52, wc: 0.2, hc: 0.1, puffs: 7 }
];

function makePuffs(w: number, h: number, cover: number, seed: number): Puff[] {
  const rand = rng(seed);
  const out: Puff[] = [];
  const size = 0.8 + cover * 0.45;
  for (const c of CLUSTERS) {
    const base = c.base * h;
    const hc = c.hc * h * size;
    const wc = c.wc * w * size;
    const count = Math.round(c.puffs * (0.7 + cover * 0.6));
    for (let i = 0; i < count; i++) {
      const t = (rand() - 0.5) * 2; // -1..1 across the cluster
      const mound = Math.max(0.12, 1 - t * t); // a rounded silhouette
      const reach = (0.25 + 0.75 * rand()) * mound;
      const r = hc * (0.12 + 0.2 * rand()) * (0.55 + 0.6 * mound);
      const x = c.cx * w + (t * wc) / 2;
      // Centres rise with the mound; the lowest puffs sit on the base so the bottom stays flat.
      const y = base - r * 0.55 - (hc - r) * reach * rand();
      out.push({ x, y, r, base, hc });
      // Cauliflower detail: small puffs along the upper rim of a big one.
      if (r > hc * 0.1) {
        for (let k = 0; k < 3; k++) {
          const ang = -Math.PI * (0.1 + 0.8 * rand());
          const rr = r * (0.3 + 0.2 * rand());
          out.push({ x: x + Math.cos(ang) * r * 0.85, y: y + Math.sin(ang) * r * 0.85, r: rr, base, hc });
        }
      }
    }
  }
  return out;
}

/**
 * The cloud field: `w * h` bytes, 0 = open sky .. 255 = lit cloud top.  Deterministic for a size and
 * seed, periodic in x (a drift step never shows a seam).  Cumulus are unions of shaded spheres:
 * round tops, a flat base clipped per cluster, light from the upper left, a darker underside, and
 * value noise on the silhouette for a fuzzy edge.
 */
export function buildField(w: number, h: number, cover: number, haze: number, seed = 11): Uint8Array {
  const height = new Float32Array(w * h).fill(-1);
  const shade = new Float32Array(w * h);
  const edgeCells = 96;
  for (const p of makePuffs(w, h, cover, seed)) {
    const r = Math.max(1.5, p.r);
    const x0 = Math.floor(p.x - r - 2);
    const x1 = Math.ceil(p.x + r + 2);
    const y0 = Math.max(0, Math.floor(p.y - r - 2));
    const y1 = Math.min(h - 1, Math.ceil(p.y + r + 2), Math.floor(p.base));
    for (let y = y0; y <= y1; y++) {
      for (let x = x0; x <= x1; x++) {
        const xx = ((x % w) + w) % w;
        const dx = x + 0.5 - p.x;
        const dy = y + 0.5 - p.y;
        const edge = valueNoise((xx / w) * edgeCells, (y / w) * edgeCells, edgeCells, seed) - 0.5;
        const q = Math.hypot(dx, dy) / r + edge * 0.34;
        if (q >= 1) continue;
        const nz = Math.sqrt(Math.max(0, 1 - q * q));
        const hgt = nz * r;
        const i = y * w + xx;
        if (hgt <= height[i]) continue;
        height[i] = hgt;
        const lit = (-0.45 * (dx / r) - 0.7 * (dy / r) + 0.55 * nz) / 1.0;
        const under = 0.6 + 0.4 * Math.min(1, Math.max(0, (p.base - y) / (p.hc * 0.45)));
        shade[i] = (0.3 + 0.7 * Math.min(1, Math.max(0, 0.5 + 0.5 * lit))) * under;
      }
    }
  }
  // Valleys between puffs are darker (a cheap ambient occlusion), then one soft pass hides the seams
  // where two puffs meet.
  const flat = new Float32Array(w * h);
  for (let i = 0; i < flat.length; i++) flat[i] = Math.max(0, height[i]);
  const blurred = boxBlur(flat, w, h, Math.max(2, Math.round(h / 26)));
  const scale = Math.max(1, h * 0.035);
  const v0 = new Float32Array(w * h);
  for (let i = 0; i < v0.length; i++) {
    if (height[i] < 0) continue;
    const ao = 1 - 0.55 * Math.min(1, Math.max(0, (blurred[i] - height[i]) / scale));
    v0[i] = smoothstep(0, 0.14 * Math.max(2, h / 40), height[i]) * shade[i] * ao;
  }
  const soft = boxBlur(v0, w, h, 1);
  const out = new Uint8Array(w * h);
  for (let y = 0; y < h; y++) {
    const hz = haze * (y / h) * (y / h);
    for (let x = 0; x < w; x++) {
      const i = y * w + x;
      const v = soft[i] + hz * (1 - Math.min(1, soft[i] * 2));
      out[i] = Math.round(Math.min(1, Math.max(0, v)) * 255);
    }
  }
  return out;
}

/** Separable box blur, wrapping in x and clamping in y. */
function boxBlur(src: Float32Array, w: number, h: number, rad: number): Float32Array {
  const tmp = new Float32Array(w * h);
  const dst = new Float32Array(w * h);
  const n = 2 * rad + 1;
  for (let y = 0; y < h; y++) {
    const row = y * w;
    let acc = 0;
    for (let k = -rad; k <= rad; k++) acc += src[row + ((k % w) + w) % w];
    for (let x = 0; x < w; x++) {
      tmp[row + x] = acc / n;
      acc += src[row + ((x + rad + 1) % w)] - src[row + ((x - rad + w * 4) % w)];
    }
  }
  for (let x = 0; x < w; x++) {
    let acc = 0;
    for (let k = -rad; k <= rad; k++) acc += tmp[Math.min(h - 1, Math.max(0, k)) * w + x];
    for (let y = 0; y < h; y++) {
      dst[y * w + x] = acc / n;
      acc += tmp[Math.min(h - 1, y + rad + 1) * w + x] - tmp[Math.max(0, y - rad) * w + x];
    }
  }
  return dst;
}

/** Map the field (shifted left by `offset` columns, wrapping) to palette words in `target`. */
export function renderField(
  field: Uint8Array,
  w: number,
  h: number,
  offset: number,
  style: Pick<WallpaperStyle, "screen">,
  packed: Uint32Array,
  target: Uint32Array
): void {
  const levels = packed.length;
  const thr = style.screen === "diagonal" ? diagonalThreshold : bayerThreshold;
  // Threshold table for one 8x8 / 4x8 tile period (both repeat every 8 px).
  const table = new Float32Array(64);
  for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) table[(y << 3) | x] = thr(x, y);
  for (let y = 0; y < h; y++) {
    const row = y * w;
    for (let x = 0; x < w; x++) {
      let sx = x + offset;
      if (sx >= w) sx -= w;
      target[row + x] = packed[quantize(field[row + sx] / 255, table[((y & 7) << 3) | (x & 7)], levels)];
    }
  }
}

/* ---------- canvas controller (DOM) ---------- */

export interface WallpaperHandle {
  stop(): void;
}

export interface WallpaperEnv {
  motion?: WallpaperMotion;
  /** Current time in ms (injectable for tests). */
  now?: () => number;
  reducedMotion?: () => boolean;
  focused?: () => boolean;
  batterySaver?: () => boolean;
  createLive?: typeof createLiveCloudRenderer | null;
  onContextLost?: () => void;
}

/**
 * Draw `kind` on `canvas` and animate according to `env.motion` (`off` = static frame,
 * `subtle` = 1 col / 4 s drift, `live` = 30 fps WebGL fbm clouds with dither post-effect, falling
 * back to the static 2D wallpaper when WebGL is unavailable).
 *
 * Pauses whenever the window is hidden, unfocused, or minimised, on battery saver, or under
 * `prefers-reduced-motion`.  `onDrawn` fires after the initial / resized frame so panels can
 * sample the backdrop tone once without per-frame reads.
 */
export function startWallpaper(
  canvas: HTMLCanvasElement,
  kind: WallpaperKind,
  onDrawn?: (source?: ToneSource | null) => void,
  env: WallpaperEnv = {}
): WallpaperHandle {
  const style = WALLPAPERS[kind];
  const packed = packPalette(style.palette);
  const now = env.now ?? (() => performance.now());
  const requestedMotion = env.motion ?? DEFAULT_WALLPAPER_MOTION;
  const reducedQuery =
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-reduced-motion: reduce)")
      : null;
  let focusedState = typeof document !== "undefined" && typeof document.hasFocus === "function" ? document.hasFocus() : true;
  let batteryFlag = false;
  let batteryHandle: BatteryLike | null = null;

  const isHidden = () => (typeof document !== "undefined" ? document.hidden : false);
  const isFocused = () => (env.focused ? env.focused() : focusedState);
  const isReduced = () => (env.reducedMotion ? env.reducedMotion() : reducedQuery?.matches ?? false);
  const isBattery = () => (env.batterySaver ? env.batterySaver() : batteryFlag);

  let field: Uint8Array = new Uint8Array(0);
  let w = 0;
  let h = 0;
  let image: ImageData | null = null;
  let words = new Uint32Array(0);
  let ctx2d: CanvasRenderingContext2D | null = null;
  let live: LiveCloudRenderer | null = null;
  let activeMotion: WallpaperMotion = requestedMotion;

  let subtleTimer: ReturnType<typeof setTimeout> | undefined;
  let liveTimer: ReturnType<typeof setTimeout> | undefined;
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;
  let rafId: number | undefined;
  let stopped = false;

  let activeElapsedMs = 0;
  let resumeEpoch: number | null = null;
  let lastLiveDrawAt = -Infinity;

  const canAnimate = () =>
    !stopped &&
    shouldAnimateWallpaper({
      motion: activeMotion,
      hidden: isHidden(),
      focused: isFocused(),
      reducedMotion: isReduced(),
      batterySaver: isBattery()
    });

  const currentElapsedMs = () =>
    activeElapsedMs + (resumeEpoch !== null ? Math.max(0, now() - resumeEpoch) : 0);

  const cancelLoops = (): void => {
    clearTimeout(subtleTimer);
    subtleTimer = undefined;
    clearTimeout(liveTimer);
    liveTimer = undefined;
    if (rafId !== undefined && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(rafId);
    }
    rafId = undefined;
  };

  const draw2d = (): void => {
    if (!ctx2d || !image) return;
    renderField(field, w, h, driftOffset(currentElapsedMs(), w), style, packed, words);
    ctx2d.putImageData(image, 0, 0);
  };

  const drawLive = (): void => {
    if (!live) return;
    live.draw(currentElapsedMs() / 1000);
  };

  const scheduleSubtle = (): void => {
    clearTimeout(subtleTimer);
    if (!canAnimate() || activeMotion !== "subtle") return;
    subtleTimer = setTimeout(() => {
      if (!canAnimate() || activeMotion !== "subtle") return;
      draw2d();
      scheduleSubtle();
    }, nextDriftDelay(currentElapsedMs()));
  };

  const scheduleLive = (): void => {
    clearTimeout(liveTimer);
    if (!canAnimate() || activeMotion !== "live" || !live) return;
    const wait = Math.max(0, LIVE_FRAME_INTERVAL_MS - (now() - lastLiveDrawAt));
    liveTimer = setTimeout(() => {
      liveTimer = undefined;
      if (!canAnimate() || activeMotion !== "live" || !live) return;
      const runFrame = () => {
        rafId = undefined;
        if (!canAnimate() || activeMotion !== "live" || !live) return;
        lastLiveDrawAt = now();
        drawLive();
        scheduleLive();
      };
      if (typeof requestAnimationFrame === "function") {
        rafId = requestAnimationFrame(runFrame);
      } else {
        runFrame();
      }
    }, wait);
  };

  const syncAnimation = (redraw = false): void => {
    if (stopped) return;
    const shouldRun = canAnimate();
    if (shouldRun) {
      if (resumeEpoch === null) resumeEpoch = now();
      if (redraw) {
        if (activeMotion === "live" && live) {
          lastLiveDrawAt = now();
          drawLive();
        } else {
          draw2d();
        }
      }
      if (activeMotion === "live" && live) scheduleLive();
      else if (activeMotion === "subtle") scheduleSubtle();
    } else {
      if (resumeEpoch !== null) {
        activeElapsedMs += Math.max(0, now() - resumeEpoch);
        resumeEpoch = null;
      }
      cancelLoops();
    }
  };

  const rebuild2d = (): void => {
    if (!ctx2d) ctx2d = canvas.getContext("2d");
    const size = wallpaperSize(window.innerWidth, window.innerHeight, style.scale);
    w = size.w;
    h = size.h;
    canvas.width = w;
    canvas.height = h;
    field = buildField(w, h, style.cover, style.haze);
    image = ctx2d ? ctx2d.createImageData(w, h) : null;
    words = image ? new Uint32Array(image.data.buffer) : new Uint32Array(0);
    draw2d();
    onDrawn?.();
  };

  const rebuild = (): void => {
    if (stopped) return;
    if (activeMotion === "live" && live) {
      const size = wallpaperSize(window.innerWidth, window.innerHeight, style.scale, LIVE_MAX_PIXELS);
      w = size.w;
      h = size.h;
      live.resize(w, h);
      lastLiveDrawAt = now();
      drawLive();
      const rgba = live.readPixelsRgba();
      onDrawn?.(rgba ? makeToneSource(rgba, w, h, Math.max(1, window.innerWidth) / w) : undefined);
    } else {
      rebuild2d();
    }
    syncAnimation(false);
  };

  if (requestedMotion === "live") {
    const factory = env.createLive === undefined ? createLiveCloudRenderer : env.createLive;
    live = factory
      ? factory(
          canvas,
          {
            paletteRgb: packPaletteRgb(style.palette),
            ditherBytes: buildDitherBytes(style.screen),
            cover: style.cover,
            haze: style.haze
          },
          () => {
            if (stopped) return;
            live?.dispose();
            live = null;
            activeMotion = "off";
            cancelLoops();
            env.onContextLost?.();
          }
        )
      : null;
    if (!live) {
      // WebGL unavailable: fall back cleanly to the static 2D cloud wallpaper.
      activeMotion = "off";
    }
  }

  const onVisibility = (): void => syncAnimation(!isHidden());
  const onFocus = (): void => {
    focusedState = true;
    syncAnimation(true);
  };
  const onBlur = (): void => {
    focusedState = false;
    syncAnimation(false);
  };
  const onReducedChange = (): void => syncAnimation(false);
  const onBatteryChange = (): void => {
    batteryFlag = isBatterySaver(batteryHandle);
    syncAnimation(!batteryFlag);
  };
  const onResize = (): void => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(rebuild, 150);
  };

  if (typeof document !== "undefined") document.addEventListener("visibilitychange", onVisibility);
  if (typeof window !== "undefined") {
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    window.addEventListener("resize", onResize);
  }
  reducedQuery?.addEventListener?.("change", onReducedChange);

  const nav = typeof navigator !== "undefined" ? (navigator as Navigator & { getBattery?: () => Promise<BatteryLike> }) : null;
  if (!env.batterySaver && nav && typeof nav.getBattery === "function") {
    void nav
      .getBattery()
      .then((b) => {
        if (stopped || !b) return;
        batteryHandle = b;
        b.addEventListener?.("chargingchange", onBatteryChange);
        b.addEventListener?.("levelchange", onBatteryChange);
        onBatteryChange();
      })
      .catch(() => {});
  }

  rebuild();
  return {
    stop() {
      stopped = true;
      cancelLoops();
      clearTimeout(resizeTimer);
      live?.dispose();
      live = null;
      if (typeof document !== "undefined") document.removeEventListener("visibilitychange", onVisibility);
      if (typeof window !== "undefined") {
        window.removeEventListener("focus", onFocus);
        window.removeEventListener("blur", onBlur);
        window.removeEventListener("resize", onResize);
      }
      reducedQuery?.removeEventListener?.("change", onReducedChange);
      batteryHandle?.removeEventListener?.("chargingchange", onBatteryChange);
      batteryHandle?.removeEventListener?.("levelchange", onBatteryChange);
      batteryHandle = null;
    }
  };
}
