import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ADAPT_LEVELS,
  adaptLevelFor,
  adaptStep,
  INITIAL_ADAPT,
  STALL_AFTER_MS,
  STEP_DOWN_AFTER,
  STEP_UP_AFTER,
  streamHealth
} from "./liveViewAdaptive.ts";
import { applyMode, cardAspect, fitWithin, requestBox, scaleBox } from "./liveViewSizing.ts";
import { loadQualityMode, saveQualityMode } from "./liveViewQualityPref.ts";
import { sessionRequest } from "../state/liveViewRequest.ts";

test("fitWithin keeps the aspect ratio and never upscales", () => {
  assert.deepEqual(fitWithin({ width: 1920, height: 1080 }, { width: 1280, height: 800 }), { width: 1280, height: 720 });
  assert.deepEqual(fitWithin({ width: 800, height: 600 }, { width: 1920, height: 1080 }), { width: 800, height: 600 });
  assert.deepEqual(fitWithin({ width: 0, height: 10 }, { width: 5, height: 5 }), { width: 0, height: 0 });
});

test("requestBox is one stream pixel per device pixel, between the readable floor and the host cap", () => {
  assert.deepEqual(requestBox(800, 500, 1), { width: 800, height: 500 });
  assert.deepEqual(requestBox(800, 500, 2), { width: 1600, height: 1000 });
  assert.deepEqual(requestBox(4000, 3000, 2), { width: 1920, height: 1080 });
  assert.equal(requestBox(100, 50, 1).width, 480);
  assert.deepEqual(requestBox(800, 500, Number.NaN), { width: 800, height: 500 });
});

test("quality modes: high pins the cap, data saver trims, auto leaves the request alone", () => {
  const box = { width: 1280, height: 800 };
  assert.deepEqual(applyMode("auto", box, 74), { box, quality: 74 });
  const high = applyMode("high", box, 74);
  assert.deepEqual(high.box, { width: 1920, height: 1080 });
  assert.ok(high.quality >= 88);
  const saver = applyMode("saver", { width: 1920, height: 1080 }, 80);
  assert.deepEqual(saver.box, { width: 960, height: 540 });
  assert.ok(saver.quality <= 55);
});

test("scaleBox never goes below a readable size or above the box", () => {
  assert.deepEqual(scaleBox({ width: 1280, height: 800 }, 0.5), { width: 640, height: 400 });
  assert.equal(scaleBox({ width: 1000, height: 600 }, 0.1).width, 480);
  assert.deepEqual(scaleBox({ width: 640, height: 400 }, 1), { width: 640, height: 400 });
});

test("the card follows the screen's aspect ratio instead of letterboxing in a 16:9 box", () => {
  assert.equal(cardAspect("1280×800"), "1600 / 1000");
  assert.equal(cardAspect("1920x1080"), "1778 / 1000");
  assert.equal(cardAspect(null), "16 / 10");
  assert.equal(cardAspect("0×0"), "16 / 10");
});

test("the card asks for a sharp picture (>= 960 wide), the viewer for the full cap", () => {
  const card = sessionRequest({ viewerOpen: false, owning: false });
  assert.ok(card.maxWidth >= 960 && card.maxWidth <= 1280);
  const retina = sessionRequest({ viewerOpen: false, owning: false, dpr: 2 });
  assert.equal(retina.maxWidth, 1280);
  const saver = sessionRequest({ viewerOpen: true, owning: false, mode: "saver" });
  assert.deepEqual([saver.maxWidth, saver.maxHeight], [960, 540]);
  const slow = sessionRequest({ viewerOpen: true, owning: false, level: 3 });
  assert.ok(slow.maxWidth < 1920 && slow.quality < 80);
});

test("the adaptive controller steps down after sustained slowness and back up after sustained health", () => {
  let st = INITIAL_ADAPT;
  for (let i = 0; i < STEP_DOWN_AFTER; i++) st = adaptStep(st, { decodeMs: 90, bufferedFrames: 0 });
  assert.equal(st.level, 1);
  // a single healthy sample resets the slow streak: no flapping
  st = adaptStep(st, { decodeMs: 5, bufferedFrames: 0 });
  st = adaptStep(st, { decodeMs: 90, bufferedFrames: 0 });
  assert.equal(st.level, 1);
  for (let i = 0; i < 50; i++) st = adaptStep(st, { decodeMs: 90, bufferedFrames: 4 });
  assert.equal(st.level, ADAPT_LEVELS.length - 1);
  for (let i = 0; i < STEP_UP_AFTER; i++) st = adaptStep(st, { decodeMs: 4, bufferedFrames: 0 });
  assert.equal(st.level, ADAPT_LEVELS.length - 2);
  assert.equal(adaptLevelFor("high", st), 0);
  assert.equal(adaptLevelFor("auto", st), st.level);
});

test("stream health: connecting, live, stalled, ended", () => {
  const now = 100_000;
  assert.equal(streamHealth({ hasFrame: false, lastActivityAtMs: null, nowMs: now, ended: false }), "connecting");
  assert.equal(streamHealth({ hasFrame: true, lastActivityAtMs: now - 500, nowMs: now, ended: false }), "live");
  assert.equal(streamHealth({ hasFrame: true, lastActivityAtMs: now - STALL_AFTER_MS - 1, nowMs: now, ended: false }), "stalled");
  assert.equal(streamHealth({ hasFrame: true, lastActivityAtMs: now, nowMs: now, ended: true }), "ended");
});

test("the saved quality mode survives bad storage", () => {
  const mem = new Map<string, string>();
  const store = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => void mem.set(k, v) };
  assert.equal(loadQualityMode(store), "auto");
  saveQualityMode("saver", store);
  assert.equal(loadQualityMode(store), "saver");
  mem.set("praxi.liveView.quality", "bogus");
  assert.equal(loadQualityMode(store), "auto");
  assert.equal(loadQualityMode({ getItem: () => { throw new Error("blocked"); } }), "auto");
});
