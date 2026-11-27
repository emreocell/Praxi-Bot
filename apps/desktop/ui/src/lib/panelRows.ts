import { matchesQuery } from "./searchText.ts";

/** What a one-line list shows for a search text and a "Show all" state. */
export interface RowWindow<T> {
  /** Rows to draw. */
  shown: T[];
  /** Rows that match but are folded away behind "Show all". */
  hidden: number;
  /** Rows that match the search (all of them while there is no search). */
  found: number;
}

/**
 * A search looks through the whole list and shows every match; without one, only the first `limit`
 * rows show until the list is expanded.
 */
export function rowWindow<T extends { name: string; meta?: string }>(
  rows: readonly T[],
  { query, expanded, limit }: { query: string; expanded: boolean; limit: number }
): RowWindow<T> {
  const searching = query.trim().length > 0;
  const matching = searching ? rows.filter((r) => matchesQuery(query, r.name, r.meta)) : [...rows];
  const shown = searching || expanded ? matching : matching.slice(0, limit);
  return { shown, hidden: matching.length - shown.length, found: matching.length };
}

/** From this many rows on, a list gets its search field. */
export function needsSearch(count: number, searchFrom: number): boolean {
  return count >= searchFrom;
}
