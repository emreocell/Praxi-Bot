import type { ReactNode } from "react";
import type { SupportedLanguage } from "../../i18n";
import type { AppSettings } from "../../store/settingsStore";
import type { ThemeMode } from "ui";

export type SettingsSectionId =
  | "general"
  | "ai"
  | "computer"
  | "usage"
  | "updates";

export const SETTINGS_SECTIONS: readonly SettingsSectionId[] = [
  "general",
  "ai",
  "computer",
  "usage",
  "updates"
];

/** One searchable entry of the settings modal. */
export interface CardDef {
  id: string;
  section: SettingsSectionId;
  title: string;
  description: string;
  /** Extra words the search also matches. */
  keywords?: string;
  /** `row` cards share a rounded group; `block` cards bring their own frame. */
  kind: "row" | "block";
  node: ReactNode;
}

/** What the modal needs from the app shell. */
export interface SettingsBridge {
  settings: AppSettings;
  update: (patch: Partial<AppSettings>) => void;
  onTheme: (mode: ThemeMode) => void;
  onAccent: (color: string) => void;
  onLanguage: (lang: SupportedLanguage) => void;
  onWindowFrame: (on: boolean) => void;
  recordingHotkey: boolean;
  onToggleRecordHotkey: () => void;
  onStopHotkey: (hotkey: string) => void;
  onToggleAutostart: () => void;
  autostartError: string | null;
  onOpenOnboarding: (restart: boolean) => void;
}
