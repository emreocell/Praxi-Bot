import {
  accentAfterThemeChange,
  BUBBLE_STYLES,
  CORNERS,
  DEFAULT_CUSTOM_THEME,
  OWN_BUBBLES,
  paletteById,
  parseAccentPreference,
  parseCustomTheme,
  parseThemeMode,
  themeFor,
  UI_STYLES,
  type AccentPreference,
  type BubbleStyle,
  type Corners,
  type CustomTheme,
  type OwnBubble,
  type UiStyle
} from "ui/theme-model";
import { DEFAULT_BACKDROP_DIM, NO_BACKDROP, parseBackdrop, parseBackdropDim } from "./backdropCatalog.ts";

/** Everything the Appearance page controls (persisted inside `AppSettings`). */
export interface AppearanceSettings {
  /** Palette id, `custom`, `system`, or a legacy name (`light`, `dark`). */
  themeMode: string;
  customTheme: CustomTheme;
  /** `""` = the bot's colour, `"theme"` = the theme's own, or `#rrggbb`. */
  accentColor: AccentPreference;
  uiStyle: UiStyle;
  corners: Corners;
  bubble: BubbleStyle;
  ownBubble: OwnBubble;
  backdrop: string;
  /** 20..88: how much of the theme background covers the scene. */
  backdropDim: number;
}

export const DEFAULT_APPEARANCE: AppearanceSettings = {
  themeMode: "obsidian",
  customTheme: DEFAULT_CUSTOM_THEME,
  accentColor: "",
  uiStyle: "solid",
  corners: "soft",
  bubble: "soft",
  ownBubble: "tint",
  backdrop: NO_BACKDROP,
  backdropDim: DEFAULT_BACKDROP_DIM
};

function oneOf<T extends string>(values: readonly T[], v: unknown, fallback: T): T {
  return values.find((x) => x === v) ?? fallback;
}

export const parseUiStyle = (v: unknown): UiStyle => oneOf(UI_STYLES, v, DEFAULT_APPEARANCE.uiStyle);
export const parseCorners = (v: unknown): Corners => oneOf(CORNERS, v, DEFAULT_APPEARANCE.corners);
export const parseBubble = (v: unknown): BubbleStyle => oneOf(BUBBLE_STYLES, v, DEFAULT_APPEARANCE.bubble);
export const parseOwnBubble = (v: unknown): OwnBubble => oneOf(OWN_BUBBLES, v, DEFAULT_APPEARANCE.ownBubble);

/**
 * Validate every appearance field of a stored settings object (plugin-store
 * entry or the localStorage fallback).  Unknown / malformed values fall back
 * to the defaults; old theme names keep working.
 */
export function normalizeAppearance(raw: Record<string, unknown>): AppearanceSettings {
  return {
    themeMode: parseThemeMode(raw.themeMode),
    customTheme: parseCustomTheme(raw.customTheme),
    accentColor: parseAccentPreference(raw.accentColor),
    uiStyle: parseUiStyle(raw.uiStyle),
    corners: parseCorners(raw.corners),
    bubble: parseBubble(raw.bubble),
    ownBubble: parseOwnBubble(raw.ownBubble),
    backdrop: parseBackdrop(raw.backdrop),
    backdropDim: parseBackdropDim(raw.backdropDim)
  };
}

/** Patch to apply when the user picks a theme (a theme with its own accent selects it). */
export function themePickPatch(
  current: Pick<AppearanceSettings, "accentColor" | "customTheme">,
  mode: string
): Pick<AppearanceSettings, "themeMode" | "accentColor"> {
  const themeMode = parseThemeMode(mode);
  const theme = themeFor(themeMode, current.customTheme, false);
  return { themeMode, accentColor: accentAfterThemeChange(current.accentColor, theme) };
}

/** Is this palette id one of the shipped themes? */
export const isShippedTheme = (id: string): boolean => paletteById(id) !== undefined;

/**
 * Appearance overrides from URL parameters (`?mock=1` review screenshots only):
 * `theme`, `accent` (`bot` | `theme` | hex), `style`, `corners`, `bubble`,
 * `meb`, `backdrop`, `dim`.  Absent parameters leave the saved value alone.
 */
export function appearanceFromQuery(
  params: { get(name: string): string | null },
  saved: AppearanceSettings
): AppearanceSettings {
  const q = (name: string) => params.get(name) ?? undefined;
  const accent = q("accent");
  const themeMode = q("theme") ?? saved.themeMode;
  const next = normalizeAppearance({
    ...saved,
    themeMode,
    accentColor: accent ?? saved.accentColor,
    uiStyle: q("style") ?? saved.uiStyle,
    corners: q("corners") ?? saved.corners,
    bubble: q("bubble") ?? saved.bubble,
    ownBubble: q("meb") ?? saved.ownBubble,
    backdrop: q("backdrop") ?? saved.backdrop,
    backdropDim: q("dim") ?? saved.backdropDim
  });
  // A theme picked by URL gets the same accent treatment as a click.
  return q("theme") && accent === undefined
    ? { ...next, ...themePickPatch({ accentColor: saved.accentColor, customTheme: next.customTheme }, next.themeMode) }
    : next;
}
