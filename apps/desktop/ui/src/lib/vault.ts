import type { Bot, EmailAccount, FillGrantInfo, FillSecretInfo, VaultKind } from "../api/types.ts";
import { matchesQuery } from "./searchText.ts";
import { isValidSecretName } from "./secretDetect.ts";

export type VaultTab = "all" | "login" | "other";

export interface VaultCsvRow {
  siteName: string;
  origin: string | null;
  loginId: string;
  password: string;
  note: string;
}

export interface VaultCsvParseResult {
  format: "chrome" | "bitwarden" | "unknown";
  rows: VaultCsvRow[];
  skipped: number;
}

/** Human-readable title for a Vault entry (explicit site name, origin host, or key). */
export function siteLabelOf(item: Pick<FillSecretInfo, "name" | "site_name" | "origins">): string {
  const explicit = item.site_name?.trim();
  if (explicit) return explicit;
  const firstOrigin = item.origins[0];
  if (firstOrigin) {
    try {
      return new URL(firstOrigin).hostname.replace(/^www\./i, "");
    } catch {
      return firstOrigin;
    }
  }
  return item.name;
}

/** Brand token fed into `BrandIcon` / `brandIcon` so common sites get crisp logos. */
export function brandQueryOf(item: Pick<FillSecretInfo, "name" | "site_name" | "origins">): string {
  const firstOrigin = item.origins[0];
  if (firstOrigin) {
    try {
      const host = new URL(firstOrigin).hostname.replace(/^www\./i, "");
      const base = host.split(".")[0];
      if (base) return base;
    } catch {
      // fall through
    }
  }
  const label = siteLabelOf(item);
  return label.split(/[\s/]+/).pop() || label;
}

/** Canonicalize a user-entered site or URL into an exact `http://` or `https://` origin. */
export function normalizeOrigin(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed || /[\s*]/.test(trimmed)) return null;
  const candidate = /^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  try {
    const u = new URL(candidate);
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (u.username || u.password) return null;
    return u.origin;
  } catch {
    return null;
  }
}

/**
 * The host whose favicon a Vault row may show: the first saved origin's bare host name, lower case,
 * with no port, path, query or fragment (so no tracking parameter is ever passed on). IP addresses,
 * single-label names (`localhost`, `router`) and other non-public shapes give null: their tile stays the initial.
 */
export function vaultFaviconDomain(origins: readonly string[]): string | null {
  const first = origins.find((o) => o.trim() !== "");
  const origin = first ? normalizeOrigin(first) : null;
  if (!origin) return null;
  const host = new URL(origin).hostname.toLowerCase().replace(/\.$/, "");
  if (host.startsWith("[") || /^[0-9.]+$/.test(host)) return null;
  if (!/^[a-z0-9-]+(\.[a-z0-9-]+)*\.[a-z]{2,}$/.test(host)) return null;
  if (/\.(local|localhost|internal|lan|home|arpa)$/.test(host)) return null;
  return host;
}

/** Build a valid, unique `fill/<name>` identifier for a newly created Vault item. */
export function suggestSecretName(
  siteOrLabel: string,
  kind: VaultKind,
  existingNames: readonly string[],
): string {
  const prefix = kind === "login" ? "login" : "secret";
  const slug = siteOrLabel
    .trim()
    .toLowerCase()
    .replace(/^https?:\/\//, "")
    .replace(/^www\./, "")
    .split("/")[0]
    .replace(/[^a-z0-9._-]+/g, "-")
    .replace(/^[-._]+|[-._]+$/g, "")
    .slice(0, 40) || "item";
  const base = `${prefix}/${slug}`;
  const taken = new Set(existingNames);
  if (!taken.has(base) && isValidSecretName(base)) return base;
  for (let i = 2; i < 1000; i += 1) {
    const candidate = `${base.slice(0, 56)}-${i}`;
    if (!taken.has(candidate) && isValidSecretName(candidate)) return candidate;
  }
  return `${prefix}/item-${Date.now().toString(36)}`;
}

/** Filter Vault entries by tab (`all` / `login` / `other`) and search query. */
export function filterVaultItems(
  items: readonly FillSecretInfo[],
  tab: VaultTab,
  query: string,
): FillSecretInfo[] {
  return items.filter((item) => {
    if (tab !== "all" && item.kind !== tab) return false;
    return matchesQuery(
      query,
      siteLabelOf(item),
      item.name,
      item.login_id ?? "",
      item.origins.join(" "),
      item.programs.join(" "),
      item.note ?? "",
    );
  });
}

export interface PinnedBotIdentity {
  account: EmailAccount;
  botName: string | null;
}

/** Bot email identities pinned at the top of the Vault, AgentMail and bot-bound first. */
export function pinnedBotIdentities(
  accounts: readonly EmailAccount[],
  bots: readonly Pick<Bot, "id" | "name">[],
): PinnedBotIdentity[] {
  const botNames = new Map(bots.map((b) => [b.id, b.name]));
  return [...accounts]
    .sort((a, b) => {
      const aScore = (a.bot ? 2 : 0) + (a.provider === "agentmail" ? 1 : 0);
      const bScore = (b.bot ? 2 : 0) + (b.provider === "agentmail" ? 1 : 0);
      if (aScore !== bScore) return bScore - aScore;
      return a.address.localeCompare(b.address);
    })
    .map((account) => ({
      account,
      botName: account.bot ? (botNames.get(account.bot) ?? null) : null,
    }));
}

/** Parse RFC 4180 CSV text into rows of strings (handles quoted commas, newlines, `""`, BOM). */
function parseCsvRecords(input: string): string[][] {
  const text = input.charCodeAt(0) === 0xfeff ? input.slice(1) : input;
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let inQuotes = false;

  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i += 1;
        } else {
          inQuotes = false;
        }
      } else {
        cell += ch;
      }
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
    } else if (ch === ",") {
      row.push(cell);
      cell = "";
    } else if (ch === "\r") {
      if (text[i + 1] === "\n") i += 1;
      row.push(cell);
      rows.push(row);
      row = [];
      cell = "";
    } else if (ch === "\n") {
      row.push(cell);
      rows.push(row);
      row = [];
      cell = "";
    } else {
      cell += ch;
    }
  }
  if (cell.length > 0 || row.length > 0) {
    row.push(cell);
    rows.push(row);
  }
  return rows.filter((r) => r.some((c) => c.trim() !== ""));
}

