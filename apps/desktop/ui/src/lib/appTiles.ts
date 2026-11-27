import type { McpCatalogEntry, McpServer } from "../api/types";
import { matchesQuery } from "./searchText.ts";

/** Chip groups of the Apps grid, in display order (also the daemon's group ids). */
export const GROUPS = ["communication", "social", "productivity", "dev", "finance", "business"] as const;
export type Group = (typeof GROUPS)[number] | "other";

/** What a tile shows and offers. */
export type TileState = "idle" | "linking" | "waiting" | "connected" | "failed" | "expired";

/** The single button a tile in `state` offers (`null` = none, the tile is busy or connected). */
export function tileAction(state: TileState): "connect" | "retry" | "reconnect" | null {
  switch (state) {
    case "idle":
      return "connect";
    case "failed":
      return "retry";
    case "expired":
      return "reconnect";
    default:
      return null;
  }
}

/** Chip group of the remote apps Praxi connects directly (OAuth), by catalog id. */
const DIRECT_GROUP: Record<string, Group> = {
  linear: "dev",
  "notion-remote": "productivity",
  stripe: "finance",
  atlassian: "dev"
};

/** A remote app Praxi signs in to itself: a remote server in the MCP catalog that needs no secret. */
export function directEntries(catalog: readonly McpCatalogEntry[], servers: readonly McpServer[]) {
  const have = new Set(servers.map((s) => s.name));
  return catalog.filter(
    (e) => e.install.transport === "http" && e.install.required_secrets.length === 0 && !have.has(e.id)
  );
}

export function directGroup(id: string): Group {
  return DIRECT_GROUP[id] ?? "other";
}

/** Whether a server in the connected list is a remote app (shown to everyone) rather than a local one (Advanced). */
export function isRemoteServer(server: McpServer): boolean {
  return server.transport === "http";
}

/** Direct entries matching the search text and chip group. */
export function filterDirect(
  entries: readonly McpCatalogEntry[],
  query: string,
  group: string
): McpCatalogEntry[] {
  return entries.filter(
    (e) => (group === "all" || directGroup(e.id) === group) && matchesQuery(query, e.name, e.description)
  );
}

/** How long a pending sign-in is waited for before the tile says it expired (minutes). */
export const SIGN_IN_WAIT_MINUTES = 10;
