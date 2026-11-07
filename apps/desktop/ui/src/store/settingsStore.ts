import { load, type Store } from "@tauri-apps/plugin-store";
import type { ThemeMode } from "ui";
import type { PermissionPreset } from "../api/types";
import { DEFAULT_STOP_HOTKEY, PRODUCT_CONFIG } from "../config/product";
import type { SupportedLanguage } from "../i18n";

export interface AppSettings {
  themeMode: ThemeMode;
  accentColor: string;
  backgroundUrl: string;
  backgroundFileName: string;
  backgroundBlur: string;
  backgroundDim: string;
  language: SupportedLanguage;
  stopHotkey: string;
  onboardingCompleted: boolean;
  onboardingSkipped: boolean;
  onboardingStep: number;
  onboardingProviderId: string;
  onboardingModel: string;
  onboardingPermissionPreset: PermissionPreset;
  onboardingBotId: string;
  autostart: boolean;
  /** OS notification when an approval or question waits (window unfocused). */
  notifyNeedsYou: boolean;
  /** ... when a run longer than 30 s finishes (window unfocused). */
  notifyLongRun: boolean;
  /** ... when a run fails. */
  notifyFailed: boolean;
  /** Notifications carry only the bot name and a generic text. */
  notifyPrivate: boolean;
  /** Debug: also show the route chip for a plain `Agent` decision at K4. */
  showAgentRoute: boolean;
}

export const DEFAULT_SETTINGS: AppSettings = {
  themeMode: "system",
  accentColor: "",
  backgroundUrl: "",
  backgroundFileName: "",
  backgroundBlur: "0px",
  backgroundDim: "0",
  language: "en",
  stopHotkey: DEFAULT_STOP_HOTKEY,
  onboardingCompleted: false,
  onboardingSkipped: false,
  onboardingStep: 1,
  onboardingProviderId: "",
  onboardingModel: "",
  onboardingPermissionPreset: "approve-destructive",
  onboardingBotId: "",
  autostart: false,
  notifyNeedsYou: true,
  notifyLongRun: true,
  notifyFailed: true,
  notifyPrivate: false,
  showAgentRoute: false
};

export function shouldOpenOnboardingOnStart(
  settings: Pick<AppSettings, "onboardingCompleted" | "onboardingSkipped">
): boolean {
  return !settings.onboardingCompleted && !settings.onboardingSkipped;
}

const LOCAL_STORAGE_KEY = "desktop_ui_settings";

let storeInstancePromise: Promise<Store | null> | null = null;

