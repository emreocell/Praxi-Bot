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

const secondsChip = (i: Record<string, unknown>): StepChip[] => {
  const n = typeof i.seconds === "number" && Number.isFinite(i.seconds) ? Math.round(i.seconds) : null;
  return n !== null && n > 0 ? [{ text: `${n} s` }] : [];
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
  web_scrape: { id: "web_read", icon: "globe", chips: urlChip },
  web_feed: { id: "web_read", icon: "globe", chips: urlChip },
  web_metadata: { id: "web_read", icon: "globe", chips: urlChip },
  web_sitemap: { id: "web_read", icon: "globe", chips: urlChip },
  pdf_extract: { id: "doc_read", icon: "file", chips: pathChip },
  docx_read: { id: "doc_read", icon: "file", chips: pathChip },
  xlsx_read: { id: "doc_read", icon: "file", chips: pathChip },
  pptx_read: { id: "doc_read", icon: "file", chips: pathChip },
  csv_read: { id: "doc_read", icon: "file", chips: pathChip },
  docx_write: { id: "doc_write", icon: "file", chips: pathChip },
  xlsx_write: { id: "doc_write", icon: "file", chips: pathChip },
  docx_append: { id: "doc_edit", icon: "file", chips: pathChip },
  xlsx_append_rows: { id: "doc_edit", icon: "file", chips: pathChip },
  xlsx_update_cells: { id: "doc_edit", icon: "file", chips: pathChip },
  git_status: { id: "git_read", icon: "file" },
  git_diff: { id: "git_read", icon: "file" },
  git_log: { id: "git_read", icon: "file" },
  git_show: { id: "git_read", icon: "file" },
  git_branch: { id: "git_change", icon: "file" },
  git_commit: { id: "git_change", icon: "file" },
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
  browser_act: { id: "browser_act", icon: "globe" },
  browser_handover: { id: "browser_handover", icon: "globe" },
  browser_click: { id: "browser_click", icon: "globe" },
  browser_fill: { id: "browser_fill", icon: "globe" },
  browser_eval: { id: "browser_eval", icon: "terminal" },
  browser_press: { id: "browser_press", icon: "globe" },
  browser_select: { id: "browser_select", icon: "globe" },
  browser_scroll: { id: "browser_view", icon: "globe" },
  browser_dismiss_consent: { id: "browser_dismiss_consent", icon: "globe" },
  browser_hover: { id: "browser_hover", icon: "globe" },
  browser_select_text: { id: "browser_select_text", icon: "globe" },
  browser_pointer: { id: "browser_pointer", icon: "globe" },
  browser_attach: { id: "browser_attach", icon: "globe" },
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
  memory_search: { id: "memory_search", icon: "memory", chips: queryChip },
  session_search: { id: "session_search", icon: "memory", chips: queryChip },
  memory_save: { id: "memory_save", icon: "memory" },
  memory_forget: { id: "memory_forget", icon: "memory", chips: queryChip },
  notes_read: { id: "notes_read", icon: "memory", chips: queryChip },
  notes_write: { id: "notes_write", icon: "memory" },
  time_now: { id: "time_now", icon: "calendar" },
  calculate: { id: "calculate", icon: "tool" },
  wait: { id: "wait", icon: "calendar", chips: secondsChip },
  knowledge_search: { id: "knowledge_search", icon: "search", chips: queryChip },
  workflow_run: { id: "workflow", icon: "workflow" },
  todo_write: { id: "todo", icon: "list" },
  todo_list: { id: "todo", icon: "list" },
  schedule_create: { id: "schedule_create", icon: "calendar" },
  schedule_update: { id: "schedule_update", icon: "calendar" },
  schedule_delete: { id: "schedule_delete", icon: "calendar" },
  schedule_pause: { id: "schedule_update", icon: "calendar" },
  schedule_resume: { id: "schedule_update", icon: "calendar" },
  schedule_list: { id: "schedule_list", icon: "calendar" },
  bot_list: { id: "bot_list", icon: "agent" },
  bot_create: { id: "bot_create", icon: "agent" },
  bot_update: { id: "bot_update", icon: "agent" },
  bot_delete: { id: "bot_delete", icon: "agent" },
  email_send: { id: "email_send", icon: "mail" },
  email_draft: { id: "email_draft", icon: "mail" },
  email_search: { id: "email_read", icon: "mail", chips: queryChip },
  email_read: { id: "email_read", icon: "mail" },
  email_identities: { id: "email_identities", icon: "mail" },
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
  // A call waiting for the owner has NOT run yet: never the "done" wording.
  if (status === "requested" || status === "running" || status === "awaiting_approval") {
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

/** Discovery plumbing (`find_tool`, `get_tool_details`, `tool_search`): never shown as a step. */
export function isDiscoveryTool(name: string): boolean {
  return name === "find_tool" || name === "get_tool_details" || name === "tool_search";
}

/**
 * `use_tool` runs another tool: the chain shows the real one.
 * `{ name: "mcp:github/search_repositories", input }` -> `github.search_repositories`.
 */
export function resolveToolCall(
  name: string,
  input: unknown
): { name: string; input: unknown } {
  if (name !== "use_tool") {
    return { name, input };
  }
  const rec = asRecord(input);
  const target = str(rec.name);
  if (!target) {
    return { name, input };
  }
  const mcp = /^mcp:([^/]+)\/(.+)$/.exec(target);
  return {
    name: mcp ? `${mcp[1]}.${mcp[2]}` : target,
    input: rec.input ?? rec.arguments ?? {}
  };
}

const TR_FRONT = /[eiöü]/;
const TR_BACK = /[aıou]/;
/** Hand-written pronunciations the vowel rule gets wrong (English names). */
const TR_LOCATIVE_OVERRIDE: Record<string, string> = {
  gmail: "de",
  drive: "da",
  slack: "ta",
  github: "da"
};

/**
 * Turkish locative for an app name: `GitHub` -> `GitHub'da`, `Gmail` ->
 * `Gmail'de`, `Slack` -> `Slack'ta`. Used by the Turkish step wording only.
 */
export function trLocative(app: string): string {
  const lower = app.toLowerCase().replace(/[^a-zçğıöşü0-9]/g, "");
  const forced = TR_LOCATIVE_OVERRIDE[lower];
  if (forced) {
    return `${app}'${forced}`;
  }
  const vowels = [...lower].filter((c) => TR_FRONT.test(c) || TR_BACK.test(c));
  const last = vowels[vowels.length - 1];
  const back = last ? TR_BACK.test(last) : true;
  const hard = /[çfhkpsşt]$/.test(lower);
  return `${app}'${hard ? "t" : "d"}${back ? "a" : "e"}`;
}

type McpVerb = "search" | "read" | "draft" | "send";

/** Coarse effect of an MCP action name; `null` keeps the generic wording. */
export function mcpVerb(action: string): McpVerb | null {
  const words = action.toLowerCase().split(/\s+/);
  if (words.includes("draft") || words.includes("drafts")) {
    return "draft";
  }
  if (words.some((w) => ["send", "post", "reply", "publish", "tweet"].includes(w))) {
    return "send";
  }
  if (words.some((w) => ["search", "find", "query"].includes(w))) {
    return "search";
  }
  if (words.some((w) => ["get", "read", "fetch", "list", "view", "show"].includes(w))) {
    return "read";
  }
  return null;
}

const repoChip = (i: Record<string, unknown>): StepChip[] => {
  const owner = str(i.owner);
  const repo = str(i.repo) ?? str(i.repository);
  return repo ? [{ text: clip(owner ? `${owner}/${repo}` : repo) }] : [];
};

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
  ({ name, input } = resolveToolCall(name, input));
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
    const chips = queryChip(rec).concat(urlChip(rec), repoChip(rec));
    const app = appName(server, brandName);
    const verb = mcpVerb(action);
    return {
      key: verb ? `chat.step.mcp_${verb}` : "chat.step.mcp",
      values: { app, appAt: trLocative(app), action },
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
