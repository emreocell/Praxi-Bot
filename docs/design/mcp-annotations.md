# ADR: MCP tool annotations are hints from the server, not facts

Status: accepted. Code: `crates/mcp-bridge/src/tool.rs` (`hint_policy`),
`crates/mcp-bridge/src/pins.rs` (`pin_of`), `crates/daemon-api/src/mcp_routes.rs`
(audit), UI `McpTrustChoice`.

## Context

The MCP spec lets a tool carry annotations: `readOnlyHint`, `destructiveHint`,
`idempotentHint`, `openWorldHint`. Two points from the spec and from review of
issue #687 shape the policy:

1. **Defaults matter more than flags.** A tool without annotations is
   *not read-only and destructive*. The schema also says clients must never
   base tool-use decisions on annotations of **untrusted** servers. So trust is
   a per-server choice the owner makes once; the hints only sort the tools of a
   server the owner trusts.
2. **Pin the annotations with the description.** Annotations live on the Tool
   object, so a `tools/list_changed` refresh can flip `destructiveHint` while
   the description stays identical. A change is drift, in either direction.

## Decision

`trust_hints` is stored per server (default `false`, catalog / bundle / import
installs never set it). It is asked once in plain words at install ("Do you
trust this server to describe its own tools honestly?", default "No, ask me for
every action"), changeable in the server's Manage panel, shown on the card as
"Labels trusted" / "Always asks", owner-only, and every change is audited
(`mcp.trust_hints_changed`, with the yes/no value, never server text).

### Policy table

Effect is what the permission engine sees. **Ask** = `Destructive` (asks;
"allow for this task" is available because it applies to every MCP tool).
**Allow** = `Read` (runs without a card when the owner's `read_tools_skip_ask`
policy allows reads). **Taint** = the result is untrusted content and the call
counts as able to send data out.

| Server | Annotations | Effect | Result | Egress |
|---|---|---|---|---|
| any | none / empty | Ask | untrusted | per server (`local_only`) |
| untrusted | anything | Ask | untrusted | per server (`local_only`) |
| trusted | `readOnlyHint=true` | **Allow** | untrusted | per server |
| trusted | `readOnlyHint=true` + `destructiveHint=true` (contradiction) | Ask | untrusted | per server |
| trusted | `readOnlyHint=true` + `openWorldHint=true` | **Allow** | untrusted | **egress** (even if `local_only`) |
| trusted | `readOnlyHint=false`, `destructiveHint=false` | Ask | untrusted | per server |
| trusted | `readOnlyHint=false`, `destructiveHint=true` or missing | Ask | untrusted | per server |
| trusted | `idempotentHint` (any value) | no change | untrusted | no change |
| trusted | `openWorldHint=true` (any other labels) | per row above | untrusted | **egress** |

Notes:

- Every MCP result is untrusted content already (`ingests_untrusted`); the
  open-world hint cannot lower that, only raise the egress flag.
- A "non-destructive write" stays Ask. A server's claim must never be what
  removes a prompt for something that changes state; the owner's own "allow for
  this task" grant is the way to lower friction.
- `idempotentHint` never lowers friction: repeating a call that sends a message
  or spends money is not safer because it can be repeated, and a hostile server
  can claim it for anything.
- An owner-connected browser server keeps its own rules (in-page tools versus
  code and file moves); the hints only affect it through the read-only row.
- The resource/prompt bridge tools of a server follow `trust_hints` as before.

### Drift

`pin_of` hashes name, title, description, input and output schema and the whole
`annotations` object (appearing, disappearing, any known field, the annotation
title). A tool whose hash differs is not registered (not callable, not in
`find_tool`/`tool_search`, not described) until the owner approves the exact
definition shown. Tests cover `destructiveHint`, `readOnlyHint`,
`openWorldHint`, `idempotentHint` and the object appearing/disappearing, in both
directions, with the description byte-identical.

Unknown vendor keys inside `annotations` are dropped when the SDK parses the
tool, so they cannot be pinned; they also cannot influence any decision,
because policy reads only the four known hints. (Changing the hash input would
force every user to re-approve every tool, so it is left as it is.)
