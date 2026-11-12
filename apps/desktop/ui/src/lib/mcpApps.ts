import type { ApiClient } from "../api/client.ts";
import type {
  Bot,
  BotInput,
  McpCatalogEntry,
  McpServer,
  McpServerConfig,
  McpServerPatch,
  McpStatus
} from "../api/types.ts";
import { matchesQuery } from "./searchText.ts";

/** The slice of the daemon client the Apps modal needs (a mock fits too). */
export type McpApi = Pick<
  ApiClient,
  | "listMcpServers"
  | "createMcpServer"
  | "updateMcpServer"
  | "deleteMcpServer"
  | "reconnectMcpServer"
  | "listMcpTools"
  | "mcpCatalog"
  | "setSecret"
>;

export const ALL_CATEGORIES = "all";

/** Distinct catalog categories, sorted. */
export function catalogCategories(catalog: McpCatalogEntry[]): string[] {
  return [...new Set(catalog.map((c) => c.category))].sort();
}

/** Catalog entries matching the search box and the category chip. */
export function filterCatalog(
  catalog: McpCatalogEntry[],
  query: string,
  category: string
): McpCatalogEntry[] {
  return catalog.filter(
    (c) =>
      (category === ALL_CATEGORIES || c.category === category) &&
      matchesQuery(query, c.name, c.description, c.category, c.id)
  );
}

/** Connected servers matching the search box. */
export function filterServers(
  servers: McpServer[],
  query: string
): McpServer[] {
  return servers.filter((s) => matchesQuery(query, s.name, s.category));
}

/** A catalog entry counts as connected when a server is named like its id. */
export function isConnected(
  entry: McpCatalogEntry,
  servers: McpServer[]
): boolean {
  return servers.some((s) => s.name === entry.id);
}

/** Name under which the secret of a catalog entry is stored. */
export function secretStoreName(entryId: string, secretName: string): string {
  return `mcp_${entryId}_${secretName}`
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_");
}

/**
 * Server config for a catalog entry.  Secrets are referenced by NAME: a stdio
 * server receives them as environment variables, an http server as the
 * `Authorization` header (the secret holds the full header value).
 */
export function configFromCatalog(entry: McpCatalogEntry): McpServerConfig {
  const { install } = entry;
  const config: McpServerConfig = {
    name: entry.id,
    transport: install.transport,
    category: entry.category,
    enabled: true
  };
  if (install.transport === "stdio") {
    config.command = install.command;
    config.args = install.args ?? [];
  } else {
    config.url = install.url;
  }
  if (install.required_secrets.length > 0) {
    if (install.transport === "stdio") {
      config.secret_env = Object.fromEntries(
        install.required_secrets.map((s) => [
          s.name,
          secretStoreName(entry.id, s.name)
        ])
      );
    } else {
      config.secret_headers = {
        Authorization: secretStoreName(
          entry.id,
          install.required_secrets[0].name
        )
      };
    }
  }
  return config;
}

export class MissingSecretError extends Error {
  readonly secrets: string[];
  constructor(secrets: string[]) {
    super(`missing: ${secrets.join(", ")}`);
    this.name = "MissingSecretError";
    this.secrets = secrets;
  }
}

/**
 * One-click connect: store each required secret (write-only), then register the
 * server.  `values` maps the required secret name to what the user typed.
 */
export async function connectCatalogEntry(
  api: Pick<McpApi, "setSecret" | "createMcpServer">,
  entry: McpCatalogEntry,
  values: Record<string, string>
): Promise<McpServer> {
  const missing = entry.install.required_secrets
    .filter((s) => !(values[s.name] ?? "").trim())
    .map((s) => s.name);
  if (missing.length > 0) {
    throw new MissingSecretError(missing);
  }
  for (const s of entry.install.required_secrets) {
    await api.setSecret(
      secretStoreName(entry.id, s.name),
      values[s.name].trim()
    );
  }
  return api.createMcpServer(configFromCatalog(entry));
}

/** Split a command line into arguments, honouring single and double quotes. */
export function splitArgs(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: string | null = null;
  let has = false;
  for (const ch of line) {
    if (quote) {
      if (ch === quote) {
        quote = null;
      } else {
        cur += ch;
      }
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      has = true;
    } else if (/\s/.test(ch)) {
      if (has || cur) {
        out.push(cur);
        cur = "";
        has = false;
      }
    } else {
      cur += ch;
    }
  }
  if (has || cur) {
    out.push(cur);
  }
  return out;
}

export interface CustomServerForm {
  name: string;
  transport: "stdio" | "http";
  command: string;
  url: string;
  /** `KEY=value` per line; these values are not secrets. */
  env: string;
}

export type CustomServerResult =
  | { ok: true; config: McpServerConfig }
  | { ok: false; error: "name" | "command" | "url" };

const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,47}$/i;

