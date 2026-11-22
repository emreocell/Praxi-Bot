import type { SettingsPageId } from "./settingsPages.ts";
import { SETTINGS_PAGES } from "./settingsPages.ts";

/** How many recent chats the palette offers. */
export const PALETTE_RECENT_CHATS = 20;

/** The chat fields the palette needs. */
export interface PaletteChat {
  id: string;
  bot_id: string;
  created_at_ms: number;
  title?: string | null;
  archived?: boolean;
}

/**
 * The most recent chats for the palette: archived chats are left out, newest first,
 * at most `limit`. The input is not modified.
 */
export function recentChats<C extends PaletteChat>(
  chats: readonly C[],
  limit: number = PALETTE_RECENT_CHATS
): C[] {
  return chats
    .filter((c) => c.archived !== true)
    .sort((a, b) => b.created_at_ms - a.created_at_ms)
    .slice(0, Math.max(0, limit));
}

/** Every settings page, in the order the settings modal lists them. */
export function settingsPageIds(): readonly SettingsPageId[] {
  return SETTINGS_PAGES;
}
