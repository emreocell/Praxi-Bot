/**
 * Derived theme model (card 198-A).
 *
 * A theme is only `{ id, bg, text, accent? }`.  Surfaces, hover, muted text,
 * lines and shadows are *derived* from those colours; `buildTokens` maps the
 * result onto the CSS custom properties the rest of the UI already consumes.
 *
 * Pure functions only (no DOM, no JSON imports) so `node --test` can run them.
 */

export interface ThemeDef {
  id: string;
  /** Page background. */
  bg: string;
  /** Primary text. */
  text: string;
  /** The theme's own accent (60/30/10 palettes); optional. */
  accent?: string;
}

export type UiStyle = "solid" | "glass" | "line";
export type Corners = "sharp" | "soft" | "round";
export type BubbleStyle = "soft" | "glass" | "line" | "plain";
export type OwnBubble = "tint" | "accent" | "neutral";

export const UI_STYLES: readonly UiStyle[] = ["solid", "glass", "line"];
export const CORNERS: readonly Corners[] = ["sharp", "soft", "round"];
export const BUBBLE_STYLES: readonly BubbleStyle[] = ["soft", "glass", "line", "plain"];
export const OWN_BUBBLES: readonly OwnBubble[] = ["tint", "accent", "neutral"];

/* ---------- colour maths ---------- */

const HEX6 = /^#[0-9a-f]{6}$/i;
const HEX3 = /^#[0-9a-f]{3}$/i;

/** `#abc` / `#aabbcc` (any case) -> `#aabbcc`; anything else -> null. */
export function normalizeHex(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const v = value.trim();
  if (HEX6.test(v)) return v.toLowerCase();
  if (HEX3.test(v)) {
    return `#${[...v.slice(1)].map((c) => c + c).join("")}`.toLowerCase();
  }
  return null;
}

function channels(hex: string): [number, number, number] {
  const h = normalizeHex(hex) ?? "#000000";
  return [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16)) as [number, number, number];
}

/** Linear blend: `t` is the weight of `b` (0 -> a, 1 -> b). */
export function mix(a: string, b: string, t: number): string {
  const ca = channels(a);
  const cb = channels(b);
  return `#${ca
    .map((v, i) => Math.round(v * (1 - t) + cb[i] * t).toString(16).padStart(2, "0"))
    .join("")}`;
}

