import type { McpServerConfig } from "../api/types";
import { type BotScope, customServerConfig } from "./mcpApps.ts";

/**
 * The "Add your own MCP server" form, as data. A server URL is the main path (with an optional
 * sign-in header or a browser sign-in); a local command is the secondary one. Nothing here talks
 * to the daemon: `validateDraft` drives the inline messages and `draftPlan` says what to store.
 */
export type McpAuthMode = "none" | "header" | "oauth";

export interface CustomMcpDraft {
  mode: "http" | "stdio";
  url: string;
  auth: McpAuthMode;
  /** The request header that carries the access key (sent on every request). */
  headerName: string;
  /** Its value: written to the secret store, never to the server settings. */
  headerValue: string;
  name: string;
  command: string;
  /** `KEY=value` per line. Values of keys that look secret go to the secret store. */
  env: string;
}

export const DEFAULT_HEADER = "Authorization";

export function emptyDraft(): CustomMcpDraft {
  return { mode: "http", url: "", auth: "none", headerName: DEFAULT_HEADER, headerValue: "", name: "", command: "", env: "" };
}

export type DraftField = "url" | "name" | "headerName" | "headerValue" | "command" | "env";

/** Why a field is not usable yet; each has its own sentence in `apps.mcpForm.err.*`. */
export type DraftError =
  | "urlMissing"
  | "urlInvalid"
  | "urlCredentials"
  | "nameMissing"
  | "nameFormat"
  | "nameTaken"
  | "headerName"
  | "headerValue"
  | "commandMissing"
  | "envLine";

/** Accepted but worth a word (`apps.mcpForm.warn.*`). */
export type DraftWarning = "urlPlain" | "urlKey";

export interface DraftCheck {
  errors: Partial<Record<DraftField, DraftError>>;
  warnings: Partial<Record<DraftField, DraftWarning>>;
}

const NAME_RE = /^[a-z0-9][a-z0-9_-]{0,47}$/i;
/** An HTTP header name (RFC 9110 token), at most 64 characters. */
const HEADER_RE = /^[A-Za-z0-9!#$%&'*+.^_`|~-]{1,64}$/;
const ENV_KEY_RE = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;
/** Variable names whose value is a key or password: stored as a secret, not in the settings file. */
const SECRET_KEY_RE = /key|token|secret|passw|pwd|auth|credential|private|(?:^|_)pat(?:_|$)/i;
/** A value that carries its own password (`postgres://user:pw@host/db`). */
const SECRET_VALUE_RE = /^[a-z][a-z0-9+.-]*:\/\/[^/@\s:]+:[^/@\s]+@/i;
/** Query parameters that usually hold a key: saved in plain text when left in the address. */
const URL_KEY_PARAM_RE = /key|token|secret|passw|auth|sig/i;
const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]", "::1"]);

function parseUrl(text: string): URL | null {
  try {
    const url = new URL(text.trim());
    return url.protocol === "https:" || url.protocol === "http:" ? url : null;
  } catch {
    return null;
  }
}

function isLocalHost(host: string): boolean {
  const h = host.toLowerCase();
  return LOCAL_HOSTS.has(h) || h.startsWith("127.") || h.endsWith(".localhost");
}

/**
 * Whether `name` is in use. Case does not count: secret names are derived from it, and the
 * Windows credential store treats `Linear` and `linear` as one.
 */
export function nameTaken(name: string, existing: readonly string[]): boolean {
  const lower = name.toLowerCase();
  return existing.some((n) => n.toLowerCase() === lower);
}

/** The address as the daemon wants it: trimmed, with a lower-case scheme. */
export function normalUrl(text: string): string {
  return text.trim().replace(/^https?:/i, (scheme) => scheme.toLowerCase());
}

/**
 * A server name from its address: `https://mcp.notion.com/mcp` -> `notion`,
 * `http://localhost:8931/mcp` -> `localhost-8931`. Empty when the address does not parse.
 */
export function suggestName(urlText: string, existing: readonly string[] = []): string {
  const url = parseUrl(urlText);
  if (!url) return "";
  const host = url.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  let base: string;
  if (isLocalHost(host) || /^[\d.:]+$/.test(host)) {
    base = `${host === "::1" ? "local" : host.replace(/\.localhost$/, "")}${url.port ? `-${url.port}` : ""}`;
  } else {
    const labels = host.split(".").filter((l) => !["www", "mcp", "api", "server", "app"].includes(l));
    // Drop the top-level domain (and a short second-level one such as `co` in `co.uk`).
    if (labels.length > 1) labels.pop();
    if (labels.length > 1 && labels[labels.length - 1].length <= 3) labels.pop();
    base = labels.at(-1) ?? host;
  }
  const clean = base.replace(/[^a-z0-9_-]+/g, "-").replace(/^[-_]+|-+$/g, "").slice(0, 40) || "server";
  if (!nameTaken(clean, existing)) return clean;
  for (let i = 2; ; i++) {
    const next = `${clean}-${i}`;
    if (!nameTaken(next, existing)) return next;
  }
}

