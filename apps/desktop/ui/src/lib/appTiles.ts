import type { ComposioApp, ComposioToolkit, McpCatalogEntry, McpServer } from "../api/types";
import { matchesQuery } from "./searchText.ts";

/** Chip groups of the Apps grid, in display order (also the daemon's group ids). */
export const GROUPS = ["communication", "social", "productivity", "dev", "finance", "business"] as const;
export type Group = (typeof GROUPS)[number] | "other";

/** What a tile shows and offers. */
export type TileState = "idle" | "linking" | "waiting" | "connected" | "failed" | "expired";

/** The state of a Composio-backed tile from the daemon's app list and the local click. */
export function composioTileState(app: ComposioApp | undefined, linking: boolean): TileState {
  switch (app?.state) {
    case "active":
      return "connected";
    case "pending":
      return "waiting";
    case "failed":
      return "failed";
    case "expired":
      return "expired";
    default:
      return linking ? "linking" : "idle";
  }
}

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

function norm(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]/g, "");
}

/**
 * Composio toolkits without the ones Praxi connects directly (same product,
 * one tile) and without the ones already in the connected list.
 */
export function visibleToolkits(
  items: readonly ComposioToolkit[],
  apps: readonly ComposioApp[],
  direct: readonly McpCatalogEntry[],
  connectedServers: readonly McpServer[]
): ComposioToolkit[] {
  const mine = new Set(apps.map((a) => a.slug));
  const names = new Set([
    ...direct.map((d) => norm(d.name)),
    ...connectedServers.filter(isRemoteServer).map((s) => norm(s.name))
  ]);
  return items.filter((t) => !mine.has(t.slug) && !names.has(norm(t.name)));
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

/**
 * Composio apps that belong under "Connected": signed in (`active`) or once signed in and now
 * needing a fresh sign-in (`expired`). An unfinished connection (`pending`, `failed`) never
 * does: it is a tile in the grid that says what is going on and can be cancelled or retried.
 */
export function settledApps(apps: readonly ComposioApp[]): ComposioApp[] {
  return apps.filter((a) => a.state === "active" || a.state === "expired");
}

/** The apps whose connection is not finished, shown as tiles in the grid. */
export function unsettledApps(apps: readonly ComposioApp[]): ComposioApp[] {
  return apps.filter((a) => a.state === "pending" || a.state === "failed");
}

/** How long a pending sign-in is waited for before the tile says it expired (minutes). */
export const SIGN_IN_WAIT_MINUTES = 10;
