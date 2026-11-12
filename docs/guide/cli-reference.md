# CLI reference

The command is `bot`. This page follows the command definitions in
`apps/cli/src/main.rs`; `bot --help` and `bot <command> --help` always show the
exact build you have.

```
bot [GLOBAL OPTIONS] <COMMAND>
```

Running `bot` with no command prints a hint to run `bot setup`.

## Global options

| Option | Env var | Default | Meaning |
| --- | --- | --- | --- |
| `--data-dir <PATH>` | `BOT_DATA_DIR` | OS app-data dir | Data directory |
| `--catalog-dir <PATH>` | `BOT_CATALOG_DIR` | built-in set | Directory of provider catalog `*.toml` files |
| `--secrets-backend <auto\|keyring\|file>` | `BOT_SECRETS_BACKEND` | `auto` | `auto` = an existing encrypted file, else the OS keychain, else an encrypted file. The file passphrase comes from `BOT_SECRETS_PASSPHRASE` or a prompt |
| `--json` | | off | Machine-readable output (JSON, or NDJSON for `chat`) for `status`, `chat`, `bots` and `approve`. Errors always go to stderr as text |
| `--port <PORT>` | `BOT_PORT` | `47821` | Port `serve` binds on `127.0.0.1`; clients connect to it |
| `--url <URL>` | `BOT_URL` | | Full daemon URL for client commands (overrides `--port`) |
| `--version`, `--help` | | | |

Commands that talk to a running daemon (`status`, `chat`, `bots`, `answer`,
`approve`, `grants`, `schedules`, `email`, `channels telegram pair`) read the API token
from `<data dir>/api-token`; make sure they use the same data directory as
`bot serve`.

## Exit codes

For the client commands:

| Code | Meaning |
| --- | --- |
| 0 | OK |
| 1 | Error |
| 2 | Usage error |
| 3 | Daemon unreachable |
| 4 | Authentication rejected (or Host/Origin refused) |
| 5 | Not found |
| 6 | Request rejected (invalid input, conflict) |
| 7 | The run ended with an error or was cancelled |

## Commands

### `bot setup`

Interactive first-run setup (language, provider, key, model, first bot,
permission preset), or scripted when `--provider` is given.

| Option | Meaning |
| --- | --- |
| `--provider <ID>` | Provider id; switches to non-interactive mode |
| `--base-url <URL>` | Makes `--provider` a custom OpenAI-compatible provider (requires `--provider`) |
| `--key-env <VAR>` | Environment variable holding the API key (requires `--provider`) |
| `--model <MODEL>` | Default model (first listed when omitted; requires `--provider`) |
| `-y`, `--yes` | Never prompt (required with `--provider`) |
| `--lang <en\|tr>` | Language of messages saved with the bot (default `en`) |
| `--bot-preset <general\|browser\|desktop>` | First bot preset (default `general`) |
| `--bot-name <NAME>` | First bot name |
| `--permission <read-only\|read-and-draft\|approve-destructive\|full>` | Default permission preset (default `approve-destructive`) |

### `bot serve`

Run the local daemon API on `127.0.0.1` until Ctrl-C or SIGTERM. Needs a usable
provider.

| Option | Meaning |
| --- | --- |
| `--shutdown-grace <SECONDS>` | Time running tool steps get to finish on shutdown (default 10); then live runs are paused for the next start |
| `--fake-provider` | Only in builds with the `fake-provider` feature: use a built-in echo provider (demos, tests) |

### `bot status`

Daemon health and version, uptime, and the number of pending approvals. Also
verifies the API token.

### `bot chat <BOT_ID> <MESSAGE>`

Send a message and stream the answer.

| Option | Meaning |
| --- | --- |
| `--chat <CHAT_ID>` | Continue an existing chat instead of starting one |
| `--mode <steer\|queue>` | What to do if the chat has a running turn (default `queue`) |

### `bot bots`

| Subcommand | Meaning |
| --- | --- |
| `bot bots list` | List bots |
| `bot bots add <FILE>` | Create a bot from a profile TOML file |

### `bot approve <TOOL_CALL_ID>`

