import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { contrastRatio, deriveTheme, paletteById } from "../../../../../packages/ui/src/themeModel.ts";
import {
  accessibilityVars,
  DEFAULT_ACCESSIBILITY,
  effectiveWallpaperMotion,
  FOCUS_RING_PX,
  normalizeAccessibility,
  parseTextScale,
  TEXT_TOKENS_PX
} from "./accessibility.ts";
import { DEFAULT_APPEARANCE, normalizeAppearance } from "./appearance.ts";

test("the defaults leave <html> untouched and garbage falls back to them", () => {
  assert.deepEqual(accessibilityVars(DEFAULT_ACCESSIBILITY), {});
  assert.deepEqual(
    normalizeAccessibility({ textScale: "huge", density: "tiny", dyslexiaFont: "yes", focusRing: 9, reduceMotion: 1 }),
    DEFAULT_ACCESSIBILITY
  );
  assert.deepEqual(normalizeAppearance({}), DEFAULT_APPEARANCE);
});

test("text size is clamped to 90-150 % and scales every font token", () => {
  assert.equal(parseTextScale(10), 90);
  assert.equal(parseTextScale(400), 150);
  assert.equal(parseTextScale("125"), 125);
  const vars = accessibilityVars({ ...DEFAULT_ACCESSIBILITY, textScale: 150 });
  for (const [token, base] of Object.entries(TEXT_TOKENS_PX)) assert.equal(vars[token], `${base * 1.5}px`);
});

test("each switch maps to its own tokens", () => {
  const v = accessibilityVars({
    ...DEFAULT_ACCESSIBILITY,
    dyslexiaFont: true,
    reduceMotion: true,
    density: "compact",
    focusRing: "extra"
  });
  assert.match(v["--font-sans"], /^"OpenDyslexic"/);
  assert.equal(v["--motion-duration-normal"], "0ms");
  assert.equal(v["--space-4"], "12.8px");
  assert.equal(v["--size-border-thick"], `${FOCUS_RING_PX.extra}px`);
});

test("reduce motion stops the live wallpaper", () => {
  assert.equal(effectiveWallpaperMotion("live", true), "off");
  assert.equal(effectiveWallpaperMotion("live", false), "live");
});

test("the High contrast themes pass AAA for text on every surface", () => {
  for (const id of ["contrast-dark", "contrast-light"]) {
    const theme = paletteById(id);
    assert.ok(theme, id);
    const d = deriveTheme(theme);
    for (const surface of [d.bg, d.surface, d.raised, d.hover]) {
      assert.ok(contrastRatio(d.text, surface) >= 7, `${id} text on ${surface}`);
      assert.ok(contrastRatio(d.secondary, surface) >= 7, `${id} secondary text on ${surface}`);
    }
  }
});

test("accessibility CSS uses theme tokens only (no hard-coded colours)", () => {
  for (const file of ["../global.css", "../components/AppearancePicker.module.css"]) {
    const css = readFileSync(new URL(file, import.meta.url), "utf8");
    const rules = css.split("\n").filter((l) => /reduce-motion|\.preview/.test(l));
    for (const l of rules) assert.doesNotMatch(l, /#[0-9a-f]{3,8}\b|rgba?\(|hsla?\(/i, l);
  }
});

test("the scaled base sizes match packages/ui/tokens.css", async () => {
  const { SPACE_TOKENS_PX } = await import("./accessibility.ts");
  const css = readFileSync(new URL("../../../../../packages/ui/tokens.css", import.meta.url), "utf8");
  for (const [token, base] of [...Object.entries(TEXT_TOKENS_PX), ...Object.entries(SPACE_TOKENS_PX)]) {
    assert.match(css, new RegExp(`${token}: ?${base}px;`), token);
  }
});