/**
 * Parse a Chrome or Bitwarden CSV password-manager export.
 * Never logs or persists passwords; returns parsed login rows for preview.
 */
export function parseVaultCsv(csvText: string): VaultCsvParseResult {
  const records = parseCsvRecords(csvText);
  if (records.length < 2) {
    return { format: "unknown", rows: [], skipped: 0 };
  }
  const headers = records[0].map((h) => h.trim().toLowerCase());
  const idx = (name: string) => headers.indexOf(name);

  const isBitwarden =
    idx("login_username") >= 0 && idx("login_password") >= 0;
  const isChrome =
    !isBitwarden &&
    idx("username") >= 0 &&
    idx("password") >= 0 &&
    (idx("url") >= 0 || idx("origin") >= 0);

  if (!isBitwarden && !isChrome) {
    return { format: "unknown", rows: [], skipped: Math.max(0, records.length - 1) };
  }

  const format = isBitwarden ? "bitwarden" : "chrome";
  const rows: VaultCsvRow[] = [];
  let skipped = 0;

  for (let r = 1; r < records.length; r += 1) {
    const rec = records[r];
    const get = (col: string) => {
      const i = idx(col);
      return i >= 0 ? (rec[i] ?? "").trim() : "";
    };

    if (isBitwarden) {
      const type = get("type").toLowerCase();
      if (type && type !== "login") {
        skipped += 1;
        continue;
      }
      const password = get("login_password");
      if (!password) {
        skipped += 1;
        continue;
      }
      const rawUri = get("login_uri").split(/[\s,]+/).find(Boolean) ?? "";
      const origin = rawUri ? normalizeOrigin(rawUri) : null;
      const loginId = get("login_username");
      const siteName = get("name") || (origin ? new URL(origin).hostname.replace(/^www\./i, "") : loginId || "Login");
      const note = get("notes");
      rows.push({ siteName, origin, loginId, password, note });
    } else {
      const password = get("password");
      if (!password) {
        skipped += 1;
        continue;
      }
      const rawUrl = get("url") || get("origin");
      const origin = rawUrl ? normalizeOrigin(rawUrl) : null;
      const loginId = get("username");
      const siteName = get("name") || (origin ? new URL(origin).hostname.replace(/^www\./i, "") : loginId || "Login");
      const note = get("note");
      rows.push({ siteName, origin, loginId, password, note });
    }
  }

  return { format, rows, skipped };
}

export interface GrantGroup {
  name: string;
  item: FillSecretInfo | null;
  label: string;
  grants: FillGrantInfo[];
}

/** Group standing grants by entry; groups sort by site label, grants by origin. Unknown entries fall back to the raw name. */
export function groupGrants(
  grants: readonly FillGrantInfo[],
  items: readonly FillSecretInfo[],
): GrantGroup[] {
  const byName = new Map(items.map((i) => [i.name, i]));
  const groups = new Map<string, GrantGroup>();
  for (const grant of grants) {
    let group = groups.get(grant.secret);
    if (!group) {
      const item = byName.get(grant.secret) ?? null;
      group = {
        name: grant.secret,
        item,
        label: item ? siteLabelOf(item) : grant.secret,
        grants: [],
      };
      groups.set(grant.secret, group);
    }
    group.grants.push(grant);
  }
  const out = [...groups.values()];
  for (const group of out) group.grants.sort((a, b) => a.origin.localeCompare(b.origin));
  return out.sort(
    (a, b) =>
      a.label.localeCompare(b.label, undefined, { sensitivity: "base" }) ||
      a.name.localeCompare(b.name),
  );
}

/** True when no other grant belongs to the same entry (revoking it turns "Ask before use" back on). */
export function isLastGrant(grants: readonly FillGrantInfo[], grant: FillGrantInfo): boolean {
  return !grants.some((g) => g.secret === grant.secret && g.id !== grant.id);
}
