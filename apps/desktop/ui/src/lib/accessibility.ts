import { setReducedTransparencyForced } from "ui/surface-model";

/** Text size, 90..150 % of the normal size. */
export const TEXT_SCALE_MIN = 90;
export const TEXT_SCALE_MAX = 150;
export const TEXT_SCALE_DEFAULT = 100;
export const TEXT_SCALE_STEP = 5;

export const DENSITIES = ["compact", "comfortable", "spacious"] as const;
export type Density = (typeof DENSITIES)[number];

export const FOCUS_RINGS = ["normal", "thick", "extra"] as const;
export type FocusRing = (typeof FOCUS_RINGS)[number];

/** The accessibility part of the Appearance settings (saved per device, like the rest). */
export interface AccessibilitySettings {
  textScale: number;
  density: Density;
  /** OpenDyslexic (bundled, SIL OFL) with a little more line spacing. */
  dyslexiaFont: boolean;
  /** No animation or transition; the wallpaper stands still. */
  reduceMotion: boolean;
  /** Panels are always painted solid, whatever the OS says. */
  reduceTransparency: boolean;
  focusRing: FocusRing;
}

export const DEFAULT_ACCESSIBILITY: AccessibilitySettings = {
  textScale: TEXT_SCALE_DEFAULT,
  density: "comfortable",
  dyslexiaFont: false,
  reduceMotion: false,
  reduceTransparency: false,
  focusRing: "normal"
};

const DENSITY_SCALE: Record<Density, number> = { compact: 0.8, comfortable: 1, spacious: 1.25 };
/** Focus outline thickness, px (the default is the 2 px token). */
export const FOCUS_RING_PX: Record<FocusRing, number> = { normal: 2, thick: 3, extra: 5 };

export function parseTextScale(v: unknown): number {
  const n = typeof v === "number" ? v : typeof v === "string" && v.trim() !== "" ? Number(v) : NaN;
  if (!Number.isFinite(n)) return TEXT_SCALE_DEFAULT;
  return Math.min(TEXT_SCALE_MAX, Math.max(TEXT_SCALE_MIN, Math.round(n)));
}

const oneOf = <T extends string>(values: readonly T[], v: unknown, fallback: T): T =>
  values.find((x) => x === v) ?? fallback;

/** Validate the accessibility fields of a stored settings object; anything odd falls back to the default. */
export function normalizeAccessibility(raw: Partial<Record<keyof AccessibilitySettings, unknown>>): AccessibilitySettings {
  return {
    textScale: parseTextScale(raw.textScale),
    density: oneOf(DENSITIES, raw.density, DEFAULT_ACCESSIBILITY.density),
    dyslexiaFont: raw.dyslexiaFont === true,
    reduceMotion: raw.reduceMotion === true,
    reduceTransparency: raw.reduceTransparency === true,
    focusRing: oneOf(FOCUS_RINGS, raw.focusRing, DEFAULT_ACCESSIBILITY.focusRing)
  };
}

/** Base sizes (px) of the tokens in packages/ui/tokens.css that follow the text size. */
export const TEXT_TOKENS_PX: Record<string, number> = {
  "--font-size-ui": 13,
  "--font-size-xs": 12,
  "--font-size-sm": 14,
  "--font-size-md": 15,
  "--font-size-lg": 20,
  "--font-size-xl": 24,
  "--font-size-display": 30
};
/** Base sizes (px) of the spacing tokens that follow the density. */
export const SPACE_TOKENS_PX: Record<string, number> = {
  "--space-1": 4,
  "--space-2": 8,
  "--space-3": 12,
  "--space-4": 16,
  "--space-5": 20,
  "--space-6": 24,
  "--space-8": 32
};
const MOTION_TOKENS = [
  "--motion-duration-fast",
  "--motion-duration-normal",
  "--motion-duration-slow",
  "--motion-duration-ripple",
  "--motion-duration-pulse",
  "--motion-duration-avatar-hover",
  "--motion-duration-avatar-press",
  "--motion-duration-avatar-state"
];

const px = (n: number) => `${Math.round(n * 100) / 100}px`;

/**
 * The CSS custom properties that carry the settings.  Only tokens that differ from the
 * stylesheet default are returned, so the defaults leave `<html>` untouched.
 */
export function accessibilityVars(a: AccessibilitySettings): Record<string, string> {
  const vars: Record<string, string> = {};
  if (a.textScale !== TEXT_SCALE_DEFAULT) {
    for (const [k, base] of Object.entries(TEXT_TOKENS_PX)) vars[k] = px((base * a.textScale) / 100);
  }
  if (a.density !== "comfortable") {
    for (const [k, base] of Object.entries(SPACE_TOKENS_PX)) vars[k] = px(base * DENSITY_SCALE[a.density]);
  }
  if (a.dyslexiaFont) {
    vars["--font-sans"] = '"OpenDyslexic", "Segoe UI", sans-serif';
    vars["--line-height-normal"] = "1.7";
    vars["--line-height-chat"] = "1.95";
    vars["--letter-spacing-wide"] = "1px";
  }
  if (a.reduceMotion) for (const k of MOTION_TOKENS) vars[k] = "0ms";
  if (a.focusRing !== "normal") vars["--size-border-thick"] = px(FOCUS_RING_PX[a.focusRing]);
  return vars;
}

/** Every property `accessibilityVars` can set (to clear the ones that no longer apply). */
const ALL_VAR_NAMES = [
  ...Object.keys(TEXT_TOKENS_PX),
  ...Object.keys(SPACE_TOKENS_PX),
  ...MOTION_TOKENS,
  "--font-sans",
  "--line-height-normal",
  "--line-height-chat",
  "--letter-spacing-wide",
  "--size-border-thick"
];

/** Write the accessibility settings onto <html>.  Live: the preview is the app itself. */
export function applyAccessibility(
  root: HTMLElement,
  raw: Partial<Record<keyof AccessibilitySettings, unknown>>
): AccessibilitySettings {
  const a = normalizeAccessibility(raw);
  const vars = accessibilityVars(a);
  for (const name of ALL_VAR_NAMES) {
    if (name in vars) root.style.setProperty(name, vars[name]);
    else root.style.removeProperty(name);
  }
  if (a.reduceMotion) root.dataset.reduceMotion = "1";
  else delete root.dataset.reduceMotion;
  setReducedTransparencyForced(a.reduceTransparency);
  return a;
}

/** The wallpaper motion to draw: "off" (one still frame) while Reduce motion is on. */
export const effectiveWallpaperMotion = <M extends string>(motion: M, reduceMotion: boolean): M | "off" =>
  reduceMotion ? "off" : motion;
