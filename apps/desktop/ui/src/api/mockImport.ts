import type { ImportItem, ImportResult, ImportRunInput, ImportScanInput } from "./types.ts";

/** Mock / demo data for "Import from other assistants": one of each kind, plus ones that cannot be imported. */

const NONE: { secrets: ImportItem["secrets"]; dropped: string[] } = { secrets: [], dropped: [] };

const BASE_ITEMS: ImportItem[] = [
  {
    id: "claude:mcp:github",
    app: "claude",
    kind: "mcp_server",
    name: "github",
    source: "~/.claude.json",
    import: true,
    default_on: true,
    reason: "ready_with_secrets",
    note: "Ready. One value goes to the secret store once you confirm it.",
    detail: "npx -y @modelcontextprotocol/server-github@1.4.0",
    secrets: [{ name: "mcp_github_GITHUB_TOKEN", key: "GITHUB_TOKEN", place: "env" }],
    dropped: []
  },
  {
    id: "cursor:mcp:filesystem",
    app: "cursor",
    kind: "mcp_server",
    name: "filesystem",
    source: "~/.cursor/mcp.json",
    import: true,
    default_on: true,
    reason: "ready",
    note: "Ready to import.",
    detail: "npx -y @modelcontextprotocol/server-filesystem@2025.8.0",
    ...structuredClone(NONE)
  },
  {
    id: "vscode:mcp:docs",
    app: "vscode",
    kind: "mcp_server",
    name: "docs",
    source: "~/.config/Code/User/mcp.json",
    import: true,
    default_on: true,
    reason: "ready_with_secrets",
    note: "Ready. One value goes to the secret store once you confirm it.",
    detail: "https://docs.example.com/mcp",
    secrets: [{ name: "mcp_docs_Authorization", key: "Authorization", place: "header" }],
    dropped: ["DOCS_REGION"]
  },
  {
    id: "codex:mcp:memory",
    app: "codex",
    kind: "mcp_server",
    name: "memory",
    source: "~/.codex/config.toml",
    import: true,
    default_on: true,
    reason: "ready",
    note: "Ready to import.",
    detail: "npx -y @modelcontextprotocol/server-memory@2025.4.0",
    ...structuredClone(NONE)
  },
  {
    id: "claude:skill:release-notes",
    app: "claude",
    kind: "skill",
    name: "release-notes",
    source: "~/.claude/skills/release-notes",
    import: true,
    default_on: true,
    reason: "ready",
    note: "Ready to import.",
    detail: "Draft release notes from merged pull requests.",
    ...structuredClone(NONE)
  },
  {
    id: "codex:skill:deploy-helper",
    app: "codex",
    kind: "skill",
    name: "deploy-helper",
    source: "~/.codex/skills/deploy-helper",
    import: true,
    default_on: false,
    reason: "review_first",
    note: "Look it over first: it has scripts.",
    detail: "Has a scripts folder.",
    ...structuredClone(NONE)
  },
  {
    id: "claude:instructions:CLAUDE.md",
    app: "claude",
    kind: "instructions",
    name: "CLAUDE.md",
    source: "~/.claude/CLAUDE.md",
    import: true,
    default_on: false,
    reason: "ready",
    note: "Added to the end of one bot's instructions, if you choose a bot.",
    detail: "1.2 KB, 24 lines",
    ...structuredClone(NONE)
  },
  {
    id: "hermes:instructions:AGENTS.md",
    app: "hermes",
    kind: "instructions",
    name: "AGENTS.md",
    source: "~/.hermes/AGENTS.md",
    import: true,
    default_on: false,
    reason: "ready",
    note: "Added to the end of one bot's instructions, if you choose a bot.",
    detail: "0.6 KB, 11 lines",
    ...structuredClone(NONE)
  },
  {
    id: "cursor:mcp:old-tool",
    app: "cursor",
    kind: "mcp_server",
    name: "old-tool",
    source: "~/.cursor/mcp.json",
    import: false,
    default_on: false,
    reason: "unpinned",
    note: "It runs a package without a fixed version, so it was left out.",
    detail: "npx old-tool",
    ...structuredClone(NONE)
  },
  {
    id: "openclaw:mcp:shell-runner",
    app: "openclaw",
    kind: "mcp_server",
    name: "shell-runner",
    source: "~/.openclaw/openclaw.json",
    import: false,
    default_on: false,
    reason: "never_list",
    note: "Its command is on Praxi's never-allowed list.",
    ...structuredClone(NONE)
  },
  {
    id: "hermes:other:auth",
    app: "hermes",
    kind: "other",
    name: "Saved logins",
    source: "~/.hermes/auth.json",
    import: false,
    default_on: false,
    reason: "never_copied",
    note: "Logins and keys of other apps are never copied.",
    ...structuredClone(NONE)
  }
];

const FOLDER_ITEM: ImportItem = {
  id: "folder:skill:review-checklist",
  app: "folder",
  kind: "skill",
  name: "review-checklist",
  source: "review-checklist.md",
  import: true,
  default_on: true,
  reason: "ready",
  note: "Ready to import.",
  detail: "A checklist for reviewing a change.",
  ...structuredClone(NONE)
};

export function mockImportScan(input: ImportScanInput): { items: ImportItem[] } {
  const items = input.folder ? [...BASE_ITEMS, FOLDER_ITEM] : BASE_ITEMS;
  return { items: structuredClone(input.only ? items.filter((i) => i.app === input.only) : items) };
}

export function mockImportRun(input: ImportRunInput): { results: ImportResult[] } {
  const all = [...BASE_ITEMS, FOLDER_ITEM];
  const results: ImportResult[] = input.ids.map((id): ImportResult => {
    const item = all.find((i) => i.id === id);
    const base = { id, secrets_stored: [] as string[], secrets_left_out: [] as string[] };
    if (!item || !item.import) {
      return { ...base, status: "failed", reason: "not_imported", message: "That item cannot be imported." };
    }
    if (item.name === "memory") {
      return { ...base, status: "skipped", reason: "already_have", message: "Praxi already has a server with this name." };
    }
    if (item.name === "deploy-helper") {
      return { ...base, status: "failed", reason: "invalid", message: "The skill file could not be read." };
    }
    const stored = item.secrets.filter((s) => input.confirmed_secrets.includes(s.name));
    const leftOut = item.secrets.filter((s) => !input.confirmed_secrets.includes(s.name));
    const done = {
      ...base,
      secrets_stored: stored.map((s) => s.name),
      secrets_left_out: leftOut.map((s) => s.key)
    };
    if (item.kind === "instructions") {
      return { ...done, status: "imported", message: "Instructions ready to add.", text: `# From ${item.source}\n\nKeep answers short. Ask before deleting anything.` };
    }
    return { ...done, status: "imported", message: item.kind === "mcp_server" ? "Added, switched off." : "Added." };
  });
  return { results };
}
