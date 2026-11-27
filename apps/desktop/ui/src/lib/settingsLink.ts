import type { SettingsPageId } from "./settingsPages.ts";

/** A place in Settings a message can point to: the page, the card on it and the field to focus. */
export interface SettingsTarget {
  page: SettingsPageId;
  /** Id of the settings card (see CARD_META); a test keeps it real. */
  card: string;
  field: string;
  /** A bot-scoped card (memory, knowledge) opens on this bot instead of the first one. */
  bot?: string;
}

/** Where the transcription service and its API key live (Settings → General → Voice dictation). */
export const VOICE_KEY_TARGET: SettingsTarget = { page: "general", card: "dictation", field: "voice-key" };

export const OPEN_SETTINGS_EVENT = "praxi:open-settings";
export const OPEN_VAULT_EVENT = "praxi:open-vault";

export function openVault(): void {
  window.dispatchEvent(new Event(OPEN_VAULT_EVENT));
}

export const OPEN_APPS_EVENT = "praxi:open-apps";

/** Where the Apps window opens: on the Advanced list (Google apps) and/or with the search filled in. */
export interface AppsStart {
  advanced?: boolean;
  query?: string;
}

/** Opens the Apps window on top of whatever is open (the guided mail flow uses it for the sign-in step). */
export function openApps(start: AppsStart = {}): void {
  window.dispatchEvent(new CustomEvent<AppsStart>(OPEN_APPS_EVENT, { detail: start }));
}

let pending: SettingsTarget | null = null;

/** Opens Settings on the target's page, scrolls the card into view, flashes it, and focuses the field. */
export function openSettingsTarget(target: SettingsTarget): void {
  pending = target;
  window.dispatchEvent(new CustomEvent<SettingsTarget>(OPEN_SETTINGS_EVENT, { detail: target }));
}

/** Read without consuming the pending target (used by SettingsModal to scroll & flash the card). */
export function peekPendingTarget(): SettingsTarget | null {
  return pending;
}

/** True once for the field a deep link asked to focus. */
export function takePendingFocus(field: string): boolean {
  if (pending?.field !== field) return false;
  pending = null;
  return true;
}

/** The bot a deep link asked `card` to open on, if any. Read while the card mounts. */
export function peekPendingBot(card: string): string | null {
  return pending?.card === card && pending.bot ? pending.bot : null;
}

/** Once the card has taken the bot, a later visit opens on its own choice again. */
export function clearPendingBot(card: string): void {
  if (pending?.card === card && pending.bot) pending = { ...pending, bot: undefined };
}
