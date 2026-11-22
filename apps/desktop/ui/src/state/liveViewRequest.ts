/**
 * What the UI asks the host for. The host never upscales and keeps a hard cap of 1920x1080, so
 * the open viewer asks for the cap and receives the screen's native size up to it; the browser
 * then draws that image 1:1 or reduces it, never stretches a small one. The card asks for a
 * picture that stays readable when the owner looks closely and sharp on a high-density display
 * (up to 1280x800), and the owner's Auto / High / Data saver choice plus the adaptive level
 * (this computer cannot keep up) trim it from there.
 */
import { ADAPT_LEVELS, type QualityMode } from "../lib/liveViewAdaptive.ts";
import { applyMode, requestBox, scaleBox } from "../lib/liveViewSizing.ts";

export interface SessionRequest {
  maxWidth: number;
  maxHeight: number;
  quality: number;
}

/** Hard cap shared with the host (`MAX_OUTPUT_WIDTH` x `MAX_OUTPUT_HEIGHT`). */
export const VIEWER_MAX_WIDTH = 1920;
export const VIEWER_MAX_HEIGHT = 1080;
/** The card is sized as if it were this many CSS pixels wide: sharp, and zoomable in place. */
const CARD_REFERENCE = { width: 960, height: 600 };
const CARD_CAP = { width: 1280, height: 800 };
const CARD_QUALITY = 74;
const VIEWER_QUALITY = 80;

export function sessionRequest(input: {
  viewerOpen: boolean;
  owning: boolean;
  mode?: QualityMode;
  /** Adaptive step, 0 = none (see `ADAPT_LEVELS`). */
  level?: number;
  dpr?: number;
}): SessionRequest {
  const mode = input.mode ?? "auto";
  const level = Math.min(Math.max(input.level ?? 0, 0), ADAPT_LEVELS.length - 1);
  const step = ADAPT_LEVELS[level];
  // Moving content is encoded at this quality (4:4:4, the host steps it down to fit a slow
  // link); the host adds one lossless frame once the screen has been still for a moment.
  let box = input.viewerOpen
    ? { width: VIEWER_MAX_WIDTH, height: VIEWER_MAX_HEIGHT }
    : (() => {
        const b = requestBox(CARD_REFERENCE.width, CARD_REFERENCE.height, input.dpr ?? 1);
        return {
          width: Math.min(b.width, CARD_CAP.width),
          height: Math.min(b.height, CARD_CAP.height)
        };
      })();
  box = scaleBox(box, step.scale);
  const base = (input.viewerOpen ? VIEWER_QUALITY : CARD_QUALITY) + step.qualityDelta;
  const applied = applyMode(mode, box, base);
  return {
    maxWidth: applied.box.width,
    maxHeight: applied.box.height,
    quality: Math.min(95, Math.max(30, applied.quality))
  };
}
