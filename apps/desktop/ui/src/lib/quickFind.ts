import { foldText, matchesQuery } from "./searchText.ts";

/**
 * The search overlay finds bots and chats by name before the message results (which come from the
 * daemon). Local and instant: the lists are already in memory. Accents and Turkish letters fold.
 */
export const QUICK_BOTS_MAX = 3;
export const QUICK_CHATS_MAX = 5;

export interface QuickBot {
  id: string;
  name: string;
}

export interface QuickChat {
  id: string;
  bot_id: string;
  title: string;
  archived?: boolean;
  pinned?: boolean;
}

/**
 * Bots whose name matches; chats whose title, with their bot's name, holds every word of the query
 * and at least one of them in the title ("atlas rapor" finds Atlas's "Haftalık rapor"; "atlas" alone
 * lists the bot, not every Atlas chat). Pinned chats first, archived last, otherwise the chat list's
 * own order (newest first). Untitled chats are never listed.
 */
export function quickFind<C extends QuickChat>(
  bots: readonly QuickBot[],
  chats: readonly C[],
  query: string
): { bots: QuickBot[]; chats: C[] } {
  const words = foldText(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return { bots: [], chats: [] };
  const nameOf = new Map(bots.map((b) => [b.id, b.name]));
  const rank = (c: C) => (c.pinned ? 0 : c.archived ? 2 : 1);
  const chatHits = chats
    .map((c, i) => ({ c, i }))
    .filter(({ c }) => {
      const title = foldText(c.title.trim());
      return title !== "" && words.some((w) => title.includes(w)) && matchesQuery(query, c.title, nameOf.get(c.bot_id));
    })
    .sort((a, b) => rank(a.c) - rank(b.c) || a.i - b.i)
    .slice(0, QUICK_CHATS_MAX)
    .map(({ c }) => c);
  return {
    bots: bots.filter((b) => matchesQuery(query, b.name)).slice(0, QUICK_BOTS_MAX),
    chats: chatHits
  };
}
