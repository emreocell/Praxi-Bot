/**
 * Backdrop tone sampling: reads what is drawn behind the panels so the surface model
 * can make them as clear as legibility allows.  The maths lives in `ui/tone-model`; this file only
 * touches the DOM: it reads a canvas or an image into a `ToneSource` and publishes the drawn
 * scene's source.  Reads happen once per draw / resize, never per frame.
 */
import { makeToneSource, mayBeAnimated, type ToneSource } from "ui/tone-model";

export { combineDim, evaluateTone, makeToneSource, mayBeAnimated, plainTone, type ToneSource } from "ui/tone-model";

/** Own images are sampled at window size; bigger windows (4K and up) use the worst case instead. */
export const MAX_IMAGE_SAMPLE_PIXELS = 4_000_000;

/** Where an image of `iw` x `ih` lands on a `w` x `h` window under `background-size: cover` (centred). */
export function coverRect(iw: number, ih: number, w: number, h: number): { x: number; y: number; width: number; height: number } {
  const k = Math.max(w / iw, h / ih);
  return { x: (w - iw * k) / 2, y: (h - ih * k) / 2, width: iw * k, height: ih * k };
}

/** The pixels of a drawn canvas that covers the window; null when it cannot be read (tainted canvas). */
export function sampleCanvasSource(canvas: HTMLCanvasElement, windowWidth: number): ToneSource | null {
  try {
    const ctx = canvas.getContext("2d", { willReadFrequently: false });
    if (!ctx || !canvas.width || !canvas.height) return null;
    const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);
    return makeToneSource(data, canvas.width, canvas.height, Math.max(1, windowWidth) / canvas.width);
  } catch {
    return null;
  }
}

/**
 * An image laid out like `background-size: cover` on a `width` x `height` window, at window
 * resolution (so a small bright patch is not averaged away).  Null = cannot be bounded (animated,
 * unreadable, or a window too large to sample): the caller keeps the worst-case floor.
 */
export function sampleImageSource(url: string, width: number, height: number): Promise<ToneSource | null> {
  return new Promise((resolve) => {
    if (typeof document === "undefined" || !url || mayBeAnimated(url)) {
      resolve(null);
      return;
    }
    const w = Math.round(width);
    const h = Math.round(height);
    if (w < 1 || h < 1 || w * h > MAX_IMAGE_SAMPLE_PIXELS) {
      resolve(null);
      return;
    }
    const img = new Image();
    img.decoding = "async";
    img.onload = () => {
      try {
        if (!img.naturalWidth || !img.naturalHeight) {
          resolve(null);
          return;
        }
        const cv = document.createElement("canvas");
        cv.width = w;
        cv.height = h;
        const ctx = cv.getContext("2d");
        if (!ctx) {
          resolve(null);
          return;
        }
        const r = coverRect(img.naturalWidth, img.naturalHeight, w, h);
        ctx.drawImage(img, r.x, r.y, r.width, r.height);
        resolve(sampleCanvasSource(cv, w));
      } catch {
        resolve(null);
      }
    };
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

/* ---------- the drawn scene's source (published by <Backdrop>) ---------- */

type Listener = () => void;
let sceneSource: { scene: string; source: ToneSource } | null = null;
const listeners = new Set<Listener>();

/** The source of the scene currently drawn, or null. */
export function getSceneSource(scene: string): ToneSource | null {
  return sceneSource && sceneSource.scene === scene ? sceneSource.source : null;
}

export function publishSceneSource(scene: string, source: ToneSource | null): void {
  sceneSource = source ? { scene, source } : null;
  for (const l of listeners) l();
}

export function subscribeSceneSource(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
