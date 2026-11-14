import assert from "node:assert/strict";
import { test } from "node:test";
import {
  appearanceFromQuery,
  DEFAULT_APPEARANCE,
  normalizeAppearance,
  parseBubble,
  parseCorners,
  parseOwnBubble,
  parseUiStyle,
  themePickPatch
} from "./appearance.ts";
import {
  BACKDROP_SCENES,
  FAVORITE_BACKDROPS,
  dimToSlider,
  parseBackdrop,
  parseBackdropDim,
  sliderToDim
} from "./backdropCatalog.ts";

test("empty or garbage storage yields the defaults", () => {
  assert.deepEqual(normalizeAppearance({}), DEFAULT_APPEARANCE);
  assert.deepEqual(
    normalizeAppearance({
      themeMode: 12,
      customTheme: "x",
      accentColor: "chartreuse",
      uiStyle: "neon",
      corners: null,
      bubble: [],
      ownBubble: {},
      backdrop: "../../etc/passwd",
      backdropDim: "lots"
    }),
    DEFAULT_APPEARANCE
  );
});

test("old saved preferences keep working", () => {
  for (const old of ["obsidian", "cobalt", "raven", "clay", "light", "dark", "system"]) {
    assert.equal(normalizeAppearance({ themeMode: old }).themeMode, old);
  }
  // A pre-v6 settings object (no new fields) is upgraded in place.
  const upgraded = normalizeAppearance({ themeMode: "raven", accentColor: "#2563EB", backgroundUrl: "x" });
  assert.equal(upgraded.themeMode, "raven");
  assert.equal(upgraded.accentColor, "#2563eb");
  assert.equal(upgraded.uiStyle, "solid");
  assert.equal(upgraded.backdrop, "none");
});

test("valid new values round-trip", () => {
  const stored = {
    themeMode: "custom",
    customTheme: { bg: "#ABCDEF", text: "#101010" },
    accentColor: "theme",
    uiStyle: "glass",
    corners: "round",
    bubble: "plain",
    ownBubble: "accent",
    backdrop: "reed2",
    backdropDim: 33.4
  };
  assert.deepEqual(normalizeAppearance(stored), {
    ...stored,
    customTheme: { bg: "#abcdef", text: "#101010" },
    backdropDim: 33
  });
});

test("axis parsers fall back to the prototype defaults", () => {
  assert.equal(parseUiStyle("line"), "line");
  assert.equal(parseUiStyle("x"), "solid");
  assert.equal(parseCorners("sharp"), "sharp");
  assert.equal(parseCorners(undefined), "soft");
  assert.equal(parseBubble("glass"), "glass");
  assert.equal(parseBubble("round"), "soft");
  assert.equal(parseOwnBubble("neutral"), "neutral");
  assert.equal(parseOwnBubble("pink"), "tint");
});

test("backdrop ids are validated against the catalog; custom and none are allowed", () => {
  assert.equal(parseBackdrop("none"), "none");
  assert.equal(parseBackdrop("custom"), "custom");
  assert.equal(parseBackdrop("silk"), "silk");
  assert.equal(parseBackdrop("nope"), "none");
  assert.equal(parseBackdrop(undefined), "none");
  assert.equal(BACKDROP_SCENES.length, 19);
  for (const id of FAVORITE_BACKDROPS) assert.equal(parseBackdrop(id), id);
});

test("backdrop dim is clamped and the slider is its mirror image", () => {
  assert.equal(parseBackdropDim(5), 20);
  assert.equal(parseBackdropDim(500), 88);
  assert.equal(parseBackdropDim("45"), 45);
  assert.equal(parseBackdropDim(NaN), 60);
  assert.equal(dimToSlider(60), 48);
  assert.equal(sliderToDim(dimToSlider(37)), 37);
  assert.equal(dimToSlider(20), 88);
});

test("picking a theme keeps a chosen colour and explicit bot; legacy theme choice becomes implicit", () => {
  const base = { accentColor: "", customTheme: DEFAULT_APPEARANCE.customTheme };
  assert.deepEqual(themePickPatch(base, "copper"), { themeMode: "copper", accentColor: "" });
  assert.deepEqual(themePickPatch({ ...base, accentColor: "theme" }, "obsidian"), { themeMode: "obsidian", accentColor: "" });
  assert.deepEqual(themePickPatch({ ...base, accentColor: "theme" }, "copper"), { themeMode: "copper", accentColor: "theme" });
  assert.deepEqual(themePickPatch({ ...base, accentColor: "#e11d48" }, "copper"), { themeMode: "copper", accentColor: "#e11d48" });
  assert.deepEqual(themePickPatch({ ...base, accentColor: "bot" }, "copper"), { themeMode: "copper", accentColor: "bot" });
  assert.deepEqual(themePickPatch(base, "bogus"), { themeMode: "obsidian", accentColor: "" });
  assert.deepEqual(themePickPatch(base, "light"), { themeMode: "light", accentColor: "" });
});

test("URL overrides (mock review screenshots) are validated and optional", () => {
  const q = (s: string) => new URLSearchParams(s);
  assert.deepEqual(appearanceFromQuery(q(""), DEFAULT_APPEARANCE), DEFAULT_APPEARANCE);
  const a = appearanceFromQuery(q("theme=copper&style=glass&corners=round&backdrop=silk&dim=30"), DEFAULT_APPEARANCE);
  assert.equal(a.themeMode, "copper");
  assert.equal(a.accentColor, "");
  assert.equal(a.uiStyle, "glass");
  assert.equal(a.corners, "round");
  assert.equal(a.backdrop, "silk");
  assert.equal(a.backdropDim, 30);
  const b = appearanceFromQuery(q("theme=moss&accent=%23e11d48&style=bogus"), DEFAULT_APPEARANCE);
  assert.equal(b.accentColor, "#e11d48");
  assert.equal(b.uiStyle, "solid");
  assert.equal(appearanceFromQuery(q("accent=bot"), { ...DEFAULT_APPEARANCE, accentColor: "theme" }).accentColor, "bot");
});
