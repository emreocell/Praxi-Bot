# C062 runtime protocol fixture

`runtime_cli.rs` is a hermetic executable, never a vendor CLI. Both crates use
this same transcript. Tests fork with a private PATH and never invoke installed
`claude` or `codex`.

Claude fixture:
- Version `2.1.300`; `--strict-mcp-config` floor `1.0.60`, setting sources floor
  `1.0.122` (the floors documented in OpenMausBot's `claude.ts`). Unknown versions
  omit those optional flags.
- `-p --output-format stream-json --input-format stream-json --verbose
  --include-partial-messages --permission-mode manual --tools ""`.
  `--disallowedTools` and `permissions.deny` list Bash, PowerShell, Read, Write,
  Edit, NotebookEdit, Glob, Grep, WebFetch, WebSearch, Task, Agent, and Workflow.
  A version below 2.1.257, or a `--help` that does not list `manual` and
  "disable all tools", is refused before the session is spawned.
- Prompt and system context in a user NDJSON stdin frame. Full assistant frames
  can supplement missing partial text; thinking blocks never become text.
- MCP uses authenticated streamable HTTP JSON responses on a random loopback
  port. Config is a per-turn private file. Tool-use transcript blocks validate
  the allowed names; only MCP tools/call executes the host tool.
- `result` ends the turn, carrying optional input/output usage.

Codex fixture:
- `app-server` with `-c features.shell_tool=false`,
  `features.unified_exec=false`, `features.view_image=false`,
  `features.browser_use=false`, `features.browser_use_external=false`,
  `features.computer_use=false`, `features.hooks=false`,
  `features.multi_agent=false`, `features.plugins=false`,
  `web_search="disabled"`, `sandbox_mode="read-only"`,
  `approval_policy="on-request"`, and `mcp_servers.<name>.enabled=false` for
  every server in the user config. Optional model flag `-c model="fixture-model"`
  (one argv token per value). `environments` is not an argv value.
- NDJSON JSON-RPC: initialize(1), initialized notification, thread/start(2),
  turn/start(3). User text, system context, and MCP configuration cross stdin.
- `thread/start` sends `environments: []`, on-request approvals, a read-only
  sandbox, and a config object that disables the same feature set and web
  search. The turn is refused unless that response echoes the sandbox, those
  features, and an empty environment list. Praxi MCP configuration is in that
  config object. Turn reiterates approval/sandbox policy.
- item/mcpToolCall/requestApproval for server `praxi` maps to an existing approval
  card. accept/decline is returned with the request ID. Tools/call independently
  goes through the permission resolver; one matching approval is consumed once.
- item/agentMessage/delta maps to assistant text, thread/tokenUsage/updated to
  subscription estimates, turn/completed to completion. Any other method or item
  type, including `execCommandApproval`, `applyPatchApproval`,
  `item/permissions/requestApproval`, `item/commandExecution/outputDelta` and
  `item/fileChange/patchUpdated`, fails with native_tools_open before the next
  stdout line is applied. Loopback MCP calls are accepted only from the turn's
  child process or one of its descendants.

This is a deliberately pinned adapter contract, not a claim of compatibility
with every CLI release. An upstream protocol change requires updating fixtures
and reviewing the fail-closed native-tool policy. No live smoke test is run.
