import assert from "node:assert/strict";
import { test } from "node:test";
import { buildThemeTokens, planSurfaces, TEXT_TARGET } from "ui/surface-model";
import { contrastRatio, deriveTheme, paletteById } from "ui/theme-model";
import { evaluateTone, makeToneSource } from "./backdropTone.ts";
import { buildField, packPalette, renderField, WALLPAPERS, WALLPAPER_DIM, WALLPAPER_KINDS } from "./wallpaper.ts";

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
