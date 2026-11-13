import {
  buildTokens,
  colorSchemeFor,
  DEFAULT_CUSTOM_THEME,
  deriveTheme,
  fallbackAccent,
  normalizeHex,
  resolveAccent,
  themeFor,
  type AccentPreference,
  type BubbleStyle,
  type Corners,
  type CustomTheme,
  type OwnBubble,
  type ThemeDef,
  type UiStyle
} from "./themeModel";

export {
  accentAfterThemeChange,
  buildTokens,
  contrastRatio,
  CUSTOM_THEME_ID,
  customThemeContrast,
  DEFAULT_CUSTOM_THEME,
  DEFAULT_THEME_ID,
  deriveTheme,
  FAVORITE_THEMES,
  isLightColor,
  mix,
  normalizeHex,
  PALETTE_IDS,
  PALETTES,
  paletteById,
  parseAccentPreference,
  parseCustomTheme,
  parseThemeMode,
  resolveAccent,
  themeFor,
  BUBBLE_STYLES,
  CORNERS,
  OWN_BUBBLES,
  UI_STYLES,
  type AccentPreference,
  type BubbleStyle,
  type Corners,
  type CustomTheme,
  type DerivedTheme,
  type OwnBubble,
  type ThemeDef,
  type UiStyle
} from "./themeModel";

/** A saved theme choice: a palette id, `custom`, `system`, or legacy `light` / `dark`. */
export type ThemeMode = string;
export type { ThemeDef as Theme };

export interface ThemeBackground {
  url?: string;
  blur?: string;
  dim?: string;
}

export interface ThemeOverrides {
  /** Already-resolved accent colour (see `resolveAccent`). */
  accentColor?: string;
  style?: UiStyle;
  corners?: Corners;
  bubble?: BubbleStyle;
  ownBubble?: OwnBubble;
  /** A canvas backdrop (or own image) is active: panels float, line style turns see-through. */
  backdropActive?: boolean;
  background?: ThemeBackground;
}

export function isReducedMotionPreferred(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function subscribeReducedMotion(
  onChange: (reduced: boolean) => void
): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const mediaQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  const handler = (event: MediaQueryListEvent) => {
    onChange(event.matches);
  };
  mediaQuery.addEventListener("change", handler);
  return () => {
    mediaQuery.removeEventListener("change", handler);
  };
}

function systemPrefersDark(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** The theme a saved mode stands for (resolves `system`, legacy names, `custom`). */
export function resolveTheme(mode: ThemeMode, custom: CustomTheme = DEFAULT_CUSTOM_THEME): ThemeDef {
  return themeFor(mode, custom, systemPrefersDark());
}

export function subscribeSystemTheme(onChange: (dark: boolean) => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
  const handler = (event: MediaQueryListEvent) => {
    onChange(event.matches);
  };
  mediaQuery.addEventListener("change", handler);
  return () => {
    mediaQuery.removeEventListener("change", handler);
  };
}

/** Accent for the given preference, theme and active bot. */
export function accentFor(pref: AccentPreference, theme: ThemeDef, botColor?: string): string {
  return resolveAccent(pref, theme, botColor, fallbackAccent(deriveTheme(theme)));
}

/** Custom properties set inline on the previous `applyTheme` call. */
let appliedProps: string[] = [];

export function applyTheme(theme: ThemeDef, overrides?: ThemeOverrides): void {
  if (typeof document === "undefined") {
    return;
  }
  const root = document.documentElement;
  for (const prop of appliedProps) root.style.removeProperty(prop);

  const style = overrides?.style ?? "solid";
  const accent = normalizeHex(overrides?.accentColor) ?? accentFor("", theme);
  const tokens = buildTokens(theme, accent, style, overrides?.backdropActive === true);
  appliedProps = Object.keys(tokens);
  for (const [prop, value] of Object.entries(tokens)) root.style.setProperty(prop, value);
  root.style.colorScheme = colorSchemeFor(theme);

  const bg = overrides?.background ?? {};
  const rawUrl = bg.url?.trim() ?? "";
  const isGif =
    rawUrl.toLowerCase().endsWith(".gif") ||
    rawUrl.toLowerCase().startsWith("data:image/gif");
  const shouldHideAnimatedBg = isGif && isReducedMotionPreferred();
  const effectiveUrl = shouldHideAnimatedBg ? "" : rawUrl;

  if (effectiveUrl) {
    const escapedUrl = effectiveUrl.replace(/"/g, '\\"');
    root.style.setProperty("--background-image", `url("${escapedUrl}")`);
    root.dataset.hasBackground = "true";
  } else {
    root.style.setProperty("--background-image", "none");
    root.dataset.hasBackground = "false";
  }
  root.style.setProperty("--background-blur", bg.blur?.trim() || "0px");
  root.style.setProperty("--background-dim", bg.dim?.trim() || "0");

  root.dataset.theme = theme.id;
  root.dataset.style = style;
  root.dataset.corners = overrides?.corners ?? "soft";
  root.dataset.bubble = overrides?.bubble ?? "soft";
  root.dataset.meb = overrides?.ownBubble ?? "tint";
  const backdropActive = overrides?.backdropActive === true;
  root.dataset.backdrop = backdropActive ? "on" : "off";
  root.dataset.float = backdropActive || style === "glass" ? "on" : "off";
}
