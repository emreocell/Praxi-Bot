import assert from "node:assert/strict";
import { test } from "node:test";
import {
  accentAfterThemeChange,
  accentKind,
  buildTokens,
  contrastRatio,
  customThemeContrast,
  deriveTheme,
  FAVORITE_THEMES,
  inkOn,
  mix,
  normalizeHex,
  PALETTES,
  paletteById,
  parseAccentPreference,
  parseCustomTheme,
  parseThemeMode,
  pushContrast,
  resolveAccent,
  themeFor,
  DEFAULT_CUSTOM_THEME
} from "ui/theme-model";

test("mix blends linearly and weights the second colour", () => {
  assert.equal(mix("#000000", "#ffffff", 0), "#000000");
  assert.equal(mix("#000000", "#ffffff", 1), "#ffffff");
  assert.equal(mix("#000000", "#ffffff", 0.5), "#808080");
  assert.equal(mix("#ff0000", "#0000ff", 0.25), "#bf0040");
});

test("normalizeHex accepts #rgb / #rrggbb and rejects the rest", () => {
  assert.equal(normalizeHex("#ABC"), "#aabbcc");
  assert.equal(normalizeHex(" #A1b2C3 "), "#a1b2c3");
  assert.equal(normalizeHex("red"), null);
  assert.equal(normalizeHex("#12345"), null);
  assert.equal(normalizeHex(42), null);
});

test("contrastRatio matches the WCAG reference values", () => {
  assert.equal(contrastRatio("#000000", "#ffffff").toFixed(2), "21.00");
  assert.equal(contrastRatio("#777777", "#777777").toFixed(2), "1.00");
  assert.equal(contrastRatio("#767676", "#ffffff").toFixed(2), "4.54");
});

test("inkOn and pushContrast always reach 4.5", () => {
  for (const c of ["#8b5cf6", "#808080", "#ef233c", "#ffffff", "#000000", "#3b82f6"]) {
    assert.ok(contrastRatio(inkOn(c), c) >= 4.5, c);
  }
  const pushed = pushContrast("#777777", "#000000", ["#ffffff"], 4.5);
  assert.ok(contrastRatio(pushed, "#ffffff") >= 4.5);
  assert.equal(pushContrast("#000000", "#111111", ["#ffffff"], 4.5), "#000000");
});

test("derivation follows the prototype: dark vs light surfaces", () => {
  const dark = deriveTheme({ id: "x", bg: "#111217", text: "#ecedf1" });
  assert.equal(dark.light, false);
  assert.equal(dark.surface, mix("#111217", "#ecedf1", 0.035));
  assert.equal(dark.raised, mix("#111217", "#ecedf1", 0.085));
  assert.equal(dark.hover, mix("#111217", "#ecedf1", 0.13));
  const light = deriveTheme({ id: "y", bg: "#f4f1ed", text: "#1a1a1a" });
  assert.equal(light.light, true);
  assert.equal(light.raised, mix("#f4f1ed", "#ffffff", 0.7));
  assert.equal(light.surface, mix("#f4f1ed", "#1a1a1a", 0.05));
  assert.equal(light.bg2, mix("#f4f1ed", "#1a1a1a", 0.12));
  const accented = deriveTheme({ id: "z", bg: "#0e1828", text: "#dbe6f6", accent: "#adc7e9" });
  assert.equal(accented.bg2, mix("#0e1828", "#adc7e9", 0.1));
});

test("faded text stays readable on every derived surface", () => {
  for (const p of PALETTES) {
    const d = deriveTheme(p);
    for (const s of [d.bg, d.surface, d.raised, d.hover]) {
      assert.ok(contrastRatio(d.muted, s) >= 4.5, `${p.id} muted`);
      assert.ok(contrastRatio(d.secondary, s) >= 4.5, `${p.id} secondary`);
    }
  }
});

