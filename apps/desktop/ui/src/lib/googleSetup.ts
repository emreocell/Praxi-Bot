/**
 * Google Workspace through Google's own MCP servers (Gmail, Drive, Calendar), with the owner's own
 * Google client.
 *
 * Praxi ships no Google client: a shared one would need Google's app verification, and every user
 * would see the "unverified app" warning. So the owner creates one in their own Google Cloud project
 * (a few minutes, this file lists the exact pages), pastes its id and secret once, and signs in. The
 * secret goes to the secret store and is never shown again; the id is not a secret (it is in the
 * address of the sign-in page) and is kept with each Google app's settings.
 *
 * Only read-only access is asked for. Changing things (sending mail, editing files) is a separate,
 * explicit step that is not offered here.
 */

import type { McpCatalogEntry, McpServer, McpServerConfig } from "../api/types.ts";
import type { McpApi } from "./mcpApps.ts";
import { secretStoreName } from "./mcpApps.ts";

/** The setup id the catalog gives the Google entries. */
export const GOOGLE_SETUP = "google";
export const GOOGLE_ISSUER = "https://accounts.google.com";
/** One secret for all Google apps: they share the client. */
export const GOOGLE_SECRET_NAME = secretStoreName("google", "client_secret");

/** The catalog entries that are signed in with the owner's own Google client. */
export function googleEntries(catalog: readonly McpCatalogEntry[]): McpCatalogEntry[] {
  return catalog.filter((e) => e.install.oauth_client?.setup === GOOGLE_SETUP);
}

/** A server that belongs to one of these entries (same name AND an http server: not an old stdio one). */
export function googleServerOf(entry: McpCatalogEntry, servers: readonly McpServer[]): McpServer | undefined {
  return servers.find((s) => s.name === entry.id && s.transport === "http");
}

/** The client id already in use by a Google app, so the next one needs no typing. */
export function googleClientIdOf(servers: readonly McpServer[]): string | null {
  for (const s of servers) {
    const o = s.oauth;
    if (s.transport === "http" && o?.client_id && o.issuer?.replace(/\/$/, "") === GOOGLE_ISSUER) {
      return o.client_id;
    }
  }
  return null;
}

const CLIENT_ID = /^\d{6,}-[a-z0-9_-]+\.apps\.googleusercontent\.com$/i;

export type GoogleFieldError = "client_id" | "client_secret";
export type GoogleSetupProblem = GoogleFieldError | "missing" | "legacy";

/** What is wrong with the pasted client, if anything (never echoes the values). */
export function checkGoogleClient(clientId: string, clientSecret: string): GoogleFieldError | null {
  if (!CLIENT_ID.test(clientId.trim())) return "client_id";
  const secret = clientSecret.trim();
  if (secret.length < 8 || /\s/.test(secret)) return "client_secret";
  return null;
}

export class GoogleClientError extends Error {
  readonly field: GoogleSetupProblem;
  constructor(field: GoogleSetupProblem) {
    super(`google client: ${field}`);
    this.name = "GoogleClientError";
    this.field = field;
  }
}

/** The server settings for a Google app: the read-only scopes, the shared client, no secret value. */
export function googleServerConfig(entry: McpCatalogEntry, clientId: string): McpServerConfig {
  const client = entry.install.oauth_client;
  if (!client || entry.install.transport !== "http" || !entry.install.url) {
    throw new Error(`catalog entry ${entry.id} is not a bring-your-own-client app`);
  }
  return {
    name: entry.id,
    transport: "http",
    url: entry.install.url,
    category: entry.category,
    enabled: true,
    // Read tools only: the rest stays hidden until the owner chooses otherwise.
    tool_allow: entry.install.tool_allow ?? [],
    oauth: {
      client_id: clientId.trim(),
      client_secret_ref: GOOGLE_SECRET_NAME,
      issuer: client.issuer,
      scopes: client.scopes,
      offline_access: client.offline_access
    }
  };
}

/** The oauth settings of `entry` for `clientId` (also what re-keys an app to another client). */
function oauthOf(entry: McpCatalogEntry, clientId: string) {
  return googleServerConfig(entry, clientId).oauth;
}

/**
 * Add a Google app: store the secret (write-only) when a new client was typed, then register the
 * server. The sign-in is the caller's next step. With no client typed, the one a Google app already
 * uses is reused; with neither, `GoogleClientError("missing")`. A typed client replaces the shared
 * secret, so every Google app already added is pointed at the new client id too. An app left over
 * from an earlier, half-finished try is updated instead of duplicated; an old archived stdio server
 * that carries the same name is never touched (`legacy`: the owner removes it first).
 */
