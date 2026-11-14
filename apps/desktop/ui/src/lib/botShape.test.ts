import assert from "node:assert/strict";
import { test } from "node:test";
import {
  BOT_SHAPE_IDS,
  BOT_SHAPES,
  botShape,
  fallbackBotShape,
  gradientMapConfig,
  normalizeBotShape
} from "../../../../../packages/ui/src/botShape.ts";

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
