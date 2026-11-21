import assert from "node:assert/strict";
import { test } from "node:test";
import {
  gazeFromPointer,
  idleSchedule,
  MAX_YAW,
  nextBlink,
  projectFace,
  seededRandom,
  sleepAfterMs,
  stateExpression
} from "../../../../../packages/ui/src/avatarBehavior.ts";
import {
  BOT_AVATAR_STATES,
  BOT_SHAPE_IDS,
  botAvatarMaterial,
  fallbackBotShape
} from "../../../../../packages/ui/src/botShape.ts";

test("shape ids and the id fallback stay stable (ids are persisted in profiles)", () => {
  assert.deepEqual(BOT_SHAPE_IDS, [
    "pebble", "cube", "diamond", "cloud", "tri", "flower", "gem", "drop",
    "star", "ghost", "capsule", "ears", "gumdrop", "heart", "clover", "boulder"
  ]);
  // Values recorded from main before the redesign: existing bots keep their character.
  const expected: Record<string, string> = {
    atlas: "gumdrop", nova: "boulder", mira: "gem", echo: "tri", praxi: "flower",
    "bot-2": "cube", "b7f3c2d1-0000-4000-8000-000000000001": "pebble"
  };
  for (const [id, shape] of Object.entries(expected)) assert.equal(fallbackBotShape(id), shape, id);
});

test("idle schedule is deterministic per seed and differs between seeds", () => {
  assert.deepEqual(idleSchedule(42, 12), idleSchedule(42, 12));
  assert.notDeepEqual(idleSchedule(42, 12), idleSchedule(43, 12));
  for (const a of idleSchedule(7, 200)) {
    assert.ok(a.delayMs >= 2400 && a.delayMs <= 7500, `delay ${a.delayMs}`);
    assert.ok(Math.abs(a.yaw) <= MAX_YAW);
  }
  const kinds = new Set(idleSchedule(99, 200).map((a) => a.kind));
  for (const k of ["glance", "look-around", "tilt", "settle", "shake-off"]) assert.ok(kinds.has(k as never), k);
  const rand = seededRandom(5);
  for (let i = 0; i < 100; i += 1) {
    const b = nextBlink(rand);
    assert.ok(b.delayMs >= 2000 && b.delayMs <= 6000);
  }
  const s = sleepAfterMs(123);
  assert.ok(s >= 45000 && s <= 80000);
  assert.equal(sleepAfterMs(123), s);
});

test("state expressions: thinking looks up, working and error narrow the eyes, waiting faces the viewer", () => {
  for (const st of BOT_AVATAR_STATES) assert.ok(stateExpression(st));
  assert.ok(stateExpression("thinking").pitch < 0);
  assert.ok(stateExpression("working").open < 1);
  assert.ok(stateExpression("error").open < stateExpression("working").open);
  assert.ok(stateExpression("error").droop > 0);
  assert.equal(stateExpression("waiting-approval").yaw, 0);
  assert.ok(stateExpression("waiting-approval").open > 1);
  assert.ok(stateExpression("idle", true).open < 0.5, "sleepy eyes are half closed");
});

test("gaze and face projection: clamped, eyes slide and the far eye foreshortens more", () => {
  assert.deepEqual(gazeFromPointer(0, 0, null), { yaw: 0, pitch: 0, distance: Infinity });
  const rect = { left: 100, top: 100, width: 80, height: 80 };
  const far = gazeFromPointer(5000, 140, rect);
  assert.ok(far.yaw > 31 && far.yaw <= 32 && far.pitch === 0);
  const centre = gazeFromPointer(140, 140, rect);
  assert.equal(centre.yaw, 0);

  const eyes = [{ x: 27, y: 36 }, { x: 37, y: 36 }];
  const rest = projectFace(0, 0, eyes, { x: 32, y: 36 }, 24);
  for (const e of rest.eyes) {
    assert.ok(Math.abs(e.tx) < 0.01 && Math.abs(e.sx - 1) < 0.01);
  }
  const right = projectFace(30, 0, eyes, { x: 32, y: 36 }, 24);
  assert.ok(right.eyes.every((e) => e.tx > 0), "eyes slide toward the turn");
  assert.ok(right.eyes[1].sx < right.eyes[0].sx, "far (right) eye is narrower");
  assert.ok(right.bodySx < 1 && right.lean > 0);
  const clamped = projectFace(500, 500, eyes, { x: 32, y: 36 }, 24);
  assert.deepEqual(clamped, projectFace(MAX_YAW, 90, eyes, { x: 32, y: 36 }, 24));
});

test("material: tint fills the body and white still gets real shading", () => {
  const coral = botAvatarMaterial("#f39a84");
  assert.equal(coral.mid, "#f39a84");
  assert.notEqual(coral.shadow, coral.base);
  const white = botAvatarMaterial("#f4f4f5");
  assert.notEqual(white.deepShadow, white.base);
  assert.ok(white.contourOpacity > 0);
});
