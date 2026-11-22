# Use Praxi as an MCP server

`bot mcp serve` exposes one selected Praxi bot to an external MCP client over
stdin/stdout. Keep the ordinary Praxi daemon running. The MCP process connects
to its loopback HTTP facade with a scoped credential stored in the secret store;
it does not read the daemon's `api-token` file.

## Create a credential and connect

Find the bot's storage UUID with `bot bots list`, then run these commands as the
local owner. Replace the example UUID with your bot's actual UUID.

```sh
bot token create --scope mcp:tasks --bot 00000000-0000-0000-0000-000000000001 --ttl 30d
bot mcp serve --bot 00000000-0000-0000-0000-000000000001
```

The first command prints a public credential `id`, allowed bots and scopes, and
`expires_at_ms`. It stores the bearer internally and never prints its value.
The second command speaks MCP on stdout; it is intended to be launched by your
MCP client, rather than used as an interactive terminal.

When more than one active credential matches the selected bot and layers, pass
the public id explicitly:

```sh
bot mcp serve --bot 00000000-0000-0000-0000-000000000001 --token-id 00000000-0000-0000-0000-000000000002
```

For example, the owner's manual Claude Code check can register the stdio command:

```sh
claude mcp add praxi -- bot mcp serve --bot 00000000-0000-0000-0000-000000000001 --token-id 00000000-0000-0000-0000-000000000002
```

Use the same data directory and secret backend as the daemon. The OS keychain
must be accessible to the launching process. An existing encrypted file can use
the established `BOT_SECRETS_PASSPHRASE` environment setting. MCP mode never
prompts for that passphrase on stdin. Avoid putting credentials in client config
files; client configuration needs only the public bot and token ids.

`--url` accepts an HTTP loopback origin only. Redirects and HTTP proxies are
disabled for this connection. `--remote` is not supported. There is no inbound
MCP HTTP listener in this feature.

## Connect your MCP client

`bot mcp serve` is a **stdio** server: every client below starts it as a child
process and talks newline-delimited JSON-RPC on its stdin/stdout. It speaks MCP
protocol revisions 2024-11-05, 2025-03-26, 2025-06-18 and 2025-11-25 (a client
asking for a newer revision gets 2025-11-25), advertises only the `tools`
capability, and exports one tool list page (no `nextCursor`).

What every recipe needs:

- **The command.** `bot` with the arguments
  `mcp serve --bot <BOT-UUID> --token-id <CREDENTIAL-ID>`. The **bot-selection
  argument** is `--bot`, the storage UUID from `bot bots list`; one server
  process serves exactly one bot. Add `--allow tasks,computer` only for the
  computer layer. Global flags such as `--data-dir <PATH>` or
  `--secrets-backend keychain|file` go **before** `mcp`.
- **An absolute path to `bot`** (for example `C:\Tools\bot.exe` or
  `/usr/local/bin/bot`). Desktop clients launched from a menu or dock do not
  inherit your shell `PATH`.
- **The token, safely.** The bearer is never part of the client configuration.
  `bot token create` stores it in the secret store and prints only a public
  credential id; the config carries `--bot` and `--token-id`, both public. With
  the OS keychain no secret is needed in the file at all. Only an encrypted-file
  store needs `BOT_SECRETS_PASSPHRASE` in the process environment: put it in the
  client's `env` only if the config file is private to you, or better launch the
  client from a shell that already exports it. Never paste a bearer or the
  daemon's `api-token` into a client file.

The examples use `<BOT-UUID>` and `<CREDENTIAL-ID>` as placeholders.

### Claude Code

