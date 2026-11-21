import assert from "node:assert/strict";
import { test } from "node:test";
import {
  gradientBase,
  gradientVars,
  hexToOklch,
  NEUTRAL_CHROMA
} from "./botGradient.ts";

test("hexToOklch parses valid 3-digit and 6-digit hex colors", () => {
  // 6-digit with hash
  const red = hexToOklch("#ff0000");
  assert.ok(red);
  assert.ok(red.l > 0.6 && red.l < 0.65);
  assert.ok(red.c > 0.25);
  assert.ok(red.h > 25 && red.h < 35);

  // 3-digit with hash
  const blue = hexToOklch("#00f");
  assert.ok(blue);
  assert.ok(blue.c > 0.2);

  // Without leading hash and mixed case
  const green = hexToOklch("00FF00");
  assert.ok(green);
  assert.ok(green.l > 0.8);

  // Trims surrounding whitespace
  const trimmed = hexToOklch("  #123456  ");
  assert.ok(trimmed);
});

test("hexToOklch returns null for invalid formats or empty inputs", () => {
  assert.equal(hexToOklch(null), null);
  assert.equal(hexToOklch(undefined), null);
  assert.equal(hexToOklch(""), null);
  assert.equal(hexToOklch("   "), null);
  assert.equal(hexToOklch("not-a-color"), null);
  assert.equal(hexToOklch("#12"), null);
  assert.equal(hexToOklch("#1234"), null);
  assert.equal(hexToOklch("#1234567"), null);
  assert.equal(hexToOklch("#xyz123"), null);
});

test("hexToOklch handles achromatic boundaries (black and white)", () => {
  const black = hexToOklch("#000000");
  assert.ok(black);
  assert.equal(black.l, 0);
  assert.equal(black.c, 0);
  assert.equal(black.h, 0);

  const white = hexToOklch("#ffffff");
  assert.ok(white);
  assert.ok(Math.abs(white.l - 1.0) < 1e-6);
  assert.ok(white.c < NEUTRAL_CHROMA);
});

test("gradientBase returns fallback for null, undefined, or invalid colors", () => {
  const fallback = gradientBase(null);
  assert.equal(fallback.l, 0.7);
  assert.equal(fallback.c, 0.12);
  assert.equal(fallback.h, 270);

  assert.deepEqual(gradientBase(undefined), fallback);
  assert.deepEqual(gradientBase("invalid"), fallback);
});

test("gradientBase provides a cool neutral tint for low-chroma colors", () => {
  // Pure white and pure grey are neutral (chroma < NEUTRAL_CHROMA)
  const whiteBase = gradientBase("#ffffff");
  assert.equal(whiteBase.h, 268); // NEUTRAL_TINT.h
  assert.equal(whiteBase.c, 0.05); // NEUTRAL_TINT.c

  const greyBase = gradientBase("#808080");
  assert.equal(greyBase.h, 268);
  assert.equal(greyBase.c, 0.05);
});

test("gradientBase clamps lightness into allowed bounds", () => {
  // Dark mode (default): lightness clamped to [0.5, 0.82]
  const darkClamped = gradientBase("#000000");
  assert.equal(darkClamped.l, 0.5);

  const lightClamped = gradientBase("#ffffff");
  assert.equal(lightClamped.l, 0.82);

  // Light mode (light = true): lightness clamped to [0.42, 0.62], min chroma 0.1
  const darkLightMode = gradientBase("#000000", true);
  assert.equal(darkLightMode.l, 0.42);
  assert.ok(darkLightMode.c >= 0.1);

  const whiteLightMode = gradientBase("#ffffff", true);
  assert.equal(whiteLightMode.l, 0.62);
  assert.ok(whiteLightMode.c >= 0.1);
});

test("gradientVars generates expected CSS custom properties", () => {
  const vars = gradientVars("#ff0000");
  assert.ok(vars["--bot"]);
  assert.ok(vars["--bot"].startsWith("oklch("));
  assert.equal(
    vars["--bot2"],
    "oklch(from var(--bot) calc(l - .08) c calc(h + 48))"
  );
  assert.equal(
    vars["--bot3"],
    "oklch(from var(--bot) calc(l - .22) calc(c * .8) calc(h - 30))"
  );
});

test("gradientVars supports light mode option", () => {
  const darkVars = gradientVars("#3b82f6", false);
  const lightVars = gradientVars("#3b82f6", true);

  assert.notEqual(darkVars["--bot"], lightVars["--bot"]);
  assert.equal(darkVars["--bot2"], lightVars["--bot2"]);
  assert.equal(darkVars["--bot3"], lightVars["--bot3"]);
});