export async function addGoogleApp(
  api: Pick<McpApi, "setSecret" | "createMcpServer" | "updateMcpServer">,
  entry: McpCatalogEntry,
  typed: { clientId: string; clientSecret: string } | null,
  servers: readonly McpServer[],
  all: readonly McpCatalogEntry[] = [entry]
): Promise<McpServer> {
  if (servers.some((s) => s.name === entry.id && s.transport !== "http")) {
    throw new GoogleClientError("legacy");
  }
  let clientId: string;
  if (typed) {
    const bad = checkGoogleClient(typed.clientId, typed.clientSecret);
    if (bad) throw new GoogleClientError(bad);
    clientId = typed.clientId.trim();
    await api.setSecret(GOOGLE_SECRET_NAME, typed.clientSecret.trim());
    // The secret is shared: the apps added earlier follow the new client.
    for (const s of servers) {
      const o = s.oauth;
      if (s.transport === "http" && o?.issuer?.replace(/\/$/, "") === GOOGLE_ISSUER && o.client_id !== clientId) {
        const other = all.find((e) => e.id === s.name);
        if (other) await api.updateMcpServer(s.name, { oauth: oauthOf(other, clientId) });
      }
    }
  } else {
    const known = googleClientIdOf(servers);
    if (!known) throw new GoogleClientError("missing");
    clientId = known;
  }
  try {
    return await api.createMcpServer(googleServerConfig(entry, clientId));
  } catch (e) {
    if ((e as { status?: unknown } | null)?.status !== 409) throw e;
    return api.updateMcpServer(entry.id, { oauth: oauthOf(entry, clientId) });
  }
}

/** One page of the setup guide. `links` are the exact pages of the Google Cloud console. */
export interface GoogleStep {
  id: "project" | "apis" | "branding" | "audience" | "scopes" | "client";
  links: { label: string; href: string }[];
}

const CONSOLE = "https://console.cloud.google.com";

/** Whether `href` is a page of the Google Cloud console: the only pages the setup guide opens. */
export function isGoogleConsoleLink(href: string): boolean {
  try {
    const url = new URL(href);
    return url.protocol === "https:" && url.host === "console.cloud.google.com" && !url.username && !url.password;
  } catch {
    return false;
  }
}
const LIBRARY = `${CONSOLE}/apis/library`;

/** The services to switch on for the apps the owner wants (each app also has its own MCP service). */
export function googleServices(entries: readonly McpCatalogEntry[]): { id: string; label: string; href: string }[] {
  const api: Record<string, string[]> = {
    "google-gmail": ["gmail.googleapis.com", "gmailmcp.googleapis.com"],
    "google-drive": ["drive.googleapis.com", "drivemcp.googleapis.com"],
    "google-calendar": ["calendar-json.googleapis.com", "calendarmcp.googleapis.com"]
  };
  return entries.flatMap((e) =>
    (api[e.id] ?? []).map((service) => ({ id: service, label: service, href: `${LIBRARY}/${service}` }))
  );
}

export const GOOGLE_STEPS: readonly GoogleStep[] = [
  { id: "project", links: [{ label: "console.cloud.google.com/projectcreate", href: `${CONSOLE}/projectcreate` }] },
  { id: "apis", links: [] },
  { id: "branding", links: [{ label: "Google Auth Platform → Branding", href: `${CONSOLE}/auth/branding` }] },
  { id: "audience", links: [{ label: "Google Auth Platform → Audience", href: `${CONSOLE}/auth/audience` }] },
  { id: "scopes", links: [{ label: "Google Auth Platform → Data Access", href: `${CONSOLE}/auth/scopes` }] },
  { id: "client", links: [{ label: "Google Auth Platform → Clients", href: `${CONSOLE}/auth/clients/create` }] }
];

/** The read-only scopes to add under "Data Access", across the chosen apps, without repeats. */
export function googleScopes(entries: readonly McpCatalogEntry[]): string[] {
  return [...new Set(entries.flatMap((e) => e.install.oauth_client?.scopes ?? []))];
}

export type TestFailure =
  | "admin_needed"
  | "not_connected"
  | "no_test"
  | "signed_out"
  | "permission"
  | "rate"
  | "other";

/** Why the Test button failed, from the daemon's code and fixed sentences. */
export function testFailure(error: unknown): TestFailure {
  const code = (error as { code?: unknown } | null)?.code;
  if (code === "host_admin_required") return "admin_needed";
  const text = (error instanceof Error ? error.message : String(error ?? "")).toLowerCase();
  if (text.includes("not connected")) return "not_connected";
  if (text.includes("has no test")) return "no_test";
  if (text.includes("authorization is required") || text.includes("401") || text.includes("unauthenticated")) {
    return "signed_out";
  }
  if (text.includes("403") || text.includes("permission") || text.includes("insufficient") || text.includes("access not configured")) {
    return "permission";
  }
  if (/\b429\b|rate limit|too many|quota/.test(text)) return "rate";
  return "other";
}