Approve or deny a pending tool call.

| Option | Meaning |
| --- | --- |
| `--scope <once\|this_chat\|always\|always_category>` | How far the approval reaches (default `once`) |
| `--deny` | Deny instead of approving |
| `--reason <TEXT>` | Reason given to the model; only with `--deny` |

### `bot answer <CALL_ID> <TEXT>`

Answer a pending question card.

### `bot grants`

| Subcommand | Meaning |
| --- | --- |
| `bot grants list <BOT_ID>` | List a bot's persisted "always allow" grants |
| `bot grants revoke <BOT_ID> --tool <NAME>` | Remove the grant of one tool |
| `bot grants revoke <BOT_ID> --category <NAME>` | Remove the grant of a permission category (`fs`, `shell`, ...) |

`--tool` and `--category` are alternatives; give exactly one.

### `bot schedules`

| Subcommand | Meaning |
| --- | --- |
| `bot schedules list` | List schedules |
| `bot schedules add <BOT_ID> <WHEN> <PROMPT>` | Add a schedule. Options: `--name <NAME>`, `--tz <IANA_TZ>`, `--chat <CHAT_ID>` |
| `bot schedules rm <ID>` | Delete a schedule |
| `bot schedules run-now <ID>` | Fire a schedule now, in the background |

### `bot email`

Manage the e-mail accounts of the running daemon (see [email](email.md)). Changes apply to the next tool call.

| Subcommand | Meaning |
| --- | --- |
| `bot email list` | List accounts (never shows passwords) |
| `bot email add <NAME> <ADDRESS>` | Add an account. Options: `--preset gmail\|outlook\|yahoo\|icloud`, `--imap-host`, `--imap-port`, `--smtp-host`, `--smtp-port`, `--smtp-security implicit\|start_tls`, `--username`, `--bot <NAME_OR_ID>` (only this bot) or `--all-bots` (one of the two is required), `--allowed-sender <ADDR>` (repeatable), `--password-env <VAR>` (hidden prompt when omitted) |
| `bot email rm <NAME>` | Remove an account and its stored password |
| `bot email test <NAME>` | IMAP login and SMTP authentication, nothing is sent; exit 6 when a server does not accept |

### `bot providers`

| Subcommand | Meaning |
| --- | --- |
| `bot providers list` | List configured providers (`*` = global default) |
| `bot providers add <ID>` | Add or update a provider. Options: `--base-url <URL>` (custom OpenAI-compatible), `--key-env <VAR>`, `--model <MODEL>`, `--no-test` |
| `bot providers test [ID]` | Test a connection (the default provider when omitted) |
| `bot providers default <ID>` | Choose the global default. Option: `--model <MODEL>` |

### `bot secrets`

| Subcommand | Meaning |
| --- | --- |
| `bot secrets list` | List secret names (never values) |
| `bot secrets set <NAME>` | Store a secret; value from `--value-env <VAR>` or a hidden prompt, never from the command line |
| `bot secrets delete <NAME>` | Delete a secret |

### `bot channels telegram`

| Subcommand | Meaning |
| --- | --- |
| `setup [--token-env <VAR>]` | Store the BotFather token (hidden prompt) and enable the channel |
| `pair` | Print a one-time 6-digit pairing code (valid 10 minutes) from the running daemon |
| `status` | Show whether the channel is configured, running, and how many users are paired |
| `disable` | Turn the channel off (the token stays stored) |

## Other binaries

- `bot-node`: the node-agent for controlling another machine (experimental, see
  the [FAQ](faq.md)). Commands: `join <bot-pair:...> [--name]`, `run`, `devices`;
  global `--data-dir` and `--relay <default|none|URL>`.
- `bot-desktop`: the Windows desktop app.

## The HTTP API

Everything the CLI and desktop app do goes through the daemon's HTTP API. The
OpenAPI description is in [`docs/api/openapi.json`](../api/openapi.json). Notable
routes without a CLI command: policy (`/v1/policy`), usage and cost
(`/v1/usage`), run control (`/v1/runs/{id}/cancel`, `/resume`, `/remember`),
provider reload, bot update/delete, and schedule update.
