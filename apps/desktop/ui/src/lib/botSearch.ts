import { matchesQuery } from "./searchText.ts";

/**
 * Bots whose name matches the sidebar search box. Case, accents and the Turkish dotted and dotless
 * i are ignored, the query is trimmed, and every word of it must occur in the name. A blank or
 * missing query keeps all bots.
 */
export function filterBots<T extends { name: string }>(bots: readonly T[], query: string | null | undefined): T[] {
  const text = query ?? "";
  return bots.filter((bot) => matchesQuery(text, bot.name));
}
