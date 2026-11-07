# C062 runtime protocol fixture

`runtime_cli.rs` is a hermetic executable, never a vendor CLI. Both crates use
this same transcript. Tests fork with a private PATH and never invoke installed
`claude` or `codex`.

Claude fixture:
- Version `2.1.300`; `--strict-mcp-config` floor `1.0.60`, setting sources floor
  `1.0.122` (the floors documented in OpenMausBot's `claude.ts`). Unknown versions
  omit those optional flags.
- `-p --output-format stream-json --input-format stream-json --verbose
  --include-partial-messages --permission-mode default --tools <mounted MCP names>`.
- Prompt and system context in a user NDJSON stdin frame. Full assistant frames
  can supplement missing partial text; thinking blocks never become text.
- MCP uses authenticated streamable HTTP JSON responses on a random loopback
  port. Config is a per-turn private file. Tool-use transcript blocks validate
  the allowed names; only MCP tools/call executes the host tool.
- `result` ends the turn, carrying optional input/output usage.

Codex fixture:
- `app-server -c features.shell_tool=false -c features.apply_patch_freeform=false`
  and optional model flag `-c model="fixture-model"` (one argv token per value).
- NDJSON JSON-RPC: initialize(1), initialized notification, thread/start(2),
  turn/start(3). User text, system context, and MCP configuration cross stdin.
- Thread uses on-request approvals, read-only sandbox, and overrides disabling
  shell, freeform patch, collaboration and web search. Praxi MCP configuration
  replaces the inherited server table. Turn reiterates approval/sandbox policy.
- item/mcpToolCall/requestApproval for server `praxi` maps to an existing approval
  card. accept/decline is returned with the request ID. Tools/call independently
  goes through the permission resolver; one matching approval is consumed once.
- item/agentMessage/delta maps to assistant text, thread/tokenUsage/updated to
  subscription estimates, turn/completed to completion. Native executable items
  and native shell/file approval requests fail with native_tools_open.

This is a deliberately pinned adapter contract, not a claim of compatibility
with every CLI release. An upstream protocol change requires updating fixtures
and reviewing the fail-closed native-tool policy. No live smoke test is run.
