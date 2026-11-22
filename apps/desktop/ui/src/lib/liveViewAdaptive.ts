/**
 * Adaptive live-view quality. The host already trims each frame to its own byte budget; this is
 * the viewer's half: when this computer cannot decode or draw frames fast enough (slow CPU, a
 * busy window), it asks the host for a smaller, cheaper picture, and climbs back once it keeps
 * up. Pure and hermetic: the state machine never touches the DOM or the clock.
 */

/** What the owner chose. `auto` adapts; the other two pin the picture. */
export type QualityMode = "auto" | "high" | "saver";

export const QUALITY_MODES: readonly QualityMode[] = ["auto", "high", "saver"];

export function isQualityMode(value: unknown): value is QualityMode {
  return value === "auto" || value === "high" || value === "saver";
}

/** Level 0 is the full-quality request; each step is cheaper. */
export const ADAPT_LEVELS = [
  { scale: 1, qualityDelta: 0 },
  { scale: 0.75, qualityDelta: -8 },
  { scale: 0.5, qualityDelta: -15 },
  { scale: 0.375, qualityDelta: -22 }
] as const;

export const MAX_ADAPT_LEVEL = ADAPT_LEVELS.length - 1;

/** A frame that takes longer than this to decode, or a queue this deep, counts as a slow sample. */
export const SLOW_DECODE_MS = 45;
export const DEEP_QUEUE_FRAMES = 3;
/** Consecutive slow samples before stepping down / healthy samples before stepping up. */
export const STEP_DOWN_AFTER = 3;
export const STEP_UP_AFTER = 20;

export interface AdaptState {
  level: number;
  slow: number;
  healthy: number;
}

export interface AdaptSample {
  decodeMs: number;
  bufferedFrames: number;
}

export const INITIAL_ADAPT: AdaptState = { level: 0, slow: 0, healthy: 0 };

export function adaptStep(state: AdaptState, sample: AdaptSample): AdaptState {
  const isSlow = sample.decodeMs > SLOW_DECODE_MS || sample.bufferedFrames >= DEEP_QUEUE_FRAMES;
  if (isSlow) {
    const slow = state.slow + 1;
    if (slow >= STEP_DOWN_AFTER && state.level < MAX_ADAPT_LEVEL) {
      return { level: state.level + 1, slow: 0, healthy: 0 };
    }
    return { level: state.level, slow, healthy: 0 };
  }
  const healthy = state.healthy + 1;
  if (healthy >= STEP_UP_AFTER && state.level > 0) {
    return { level: state.level - 1, slow: 0, healthy: 0 };
  }
  return { level: state.level, slow: 0, healthy };
}

/** Whether a sample should be fed at all: only `auto` adapts, the others stay where they are. */
export function adaptLevelFor(mode: QualityMode, state: AdaptState): number {
  return mode === "auto" ? state.level : 0;
}

/** How a stream looks to the viewer right now. */
export type StreamHealth = "connecting" | "live" | "stalled" | "ended";

/** No frame and no heartbeat for this long and the picture is called stalled, not live. */
export const STALL_AFTER_MS = 4000;

export function streamHealth(input: {
  hasFrame: boolean;
  lastActivityAtMs: number | null;
  nowMs: number;
  ended: boolean;
}): StreamHealth {
  if (input.ended) return "ended";
  if (!input.hasFrame) return "connecting";
  if (input.lastActivityAtMs === null) return "live";
  return input.nowMs - input.lastActivityAtMs > STALL_AFTER_MS ? "stalled" : "live";
}
