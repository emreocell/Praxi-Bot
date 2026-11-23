import type { SettingsPageId } from "./settingsPages.ts";

/** A place in Settings a message can point to: the page, the card on it and the field to focus. */
export interface SettingsTarget {
  page: SettingsPageId;
  /** Id of the settings card (see CARD_META); a test keeps it real. */
  card: string;
  field: string;
}

/** Where the transcription service and its API key live (Settings → Voice → Dictation service). */
export const VOICE_KEY_TARGET: SettingsTarget = { page: "voice", card: "dictation", field: "voice-key" };

export const OPEN_SETTINGS_EVENT = "praxi:open-settings";

let pending: SettingsTarget | null = null;

/** Opens Settings on the target's page; the card focuses the field once it is on screen. */
export function openSettingsTarget(target: SettingsTarget): void {
  pending = target;
  window.dispatchEvent(new CustomEvent<SettingsTarget>(OPEN_SETTINGS_EVENT, { detail: target }));
}

/** True once for the field a deep link asked to focus. */
export function takePendingFocus(field: string): boolean {
  if (pending?.field !== field) return false;
  pending = null;
  return true;
}
