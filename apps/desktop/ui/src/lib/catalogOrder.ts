import type { ComposioToolkit } from "../api/types";
import { GROUPS, type Group } from "./appTiles.ts";

/**
 * The apps most people look for first, in that order: the "Popular" row of the Apps page.
 * It is the head of the daemon's pinned list (`crates/composio/src/curated.json`, same order;
 * a test keeps the two in step), so the offline list and search ties follow it too.
 */
export const POPULAR_APPS = [
  "gmail",
  "googlecalendar",
  "googledrive",
  "outlook",
  "slack",
  "whatsapp",
  "telegram",
  "notion",
  "github",
  "googlesheets",
  "googledocs",
  "microsoft_teams"
] as const;

/** How many catalog apps the first page asks for: the popular row plus a few of every category. */
export const CATALOG_PAGE_SIZE = 48;

export type CatalogSectionId = "popular" | Group;

export interface CatalogSection {
  id: CatalogSectionId;
  items: ComposioToolkit[];
}

const CATEGORY_ORDER: readonly Group[] = [...GROUPS, "other"];

function groupOf(item: ComposioToolkit): Group {
  return (CATEGORY_ORDER as readonly string[]).includes(item.group) ? (item.group as Group) : "other";
}

/**
 * The unfiltered catalog as the Apps page shows it: "Popular" first (in `POPULAR_APPS` order,
 * whatever order the page came in), then one section per category in the chip order, each
 * keeping the daemon's popularity order. Empty sections are left out.
 */
export function catalogSections(items: readonly ComposioToolkit[]): CatalogSection[] {
  const rank = new Map<string, number>(POPULAR_APPS.map((slug, i) => [slug, i]));
  const popular = items
    .filter((item) => rank.has(item.slug))
    .sort((a, b) => (rank.get(a.slug) ?? 0) - (rank.get(b.slug) ?? 0));
  const rest = items.filter((item) => !rank.has(item.slug));
  const sections: CatalogSection[] = [{ id: "popular", items: popular }];
  for (const group of CATEGORY_ORDER) {
    sections.push({ id: group, items: rest.filter((item) => groupOf(item) === group) });
  }
  return sections.filter((s) => s.items.length > 0);
}
