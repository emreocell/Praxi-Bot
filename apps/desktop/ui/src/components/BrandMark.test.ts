import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { brandIcon } from "../lib/brandIcons.ts";

const source = readFileSync(new URL("./BrandMark.tsx", import.meta.url), "utf8");

/** Mirrors BrandMark tone selection (exported only for this contract test). */
function brandMarkTone(name: string, monochrome: boolean): string {
  const brand = brandIcon(name);
  const brightness = (hex: string): number => {
    const n = parseInt(hex.slice(1), 16);
    if (Number.isNaN(n)) return 128;
    return 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255);
  };
  return monochrome || brand.monochrome || !brand.known
    ? "mono"
    : brightness(brand.color) > 200
      ? "light"
      : brightness(brand.color) < 70
        ? "dark"
        : "mid";
}

test("monochrome prop forces data-tone mono even for coloured preview marks", () => {
  assert.match(source, /const tone = monochrome \|\| brand\.monochrome \|\| !brand\.known/);
  for (const name of ["discord", "gmail", "github"]) {
    assert.equal(brandMarkTone(name, false), name === "github" ? "dark" : "mid", `${name} without monochrome`);
    assert.equal(brandMarkTone(name, true), "mono", `${name} with monochrome`);
  }
});

test("slack multicolour SVG is skipped when monochrome is requested", () => {
  assert.match(source, /if \(key === "slack" && !monochrome\)/);
});