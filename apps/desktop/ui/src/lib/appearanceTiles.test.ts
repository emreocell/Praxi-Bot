import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const css = readFileSync(new URL("../components/AppearancePicker.module.css", import.meta.url), "utf8");

/** Every top-level rule for `selector` (exact selector list match), in source order. */
function rules(selector: string): string[] {
  const out: string[] = [];
  const re = /(^|\n)([^{}\n]+)\{([^}]*)\}/g;
  for (let m = re.exec(css); m; m = re.exec(css)) {
    if ((m[2] ?? "").split(",").map((s) => s.trim()).includes(selector)) out.push(m[3] ?? "");
  }
  return out;
}

test("theme, background and wallpaper tiles: the picture fills the whole tile (no inner padding)", () => {
  const preview = rules(".preview");
  // One owner for the tile frame: a second `.preview` rule (the accessibility text sample) used to add
  // padding to every tile, so the wallpaper and background pictures no longer reached the tile's edges.
  assert.equal(preview.length, 1, "exactly one .preview rule");
  assert.match(preview[0] ?? "", /padding:\s*var\(--space-0\)/);
  assert.match(preview[0] ?? "", /overflow:\s*hidden/);
  assert.match(css, /\.preview canvas \{[^}]*width:\s*100%;[^}]*height:\s*100%/);
  assert.match(css, /\.cover \{[^}]*object-fit:\s*cover/);
});

test("the accessibility text sample has its own class", () => {
  assert.equal(rules(".textSample").length, 1);
  const cards = readFileSync(new URL("../views/settings/AppearanceCards.tsx", import.meta.url), "utf8");
  assert.match(cards, /className=\{pickerStyles\.textSample\} data-testid="a11y-preview"/);
});