function envLines(text: string): { key: string; value: string }[] | null {
  const out: { key: string; value: string }[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const i = line.indexOf("=");
    const key = i > 0 ? line.slice(0, i).trim() : "";
    if (!ENV_KEY_RE.test(key)) return null;
    out.push({ key, value: line.slice(i + 1).trim() });
  }
  return out;
}

/** Inline messages for the current draft. `existing` are the names already in use. */
export function validateDraft(d: CustomMcpDraft, existing: readonly string[]): DraftCheck {
  const errors: DraftCheck["errors"] = {};
  const warnings: DraftCheck["warnings"] = {};
  const name = d.name.trim();
  if (!name) errors.name = "nameMissing";
  else if (!NAME_RE.test(name)) errors.name = "nameFormat";
  else if (nameTaken(name, existing)) errors.name = "nameTaken";
  if (d.mode === "http") {
    const url = parseUrl(d.url);
    if (!d.url.trim()) errors.url = "urlMissing";
    else if (!url || !/^https?:\/\/\S+$/i.test(d.url.trim())) errors.url = "urlInvalid";
    // A password in the address would be saved in plain text: it belongs in the header.
    else if (url.username || url.password) errors.url = "urlCredentials";
    else if (url.protocol === "http:" && !isLocalHost(url.hostname)) warnings.url = "urlPlain";
    else if ([...url.searchParams.keys()].some((k) => URL_KEY_PARAM_RE.test(k))) warnings.url = "urlKey";
    if (d.auth === "header") {
      if (!HEADER_RE.test(d.headerName.trim())) errors.headerName = "headerName";
      if (!d.headerValue.trim()) errors.headerValue = "headerValue";
    }
  } else {
    if (!d.command.trim()) errors.command = "commandMissing";
    if (envLines(d.env) === null) errors.env = "envLine";
  }
  return { errors, warnings };
}

export function isDraftValid(check: DraftCheck): boolean {
  return Object.keys(check.errors).length === 0;
}

/** Secret store name of a header or variable of a custom server (`mcp/<server>/header/<name>`). */
export function customSecretName(server: string, kind: "header" | "env", key: string): string {
  return `mcp/${server}/${kind}/${key.replace(/[^A-Za-z0-9._-]/g, "-")}`;
}

export interface DraftPlan {
  /** Saved with `enabled: false`: nothing runs until the owner turns it on (or tests it). */
  config: McpServerConfig;
  /** Written to the secret store before the server is saved; the config names them only. */
  secrets: { name: string; value: string }[];
  /** Start the browser sign-in once the server asks for it. */
  signIn: boolean;
}

/**
 * What to store for a valid draft: the server settings (off, every action asks unless the owner
 * chose otherwise, the chosen bots) and the secrets it names. `null` when the draft has an error.
 */
export function draftPlan(
  d: CustomMcpDraft,
  existing: readonly string[],
  scope: BotScope,
  trust: boolean
): DraftPlan | null {
  if (!isDraftValid(validateDraft(d, existing))) return null;
  const name = d.name.trim();
  const plain: string[] = [];
  const secrets: DraftPlan["secrets"] = [];
  const secretEnv: Record<string, string> = {};
  if (d.mode === "stdio") {
    for (const { key, value } of envLines(d.env) ?? []) {
      if ((SECRET_KEY_RE.test(key) || SECRET_VALUE_RE.test(value)) && value) {
        const ref = customSecretName(name, "env", key);
        secrets.push({ name: ref, value });
        secretEnv[key] = ref;
      } else {
        plain.push(`${key}=${value}`);
      }
    }
  }
  const base = customServerConfig(
    { name, transport: d.mode, command: d.command, url: normalUrl(d.url), env: plain.join("\n") },
    [...existing],
    scope,
    trust
  );
  if (!base.ok) return null;
  const config: McpServerConfig = { ...base.config, enabled: false };
  if (Object.keys(secretEnv).length > 0) config.secret_env = secretEnv;
  if (d.mode === "http" && d.auth === "header") {
    const header = d.headerName.trim();
    const ref = customSecretName(name, "header", header);
    secrets.push({ name: ref, value: d.headerValue.trim() });
    config.secret_headers = { [header]: ref };
  }
  return { config, secrets, signIn: d.mode === "http" && d.auth === "oauth" };
}
