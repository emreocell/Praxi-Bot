import assert from "node:assert/strict";
import { test } from "node:test";
import {
  BACKDROP_DIM_MAX,
  BACKDROP_DIM_MIN,
  BACKDROP_SCENES,
  CUSTOM_BACKDROP,
  DEFAULT_BACKDROP_DIM,
  FAVORITE_BACKDROPS,
  NO_BACKDROP,
  backdropInfo,
  backdropIsActive,
  backdropSize,
  backdropsInGroup,
  dimToSlider,
  parseBackdrop,
  parseBackdropDim,
  sliderToDim
} from "./backdropCatalog.ts";

test("scene ids are unique and the favourites all exist", () => {
  const ids = BACKDROP_SCENES.map((s) => s.id);
  assert.equal(new Set(ids).size, ids.length);
  for (const f of FAVORITE_BACKDROPS) assert.ok(backdropInfo(f), f);
  assert.equal(backdropInfo("nope"), undefined);
});

test("groups partition the catalog", () => {
  const total = (["dotted", "grain", "glass", "other"] as const).flatMap((g) => backdropsInGroup(g));
  assert.equal(total.length, BACKDROP_SCENES.length);
  assert.ok(backdropsInGroup("glass").every((s) => s.group === "glass"));
});

test("parseBackdrop keeps known ids and custom, and falls back to none", () => {
  assert.equal(parseBackdrop("aurora"), "aurora");
  assert.equal(parseBackdrop(CUSTOM_BACKDROP), CUSTOM_BACKDROP);
  assert.equal(parseBackdrop("unknown"), NO_BACKDROP);
  assert.equal(parseBackdrop(42), NO_BACKDROP);
  assert.equal(parseBackdrop(undefined), NO_BACKDROP);
});

test("parseBackdropDim clamps, rounds and defaults", () => {
  assert.equal(parseBackdropDim(50.6), 51);
  assert.equal(parseBackdropDim("70"), 70);
  assert.equal(parseBackdropDim(0), BACKDROP_DIM_MIN);
  assert.equal(parseBackdropDim(500), BACKDROP_DIM_MAX);
  assert.equal(parseBackdropDim("abc"), DEFAULT_BACKDROP_DIM);
  assert.equal(parseBackdropDim(null), DEFAULT_BACKDROP_DIM);
  assert.equal(parseBackdropDim(Infinity), DEFAULT_BACKDROP_DIM);
});

test("the slider mirrors the stored dim", () => {
  assert.equal(dimToSlider(BACKDROP_DIM_MIN), BACKDROP_DIM_MAX);
  assert.equal(dimToSlider(BACKDROP_DIM_MAX), BACKDROP_DIM_MIN);
  for (const d of [20, 33, 60, 88]) assert.equal(sliderToDim(dimToSlider(d)), d);
  assert.equal(sliderToDim(0), BACKDROP_DIM_MAX);
});

test("a backdrop is active when it is a scene, or custom with an image", () => {
  assert.equal(backdropIsActive(NO_BACKDROP, "x.png"), false);
  assert.equal(backdropIsActive("ink", ""), true);
  assert.equal(backdropIsActive(CUSTOM_BACKDROP, ""), false);
  assert.equal(backdropIsActive(CUSTOM_BACKDROP, "   "), false);
  assert.equal(backdropIsActive(CUSTOM_BACKDROP, "x.png"), true);
});

test("backdropSize divides by the scale and caps the pixel count", () => {
  assert.deepEqual(backdropSize(1000, 500, 3), { w: 334, h: 167 });
  const big = backdropSize(4000, 3000, 1);
  assert.ok(big.w * big.h <= 2_000_000 * 1.01);
  assert.deepEqual(backdropSize(0, 0, 3), { w: 1, h: 1 });
  assert.deepEqual(backdropSize(100, 100, 1, 100), { w: 10, h: 10 });
});