function isTauriRuntime(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

async function getTauriStore(): Promise<Store | null> {
  if (!isTauriRuntime()) {
    return null;
  }
  if (!storeInstancePromise) {
    storeInstancePromise = load(PRODUCT_CONFIG.storeFileName, {
      defaults: { ...DEFAULT_SETTINGS },
      autoSave: true
    }).catch(() => null);
  }
  return storeInstancePromise;
}

export async function loadSettings(): Promise<AppSettings> {
  const store = await getTauriStore();
  if (store) {
    try {
      const themeMode =
        (await store.get<ThemeMode>("themeMode")) ?? DEFAULT_SETTINGS.themeMode;
      const accentColor =
        (await store.get<string>("accentColor")) ??
        DEFAULT_SETTINGS.accentColor;
      const backgroundUrl =
        (await store.get<string>("backgroundUrl")) ??
        DEFAULT_SETTINGS.backgroundUrl;
      const backgroundFileName =
        (await store.get<string>("backgroundFileName")) ??
        DEFAULT_SETTINGS.backgroundFileName;
      const backgroundBlur =
        (await store.get<string>("backgroundBlur")) ??
        DEFAULT_SETTINGS.backgroundBlur;
      const backgroundDim =
        (await store.get<string>("backgroundDim")) ??
        DEFAULT_SETTINGS.backgroundDim;
      const language =
        (await store.get<SupportedLanguage>("language")) ??
        DEFAULT_SETTINGS.language;
      const stopHotkey =
        (await store.get<string>("stopHotkey")) ?? DEFAULT_SETTINGS.stopHotkey;
      const onboardingCompleted =
        (await store.get<boolean>("onboardingCompleted")) ??
        DEFAULT_SETTINGS.onboardingCompleted;
      const onboardingSkipped =
        (await store.get<boolean>("onboardingSkipped")) ??
        DEFAULT_SETTINGS.onboardingSkipped;
      const onboardingStep =
        (await store.get<number>("onboardingStep")) ??
        DEFAULT_SETTINGS.onboardingStep;
      const onboardingProviderId =
        (await store.get<string>("onboardingProviderId")) ??
        DEFAULT_SETTINGS.onboardingProviderId;
      const onboardingModel =
        (await store.get<string>("onboardingModel")) ??
        DEFAULT_SETTINGS.onboardingModel;
      const onboardingPermissionPreset =
        (await store.get<PermissionPreset>("onboardingPermissionPreset")) ??
        DEFAULT_SETTINGS.onboardingPermissionPreset;
      const onboardingBotId =
        (await store.get<string>("onboardingBotId")) ??
        DEFAULT_SETTINGS.onboardingBotId;
      const autostart =
        (await store.get<boolean>("autostart")) ?? DEFAULT_SETTINGS.autostart;
      const notifyNeedsYou =
        (await store.get<boolean>("notifyNeedsYou")) ??
        DEFAULT_SETTINGS.notifyNeedsYou;
      const notifyLongRun =
        (await store.get<boolean>("notifyLongRun")) ??
        DEFAULT_SETTINGS.notifyLongRun;
      const notifyFailed =
        (await store.get<boolean>("notifyFailed")) ??
        DEFAULT_SETTINGS.notifyFailed;
      const notifyPrivate =
        (await store.get<boolean>("notifyPrivate")) ??
        DEFAULT_SETTINGS.notifyPrivate;
      const showAgentRoute =
        (await store.get<boolean>("showAgentRoute")) ??
        DEFAULT_SETTINGS.showAgentRoute;

      return {
        themeMode,
        accentColor,
        backgroundUrl,
        backgroundFileName,
        backgroundBlur,
        backgroundDim,
        language,
        stopHotkey,
        onboardingCompleted,
        onboardingSkipped,
        onboardingStep,
        onboardingProviderId,
        onboardingModel,
        onboardingPermissionPreset,
        onboardingBotId,
        autostart,
        notifyNeedsYou,
        notifyLongRun,
        notifyFailed,
        notifyPrivate,
        showAgentRoute
      };
    } catch {
      // Fall back to localStorage below if reading from plugin-store fails
    }
  }

  if (typeof window !== "undefined" && window.localStorage) {
    try {
      const raw = window.localStorage.getItem(LOCAL_STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<AppSettings>;
        return {
          ...DEFAULT_SETTINGS,
          ...parsed
        };
      }
    } catch {
      // Ignore malformed localStorage
    }
  }

  return { ...DEFAULT_SETTINGS };
}

export async function saveSettings(settings: AppSettings): Promise<void> {
  if (typeof window !== "undefined" && window.localStorage) {
    try {
      window.localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(settings));
    } catch {
      // Ignore storage quota errors
    }
  }

  const store = await getTauriStore();
  if (store) {
    try {
      await store.set("themeMode", settings.themeMode);
      await store.set("accentColor", settings.accentColor);
      await store.set("backgroundUrl", settings.backgroundUrl);
      await store.set("backgroundFileName", settings.backgroundFileName);
      await store.set("backgroundBlur", settings.backgroundBlur);
      await store.set("backgroundDim", settings.backgroundDim);
      await store.set("language", settings.language);
      await store.set("stopHotkey", settings.stopHotkey);
      await store.set("onboardingCompleted", settings.onboardingCompleted);
      await store.set("onboardingSkipped", settings.onboardingSkipped);
      await store.set("onboardingStep", settings.onboardingStep);
      await store.set("onboardingProviderId", settings.onboardingProviderId);
      await store.set("onboardingModel", settings.onboardingModel);
      await store.set(
        "onboardingPermissionPreset",
        settings.onboardingPermissionPreset
      );
      await store.set("onboardingBotId", settings.onboardingBotId);
      await store.set("autostart", settings.autostart);
      await store.set("notifyNeedsYou", settings.notifyNeedsYou);
      await store.set("notifyLongRun", settings.notifyLongRun);
      await store.set("notifyFailed", settings.notifyFailed);
      await store.set("notifyPrivate", settings.notifyPrivate);
      await store.set("showAgentRoute", settings.showAgentRoute);
      await store.save();
    } catch {
      // Ignore store write errors in non-Tauri environments
    }
  }
}
