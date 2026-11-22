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
import {
  DEFAULT_SURFACE,
  parsePanelBlur,
  parsePanelTransparency,
  parseTransparentChat,
  parseVeil,
  VEIL_DEFAULT
} from "ui/surface-model";
import {
  DEFAULT_WALLPAPER,
  DEFAULT_WALLPAPER_MOTION,
  parseWallpaper,
  parseWallpaperMotion,
  type WallpaperMotion,
  type WallpaperSetting
} from "./wallpaper.ts";
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
  /** Animated dithered-cloud wallpaper (`off` by default); takes the backdrop's place while on. */
  wallpaper: WallpaperSetting;
  /** Cloud motion mode (`off`, `subtle` default, `live` 30 fps WebGL). */
  wallpaperMotion: WallpaperMotion;
  /** Strength of the soft veil over the wallpaper, 0-100 %. */
  wallpaperVeil: number;
  /** 20..88: how much of the theme background covers the scene. */
  backdropDim: number;
  /** 0..100: tinted / glass panels, 0 = most opaque, 100 = the legibility floor. */
  panelTransparency: number;
  /** Glass blur radius, px (12..48). */
  panelBlur: number;
  /** The center chat area has no background of its own (v6 look; derived: `!chatPanel`). */
  transparentChat: boolean;
  /**
   * The owner switched the center panel back on. A separate flag from the old
   * `transparentChat` on purpose: settings saved before the v6 design stored
   * `transparentChat: false` without the user ever choosing it, so only this
   * explicit flag turns the panel on.
   */
  chatPanel: boolean;
  /** Paint the chosen panel style even when the OS asks apps to reduce transparency. */
  transparencyOverride: boolean;
  /** Bots use the photoreal clay body renders (default) instead of the drawn SVG clay. */
  photorealAvatars: boolean;
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
  wallpaper: DEFAULT_WALLPAPER,
  wallpaperMotion: DEFAULT_WALLPAPER_MOTION,
  wallpaperVeil: VEIL_DEFAULT,
  backdropDim: DEFAULT_BACKDROP_DIM,
  panelTransparency: DEFAULT_SURFACE.transparency,
  panelBlur: DEFAULT_SURFACE.blur,
  transparentChat: true,
  chatPanel: false,
  transparencyOverride: false,
  photorealAvatars: true
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
  const chatPanel = raw.chatPanel === true;
  return {
    themeMode: parseThemeMode(raw.themeMode),
    customTheme: parseCustomTheme(raw.customTheme),
    accentColor: parseAccentPreference(raw.accentColor),
    uiStyle: parseUiStyle(raw.uiStyle),
    corners: parseCorners(raw.corners),
    bubble: parseBubble(raw.bubble),
    ownBubble: parseOwnBubble(raw.ownBubble),
    backdrop: parseBackdrop(raw.backdrop),
    wallpaper: parseWallpaper(raw.wallpaper),
    wallpaperMotion: parseWallpaperMotion(raw.wallpaperMotion),
    wallpaperVeil: parseVeil(raw.wallpaperVeil),
    backdropDim: parseBackdropDim(raw.backdropDim),
    panelTransparency: parsePanelTransparency(raw.panelTransparency),
    panelBlur: parsePanelBlur(raw.panelBlur),
    transparentChat: !chatPanel,
    chatPanel,
    transparencyOverride: raw.transparencyOverride === true,
    // Default on: only an explicit `false` turns the photoreal skin off.
    photorealAvatars: raw.photorealAvatars !== false
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
 * `meb`, `backdrop`, `wallpaper`, `dim`, `alpha` (panel transparency), `blur`, `clear` (transparent chat area, `1`).
 * Absent parameters leave the saved value alone.
 */
export function appearanceFromQuery(
  params: { get(name: string): string | null },
  saved: AppearanceSettings
): AppearanceSettings & { backgroundUrl?: string } {
  // `bgcolor=rrggbb`: a plain colour as the own-image backdrop (review screenshots of flat backdrops).
  const flat = /^[0-9a-f]{6}$/i.exec(params.get("bgcolor") ?? "")?.[0];
  if (flat) {
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="#${flat}"/></svg>`;
    const rest = appearanceFromQuery({ get: (n) => (n === "bgcolor" ? null : n === "backdrop" ? "custom" : params.get(n)) }, saved);
    return { ...rest, backgroundUrl: `data:image/svg+xml,${encodeURIComponent(svg)}` };
  }
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
    wallpaper: q("wallpaper") ?? saved.wallpaper,
    wallpaperMotion: q("motion") ?? q("wallpaperMotion") ?? saved.wallpaperMotion,
    wallpaperVeil: q("veil") !== undefined ? parseVeil(q("veil")) : saved.wallpaperVeil,
    backdropDim: q("dim") ?? saved.backdropDim,
    panelTransparency: q("alpha") ?? saved.panelTransparency,
    panelBlur: q("blur") ?? saved.panelBlur,
    chatPanel: q("clear") !== undefined ? !parseTransparentChat(q("clear")) : saved.chatPanel,
    // Review screenshots run in headless Chrome, which reports the OS transparency preference too.
    transparencyOverride: q("rt") === "ignore" ? true : saved.transparencyOverride
  });
  // A theme picked by URL gets the same accent treatment as a click.
  return q("theme") && accent === undefined
    ? { ...next, ...themePickPatch({ accentColor: saved.accentColor, customTheme: next.customTheme }, next.themeMode) }
    : next;
}