test("ships the 29 prototype themes with the five favourites", () => {
  assert.equal(PALETTES.length, 29);
  assert.deepEqual(FAVORITE_THEMES, ["obsidian", "cobalt", "clay", "porcelain", "moss"]);
  assert.equal(PALETTES.filter((p) => p.accent).length, 15);
  // cobalt and raven keep the accents they had before v6.
  assert.equal(paletteById("cobalt")!.accent, "#9DBBFF");
  assert.equal(paletteById("raven")!.accent, "#E4B683");
  for (const id of FAVORITE_THEMES) assert.ok(paletteById(id));
});

test("buildTokens covers the existing token names and applies the style axis", () => {
  const theme = paletteById("obsidian")!;
  const solid = buildTokens(theme, "#3b82f6", "solid");
  for (const k of [
    "--color-background", "--color-surface", "--color-surface-elevated", "--color-surface-active",
    "--color-text", "--color-text-secondary", "--color-text-muted", "--color-accent", "--color-accent-contrast",
    "--color-accent-text", "--color-border", "--color-border-strong", "--color-focus", "--color-success",
    "--color-warning", "--color-danger", "--shadow-card", "--shadow-overlay", "--color-code-bg"
  ]) assert.ok(solid[k], k);
  assert.equal(solid["--color-accent"], "#3b82f6");
  assert.equal(solid["--color-surface"], deriveTheme(theme).surface);
  const glass = buildTokens(theme, "#3b82f6", "glass");
  assert.match(glass["--color-surface"], /transparent/);
  assert.equal(glass["--color-surface-opaque"], solid["--color-surface"]);
  const line = buildTokens(theme, "#3b82f6", "line", false);
  assert.equal(line["--color-surface"], deriveTheme(theme).bg);
  assert.match(buildTokens(theme, "#3b82f6", "line", true)["--color-surface"], /transparent/);
});

test("parseThemeMode keeps old names and falls back for unknown values", () => {
  for (const old of ["obsidian", "cobalt", "raven", "clay", "light", "dark", "system", "custom"]) {
    assert.equal(parseThemeMode(old), old);
  }
  assert.equal(parseThemeMode("moss"), "moss");
  assert.equal(parseThemeMode("sunburst"), "obsidian");
  assert.equal(parseThemeMode(undefined), "obsidian");
  assert.equal(parseThemeMode(7), "obsidian");
  assert.equal(parseThemeMode("__proto__"), "obsidian");
});

test("themeFor resolves legacy names, system and custom", () => {
  assert.equal(themeFor("light", DEFAULT_CUSTOM_THEME, true).id, "clay");
  assert.equal(themeFor("dark", DEFAULT_CUSTOM_THEME, false).id, "obsidian");
  assert.equal(themeFor("system", DEFAULT_CUSTOM_THEME, true).id, "obsidian");
  assert.equal(themeFor("system", DEFAULT_CUSTOM_THEME, false).id, "clay");
  assert.equal(themeFor("raven", DEFAULT_CUSTOM_THEME, false).id, "raven");
  const custom = themeFor("custom", { bg: "#223344", text: "#eeeeee" }, false);
  assert.deepEqual(custom, { id: "custom", bg: "#223344", text: "#eeeeee" });
});

test("custom themes are validated and warn below 4.5", () => {
  assert.deepEqual(parseCustomTheme({ bg: "#000", text: "nope" }), { bg: "#000000", text: DEFAULT_CUSTOM_THEME.text });
  assert.deepEqual(parseCustomTheme(null), DEFAULT_CUSTOM_THEME);
  assert.equal(customThemeContrast({ bg: "#101820", text: "#f2e9dc" }).ok, true);
  const bad = customThemeContrast({ bg: "#777777", text: "#888888" });
  assert.equal(bad.ok, false);
  assert.ok(bad.ratio < 1.5);
});

test("accent preference validation", () => {
  assert.equal(parseAccentPreference(""), "");
  assert.equal(parseAccentPreference("theme"), "theme");
  assert.equal(parseAccentPreference("bot"), "bot");
  assert.equal(parseAccentPreference("#ABCDEF"), "#abcdef");
  assert.equal(parseAccentPreference("blue"), "");
  assert.equal(parseAccentPreference(null), "");
});