See the official [Claude Code MCP guide](https://code.claude.com/docs/en/mcp). Options
go before the server name, and `--` separates the command:

```sh
claude mcp add --scope user praxi -- bot mcp serve --bot <BOT-UUID> --token-id <CREDENTIAL-ID>
```

`--scope local` (default, this project and you), `project` (writes a shared
`.mcp.json` at the project root; commit only public ids) and `user` (all your
projects, stored in `~/.claude.json`). Equivalent `.mcp.json`:

```json
{
  "mcpServers": {
    "praxi": {
      "type": "stdio",
      "command": "bot",
      "args": ["mcp", "serve", "--bot", "<BOT-UUID>", "--token-id", "<CREDENTIAL-ID>"]
    }
  }
}
```

Check with `claude mcp list` or `/mcp`. Claude Code's MCP startup and tool-call
timeouts can be raised with the `MCP_TIMEOUT` and `MCP_TOOL_TIMEOUT`
environment variables, which matters for calls that wait for an approval.

### Claude Desktop

See the official [local MCP server quickstart](https://modelcontextprotocol.io/docs/develop/connect-local-servers).
Open Settings, Developer, Edit Config, or edit `claude_desktop_config.json`
directly:

| OS | Location |
| --- | --- |
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "praxi": {
      "command": "/usr/local/bin/bot",
      "args": ["mcp", "serve", "--bot", "<BOT-UUID>", "--token-id", "<CREDENTIAL-ID>"]
    }
  }
}
```

On Windows write backslashes doubled in JSON (`"C:\\Tools\\bot.exe"`). Restart
Claude Desktop after every edit.

### Cursor

See the official [Cursor MCP setup guide](https://prod.cursor.com/help/customization/mcp). Global file
`~/.cursor/mcp.json` (`%USERPROFILE%\.cursor\mcp.json` on Windows), or
`.cursor/mcp.json` inside a project:

```json
{
  "mcpServers": {
    "praxi": {
      "command": "bot",
      "args": ["mcp", "serve", "--bot", "<BOT-UUID>", "--token-id", "<CREDENTIAL-ID>"]
    }
  }
}
```

Save the file, restart Cursor, then open Customize → MCPs to check the server.

### VS Code (GitHub Copilot, agent mode)

See the official [VS Code MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration).
The example below uses the editor-managed format with the top-level key
`servers`. Workspace file `.vscode/mcp.json`, or the user
profile file opened with the **MCP: Open User Configuration** command (under
`%APPDATA%\Code\User\mcp.json`, `~/Library/Application Support/Code/User/mcp.json`
or `~/.config/Code/User/mcp.json`):

```json
{
  "servers": {
    "praxi": {
      "type": "stdio",
      "command": "bot",
      "args": ["mcp", "serve", "--bot", "<BOT-UUID>", "--token-id", "<CREDENTIAL-ID>"]
    }
  }
}
```

If you must pass `BOT_SECRETS_PASSPHRASE`, use VS Code's `inputs` mechanism so it
is prompted and stored in VS Code's secret storage, not in the file:

```json
{
  "inputs": [
    { "type": "promptString", "id": "praxi-pass", "description": "Praxi store passphrase", "password": true }
  ],
  "servers": {
    "praxi": {
      "type": "stdio",
      "command": "bot",
      "args": ["--secrets-backend", "file", "mcp", "serve", "--bot", "<BOT-UUID>", "--token-id", "<CREDENTIAL-ID>"],
      "env": { "BOT_SECRETS_PASSPHRASE": "${input:praxi-pass}" }
    }
  }
}
```

Start the server from the "Start" code lens above the entry, then use agent mode.
This recipe is for editor-managed MCP. The newer Copilot harness uses portable
`mcpServers` configuration; VS Code input variables are specific to the editor
format and are not forwarded to harness sessions.

### Zed

See the official [Zed MCP guide](https://zed.dev/docs/ai/mcp). Zed calls them context
servers and keeps them in the settings file (`~/.config/zed/settings.json` on
macOS and Linux, `%APPDATA%\Zed\settings.json` on Windows; open it with the
`zed: open settings file` command):

```json
{
  "context_servers": {
    "praxi": {
      "command": "bot",
      "args": ["mcp", "serve", "--bot", "<BOT-UUID>", "--token-id", "<CREDENTIAL-ID>"],
      "env": {}
    }
  }
}
```

The Agent panel settings show a green dot once the server initialized.

### Generic stdio client

Any client that can launch a command works: command `bot`, arguments as above,
no network, no extra environment. Smoke test without a client:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  | bot mcp serve --bot <BOT-UUID> --token-id <CREDENTIAL-ID>
```

### Verification status