/** WCAG relative luminance. */
export function luminance(hex: string): number {
  const [r, g, b] = channels(hex).map((v) => {
    const c = v / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return r * 0.2126 + g * 0.7152 + b * 0.0722;
}

/** WCAG contrast ratio (1..21). */
export function contrastRatio(foreground: string, background: string): number {
  const a = luminance(foreground);
  const b = luminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

export function isLightColor(bg: string): boolean {
  return luminance(bg) > 0.4;
}

/**
 * Pull `fg` toward `toward` in small steps until it reaches `min` contrast
 * against every colour in `against` (used for status colours, accent text...).
 */
export function pushContrast(
  fg: string,
  toward: string,
  against: readonly string[],
  min: number
): string {
  const ok = (c: string) => against.every((bg) => contrastRatio(c, bg) >= min);
  if (ok(fg)) return fg;
  for (let t = 0.05; t <= 1.0001; t += 0.05) {
    const c = mix(fg, toward, t);
    if (ok(c)) return c;
  }
  return toward;
}

/**
 * `mix(text, bg, t)` with `t` reduced until it still reaches `min` contrast on
 * every colour in `against` (a faded text colour must stay readable).
 */
function fade(text: string, bg: string, t: number, against: readonly string[], min: number): string {
  for (let w = t; w > 0.0001; w -= 0.02) {
    const c = mix(text, bg, w);
    if (against.every((a) => contrastRatio(c, a) >= min)) return c;
  }
  return text;
}

/** Best ink on top of `color`: soft near-black / white when they reach 4.5, else pure black / white. */
export function inkOn(color: string): string {
  const soft = ["#111111", "#ffffff"];
  const ok = soft.filter((c) => contrastRatio(color, c) >= 4.5);
  if (ok.length) return ok.sort((a, b) => contrastRatio(color, b) - contrastRatio(color, a))[0];
  return contrastRatio(color, "#000000") >= contrastRatio(color, "#ffffff") ? "#000000" : "#ffffff";
}

/* ---------- the 29 shipped themes ---------- */

/** `[id, bg, text, accent?]` — names live in i18n (`theme.names.<id>`). */
const RAW: ReadonlyArray<readonly [string, string, string, string?]> = [
  ["obsidian", "#111217", "#ecedf1"],
  ["cobalt", "#010102", "#f3f3f5"],
  ["clay", "#f4f1ed", "#1a1a1a"],
  ["porcelain", "#f6f7f9", "#15171c"],
  ["moss", "#0b100d", "#e7efe5"],
  ["raven", "#14110d", "#fdefd6"],
  ["navy", "#0a0e1a", "#e6ebf7"],
  ["wine", "#150b0f", "#f6e6ea"],
  ["graphite", "#1b1b1b", "#ededed"],
  ["plum", "#120d1a", "#ece4f7"],
  ["terminal", "#060a06", "#b9f5c4"],
  ["sand", "#efe6d6", "#2a2118"],
  ["lavender", "#f2effa", "#1d1830"],
  ["mint", "#edf5f0", "#12211a"],
  ["rose", "#faeff0", "#2b1418"],
  ["sky", "#eef4fb", "#101c2c"],
  ["midnight", "#0e1828", "#dbe6f6", "#adc7e9"],
  ["champagne", "#0c0b0a", "#f1e9d8", "#cdb27a"],
  ["copper", "#0e0b09", "#f3e7da", "#cf8546"],
  ["storm", "#1c1d20", "#e4e6ea", "#9fb0c9"],
  ["neon", "#07060b", "#ecebf5", "#b9a2f7"],
  ["race", "#0b0c0f", "#e9edf2", "#ef233c"],
  ["forest", "#0d1712", "#e9efe6", "#7fc8a0"],
  ["blush", "#f7e1e6", "#2a2a2f", "#c2536f"],
  ["oat", "#efe9dc", "#1d4a35", "#1d4a35"],
  ["seafoam", "#d2e5de", "#22302c", "#2f6b5c"],
  ["khaki", "#dbd1b6", "#3a3c26", "#5b5e3c"],
  ["frost", "#f1f3f5", "#3b3f46", "#4a6fa5"],
  ["chalk", "#f4f4f2", "#121214", "#121214"]
];

export const PALETTES: readonly ThemeDef[] = RAW.map(([id, bg, text, accent]) =>
  accent ? { id, bg, text, accent } : { id, bg, text }
);

export const PALETTE_IDS: readonly string[] = PALETTES.map((p) => p.id);

/** The five themes shown on the Appearance page. */
export const FAVORITE_THEMES: readonly string[] = ["obsidian", "cobalt", "clay", "porcelain", "moss"];

export const CUSTOM_THEME_ID = "custom";

export interface CustomTheme {
  bg: string;
  text: string;
}

export const DEFAULT_CUSTOM_THEME: CustomTheme = { bg: "#101820", text: "#f2e9dc" };

/** Names kept valid in saved preferences. */
export const LEGACY_THEME_ALIASES: Readonly<Record<string, string>> = {
  light: "clay",
  dark: "obsidian"
};

export const DEFAULT_THEME_ID = "obsidian";

export function paletteById(id: string): ThemeDef | undefined {
  return PALETTES.find((p) => p.id === id);
}

/** Validate a stored custom theme; falls back to the default for bad parts. */
export function parseCustomTheme(value: unknown): CustomTheme {
  const v = (value && typeof value === "object" ? value : {}) as Record<string, unknown>;
  return {
    bg: normalizeHex(v.bg) ?? DEFAULT_CUSTOM_THEME.bg,
    text: normalizeHex(v.text) ?? DEFAULT_CUSTOM_THEME.text
  };
}

/**
 * Narrow a stored value to a valid theme mode.  Old names (`light`, `dark`,
 * `obsidian`, `cobalt`, `raven`, `clay`, `system`) keep working; anything
 * unknown becomes the default.  `light` / `dark` are kept as-is (they are
 * resolved by `themeFor`) so a round trip does not rewrite the user's value.
 */
export function parseThemeMode(value: unknown): string {
  if (typeof value !== "string") return DEFAULT_THEME_ID;
  if (value === "system" || value === CUSTOM_THEME_ID) return value;
  if (Object.hasOwn(LEGACY_THEME_ALIASES, value)) return value;
  return paletteById(value) ? value : DEFAULT_THEME_ID;
}

/** Concrete theme for a saved mode. `prefersDark` only matters for `system`. */
export function themeFor(mode: string, custom: CustomTheme, prefersDark: boolean): ThemeDef {
  const m = parseThemeMode(mode);
  if (m === CUSTOM_THEME_ID) return { id: CUSTOM_THEME_ID, ...custom };
  if (m === "system") return paletteById(prefersDark ? "obsidian" : "clay") as ThemeDef;
  const id = LEGACY_THEME_ALIASES[m] ?? m;
  return (paletteById(id) ?? paletteById(DEFAULT_THEME_ID)) as ThemeDef;
}

/* ---------- derivation (prototype `tok()`) ---------- */

export interface DerivedTheme {
  light: boolean;
  bg: string;
  /** Second gradient stop for the page background. */
  bg2: string;
  surface: string;
  raised: string;
  hover: string;
  text: string;
  /** Between text and muted. */
  secondary: string;
  muted: string;
  faint: string;
  /** Hairline borders (opaque equivalents on bg). */
  lineStrong: string;
  focus: string;
  success: string;
  warning: string;
  danger: string;
  dangerInk: string;
  accent?: string;
}

const DARK_STATUS = { success: "#92CEAC", warning: "#E8BE7C", danger: "#F4A3A3" };
const LIGHT_STATUS = { success: "#25633F", warning: "#80500C", danger: "#A52D2D" };

export function deriveTheme(theme: ThemeDef): DerivedTheme {
  const bg = normalizeHex(theme.bg) ?? "#111217";
  const text = normalizeHex(theme.text) ?? "#ecedf1";
  const accent = normalizeHex(theme.accent) ?? undefined;
  const light = isLightColor(bg);
  const surface = mix(bg, text, light ? 0.05 : 0.035);
  const raised = light ? mix(bg, "#ffffff", 0.7) : mix(bg, text, 0.085);
  const hover = mix(bg, text, light ? 0.1 : 0.13);
  const surfaces = [bg, surface, raised, hover];
  const muted = fade(text, bg, 0.38, surfaces, 4.5);
  const secondary = fade(text, bg, 0.18, surfaces, 4.5);
  const lineStrong = fade(text, bg, 0.5, [bg, surface, raised], 3);
  const base = light ? LIGHT_STATUS : DARK_STATUS;
  // A status colour is also the text of its own 10 % badge, so test that too.
  const status = (c: string) => {
    const good = (v: string) =>
      surfaces.every((s) => contrastRatio(v, s) >= 4.5) && contrastRatio(v, mix(surface, v, 0.1)) >= 4.5;
    for (let t = 0; t <= 1.0001; t += 0.05) {
      const v = mix(c, text, t);
      if (good(v)) return v;
    }
    return text;
  };
  const danger = status(base.danger);
  return {
    light,
    bg,
    bg2: mix(bg, accent ?? text, light ? 0.12 : 0.1),
    surface,
    raised,
    hover,
    text,
    secondary,
    muted,
    faint: mix(text, bg, 0.62),
    lineStrong,
    focus: lineStrong,
    success: status(base.success),
    warning: status(base.warning),
    danger,
    dangerInk: inkOn(danger),
    accent
  };
}

/** Accent used when neither the bot nor the theme provides one. */
export function fallbackAccent(d: Pick<DerivedTheme, "light">): string {
  return d.light ? "#C86B4A" : "#B6B8FA";
}

export type AccentPreference = string; // "" (bot colour) | "theme" | "#rrggbb"

/** Validate a stored accent preference. */
export function parseAccentPreference(value: unknown): AccentPreference {
  if (value === "theme") return "theme";
  return normalizeHex(value) ?? "";
}

/**
 * Resolve the colour to use as accent.
 * - `""`      the bot's colour (falls back to the theme's, then a default)
 * - `"theme"` the theme's own accent (falls back to the bot's)
 * - `#rrggbb` that fixed colour
 */
export function resolveAccent(
  pref: AccentPreference,
  theme: Pick<ThemeDef, "accent">,
  botColor: string | undefined,
  fallback: string
): string {
  const fixed = normalizeHex(pref);
  if (fixed) return fixed;
  const bot = normalizeHex(botColor);
  const own = normalizeHex(theme.accent);
  if (pref === "theme") return own ?? bot ?? fallback;
  return bot ?? own ?? fallback;
}

/**
 * Accent preference after the user picks a theme: a theme that defines its own
 * accent selects "theme"; leaving such a theme drops back to the bot colour.
 */
export function accentAfterThemeChange(current: AccentPreference, next: Pick<ThemeDef, "accent">): AccentPreference {
  if (next.accent) return "theme";
  return current === "theme" ? "" : current;
}

/** Warning text for a custom theme, or null when the text is readable. */
export function customThemeContrast(custom: CustomTheme): { ratio: number; ok: boolean } {
  const d = deriveTheme({ id: CUSTOM_THEME_ID, ...custom });
  const ratio = Math.min(
    contrastRatio(d.text, d.bg),
    contrastRatio(d.text, d.surface),
    contrastRatio(d.text, d.raised)
  );
  return { ratio, ok: ratio >= 4.5 };
}

/* ---------- CSS custom properties ---------- */

const alpha = (color: string, pct: number) => `color-mix(in srgb, ${color} ${pct}%, transparent)`;

/**
 * Every colour / shadow token the UI consumes.  `accent` is the *resolved*
 * accent colour.  `style` shapes how surfaces are painted.
 */
export function buildTokens(
  theme: ThemeDef,
  accentColor: string,
  style: UiStyle = "solid",
  backdropActive = false
): Record<string, string> {
  const d = deriveTheme(theme);
  const accent = normalizeHex(accentColor) ?? fallbackAccent(d);
  const surfaces = [d.bg, d.surface, d.raised, d.hover];
  const accentInk = inkOn(accent);
  const accentText = pushContrast(accent, d.text, surfaces, 4.5);
  // Hover moves away from the ink colour, so the button label never loses contrast.
  const accentHover = luminance(accentInk) > 0.5 ? mix(accent, "#000000", 0.1) : mix(accent, "#ffffff", 0.1);
  const lineWeak = alpha(d.text, 8);
  const lineMid = alpha(d.text, 15);
  const shadowInk = d.light ? "#28283c" : "#000000";
  const shadowStrength = d.light ? 0.2 : 0.6;

  let surface = d.surface;
  let elevated = d.raised;
  let border = lineWeak;
  if (style === "glass") {
    surface = alpha(d.surface, 52);
    elevated = alpha(d.raised, 55);
    border = lineMid;
  } else if (style === "line") {
    const base = backdropActive ? alpha(d.bg, 72) : d.bg;
    surface = base;
    elevated = base;
    border = lineMid;
  }

  const tokens: Record<string, string> = {
    "--color-background": d.bg,
    "--color-surface": surface,
    "--color-surface-elevated": elevated,
    "--color-surface-active": d.hover,
    "--color-surface-opaque": d.surface,
    "--color-surface-elevated-opaque": d.raised,
    "--color-text": d.text,
    "--color-text-secondary": d.secondary,
    "--color-text-muted": d.muted,
    "--color-text-faint": d.faint,
    "--color-accent": accent,
    "--color-accent-contrast": accentInk,
    "--color-accent-text": accentText,
    "--color-border": border,
    "--color-border-strong": d.lineStrong,
    "--color-text-inverse": accentInk,
    "--color-accent-hover": accentHover,
    "--color-accent-subtle": `color-mix(in srgb, ${accent} 12%, ${d.surface})`,
    "--color-surface-overlay": alpha(d.surface, 88),
    "--color-backdrop-dim": d.bg,
    "--color-modal-backdrop": d.light ? "#1a1a1a66" : "#00000099",
    "--color-success": d.success,
    "--color-warning": d.warning,
    "--color-danger": d.danger,
    "--color-danger-contrast": d.dangerInk,
    "--color-danger-subtle": `color-mix(in srgb, var(--color-danger) 10%, ${d.surface})`,
    "--color-warning-subtle": `color-mix(in srgb, var(--color-warning) 10%, ${d.surface})`,
    "--color-focus": d.focus,
    "--color-scrollbar": mix(d.bg, d.text, 0.25),
    "--color-selection": `color-mix(in srgb, ${accent} 35%, ${d.bg})`,
    "--color-selection-text": d.text,
    "--color-code-bg": d.bg,
    "--color-code-text": d.text,
    "--color-code-keyword": accentText,
    "--color-code-string": d.success,
    "--color-code-number": d.warning,
    "--color-code-comment": d.muted,
    "--color-overlay-target-bg": alpha(accent, 16),
    "--color-overlay-ripple": alpha(accent, 48),
    "--color-overlay-human-bg": "var(--color-warning-subtle)",
    "--color-accent-deep": accentText,
    "--color-primary": d.text,
    "--color-primary-contrast": d.bg,
    "--color-logo-tile": "#F5F5F3",
    "--color-creature-eyes": "#172125",
    "--color-bg-gradient": d.bg2,
    "--color-shadow": `color-mix(in srgb, ${shadowInk} ${Math.round(shadowStrength * 100)}%, transparent)`,
    "--shadow-sm": `0 1px 2px ${alpha(shadowInk, d.light ? 12 : 30)}`,
    "--shadow-card": `0 4px 16px ${alpha(shadowInk, d.light ? 10 : 18)}`,
    "--shadow-overlay": d.light
      ? `0 24px 80px ${alpha(shadowInk, 30)}, 0 2px 8px ${alpha(shadowInk, 14)}`
      : `0 32px 100px ${alpha(shadowInk, 50)}, 0 0 0 1px ${alpha(d.text, 2)}`,
    "--shadow-focus": "0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)",
    "--shadow-target": "0 0 0 4px var(--color-accent-subtle)",
    "--shadow-human-banner": "0 0 0 3px var(--color-warning-subtle)"
  };
  return tokens;
}

/** `color-scheme` value for a theme. */
export function colorSchemeFor(theme: ThemeDef): "light" | "dark" {
  return deriveTheme(theme).light ? "light" : "dark";
}
