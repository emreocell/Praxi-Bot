/**
 * "Search all chats" from a right-click menu: the explicit all-chats search lives in the bot panel,
 * which may not be on screen yet.  The request waits here until the panel takes it, so the order
 * of "open panel" and "run search" never matters.
 */
export const SEARCH_ALL_CHATS_EVENT = "praxi:search-all-chats";

export interface SearchRequest {
  query: string;
  /** Distinguishes two requests with the same text. */
  n: number;
}

let pending: SearchRequest | null = null;
let counter = 0;

/** One line, at most 200 characters (the search box limit). */
export function searchPhrase(text: string): string {
  return text.replace(/\s+/g, " ").trim().slice(0, 200);
}

export function requestSearchAllChats(text: string): void {
  const query = searchPhrase(text);
  if (!query || typeof window === "undefined") return;
  pending = { query, n: ++counter };
  window.dispatchEvent(new CustomEvent(SEARCH_ALL_CHATS_EVENT));
}

/** The waiting request, once. */
export function takeSearchRequest(): SearchRequest | null {
  const r = pending;
  pending = null;
  return r;
}