These snippets follow the official configuration pages linked above, checked on
2026-11-22. They are **unverified in installed clients**: they were not run
against installed copies of each client, so no
client version is claimed as verified. What *is* tested in CI is the server side
every client depends on: `apps/cli/src/mcp_serve/compat_tests.rs` drives raw
JSON-RPC frames (protocol-version negotiation, capabilities, `tools/list` and
cursors, `tools/call` results with `isError`, `ping`, unknown methods, batches,
invalid JSON, oversized frames and `notifications/cancelled`), and
`apps/cli/tests/mcp_server_cancellation.rs` drives the real process against a real
loopback daemon with a fake provider: actual tool catalog and annotations,
task start/status/result/cancel, large valid input, over-limit rejection,
live expiry/revocation, rate limiting, and cancellation during a pending
computer approval. The hermetic tests use an encrypted-file credential store;
they do not verify OS keychain access from client launch environments.
If you confirm a recipe on a specific
client version, please add it here.

## Task tools

The default layer is `--allow tasks`, requiring scope `mcp:tasks`.

| Tool | Arguments | Result | Read-only hint |
| --- | --- | --- | --- |
| `bot_list` | `{}` | The selected bot's id and name | Yes |
| `task_start` | `{ "text": "...", "bot": "optional selected UUID" }` | New run id | No |
| `task_status` | `{ "run": "UUID" }` | State and external-agent attribution | Yes |
| `task_result` | `{ "run": "UUID" }` | Readiness, bounded redacted text and error flag | Yes |
| `task_cancel` | `{ "run": "UUID" }` | Cancellation acknowledgment | No |
| `schedule_list` | `{}` | This bot's schedule ids, names and enabled states | Yes |

Every task starts in its own fresh chat. Task start returns immediately; use
status/result while the bot works or waits for approval. A credential can inspect
and cancel only runs it created for the selected bot. Schedule prompts and
arbitrary chat history are not exposed by these tools.

The bot uses its configured permissions. External task text is persisted with
role `external_agent`, then presented to the model in an explicitly untrusted
data block. It cannot override bot policy. Automatic memory recall and cached
command/workflow routing are skipped. The task chat cannot accept follow-up
messages or become a later human chat; start a separate task or chat instead.

## Computer tools

Computer access requires both the credential scope and an explicit server layer:

```sh
bot token create --scope mcp:tasks,mcp:computer --bot 00000000-0000-0000-0000-000000000001 --ttl 1d
bot mcp serve --bot 00000000-0000-0000-0000-000000000001 --allow tasks,computer --token-id 00000000-0000-0000-0000-000000000002
```

`--allow computer` also works for a computer-only credential. The server lists
the selected bot's available `browser_*` and `desktop_*` tools from the existing
registry. `browser_eval` and shell tools are never exported by this layer.
The exported names are explicitly reviewed; adding a new native computer tool
requires updating this allowlist, including any future batch-action tool.

Each computer call starts a normal attributed tool run, using the same permission
engine, approval cards, computer target, control gate, timeout and audit path as
other Praxi calls. The MCP request waits for completion or approval for at most
ten minutes, then cancels the run and returns an error. A refusal also becomes an
MCP error result. No MCP operation can approve an action, release control, change
policy or change the bot's configuration.

## Approvals and authority

Every external-agent tool call requires **Approve once**, including reads and
Meta tools. Existing grants, the read shortcut and a full bot policy cannot waive
this floor. The gate resolves both the bot's existing Human policy and its
ExternalAgent policy; a deny from either wins. An explicit ExternalAgent allow
cannot relax a Human deny, and a Human allow cannot bypass a base or
ExternalAgent deny. When both permit the call, it still needs Approve once.
Approve in Praxi's existing UI, CLI or Telegram approval flow. Persistent scopes
such as Always, Always category and This chat are rejected for this initiator.

This is deliberately stricter than ordinary Praxi runs. The independent native
egress/taint work is not present on this feature's base, so this version does not
claim to implement that separate policy. It uses a persistent external source
and per-call approval instead. Both resumed runs and already persisted children
retain that source. A persisted external child's task and parent context also
receive the fixed untrusted User-data wrapper. New subagent spawning is denied
so the credential's run cap
cannot be exceeded through fan-out. Native `cli:` model sessions, including
fallbacks and child model roles, are refused because they can operate tools
outside the host's per-call gate. Use an API-backed model for MCP tasks.

