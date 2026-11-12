# MCP servers

The daemon (`bot serve`) loads `<data dir>/mcp.toml` at startup and **runs** every
enabled server in that file. A `stdio` server is a program the daemon starts
as the same user the daemon runs as: it can read and write whatever that user
can. An `http` server is a URL the daemon calls.

You can also add, change and remove servers while the daemon is running, with
`POST`, `PUT` and `DELETE` on `/v1/mcp/servers` (see
[`docs/api/openapi.json`](../api/openapi.json)). Every change is written back
to `mcp.toml` before it is applied. `GET /v1/mcp/servers` returns the stored
config: tool filters, the working directory, plain environment variables,
header names, and secret **names**. It never returns a secret value. Store
values with `POST /v1/secrets/{name}` (write-only).

`PUT /v1/mcp/servers/{name}` merges into the stored server. A field you leave
out keeps its previous value. `bots: null` means every bot.

## What a server becomes

- **Two transports:** a local program spoken to over stdin/stdout (`stdio`), or a
  streamable HTTP endpoint (`http`).
- **Naming:** a tool `search` from a server with id `web` is exposed as
  `mcp:web/search`.
- **Filtering:** per server, `tool_allow` and `tool_exclude` glob lists over the
  server-side tool name; the exclude list is applied after the allow list.
- **`readOnlyHint` is ignored unless you opt in.** Tools are *destructive*
  (they ask for approval) even when the server marks them read-only. Set
  `trust_hints = true` on that server only if you trust it to lower its own
  tools to read-only.
- **Who may use it:** `bots` is a list of bot ids (the bot's storage UUID, not
  its profile id), or omitted / `null` for every bot. An empty list `[]` means
  no bot at all, never every bot. A bot that is not listed does not see the
  server's tools in `find_tool` or the tool list, and a call from it is refused.
  In the desktop app set this per server in Apps (Bots button) or per bot in the
  bot form (Apps card). Deleting a bot removes its id from every server's list;
  a list that becomes empty stays `[]`.
- **Isolation of the environment:** a stdio server only receives the variables
  you list (plus a tiny OS-essential set). Nothing else is inherited. The
  program itself still runs as the daemon user.
- **Commands that the shell tool would always flag are refused.** The same
  never-list that forces approval for `shell_run` (for example `rm -rf /`)
  rejects the stdio `command` and `args` before the process is started, both
  for `mcp.toml` at startup and for add/update through the API.
- **Resilience:** reconnects with backoff, and quarantines a server that keeps
  failing. On shutdown the daemon closes each session and waits for the child
  to exit. Stdio servers are started in their own process group (Unix) or a
  Windows job that dies when the job closes, so an `npx` or `uvx` wrapper and
  the process it launched are both stopped.

## `mcp.toml`

The file lives in the data directory (`BOT_DATA_DIR`, or the OS application
data directory for Praxi). The daemon creates it if it is missing. If the file
cannot be parsed, no server from it is started and the daemon will not
overwrite it until you fix it and restart.

```toml
[[server]]
id = "files"
enabled = true
category = "fs"          # accounts (default) | fs | shell | browser | ...
trust_hints = false      # true: believe this server's readOnlyHint
# bots = ["bot-a"]       # omit or null: every bot
tool_allow = ["read_*"]

[server.transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem@2.2.0", "."]

[server.secret_env]
API_KEY = "mcp/files-key"   # name of a secret, never its value
```

Server ids may use only letters, digits, `.`, `_` and `-`. `category = "meta"`
is rejected. Catalog packages are pinned to an exact version (`name@1.2.3` or
`name==1.2.3`); `@latest` and a bare `npx -y` / `uvx` package are not accepted
in the curated catalog.

A stdio `command` is executed. Do not point it at a shell one-liner or at a
program you would not run yourself in a terminal as the daemon user.
