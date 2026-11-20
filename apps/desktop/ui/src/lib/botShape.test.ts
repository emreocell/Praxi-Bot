import assert from "node:assert/strict";
import { test } from "node:test";
import {
  BOT_AVATAR_STATES,
  BOT_SHAPE_GEOMETRY,
  BOT_SHAPE_IDS,
  BOT_SHAPES,
  botAvatarMaterial,
  botAvatarTiming,
  botShape,
  CHARACTER_COLORS,
  fallbackBotShape,
  gradientMapConfig,
  normalizeBotAvatarState,
  normalizeBotShape,
  resolveAvatarMotion
} from "../../../../../packages/ui/src/botShape.ts";
import { computeEyeGazeOffset } from "../../../../../packages/ui/src/eyeGaze.ts";
import { contrastRatio } from "../../../../../packages/ui/src/themeModel.ts";

test("character registry exposes all 16 clay shapes with valid 64x64 eye coordinates", () => {
  assert.equal(BOT_SHAPES.length, 16);
  assert.equal(new Set(BOT_SHAPE_IDS).size, 16);
  for (const s of BOT_SHAPES) {
    assert.ok(s.leftEyeX > 0 && s.leftEyeX < s.rightEyeX);
    assert.ok(s.rightEyeX < 64);
    assert.ok(s.eyeY > 0 && s.eyeY < 64);
  }
});

test("bot shape fallback is deterministic and resolves explicit canonical and alias names", () => {
  const original = botShape("atlas-stable-id");
  botShape("another-bot");
  assert.deepEqual(botShape("atlas-stable-id"), original);
  assert.equal(fallbackBotShape("atlas-stable-id"), original.id);

  for (const id of ["", "🌱-nova", "ı-İ-atlas", "x".repeat(1024)]) {
    const picked = botShape(id);
    assert.deepEqual(picked, botShape(id));
    assert.ok(BOT_SHAPE_IDS.includes(picked.id));
  }

  assert.equal(normalizeBotShape("cloud"), "cloud");
  assert.equal(normalizeBotShape("BULUT"), "cloud");
  assert.equal(normalizeBotShape("elmas"), "diamond");
  assert.equal(normalizeBotShape("nonexistent-shape"), null);
  assert.equal(botShape("atlas-stable-id", "cicek").id, "flower");
  assert.equal(botShape("atlas-stable-id", "unknown").id, original.id);
});

test("deterministic fallback distributes synthetic bot IDs across distinct shapes", () => {
  const seen = new Set(Array.from({ length: 64 }, (_, i) => fallbackBotShape(`bot-${i}`)));
  assert.ok(seen.size >= 12);
});