/** Validate the advanced "add MCP server" form and build the config. */
export function customServerConfig(
  form: CustomServerForm,
  existing: string[] = [],
  scope: BotScope = { mode: "all" }
): CustomServerResult {
  const name = form.name.trim();
  if (!NAME_RE.test(name) || existing.includes(name)) {
    return { ok: false, error: "name" };
  }
  const config: McpServerConfig = {
    name,
    transport: form.transport,
    category: "other",
    enabled: true
  };
  // `bots` is written only when narrowed; omitted = every bot.
  const bots = botsFromScope(scope);
  if (bots !== null) {
    config.bots = bots;
  }
  if (form.transport === "stdio") {
    const parts = splitArgs(form.command.trim());
    if (parts.length === 0) {
      return { ok: false, error: "command" };
    }
    config.command = parts[0];
    config.args = parts.slice(1);
    const env: Record<string, string> = {};
    for (const line of form.env.split(/\r?\n/)) {
      const i = line.indexOf("=");
      if (i > 0) {
        env[line.slice(0, i).trim()] = line.slice(i + 1).trim();
      }
    }
    if (Object.keys(env).length > 0) {
      config.env = env;
    }
  } else {
    const url = form.url.trim();
    if (!/^https?:\/\/\S+$/i.test(url)) {
      return { ok: false, error: "url" };
    }
    config.url = url;
  }
  return { ok: true, config };
}

export type StatusTone = "ok" | "busy" | "bad" | "muted";

export function statusTone(status: McpStatus): StatusTone {
  switch (status) {
    case "connected":
      return "ok";
    case "connecting":
      return "busy";
    case "error":
    case "quarantined":
      return "bad";
    case "disconnected":
      return "muted";
  }
}

/**
 * Who may use a server, as the editor sees it.  `all` is `bots: null`;
 * `some` is an explicit list of bot storage UUIDs (what the daemon compares
 * against `ctx.bot_id`), and an empty list means NO bot, never "all".
 */
export type BotScope = { mode: "all" } | { mode: "some"; ids: string[] };

export function botScope(server: Pick<McpServer, "bots">): BotScope {
  return server.bots === null || server.bots === undefined
    ? { mode: "all" }
    : { mode: "some", ids: [...server.bots] };
}

/** The `bots` value to send: `null` = all bots, otherwise the (maybe empty) list. */
export function botsFromScope(scope: BotScope): string[] | null {
  return scope.mode === "all" ? null : [...scope.ids];
}

/**
 * Tick / untick one bot.  Ticking while on "all bots" narrows to just that
 * bot; unticking the last bot leaves an explicit empty list.
 */
export function toggleBotInScope(
  scope: BotScope,
  botId: string,
  on: boolean
): BotScope {
  const ids = scope.mode === "all" ? [] : scope.ids;
  if (on) {
    return { mode: "some", ids: ids.includes(botId) ? ids : [...ids, botId] };
  }
  return { mode: "some", ids: ids.filter((id) => id !== botId) };
}

/** Drop ids of bots that no longer exist; this never turns a list into "all". */
export function botSelection(
  scope: BotScope,
  bots: Pick<Bot, "id">[]
): BotScope {
  return scope.mode === "all"
    ? scope
    : { mode: "some", ids: scope.ids.filter((id) => bots.some((b) => b.id === id)) };
}

/**
 * Whether a bot may use a server.  `botId` is `undefined` for a bot that is
 * not created yet: it only gets the servers open to every bot.
 */
export function botHasAccess(
  server: Pick<McpServer, "bots">,
  botId: string | undefined
): boolean {
  if (server.bots === null || server.bots === undefined) {
    return true;
  }
  return botId !== undefined && server.bots.includes(botId);
}

/**
 * The patch that applies one tick of the bot form's Apps checklist, or `null`
 * when nothing needs to change.  On a list it is an atomic add / remove of this
 * one id (the daemon applies it under its lock).  Unticking on an open server
 * narrows it to every other bot, an explicit list.
 */
export function appsPatch(
  server: Pick<McpServer, "bots">,
  botId: string,
  on: boolean,
  allBotIds: string[]
): McpServerPatch | null {
  if (server.bots === null || server.bots === undefined) {
    return on ? null : { bots: allBotIds.filter((id) => id !== botId) };
  }
  return on ? { bots_add: [botId] } : { bots_remove: [botId] };
}

/**
 * The patch that turns the scope the editor opened with into the one the user
 * chose.  List to list is an add / remove diff (applied atomically by the
 * daemon, so another edit made meanwhile is not overwritten); switching from
 * or to "all bots" has to be absolute.
 */
export function scopePatch(from: BotScope, to: BotScope): McpServerPatch {
  if (to.mode === "all") {
    return { bots: null };
  }
  if (from.mode === "all") {
    return { bots: [...to.ids] };
  }
  return {
    bots_add: to.ids.filter((id) => !from.ids.includes(id)),
    bots_remove: from.ids.filter((id) => !to.ids.includes(id))
  };
}

/** Servers whose tick differs from the snapshot taken when the form opened. */
export function changedApps(
  initial: Record<string, boolean>,
  allowed: Record<string, boolean>
): string[] {
  return Object.keys(allowed).filter((name) => allowed[name] !== initial[name]);
}