test("accent resolution: implicit default follows the theme when it has an accent, else the bot", () => {
  const themed = { accent: "#cf8546" };
  const plain = {};
  // implicit (stored "", also every pre-v6 file): theme colour if there is one
  assert.equal(resolveAccent("", themed, "#3b82f6", "#111111"), "#cf8546");
  assert.equal(resolveAccent("", plain, "#3b82f6", "#111111"), "#3b82f6");
  assert.equal(resolveAccent("", plain, undefined, "#111111"), "#111111");
  assert.equal(resolveAccent("", themed, undefined, "#111111"), "#cf8546");
  // explicit bot choice wins over the theme's accent
  assert.equal(resolveAccent("bot", themed, "#3b82f6", "#111111"), "#3b82f6");
  assert.equal(resolveAccent("bot", themed, undefined, "#111111"), "#cf8546");
  assert.equal(resolveAccent("theme", themed, "#3b82f6", "#111111"), "#cf8546");
  assert.equal(resolveAccent("theme", plain, "#3b82f6", "#111111"), "#3b82f6");
  assert.equal(resolveAccent("#e11d48", themed, "#3b82f6", "#111111"), "#e11d48");
  assert.equal(resolveAccent("#BAD", plain, "#3b82f6", "#111111"), "#bbaadd");
});

test("accentKind says which swatch is selected", () => {
  assert.equal(accentKind("", { accent: "#cf8546" }), "theme");
  assert.equal(accentKind("", {}), "bot");
  assert.equal(accentKind("bot", { accent: "#cf8546" }), "bot");
  assert.equal(accentKind("#e11d48", {}), "fixed");
});

test("theme changes only move implicit accent values; a chosen colour or explicit bot stays", () => {
  assert.equal(accentAfterThemeChange("", { accent: "#cf8546" }), "");
  assert.equal(accentAfterThemeChange("bot", { accent: "#cf8546" }), "bot");
  assert.equal(accentAfterThemeChange("#e11d48", { accent: "#cf8546" }), "#e11d48");
  assert.equal(accentAfterThemeChange("#e11d48", {}), "#e11d48");
  assert.equal(accentAfterThemeChange("theme", { accent: "#cf8546" }), "theme");
  assert.equal(accentAfterThemeChange("theme", {}), "");
  assert.equal(accentAfterThemeChange("", {}), "");
});

test("degenerate colours never throw and always give valid hex", () => {
  const cases: Array<[string, string]> = [
    ["#808080", "#808080"], ["#000000", "#000000"], ["#ffffff", "#ffffff"], ["#808080", "#ffffff"], ["#777", "#888"]
  ];
  const hex = /^#[0-9a-f]{6}$/;
  for (const [bg, text] of cases) {
    const d = deriveTheme({ id: "x", bg, text });
    for (const k of ["bg", "bg2", "surface", "raised", "hover", "text", "secondary", "muted", "faint", "lineStrong", "focus", "success", "warning", "danger", "dangerInk"] as const) {
      assert.match(d[k], hex, `${bg}/${text} ${k}`);
    }
    const tokens = buildTokens({ id: "x", bg, text }, "#808080");
    assert.ok(tokens["--color-accent"]);
    assert.equal(customThemeContrast({ bg, text }).ok, contrastRatio(text, bg) >= 4.5 && customThemeContrast({ bg, text }).ratio >= 4.5);
  }
  assert.equal(customThemeContrast({ bg: "#808080", text: "#808080" }).ok, false);
  assert.equal(customThemeContrast({ bg: "#808080", text: "#ffffff" }).ok, false);
});

test("muted text is visibly different from text on every shipped theme", () => {
  for (const p of PALETTES) {
    const d = deriveTheme(p);
    assert.notEqual(d.muted, d.text, p.id);
    assert.notEqual(d.secondary, d.muted, p.id);
  }
});