test("gradientMapConfig produces monotonic saturated 5-stop tables and handles invalid inputs", () => {
  const teal = gradientMapConfig("#2fc4b2");
  assert.equal(teal.color, "#2fc4b2");
  assert.equal(teal.stops.length, 5);
  assert.equal(teal.stops[3], "#2fc4b2");
  assert.equal(teal.tableR.split(" ").length, 5);
  assert.equal(teal.tableG.split(" ").length, 5);
  assert.equal(teal.tableB.split(" ").length, 5);

  const shortHex = gradientMapConfig(" #ABC ");
  assert.equal(shortHex.color, "#aabbcc");

  for (const weird of [null, undefined, "", "var(--color-accent)", "#000000", "#ffffff", "not-a-hex"]) {
    const cfg = gradientMapConfig(weird);
    assert.match(cfg.color, /^#[0-9a-f]{6}$/);
    for (const table of [cfg.tableR, cfg.tableG, cfg.tableB]) {
      const nums = table.split(" ").map(Number);
      assert.equal(nums.length, 5);
      assert.ok(nums.every((n) => Number.isFinite(n) && n >= 0 && n <= 1));
    }
  }
});

test("all 16 character shapes define valid procedural SVG geometry", () => {
  for (const shape of BOT_SHAPES) {
    const geo = BOT_SHAPE_GEOMETRY[shape.id];
    assert.ok(geo, `missing geometry for ${shape.id}`);
    assert.match(geo.path, /^M\s.+Z$/);
    assert.ok(geo.eyeScale > 0.75 && geo.eyeScale <= 1.15);
    assert.ok(geo.shadowRx >= 10 && geo.shadowRx <= 24);
  }
});

test("botAvatarMaterial preserves chosen tint and guarantees readable eye contrast across light and dark colours", () => {
  for (const hex of [...CHARACTER_COLORS, "#0d0f14", "#1b1f2a", "#ffffff"]) {
    const mat = botAvatarMaterial(hex);
    assert.equal(mat.base, hex.toLowerCase());
    assert.match(mat.highlight, /^#[0-9a-f]{6}$/);
    assert.match(mat.shadow, /^#[0-9a-f]{6}$/);
    assert.match(mat.deepShadow, /^#[0-9a-f]{6}$/);
    assert.match(mat.rimLight, /^#[0-9a-f]{6}$/);
    assert.match(mat.contour, /^#[0-9a-f]{6}$/);
    assert.match(mat.eyeColor, /^#[0-9a-f]{6}$/);
    assert.ok(
      contrastRatio(mat.eyeColor, mat.base) >= 3.5,
      `eye contrast too low for ${hex}: ${contrastRatio(mat.eyeColor, mat.base).toFixed(2)}`
    );
  }
  const darkMat = botAvatarMaterial("#111318");
  assert.equal(darkMat.eyeColor, "#f4f6fa");
  assert.equal(darkMat.catchlightOpacity, 0);
  const lightMat = botAvatarMaterial("#f4f4f5");
  assert.ok(lightMat.catchlightOpacity > 0);
});

test("botAvatarTiming produces deterministic, irregular non-lockstep timings across a list of bots", () => {
  const ids = ["atlas", "nova", "mira", "echo", "praxi", "byte", "sage", "luna"];
  const timings = ids.map((id) => botAvatarTiming(id));
  assert.deepEqual(botAvatarTiming("atlas"), timings[0]);
  for (const t of timings) {
    assert.ok(t.breathDurationSec >= 3.8 && t.breathDurationSec <= 5.6);
    assert.ok(t.breathDelaySec <= 0 && t.breathDelaySec >= -t.breathDurationSec);
    assert.ok(t.blinkDurationSec >= 4.3 && t.blinkDurationSec <= 7.2);
    assert.ok(t.blinkDelaySec <= 0 && t.blinkDelaySec >= -t.blinkDurationSec);
  }
  const uniqueBreathDelays = new Set(timings.map((t) => t.breathDelaySec));
  const uniqueBlinkDelays = new Set(timings.map((t) => t.blinkDelaySec));
  assert.ok(uniqueBreathDelays.size >= 5, "breathing phases should vary across bots");
  assert.ok(uniqueBlinkDelays.size >= 5, "blink phases should vary across bots");
});

test("normalizeBotAvatarState maps canonical and daemon snake_case states safely", () => {
  for (const st of BOT_AVATAR_STATES) {
    assert.equal(normalizeBotAvatarState(st), st);
  }
  assert.equal(normalizeBotAvatarState("waiting_approval"), "waiting-approval");
  assert.equal(normalizeBotAvatarState("THINKING"), "thinking");
  assert.equal(normalizeBotAvatarState("unknown"), "idle");
  assert.equal(normalizeBotAvatarState(null), "idle");
  assert.equal(normalizeBotAvatarState(undefined), "idle");
});

test("resolveAvatarMotion respects prefers-reduced-motion and disables cursor gaze in lists", () => {
  assert.deepEqual(
    resolveAvatarMotion({ size: "md", state: "idle", prefersReducedMotion: false }),
    { breathe: true, blink: true, gaze: false, state: "idle", reducedMotion: false }
  );
  assert.deepEqual(
    resolveAvatarMotion({ size: "hero", state: "idle", prefersReducedMotion: false }),
    { breathe: true, blink: true, gaze: true, state: "idle", reducedMotion: false }
  );
  assert.deepEqual(
    resolveAvatarMotion({ size: "sm", state: "idle", live: false, prefersReducedMotion: false }),
    { breathe: false, blink: false, gaze: false, state: "idle", reducedMotion: false }
  );
  assert.deepEqual(
    resolveAvatarMotion({ size: "panel", state: "thinking", prefersReducedMotion: false }),
    { breathe: false, blink: true, gaze: false, state: "thinking", reducedMotion: false }
  );
  assert.deepEqual(
    resolveAvatarMotion({ size: "hero", state: "working", live: true, prefersReducedMotion: true }),
    { breathe: false, blink: false, gaze: false, state: "working", reducedMotion: true }
  );
});

test("computeEyeGazeOffset clamps cursor offsets smoothly and returns zero for degenerate bounds", () => {
  assert.deepEqual(computeEyeGazeOffset(100, 100, null), { xPct: 0, yPct: 0 });
  assert.deepEqual(computeEyeGazeOffset(100, 100, { left: 0, top: 0, width: 0, height: 0 }), {
    xPct: 0,
    yPct: 0
  });
  const rect = { left: 100, top: 100, width: 80, height: 80 };
  assert.deepEqual(computeEyeGazeOffset(140, 140, rect), { xPct: 0, yPct: 0 });
  const right = computeEyeGazeOffset(900, 140, rect, 5, 4);
  assert.equal(right.xPct, 5);
  assert.equal(right.yPct, 0);
  const topLeft = computeEyeGazeOffset(-500, -500, rect, 5, 4);
  assert.ok(topLeft.xPct < 0 && topLeft.xPct >= -5);
  assert.ok(topLeft.yPct < 0 && topLeft.yPct >= -4);
});
