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

test("settings saved before the surface system load as solid, open (v6) chat area, OS preference respected", () => {
  // A 198-A settings object: style and backdrop, none of the 244-E fields.
  const old = normalizeAppearance({ themeMode: "obsidian", uiStyle: "solid", backdrop: "aurora", backdropDim: 40 });
  assert.equal(old.uiStyle, "solid");
  assert.equal(old.transparentChat, true);
  // an old explicit transparentChat:false was never a choice for the v6 look: still open
  assert.equal(normalizeAppearance({ transparentChat: false }).transparentChat, true);
  // only the explicit panel flag brings the center card back
  assert.equal(normalizeAppearance({ chatPanel: true }).transparentChat, false);
  assert.equal(old.transparencyOverride, false);
  assert.equal(old.panelTransparency, DEFAULT_APPEARANCE.panelTransparency);
  assert.equal(old.panelBlur, DEFAULT_APPEARANCE.panelBlur);
  // A user who picked glass before keeps glass.
  assert.equal(normalizeAppearance({ uiStyle: "glass" }).uiStyle, "glass");
  assert.equal(normalizeAppearance({ uiStyle: "tinted" }).uiStyle, "tinted");
  // Only a real boolean overrides the OS transparency preference.
  assert.equal(normalizeAppearance({ transparencyOverride: "true" }).transparencyOverride, false);
});

test("review query: surface parameters and a flat colour backdrop", () => {
  const params = new URLSearchParams("style=glass&alpha=90&blur=40&clear=1&bgcolor=4a7bd0&rt=ignore");
  const q = appearanceFromQuery(params, DEFAULT_APPEARANCE);
  assert.equal(q.uiStyle, "glass");
  assert.equal(q.panelTransparency, 90);
  assert.equal(q.panelBlur, 40);
  assert.equal(q.transparentChat, true);
  assert.equal(q.transparencyOverride, true);
  assert.equal(q.backdrop, "custom");
  assert.match(q.backgroundUrl ?? "", /^data:image\/svg\+xml,.*4a7bd0/);
  assert.equal(appearanceFromQuery(new URLSearchParams("bgcolor=red"), DEFAULT_APPEARANCE).backgroundUrl, undefined);
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
    wallpaper: "off",
    wallpaperMotion: "live",
    wallpaperVeil: 35,
    backdropDim: 33.4,
    panelTransparency: 80,
    panelBlur: 40,
    transparentChat: true,
    chatPanel: false,
    transparencyOverride: true,
    photorealAvatars: false
  };
  assert.deepEqual(normalizeAppearance(stored), {
    ...stored,
    customTheme: { bg: "#abcdef", text: "#101010" },
    backdropDim: 33
  });
});

test("photoreal clay avatars default on; only an explicit false turns them off", () => {
  assert.equal(DEFAULT_APPEARANCE.photorealAvatars, true);
  assert.equal(normalizeAppearance({}).photorealAvatars, true);
  assert.equal(normalizeAppearance({ photorealAvatars: "no" }).photorealAvatars, true);
  assert.equal(normalizeAppearance({ photorealAvatars: false }).photorealAvatars, false);
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
  const b = appearanceFromQuery(q("theme=moss&accent=%23e11d48&style=bogus&wallpaper=night&motion=live"), DEFAULT_APPEARANCE);
  assert.equal(b.accentColor, "#e11d48");
  assert.equal(b.uiStyle, "solid");
  assert.equal(b.wallpaper, "night");
  assert.equal(b.wallpaperMotion, "live");
  assert.equal(appearanceFromQuery(q("accent=bot"), { ...DEFAULT_APPEARANCE, accentColor: "theme" }).accentColor, "bot");
});
