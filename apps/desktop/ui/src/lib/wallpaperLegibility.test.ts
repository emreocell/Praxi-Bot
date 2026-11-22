import assert from "node:assert/strict";
import { test } from "node:test";
import { buildThemeTokens, parseVeil, planSurfaces, TEXT_TARGET, veilAlpha, VEIL_DEFAULT, VEIL_MAX, VEIL_MIN } from "ui/surface-model";
import { contrastRatio, deriveTheme, paletteById } from "ui/theme-model";
import { evaluateTone, makeToneSource } from "./backdropTone.ts";
import { buildField, hexToRgb, packPalette, renderField, WALLPAPERS, WALLPAPER_DIM, WALLPAPER_KINDS } from "./wallpaper.ts";

/** The real wallpaper pixels at 1280x800 / its own scale, as an RGBA tone source. */
function sourceFor(kind: (typeof WALLPAPER_KINDS)[number]) {
  const style = WALLPAPERS[kind];
  const w = Math.ceil(1280 / style.scale);
  const h = Math.ceil(800 / style.scale);
  const words = new Uint32Array(w * h);
  renderField(buildField(w, h, style.cover, style.haze), w, h, 0, style, packPalette(style.palette), words);
  return makeToneSource(new Uint8ClampedArray(words.buffer), w, h, 1280 / w);
}

for (const kind of WALLPAPER_KINDS) {
  for (const id of ["obsidian", "cobalt", "raven", "clay"]) {
    test(`${kind} wallpaper under ${id}: panel text keeps its contrast on the busiest pixels`, () => {
      const theme = paletteById(id)!;
      const d = deriveTheme(theme);
      const tone = evaluateTone(sourceFor(kind), { bg: d.bg, dim: WALLPAPER_DIM / 100 });
      const opts = { transparency: 100, blur: 28, transparentChat: true, tone };
      const plan = planSurfaces(d, "glass", true, opts);
      for (const layer of plan.layers) {
        const r = contrastRatio(d.text, layer.color);
        assert.ok(r >= layer.textMin, `${layer.name}: body text ${r.toFixed(2)} < ${layer.textMin}`);
      }
      const tokens = buildThemeTokens(theme, d.accent ?? "#3b82f6", "glass", true, opts);
      for (const layer of plan.layers) {
        for (const key of ["--color-text-secondary", "--color-text-muted"]) {
          const r = contrastRatio(tokens[key], layer.color);
          assert.ok(r >= 4.5, `${layer.name}: ${key} ${r.toFixed(2)} < 4.5`);
        }
      }
      assert.ok(TEXT_TARGET >= 7);
    });
  }
}

/* ---- Wallpaper veil: a light full-window scrim, readability from the message plates ---- */

for (const kind of WALLPAPER_KINDS) {
  for (const id of ["obsidian", "cobalt", "raven", "clay"]) {
    test(`${kind} wallpaper under ${id}: veil 0 / default / 100 keeps body text >= 4.5:1 on the worst pixels`, () => {
      const theme = paletteById(id)!;
      const d = deriveTheme(theme);
      const tone = evaluateTone(sourceFor(kind), { bg: d.bg, dim: WALLPAPER_DIM / 100 });
      for (const veil of [VEIL_MIN, VEIL_DEFAULT, VEIL_MAX]) {
        const opts = { transparency: 100, blur: 28, transparentChat: true, tone, veil };
        const plan = planSurfaces(d, "glass", true, opts);
        assert.ok(plan.veil, "veil mode on");
        assert.ok(plan.layers.some((l) => l.name === "message plate"));
        for (const layer of plan.layers) {
          const r = contrastRatio(d.text, layer.color);
          assert.ok(r >= layer.textMin, `veil ${veil} ${layer.name}: body text ${r.toFixed(2)} < ${layer.textMin}`);
          assert.ok(r >= 4.5);
        }
        const tokens = buildThemeTokens(theme, d.accent ?? "#3b82f6", "glass", true, opts);
        for (const layer of plan.layers) {
          for (const key of ["--color-text-secondary", "--color-text-muted"]) {
            const r = contrastRatio(tokens[key], layer.color);
            assert.ok(r >= 4.5, `veil ${veil} ${layer.name}: ${key} ${r.toFixed(2)} < 4.5`);
          }
        }
      }
    });
    test(`${kind} wallpaper under ${id}: the default veil stays light (never heavier than the old column scrim above 0.2)`, () => {
      const theme = paletteById(id)!;
      const d = deriveTheme(theme);
      const tone = evaluateTone(sourceFor(kind), { bg: d.bg, dim: WALLPAPER_DIM / 100 });
      const base = { transparency: 100, blur: 28, transparentChat: true, tone };
      const old = planSurfaces(d, "glass", true, base).scrimAlpha;
      const soft = planSurfaces(d, "glass", true, { ...base, veil: VEIL_DEFAULT }).scrimAlpha;
      assert.ok(soft <= 0.2, `default veil ${soft} should be <= 0.2`);
      assert.ok(old <= 0.2 || soft < old, `veil ${soft} should be lighter than the old scrim ${old}`);
    });
  }
}

test("veil helpers: parse clamps, alpha is monotonic and capped", () => {
  assert.equal(parseVeil("nonsense"), VEIL_DEFAULT);
  assert.equal(parseVeil(500), VEIL_MAX);
  assert.equal(parseVeil(-3), VEIL_MIN);
  assert.equal(veilAlpha(0), 0);
  assert.ok(veilAlpha(100) <= 0.5 && veilAlpha(60) < veilAlpha(61));
});

/* ---- Live clouds worst-case palette coverage (any drifted frame keeps contrast) ---- */

for (const kind of WALLPAPER_KINDS) {
  for (const id of ["obsidian", "cobalt", "raven", "clay"]) {
    test(`${kind} live-cloud palette under ${id}: worst-case solid sky/cloud patches keep >= 4.5:1 / >= 7:1 contrast`, () => {
      const style = WALLPAPERS[kind];
      // Build a synthetic 64x64 source split evenly into all 4 palette stops (worst solid cell of any frame).
      const w = 64;
      const h = 64;
      const rgba = new Uint8ClampedArray(w * h * 4);
      const rgbs = style.palette.map(hexToRgb);
      for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) {
          const idx = ((y >> 4) + (x >> 4)) % rgbs.length;
          const [r, g, b] = rgbs[idx];
          const p = (y * w + x) * 4;
          rgba[p] = r;
          rgba[p + 1] = g;
          rgba[p + 2] = b;
          rgba[p + 3] = 255;
        }
      }
      const source = makeToneSource(rgba, w, h, 20);
      const theme = paletteById(id)!;
      const d = deriveTheme(theme);
      const tone = evaluateTone(source, { bg: d.bg, dim: WALLPAPER_DIM / 100 });
      for (const veil of [VEIL_MIN, VEIL_DEFAULT, VEIL_MAX]) {
        const plan = planSurfaces(d, "glass", true, { transparency: 100, blur: 28, transparentChat: true, tone, veil });
        for (const layer of plan.layers) {
          assert.ok(contrastRatio(d.text, layer.color) >= layer.textMin);
        }
      }
    });
  }
}
