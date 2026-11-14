import { useEffect, useState } from "react";
import { accentFor, applyTheme, resolveTheme, subscribeSystemTheme, type Theme } from "ui";
import { backdropIsActive } from "../lib/backdropCatalog";
import { loadSettingsSync, type AppSettings } from "../store/settingsStore";

export interface AppliedAppearance {
  theme: Theme;
  /** The resolved accent colour. */
  accent: string;
}

type AppearanceInput = Pick<AppSettings, "themeMode" | "customTheme" | "accentColor">;

/** The window's own backdrop / style axes only apply to the main window. */
type FullAppearanceInput = AppearanceInput &
  Pick<
    AppSettings,
    | "uiStyle"
    | "corners"
    | "bubble"
    | "ownBubble"
    | "backdrop"
    | "backgroundUrl"
    | "backgroundBlur"
    | "backgroundDim"
  >;

/** Write the appearance onto <html> (no React): also used before the first render. */
export function paintAppearance(
  settings: FullAppearanceInput | AppearanceInput,
  theme: Theme,
  accent: string,
  full: boolean
): void {
  const f = settings as FullAppearanceInput;
  applyTheme(theme, {
    accentColor: accent,
    ...(full
      ? {
          style: f.uiStyle,
          corners: f.corners,
          bubble: f.bubble,
          ownBubble: f.ownBubble,
          backdropActive: backdropIsActive(f.backdrop, f.backgroundUrl),
          background: { url: f.backgroundUrl, blur: f.backgroundBlur, dim: f.backgroundDim }
        }
      : {})
  });
}

/** Apply the saved settings synchronously (localStorage mirror) so the first paint has the right colours. */
export function paintSavedAppearanceEarly(overlay: boolean): void {
  document.documentElement.dataset.booting = "1";
  const saved = loadSettingsSync();
  if (!saved) return;
  const theme = resolveTheme(saved.themeMode, saved.customTheme);
  paintAppearance(saved, theme, accentFor(saved.accentColor, theme, undefined), !overlay);
}

/**
 * Paint the saved appearance onto <html> and re-paint when the theme, the
 * accent (which may follow the active bot) or the OS colour scheme changes.
 * Returns the theme and accent in effect.
 */
export function useApplyAppearance(
  settings: FullAppearanceInput,
  botColor: string | undefined
): AppliedAppearance;
export function useApplyAppearance(
  settings: AppearanceInput,
  botColor: string | undefined,
  overlay: true
): AppliedAppearance;
export function useApplyAppearance(
  settings: FullAppearanceInput | AppearanceInput,
  botColor: string | undefined,
  overlay?: true
): AppliedAppearance {
  // Re-resolve `system` when the OS switches between light and dark.
  const [, setSystemTick] = useState(0);
  useEffect(() => {
    if (settings.themeMode !== "system") return undefined;
    return subscribeSystemTheme(() => setSystemTick((n) => n + 1));
  }, [settings.themeMode]);

  const theme = resolveTheme(settings.themeMode, settings.customTheme);
  const accent = accentFor(settings.accentColor, theme, botColor);
  const full = overlay ? null : (settings as FullAppearanceInput);
  const dep = [
    theme.id,
    theme.bg,
    theme.text,
    settings.accentColor,
    botColor,
    full?.uiStyle,
    full?.corners,
    full?.bubble,
    full?.ownBubble,
    full?.backdrop,
    full?.backgroundUrl,
    full?.backgroundBlur,
    full?.backgroundDim
  ];
  useEffect(() => {
    paintAppearance(settings, theme, accent, full !== null);
    // First paint is done: colour transitions may run from now on.
    const id = requestAnimationFrame(() => requestAnimationFrame(() => delete document.documentElement.dataset.booting));
    return () => cancelAnimationFrame(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `dep` lists every input
  }, dep);
  return { theme, accent };
}

