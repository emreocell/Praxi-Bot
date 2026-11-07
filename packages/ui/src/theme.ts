import darkThemeJson from "../themes/dark.json";
import lightThemeJson from "../themes/light.json";

export type ThemeMode = "light" | "dark" | "system";

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

  for (const [token, value] of Object.entries(theme.tokens)) {
    const propName = token.startsWith("--") ? token : `--${token}`;
    root.style.setProperty(propName, value);
  }

  const accent = overrides?.accentColor?.trim();
  if (accent) {
    root.style.setProperty("--color-accent", accent);
    root.style.setProperty(
      "--color-accent-hover",
      `color-mix(in srgb, ${accent} 85%, black)`
    );
    root.style.setProperty(
      "--color-accent-subtle",
      `color-mix(in srgb, ${accent} 14%, transparent)`
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
  root.dataset.theme = theme.mode ?? theme.name;
}
