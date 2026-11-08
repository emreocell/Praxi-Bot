import darkThemeJson from "../themes/dark.json";
import lightThemeJson from "../themes/light.json";
import cobaltThemeJson from "../themes/cobalt.json";
import ravenThemeJson from "../themes/raven.json";

export type ThemeName = "obsidian" | "cobalt" | "raven" | "clay";
/** light/dark remain valid for existing saved preferences. */
export type ThemeMode = ThemeName | "light" | "dark" | "system";
export const THEME_NAMES: readonly ThemeName[] = ["obsidian", "cobalt", "raven", "clay"];

export interface ThemeBackground {
  url?: string;
  blur?: string;
  dim?: string;
}

export interface Theme {
  name: string;
  mode?: "light" | "dark";
  tokens: Record<string, string>;
  background?: ThemeBackground;
}

export interface ThemeOverrides {
  accentColor?: string;
  background?: ThemeBackground;
}

export const lightTheme: Theme = lightThemeJson as Theme;
export const darkTheme: Theme = darkThemeJson as Theme;
export const themes: Record<ThemeName, Theme> = {
  obsidian: darkTheme,
  cobalt: cobaltThemeJson as Theme,
  raven: ravenThemeJson as Theme,
  clay: lightTheme
};

export function contrastRatio(foreground: string, background: string): number {
  const luminance = (hex: string) => {
    const values = hex.replace("#", "").match(/.{2}/g)?.slice(0, 3).map((v) => {
      const c = parseInt(v, 16) / 255;
      return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
    }) ?? [0, 0, 0];
    return values[0] * 0.2126 + values[1] * 0.7152 + values[2] * 0.0722;
  };
  const a = luminance(foreground), b = luminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
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

export function resolveTheme(mode: ThemeMode): Theme {
  if (Object.hasOwn(themes, mode)) return themes[mode as ThemeName];
  if (mode === "light") {
    return lightTheme;
  }
  if (mode === "dark") {
    return darkTheme;
  }
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    const isDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    return isDark ? darkTheme : lightTheme;
  }
  return lightTheme;
}

export function subscribeSystemTheme(onChange: (theme: Theme) => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
  const handler = (event: MediaQueryListEvent) => {
    onChange(event.matches ? darkTheme : lightTheme);
  };
  mediaQuery.addEventListener("change", handler);
  return () => {
    mediaQuery.removeEventListener("change", handler);
  };
}

export function applyTheme(theme: Theme, overrides?: ThemeOverrides): void {
  if (typeof document === "undefined") {
    return;
  }
  const root = document.documentElement;

  // Built-ins are driven by :root[data-theme]; custom JSON themes still work.
  for (const token of Object.keys(darkTheme.tokens)) root.style.removeProperty(token);
  for (const [token, value] of Object.entries(theme.tokens)) {
    if (THEME_NAMES.includes(theme.name as ThemeName)) continue;
    const propName = token.startsWith("--") ? token : `--${token}`;
    root.style.setProperty(propName, value);
  }

  const accent = overrides?.accentColor?.trim();
  if (accent && /^#[0-9a-f]{6}$/i.test(accent)) {
    const contrast = contrastRatio(accent, "#111111") >= contrastRatio(accent, "#FFFFFF") ? "#111111" : "#FFFFFF";
    root.style.setProperty("--color-accent", accent);
    root.style.setProperty("--color-accent-contrast", contrast);
    root.style.setProperty("--color-text-inverse", contrast);
    // Arbitrary custom colors cannot make small links unreadable.
    root.style.setProperty("--color-accent-text", contrastRatio(accent, theme.tokens["--color-surface-active"]) >= 4.5 ? accent : theme.tokens["--color-text"]);
    root.style.setProperty(
      "--color-accent-hover",
      accent
    );
    root.style.setProperty(
      "--color-accent-subtle",
      `color-mix(in srgb, ${accent} 12%, var(--color-surface))`
    );
    root.style.setProperty(
      "--color-overlay-target-bg",
      `color-mix(in srgb, ${accent} 16%, transparent)`
    );
    root.style.setProperty(
      "--color-overlay-ripple",
      `color-mix(in srgb, ${accent} 48%, transparent)`
    );
  }

  const bg: ThemeBackground = {
    ...theme.background,
    ...overrides?.background
  };

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
  root.dataset.theme = theme.name;
}