These are application permissions. A program running as the same OS user is
outside that boundary and may already have filesystem or keychain access.

## Limits, revocation and storage

Each credential allows 30 tool calls per rolling minute and two active runs,
including paused runs and persisted descendants. A separate owner-created
credential has its own limits. The rate window is in memory and resets on daemon
restart; active-run admission is reconstructed from stored runs. Daemon payloads
are bounded to 64 KiB of JSON and passed through the production redactor. MCP
adds its bounded text wrapper and protocol framing. Results
are labeled as untrusted data. Inputs are limited to 32 KiB, with a 64 KiB MCP
frame limit before JSON-RPC decoding.

Credential lifetimes range from 60 seconds to 30 days. Expiry is checked on each
facade request and while computer calls wait. Revoke a credential with its public
id:

```sh
bot token revoke 00000000-0000-0000-0000-000000000002
```

Revocation invalidates the grant before cancelling outstanding tasks and their
descendants. It does not undo actions already approved and completed. Scoped
tokens can use only `GET /v1/mcp-server/tools` and
`POST /v1/mcp-server/call`; owner policy, secret, provider, MCP configuration and
approval endpoints reject them. Every paired peer role is refused on all four
MCP facade and credential-management routes.

The reserved `mcp-server/` secret-store namespace contains the bearer and grant.
Every ASCII case spelling is reserved on all platforms, including the aliases
that Windows credential storage treats as the same name.
It cannot be selected as a provider or connector secret, modified through generic
secret commands, exported in a state bundle, or imported from one. A moved
installation needs newly issued MCP credentials. Audit entries contain operation
labels and public ids only, and a failed audit write prevents the action.

## Troubleshooting

`no matching scoped token; run bot token create first` means that no live credential permits this bot and
the requested layers. Create a matching credential as the owner, or select its
id. `multiple matching scoped tokens; specify --token-id` requires that flag. An expired or
revoked credential cannot reconnect.

If a call is waiting, review the approval in Praxi. If a bot uses a native CLI
model, select an API-backed model. If the two-run cap is reached, complete or
cancel existing tasks. A 429 response is returned as an MCP tool error; wait for
the rolling rate window before retrying.

### Common failures in a client

| Message or symptom | Cause and fix |
| --- | --- |
| `cannot unlock the scoped credential store without an interactive prompt` | The process cannot reach the secret store: the OS keychain is locked or unavailable in the client's launch environment (SSH session, service, sandboxed launcher), or the store is an encrypted file and `BOT_SECRETS_PASSPHRASE` is unset. Launch the client from a normal desktop session, or select the file backend with `--secrets-backend file` and provide the passphrase through the client's environment or input mechanism. |
| `no matching scoped token; run bot token create first` right after `bot token create` | The server looked in a different data directory than the one that created the credential. Pass the same `--data-dir` (before `mcp`) to both commands, or set `BOT_DATA_DIR` for both. |
| Server shows as failed or `MCP stdio initialization failed` | The client could not start `bot` (use an absolute path), or the daemon is not running on the expected port (`--port`, `--url`). Run the exact command by hand and read its stderr. |
| `MCP daemon refused the request (401)` after a working connection | The scoped credential may have expired or been revoked. Create a matching credential and update `--token-id`. The server deliberately hides daemon error details. |
| `MCP daemon refused the request (403)` | The credential does not permit the selected bot, layer or operation. Create a matching scoped credential or choose a permitted operation. |
| `MCP daemon refused the request (429)` | The credential reached the rolling call limit or the active-run limit. Wait for the rate window, or finish/cancel an existing run. |
| A call hangs, then the client reports a timeout | The call is waiting for **Approve once** in Praxi. Approve it there, or raise the client's tool timeout (Claude Code: `MCP_TOOL_TIMEOUT`). A client-side cancel stops the call and its pending approval. |
| `tools/list` with a `cursor` fails with `-32602` | The tool list is a single page and the server never issues a cursor; send no cursor. |
| Client sends JSON-RPC batches | Batching was removed in MCP 2025-06-18 and is rejected with `-32600`. Update the client. |
| Connection closes on a very large request | A single JSON-RPC line over 64 KiB closes the transport. Keep task text small (tool inputs are limited to 32 KiB). |
