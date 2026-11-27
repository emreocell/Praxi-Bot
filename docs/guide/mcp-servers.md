# MCP servers

The daemon (`bot serve`) loads `<data dir>/mcp.toml` at startup and **runs** every
enabled server in that file. A `stdio` server is a program the daemon starts
as the same user the daemon runs as: it can read and write whatever that user
can. An `http` server is a URL the daemon calls. For connecting apps through the
desktop **Apps** dialog (direct OAuth apps, Composio, per-bot access, and
signing out), see [Connecting apps](apps.md).

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
- **Do you trust a server to label its own tools?** Every MCP tool may carry
  labels (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`).
  They are the server's own claims, so they count only if you say you trust
  that server, once, when you add it (Apps: "Do you trust this server to
  describe its own tools honestly?", default **No, ask me for every action**).
  You can change it later in the server's Manage panel; the card shows
  "Labels trusted" or "Always asks". In `mcp.toml` it is `trust_hints`, default
  `false`, and every change is written to the audit log.
  - **No (default):** every tool of the server asks, whatever it claims.
  - **Yes:** a tool labelled read-only (and not also destructive) runs without
    asking. Anything else still asks, including a tool with no labels at all,
    a write that calls itself "non-destructive" (you can still use "allow for
    this task" on its card) and a tool marked only "idempotent". A tool marked
    `openWorldHint` (it reaches the web, e-mail or other outside systems) is
    treated as able to send data out, even if it only reads.
  - **If a label changes later**, for example a tool that said "destructive"
    now says "not destructive", the tool is held back until you approve it
    again, exactly like a changed description. The labels are part of what is
    pinned.
  Full table: [MCP annotation policy](../design/mcp-annotations.md).
- **What the results are.** An MCP result is untrusted. It is also a private
  read unless `public_content = true`, and every tool is treated as able to
  send data off the machine unless `local_only = true`. Those two flags are
  how the egress gate decides whether a later send must ask. Both default to
  false.
- **A browser server.** `browser = true` tells Praxi you connected this
  server as a browser (Playwright and similar). The built-in Playwright preset
  counts as one without it. Its in-page tools (navigate, snapshot, click,
  type, evaluate, ...) then do not ask after a web page alone and ask once the
  task also read something private; code outside the page and file transfers
  always ask. Only you can set it (config or the owner-only API). See
  [Security model, item 12a](security.md).
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
public_content = false   # true: results are not treated as private reads
local_only = false       # true: tools cannot send data off this machine
browser = false          # true: you connected this server as a browser (security.md 12a)
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
in the curated catalog. Every catalog entry also notes its licence and what
signing in needs; see [What you need to connect each app](app-catalog.md).

A stdio `command` is executed. Do not point it at a shell one-liner or at a
program you would not run yourself in a terminal as the daemon user.

For HTTP OAuth, client identity is optional (CIMD, pre-registered client, then
legacy dynamic registration). A confidential client uses a secret **name**:

```toml
[[server]]
id = "remote"
[server.transport]
type = "http"
url = "https://example.com/mcp"
[server.oauth]
client_id = "registered-client"
client_secret_ref = "mcp/remote-client-secret"
# client_metadata_url = "https://your-domain.example/mcp-client.json"
# allow_loopback_http = true   # local development only; default false
```

Two more optional `[server.oauth]` fields: `scopes = ["..."]` (what the
sign-in asks for; empty means what the server advertises) and
`offline_access = true` (also ask for a refresh token, which Google needs to stay
signed in). The Google cards use both; see
[Google Workspace](google-workspace.md).

Store that secret using `bot secrets set mcp/remote-client-secret`; never put
its value in `mcp.toml`. Access and refresh tokens live in the secret store
under `mcp/remote/oauth`. Client secrets are resolved again on reconnect so
rotation takes effect. OAuth settings are refused on stdio transports.

### Signing in when the daemon runs on another computer

`POST /v1/mcp/servers/{name}/auth/start` takes an optional body
`{"relay_port": 53123}`. The sign-in page opens in the browser of the computer
you are at; the authorization server sends that browser back to
`http://127.0.0.1:53123/callback` on **that** computer, where the desktop app
listens on the given port for exactly that redirect. The app forwards the
address to `POST /v1/mcp/servers/{name}/auth/callback` over the paired link. The
daemon opens no listener in this mode, accepts the address once, and checks that
it is exactly the redirect it announced (`redirect_uri` in the answer) and that
its `state` matches before it trusts it. Ports below 1024 are refused. Both
routes are owner-only, and on a host only its administrator desktop may call
them.