/** Result of writing the bot form's ticks. */
export interface AppsResult {
  /** Servers whose update failed (request error, or the list could not be read). */
  failed: string[];
  /** Servers whose update was accepted but did not take effect (e.g. now open to all bots). */
  notApplied: string[];
}

/** Whether `result` gives (`on`) or denies (`!on`) the bot access, as intended. */
export function appTickApplied(
  result: Pick<McpServer, "bots">,
  botId: string,
  on: boolean
): boolean {
  return botHasAccess(result, botId) === on;
}

/**
 * Apply only the ticks the user changed in this form session, each against the
 * server's current state, and check every response against the intent.  Never
 * throws: a failing list call or update is reported in `failed`.
 */
export async function applyAppsSelection(
  api: Pick<McpApi, "listMcpServers" | "updateMcpServer">,
  botId: string,
  initial: Record<string, boolean>,
  allowed: Record<string, boolean>,
  allBotIds: string[]
): Promise<AppsResult> {
  const result: AppsResult = { failed: [], notApplied: [] };
  const changed = changedApps(initial, allowed);
  if (changed.length === 0) {
    return result;
  }
  let fresh: McpServer[];
  try {
    fresh = await api.listMcpServers();
  } catch {
    result.failed = changed;
    return result;
  }
  for (const name of changed) {
    const server = fresh.find((s) => s.name === name);
    if (!server) {
      continue;
    }
    const patch = appsPatch(server, botId, allowed[name], allBotIds);
    if (patch === null) {
      continue;
    }
    try {
      const updated = await api.updateMcpServer(name, patch);
      if (!appTickApplied(updated, botId, allowed[name])) {
        result.notApplied.push(name);
      }
    } catch {
      result.failed.push(name);
    }
  }
  return result;
}

/**
 * Whether the server's resulting `bots` matches what the editor meant: all
 * bots, or a list that has every ticked bot and none of the unticked ones.
 */
export function scopeApplied(
  from: BotScope,
  to: BotScope,
  result: Pick<McpServer, "bots">
): boolean {
  const now = botScope(result);
  if (to.mode === "all") {
    return now.mode === "all";
  }
  if (now.mode === "all") {
    return false;
  }
  const removed = from.mode === "some" ? from.ids.filter((id) => !to.ids.includes(id)) : [];
  return to.ids.every((id) => now.ids.includes(id)) && removed.every((id) => !now.ids.includes(id));
}

/**
 * Save the server editor: the diff against the scope it OPENED with, then
 * check the response.  `applied: false` means the server is not in the state
 * the user chose (for example it was set to all bots meanwhile).
 */
export async function saveServerScope(
  api: Pick<McpApi, "updateMcpServer">,
  name: string,
  from: BotScope,
  to: BotScope
): Promise<{ applied: boolean; server: McpServer }> {
  const server = await api.updateMcpServer(name, scopePatch(from, to));
  return { applied: scopeApplied(from, to, server), server };
}

/**
 * Save the bot form: create or update the bot, then its app ticks.  The bot
 * the form holds afterwards (`bot`) is the one a retry must update, so a
 * failure in the apps step can never lead to a second created bot.
 */
export async function saveBotAndApps(
  api: Pick<ApiClient, "createBot" | "updateBot" | "listMcpServers" | "updateMcpServer">,
  target: Bot | undefined,
  input: BotInput,
  initial: Record<string, boolean>,
  allowed: Record<string, boolean>,
  allBotIds: string[]
): Promise<{ bot: Bot } & AppsResult> {
  const bot = target
    ? await api.updateBot(target.id, input)
    : await api.createBot(input);
  const apps = await applyAppsSelection(api, bot.id, initial, allowed, allBotIds);
  return { bot, ...apps };
}

/** Like `botAccess`, but ids of bots that no longer exist read `deleted`. */
export function botAccessLabels(
  server: Pick<McpServer, "bots">,
  bots: Pick<Bot, "id" | "name">[],
  deleted: string
): { all: true } | { all: false; names: string[] } {
  if (server.bots === null || server.bots === undefined) {
    return { all: true };
  }
  return {
    all: false,
    names: server.bots.map((id) => bots.find((b) => b.id === id)?.name ?? deleted)
  };
}

/** Which bots may use a server: everyone, or the named bots. */
export function botAccess(
  server: Pick<McpServer, "bots">,
  bots: Pick<Bot, "id" | "name">[]
): { all: true } | { all: false; names: string[] } {
  if (server.bots === null || server.bots === undefined) {
    return { all: true };
  }
  return {
    all: false,
    names: server.bots.map((id) => bots.find((b) => b.id === id)?.name ?? id)
  };
}

/**
 * Apply an `mcp_server_changed` event to the list.  `null` = the server is
 * unknown here, so the list must be refetched.
 */
export function applyServerEvent(
  servers: McpServer[],
  ev: { name: string; status: McpStatus }
): McpServer[] | null {
  if (!servers.some((s) => s.name === ev.name)) {
    return null;
  }
  return servers.map((s) =>
    s.name === ev.name
      ? { ...s, status: ev.status, error: ev.status === "error" ? s.error : null }
      : s
  );
}
