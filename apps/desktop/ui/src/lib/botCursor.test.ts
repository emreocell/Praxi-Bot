import assert from "node:assert/strict";
import { test } from "node:test";
import {
  centerOf,
  parseBotPointer,
  placeCursor,
  rippleDue,
  showBotCursor
} from "./botCursor.ts";

test("a wire pointer is read, clamped to the picture and never trusted when malformed", () => {
  assert.deepEqual(parseBotPointer({ x: 0.25, y: 1, click_seq: 3 }), { x: 0.25, y: 1, clickSeq: 3 });
  assert.deepEqual(parseBotPointer({ x: 0, y: 0 }), { x: 0, y: 0, clickSeq: 0 });
  for (const bad of [
    null,
    undefined,
    "x",
    {},
    { x: -0.1, y: 0.5 },
    { x: 0.5, y: 1.01 },
    { x: Number.NaN, y: 0.5 },
    { x: Infinity, y: 0.5 },
    { x: "0.5", y: 0.5 }
  ]) {
    assert.equal(parseBotPointer(bad), null, JSON.stringify(bad));
  }
  assert.equal(parseBotPointer({ x: 0.5, y: 0.5, click_seq: -2 })?.clickSeq, 0);
  assert.equal(parseBotPointer({ x: 0.5, y: 0.5, click_seq: 1.5 })?.clickSeq, 0);
});

test("the pointer lands inside the letterboxed picture, not the stage", () => {
  // A 1000x600 stage showing a 16:9 picture centred in it.
  const picture = { x: 0, y: 18.75, width: 1000, height: 562.5 };
  assert.deepEqual(placeCursor({ x: 0, y: 0 }, picture), { left: 0, top: 18.75 });
  assert.deepEqual(placeCursor({ x: 0.5, y: 0.5 }, picture), { left: 500, top: 300 });
  assert.deepEqual(placeCursor({ x: 1, y: 1 }, picture), { left: 1000, top: 581.25 });
});

test("a ripple plays when the click counter grows, not for the first frame seen", () => {
  assert.equal(rippleDue(null, 5), false);
  assert.equal(rippleDue(5, 5), false);
  assert.equal(rippleDue(5, 6), true);
  assert.equal(rippleDue(6, 2), false, "a reset counter is not a click");
});

test("the overlay rests the pointer at the centre of the target", () => {
  assert.deepEqual(centerOf({ x: 100, y: 50, width: 40, height: 20 }), { x: 120, y: 60 });
});

test("the viewer hides the bot pointer when the frame has none or while the owner drives", () => {
  const pose = { x: 0.5, y: 0.5, clickSeq: 0 };
  assert.equal(showBotCursor({ pose, ownerDriving: false }), true);
  assert.equal(showBotCursor({ pose: null, ownerDriving: false }), false);
  assert.equal(showBotCursor({ pose, ownerDriving: true }), false);
});
