import type { ReactNode } from "react";
import type { SupportedLanguage } from "../../i18n";
import type { AppSettings } from "../../store/settingsStore";

export type SettingsSectionId =
  | "general"
  | "appearance"
  | "ai"
  | "computer"
  | "mail"
  | "usage"
  | "diagnostics"
  | "updates";

export const SETTINGS_SECTIONS: readonly SettingsSectionId[] = [
  "general",
  "appearance",
  "ai",
  "computer",
  "mail",
  "usage",
  "diagnostics",
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
  /**
   * A `row` card that is its own topic (a big picker, not one line): it gets its own rounded
   * group with a gap above and below instead of sharing one with the neighbouring cards.
   */
  standalone?: boolean;
  /**
   * A whole tool that is a page of its own (the Vault): it fills the settings pane, and a search
   * result shows only its heading (a jump), not the tool itself.
   */
  fullPage?: boolean;
  node: ReactNode;
}

/** What the modal needs from the app shell. */
export interface SettingsBridge {
  settings: AppSettings;
  /** `deferSave`: apply now, persist / broadcast after a short pause (colour pickers, sliders). */
  update: (patch: Partial<AppSettings>, opts?: { deferSave?: boolean }) => void;
  /** Colour of the bot in view (the accent follows it by default). */
  botColor?: string;
  onLanguage: (lang: SupportedLanguage) => void;
  onWindowFrame: (on: boolean) => void;
  recordingHotkey: boolean;
  onToggleRecordHotkey: () => void;
  onStopHotkey: (hotkey: string) => void;
  onToggleAutostart: () => void;
  autostartError: string | null;
  onOpenOnboarding: (restart: boolean) => void;
}
