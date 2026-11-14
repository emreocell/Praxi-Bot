/**
 * Tool name -> human wording for the thinking chain (card 198 D).
 *
 * Pure logic: the chain renders `label(...)` through i18n (`chat.step.*`), so
 * a step reads "Searched the web" instead of `web_search`. Unknown tools and
 * MCP tools (`server.tool`) get a generic wording that still names the app.
 * The raw name and input stay available in the step's detail view.
 */

/** Icon family of a step; the view maps it to a lucide icon or a brand glyph. */
export type StepIcon =
  | "search"
  | "globe"
  | "file"
  | "folder"
  | "terminal"
  | "mail"
  | "calendar"
  | "memory"
  | "workflow"
  | "agent"
  | "desktop"
  | "list"
  | "skill"
  | "tool";

export interface StepChip {
  text: string;
  /** `globe` for a web host chip; none for plain text. */
  icon?: "globe";
}

export interface StepLabel {
  /** i18n base key: `<key>.running` and `<key>.done` exist for every id. */
  key: string;
  values: Record<string, string>;
  icon: StepIcon;
  /** simple-icons brand slug (MCP apps with a known brand). */
  brand?: string;
  /** Short source / target chips (query, host, file name). */
  chips: StepChip[];
}

interface Entry {
  id: string;
  icon: StepIcon;
  chips?: (input: Record<string, unknown>) => StepChip[];
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

function clip(s: string, max = 48): string {
  const flat = s.replace(/\s+/g, " ");
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/** `https://www.example.com/a` -> `example.com`; `null` when not a URL. */
export function hostOf(url: string): string | null {
  try {
    const host = new URL(url).hostname.replace(/^www\./, "");
    return host || null;
  } catch {
    return null;
  }
}

function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

const queryChip = (i: Record<string, unknown>): StepChip[] => {
  const q = str(i.query) ?? str(i.q);
  return q ? [{ text: clip(q) }] : [];
};
const urlChip = (i: Record<string, unknown>): StepChip[] => {
  const u = str(i.url);
  const host = u ? hostOf(u) : null;
  return host ? [{ text: host, icon: "globe" }] : [];
};
const pathChip = (i: Record<string, unknown>): StepChip[] => {
  const p = str(i.path) ?? str(i.file) ?? str(i.src) ?? str(i.from);
  return p ? [{ text: clip(baseName(p)) }] : [];
};

/*
 * Only tools whose effect is known get a specific sentence. There are no
 * prefix rows on purpose: a family name must never turn an unlisted (possibly
 * writing or scripting) tool into a "read" or "used" line. Unlisted names fall
 * through to the generic wording, which still contains the tool name.
 */
const TABLE: Record<string, Entry> = {
  web_search: { id: "web_search", icon: "search", chips: queryChip },
  web_extract: { id: "web_read", icon: "globe", chips: urlChip },
  web_feed: { id: "web_read", icon: "globe", chips: urlChip },
  web_metadata: { id: "web_read", icon: "globe", chips: urlChip },
  web_sitemap: { id: "web_read", icon: "globe", chips: urlChip },
  pdf_extract: { id: "doc_read", icon: "file", chips: pathChip },
  fs_read: { id: "fs_read", icon: "file", chips: pathChip },
  fs_write: { id: "fs_write", icon: "file", chips: pathChip },
  fs_edit: { id: "fs_write", icon: "file", chips: pathChip },
  fs_move: { id: "fs_move", icon: "file", chips: pathChip },
  fs_delete: { id: "fs_delete", icon: "file", chips: pathChip },
  fs_list: { id: "fs_list", icon: "folder", chips: pathChip },
  fs_search: { id: "fs_search", icon: "search", chips: queryChip },
  shell_run: { id: "shell", icon: "terminal" },
  "shell.run": { id: "shell", icon: "terminal" },
  script_run: { id: "shell", icon: "terminal" },
  browser_open: { id: "browser_open", icon: "globe", chips: urlChip },
  browser_click: { id: "browser_click", icon: "globe" },
  browser_fill: { id: "browser_fill", icon: "globe" },
  browser_eval: { id: "browser_eval", icon: "terminal" },
  browser_press: { id: "browser_press", icon: "globe" },
  browser_select: { id: "browser_select", icon: "globe" },
  browser_scroll: { id: "browser_view", icon: "globe" },
  browser_back: { id: "browser_view", icon: "globe" },
  browser_tabs: { id: "browser_view", icon: "globe" },
  browser_wait: { id: "browser_view", icon: "globe" },
  browser_snapshot: { id: "browser_view", icon: "globe" },
  browser_screenshot: { id: "browser_view", icon: "globe" },
  browser_idle: { id: "browser_view", icon: "globe" },
  desktop_click: { id: "desktop_pointer", icon: "desktop" },
  desktop_drag: { id: "desktop_pointer", icon: "desktop" },
  desktop_scroll: { id: "desktop_pointer", icon: "desktop" },
  desktop_type: { id: "desktop_type", icon: "desktop" },
  desktop_key: { id: "desktop_key", icon: "desktop" },
  desktop_set_value: { id: "desktop_set", icon: "desktop" },
  desktop_launch: { id: "desktop_launch", icon: "desktop" },
  desktop_get_state: { id: "desktop_look", icon: "desktop" },
  desktop_list_apps: { id: "desktop_look", icon: "desktop" },
  skill_write: { id: "skill_write", icon: "skill" },
  skill_read: { id: "skill_look", icon: "skill" },
  skill_list: { id: "skill_look", icon: "skill" },
  workflow_run: { id: "workflow", icon: "workflow" },
  todo_write: { id: "todo", icon: "list" },
  todo_list: { id: "todo", icon: "list" },
  schedule_create: { id: "schedule_create", icon: "calendar" },
  schedule_update: { id: "schedule_update", icon: "calendar" },
  schedule_delete: { id: "schedule_delete", icon: "calendar" },
  schedule_pause: { id: "schedule_update", icon: "calendar" },
  schedule_resume: { id: "schedule_update", icon: "calendar" },
  schedule_list: { id: "schedule_list", icon: "calendar" },
  email_send: { id: "email_send", icon: "mail" },
  email_draft: { id: "email_draft", icon: "mail" },
  email_search: { id: "email_read", icon: "mail", chips: queryChip },
  email_read: { id: "email_read", icon: "mail" },
  email_list_folders: { id: "email_read", icon: "mail" },
  email_mark_read: { id: "email_mark", icon: "mail" },
  spawn_subagent: { id: "agent", icon: "agent" },
  ask_user: { id: "ask", icon: "agent" }
};

export const KNOWN_TOOL_NAMES: readonly string[] = Object.keys(TABLE);

/**
 * Which wording a step uses. A call that errored, was denied or expired must
 * never read as done ("Deleted a file").
 */
export function stepPhase(
  status: string
): "running" | "failed" | "done" {
  if (status === "requested" || status === "running") {
    return "running";
  }
  return status === "error" || status === "denied" || status === "expired"
    ? "failed"
    : "done";
}

/** Tools that really send something out (not merely mention mail). */
export function isSendTool(name: string): boolean {
  return name
    .toLowerCase()
    .split(/[._\-:]+/)
    .some((t) => ["send", "post", "reply", "publish", "tweet"].includes(t));
}

/** `get_open_PRs` / `getOpenPRs` -> `get open PRs`. */
export function humanizeName(raw: string): string {
  return raw
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_\-.:/]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/** `github` -> `GitHub` via the caller's brand lookup; else capitalised. */
function appName(
  server: string,
  brandName?: (name: string) => string | null
): string {
  const known = brandName?.(server);
  if (known) {
    return known;
  }
  const h = humanizeName(server);
  return h ? h.charAt(0).toUpperCase() + h.slice(1) : server;
}

function asRecord(input: unknown): Record<string, unknown> {
  return input !== null && typeof input === "object" && !Array.isArray(input)
    ? (input as Record<string, unknown>)
    : {};
}

/**
 * Describe one tool call for the chain.
 *
 * `brandName` resolves a server name to a known brand's display name (the view
 * passes `brandIcon`); keeping it injected keeps this module free of imports.
 */
export function describeTool(
  name: string,
  input: unknown,
  brandName?: (name: string) => string | null
): StepLabel {
  const rec = asRecord(input);
  const exact = TABLE[name];
  const entry = exact ?? null;
  if (entry) {
    return {
      key: `chat.step.${entry.id}`,
      values: {},
      icon: entry.icon,
      chips: entry.chips?.(rec) ?? []
    };
  }
  // MCP tools are `server.tool` (also tolerate `server__tool`).
  const split = /^([^.]+)\.(.+)$/.exec(name) ?? /^([^_]+)__(.+)$/.exec(name);
  if (split) {
    const server = split[1] ?? name;
    const action = humanizeName(split[2] ?? "");
    const chips = queryChip(rec).concat(urlChip(rec));
    return {
      key: "chat.step.mcp",
      values: { app: appName(server, brandName), action },
      icon: "tool",
      brand: server,
      chips: chips.slice(0, 2)
    };
  }
  return {
    key: "chat.step.generic",
    values: { name: humanizeName(name) || name },
    icon: "tool",
    chips: queryChip(rec)
  };
}
