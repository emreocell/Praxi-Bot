import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ADAPT_LEVELS,
  DEEP_QUEUE_FRAMES,
  INITIAL_ADAPT,
  MAX_ADAPT_LEVEL,
  QUALITY_MODES,
  SLOW_DECODE_MS,
  STALL_AFTER_MS,
  STEP_DOWN_AFTER,
  STEP_UP_AFTER,
  adaptLevelFor,
  adaptStep,
  isQualityMode,
  streamHealth,
  type AdaptState
} from "./liveViewAdaptive.ts";

const healthySample = { decodeMs: SLOW_DECODE_MS, bufferedFrames: DEEP_QUEUE_FRAMES - 1 };
const slowDecodeSample = { decodeMs: SLOW_DECODE_MS + 1, bufferedFrames: 0 };
const deepQueueSample = { decodeMs: 0, bufferedFrames: DEEP_QUEUE_FRAMES };

function repeatStep(start: AdaptState, sample: { decodeMs: number; bufferedFrames: number }, times: number): AdaptState {
  let state = start;
  for (let i = 0; i < times; i += 1) {
    state = adaptStep(state, sample);
  }
  return state;
}

test("adaptStep counts slow samples and steps down on the third consecutive slow one", () => {
  const afterOne = adaptStep(INITIAL_ADAPT, slowDecodeSample);
  assert.deepEqual(afterOne, { level: 0, slow: 1, healthy: 0 });

  const afterTwo = adaptStep(afterOne, slowDecodeSample);
  assert.deepEqual(afterTwo, { level: 0, slow: 2, healthy: 0 });

  const afterThree = adaptStep(afterTwo, slowDecodeSample);
  assert.deepEqual(afterThree, { level: 1, slow: 0, healthy: 0 });
  assert.equal(STEP_DOWN_AFTER, 3);
});

test("adaptStep resets slow counter on an intervening healthy sample", () => {
  const twoSlow = repeatStep(INITIAL_ADAPT, slowDecodeSample, 2);
  const interrupted = adaptStep(twoSlow, healthySample);
  assert.deepEqual(interrupted, { level: 0, slow: 0, healthy: 1 });
});

test("adaptStep treats a deep queue as slow even with a fast decode", () => {
  const stepped = repeatStep(INITIAL_ADAPT, deepQueueSample, STEP_DOWN_AFTER);
  assert.deepEqual(stepped, { level: 1, slow: 0, healthy: 0 });
});

test("adaptStep steps up after 20 healthy samples and clamps between 0 and MAX_ADAPT_LEVEL", () => {
  const atMax = repeatStep(INITIAL_ADAPT, slowDecodeSample, STEP_DOWN_AFTER * (MAX_ADAPT_LEVEL + 2));
  assert.equal(atMax.level, MAX_ADAPT_LEVEL);
  assert.equal(ADAPT_LEVELS.length - 1, MAX_ADAPT_LEVEL);

  const nineteenHealthy = repeatStep({ level: MAX_ADAPT_LEVEL, slow: 0, healthy: 0 }, healthySample, STEP_UP_AFTER - 1);
  assert.deepEqual(nineteenHealthy, { level: MAX_ADAPT_LEVEL, slow: 0, healthy: STEP_UP_AFTER - 1 });

  const steppedUp = adaptStep(nineteenHealthy, healthySample);
  assert.deepEqual(steppedUp, { level: MAX_ADAPT_LEVEL - 1, slow: 0, healthy: 0 });

  const clampedAtZero = repeatStep(INITIAL_ADAPT, healthySample, STEP_UP_AFTER + 5);
  assert.equal(clampedAtZero.level, 0);
});

test("adaptLevelFor follows state only in auto mode and pins high and saver to 0", () => {
  const state = { level: 2, slow: 1, healthy: 0 };
  assert.equal(adaptLevelFor("auto", state), 2);
  assert.equal(adaptLevelFor("high", state), 0);
  assert.equal(adaptLevelFor("saver", state), 0);
});

test("isQualityMode accepts only the three supported modes", () => {
  for (const mode of QUALITY_MODES) {
    assert.equal(isQualityMode(mode), true);
  }
  for (const bad of ["", "low", "AUTO", 0, 1, null, undefined, {}]) {
    assert.equal(isQualityMode(bad), false);
  }
});

test("streamHealth evaluates ended, connecting, live and stalled boundaries", () => {
  assert.equal(
    streamHealth({ hasFrame: false, lastActivityAtMs: null, nowMs: 10_000, ended: true }),
    "ended"
  );
  assert.equal(
    streamHealth({ hasFrame: false, lastActivityAtMs: 9_000, nowMs: 10_000, ended: false }),
    "connecting"
  );
  assert.equal(
    streamHealth({ hasFrame: true, lastActivityAtMs: null, nowMs: 10_000, ended: false }),
    "live"
  );
  assert.equal(
    streamHealth({ hasFrame: true, lastActivityAtMs: 1_000, nowMs: 1_000 + STALL_AFTER_MS, ended: false }),
    "live"
  );
  assert.equal(
    streamHealth({ hasFrame: true, lastActivityAtMs: 1_000, nowMs: 1_000 + STALL_AFTER_MS + 1, ended: false }),
    "stalled"
  );
});
