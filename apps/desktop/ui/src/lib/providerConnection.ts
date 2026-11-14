import type { Connection } from "../api/connection.ts";

export type ProviderConnection = Pick<Connection, "base_url" | "started_by_app">;

export interface ProviderManagementTarget {
  connection: ProviderConnection | null;
  baseUrl: string | undefined;
}

export const PROVIDERS_LOCAL_ONLY = "providers_require_app_daemon";

/** The native provider CLI writes locally; attached URLs may be remote proxies. */
export function canManageProviders({ connection, baseUrl }: ProviderManagementTarget): boolean {
  if (!connection?.started_by_app || !baseUrl) return false;
  try {
    const url = new URL(connection.base_url);
    return url.protocol === "http:" &&
      ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) &&
      connection.base_url.replace(/\/$/, "") === baseUrl.replace(/\/$/, "");
  } catch {
    return false;
  }
}
