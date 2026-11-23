# ADR: pin approved MCP tool definitions

Status: proposed. PR #633 (closes #580) was still open when this was written;
update this status when it merges. Code on that branch:
`crates/mcp-bridge/src/pins.rs`, `crates/mcp-bridge/src/manager.rs`,
`crates/daemon-api/src/owner.rs`.

## Context

An MCP server is outside our control. After the owner approved it, it can
rewrite a tool's description or schema ("rug pull") and put instructions in
front of the model. Tool descriptions reach the model as trusted-looking text,
so a changed description is a prompt-injection channel.

## Decision

- On the first listing of a tool, store a SHA-256 over its name, title,
  description, input/output schema and annotations (`FilePinStore`,
  `<data_dir>/mcp-tool-pins.json`). Trust on first use: the owner connecting the
  server is the approval.
- A later listing with a different hash is **not registered**: not callable and
  not reachable through `find_tool` / `get_tool_details` / `use_tool`. The
  server snapshot carries before/after so the UI can show a text-only diff.
- `POST /v1/mcp/servers/{name}/approve-tool-changes` re-approves. It is
  owner-only (`owner.rs`), denied to peers and audited as
  `mcp.tool_changes_approved`. It accepts only the definition the owner was
  shown; a further change in between stays held back with a fresh diff.
- Editing server settings (such as a tool filter) keeps the pins. Only removing
  the server or changing its command/URL forgets them.
- A missing or unreadable pin file starts empty (re-approval on first connect);
  write failures are logged, not fatal.

## Consequences

- A server can no longer change what the model reads without the owner seeing it.
- A brand-new tool added later is still pinned on first sight, so that variant
  of the attack is not covered; it needs its own decision.
- Legitimate server upgrades ask for one click per changed tool.

## Alternatives rejected

- Pinning the package version only (the catalog already does): a pinned
  package or a remote server can still serve different text.
- Warning but still registering the tool: the model would already have read it.
- Resetting pins on every settings edit: it would approve a change silently.
