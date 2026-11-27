import type { McpServer } from "../api/types.ts";

/**
 * The bot editor groups the connected apps by what they let the bot do (send mail, change
 * files, work on code ...), in this order; an unknown or missing category goes to "other".
 */
export const APP_GROUPS = [
  "communication",
  "productivity",
  "files",
  "data",
  "developer",
  "browser",
  "search",
  "home",
  "other"
] as const;

export type AppGroup = (typeof APP_GROUPS)[number];

export function appGroupOf(category: string | null | undefined): AppGroup {
  return APP_GROUPS.find((g) => g === category) ?? "other";
}

/** Non-empty groups in display order; apps keep their order inside a group. */
export function appGroups<S extends Pick<McpServer, "name" | "category">>(
  servers: readonly S[]
): Array<{ group: AppGroup; servers: S[] }> {
  return APP_GROUPS.map((group) => ({
    group,
    servers: servers.filter((s) => appGroupOf(s.category) === group)
  })).filter((g) => g.servers.length > 0);
}
