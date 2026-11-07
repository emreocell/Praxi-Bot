# MCP servers

> **Status: Planned for the daemon.** Praxi contains an MCP client library
> (`crates/mcp-bridge`) with tests, but the daemon (`bot serve`) does not load
> or start MCP servers yet, and there is no CLI command or desktop screen to add
> one. This page describes what the library does so you know what is coming.
> Do not expect a `mcp.toml` placed in your data directory to take effect today.

The Model Context Protocol (MCP) is a standard way for tools to be offered to AI
agents. With MCP support, any MCP server (a program you run, or a web endpoint)
becomes a set of tools a bot can use.

## What the library supports

- **Two transports:** a local program spoken to over stdin/stdout (`stdio`), or a
  streamable HTTP endpoint (`http`, with optional extra headers such as
  `Authorization`).
- **Naming:** a tool `search` from a server with id `web` is exposed as
  `mcp:web/search`.
- **Filtering:** per server, `tool_allow` and `tool_exclude` glob lists over the
  server-side tool name; the exclude list is applied after the allow list.
- **Safe by default:** tools count as *destructive* (so they ask for approval)
  unless the server marks them with the `readOnlyHint` annotation.
- **Isolation:** a stdio server only receives the environment variables you list
  for it (plus a tiny OS-essential set); nothing is inherited implicitly.
- **Resilience:** reconnects with backoff, and quarantines a server that keeps
  failing.
- **Persistence:** the planned configuration is a TOML file listing servers.

Illustrative shape of one server entry (the schema of the library, not a file
that Praxi reads yet):

```toml
[[server]]
id = "files"
enabled = true
tool_allow = ["read_*"]

[server.transport]
type = "stdio"
command = "npx"
args = ["-y", "some-mcp-server"]
[server.transport.env]
API_TOKEN = "..."
```

Server ids may use only letters, digits, `.`, `_` and `-`.

## Until then

You can extend what a bot does today with [skills](skills.md),
[workflows](workflows.md) (including a `tool` step), and the built-in tools
listed in [Bots and profiles](bots-and-profiles.md). The `script_run` tool lets a
bot run small scripts that can call other tools, and every inner call is
permission-checked like a direct one.
