/**
 * Sizing math for the live picture: what to ask the host for, and how a received frame fits a
 * box. The host never upscales and caps at 1920x1080 (`MAX_OUTPUT_*`), keeps the aspect ratio
 * and down-scales with a Lanczos filter, so the request is a bounding box, not an exact size.
 */
import type { QualityMode } from "./liveViewAdaptive.ts";

export const HOST_MAX_WIDTH = 1920;
export const HOST_MAX_HEIGHT = 1080;
/** Never ask for less than this: below it text on a desktop is unreadable. */
export const MIN_REQUEST_WIDTH = 480;

export interface Box {
  width: number;
  height: number;
}

/** Largest size that fits `box` with the source aspect ratio kept; never above the source. */
export function fitWithin(source: Box, box: Box): Box {
  if (source.width <= 0 || source.height <= 0 || box.width <= 0 || box.height <= 0) {
    return { width: 0, height: 0 };
  }
  const scale = Math.min(1, box.width / source.width, box.height / source.height);
  return { width: Math.round(source.width * scale), height: Math.round(source.height * scale) };
}

/**
 * Bounding box to request for a display area of `cssWidth` x `cssHeight` CSS pixels on a screen
 * with the given device pixel ratio: one stream pixel per device pixel (sharp on HiDPI), never
 * above the host cap, never below a readable floor.
 */
export function requestBox(cssWidth: number, cssHeight: number, dpr: number): Box {
  const ratio = Number.isFinite(dpr) && dpr >= 1 ? Math.min(dpr, 3) : 1;
  const w = Math.ceil(Math.max(0, cssWidth) * ratio);
  const h = Math.ceil(Math.max(0, cssHeight) * ratio);
  const width = Math.min(HOST_MAX_WIDTH, Math.max(MIN_REQUEST_WIDTH, w));
  const height = Math.min(HOST_MAX_HEIGHT, Math.max(Math.round((MIN_REQUEST_WIDTH * 10) / 16), h));
  return { width, height };
}

const MODE_CAP: Record<QualityMode, Box | null> = {
  auto: null,
  high: { width: HOST_MAX_WIDTH, height: HOST_MAX_HEIGHT },
  saver: { width: 960, height: 540 }
};

const MODE_QUALITY: Record<QualityMode, number> = { auto: 0, high: 88, saver: 55 };

/** Apply the owner's quality choice to a base request box and JPEG quality. */
export function applyMode(
  mode: QualityMode,
  box: Box,
  quality: number
): { box: Box; quality: number } {
  if (mode === "high") {
    return { box: MODE_CAP.high as Box, quality: Math.max(quality, MODE_QUALITY.high) };
  }
  if (mode === "saver") {
    const cap = MODE_CAP.saver as Box;
    return {
      box: { width: Math.min(box.width, cap.width), height: Math.min(box.height, cap.height) },
      quality: Math.min(quality, MODE_QUALITY.saver)
    };
  }
  return { box, quality };
}

/** Scale a request box by an adaptive level factor, keeping it readable. */
export function scaleBox(box: Box, scale: number): Box {
  if (scale >= 1) return box;
  const width = Math.max(MIN_REQUEST_WIDTH, Math.round(box.width * scale));
  const height = Math.max(Math.round((MIN_REQUEST_WIDTH * 10) / 16), Math.round(box.height * scale));
  return { width: Math.min(width, box.width), height: Math.min(height, box.height) };
}

/**
 * CSS `aspect-ratio` for the card from a `1280×800` resolution label, so a 16:10 screen is not
 * letterboxed in a 16:9 box. Unknown or absurd sizes fall back to 16 / 10.
 */
export function cardAspect(resolution: string | null): string {
  const m = resolution ? /^(\d+)\s*[×x]\s*(\d+)$/.exec(resolution.trim()) : null;
  if (!m) return "16 / 10";
  const w = Number(m[1]);
  const h = Number(m[2]);
  if (w <= 0 || h <= 0) return "16 / 10";
  const ratio = Math.min(2.4, Math.max(1.2, w / h));
  return `${Math.round(ratio * 1000)} / 1000`;
}
