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
| `--secrets-backend <auto\|keyring\|file>` | `BOT_SECRETS_BACKEND` | `auto` | `auto` = an existing encrypted file, else the OS keychain (the file backend is chosen automatically only if `BOT_SECRETS_PASSPHRASE` is set; otherwise pass `--secrets-backend file`). The file passphrase comes from `BOT_SECRETS_PASSPHRASE` or a prompt |
| `--json` | | off | Machine-readable output (JSON, or NDJSON for `chat`) for `status`, `chat`, `bots`, `answer`, `approve`, `grants`, `schedules`, `email`, `mcp add`, `doctor`, `serve`, `backup`, `restore`, and `channels telegram pair` / `status`. `host status`, `devices`, `revoke`, `standby`, `activate`, and `workflow` already print JSON; `--json` makes `host invite` print JSON instead of a QR. Errors always go to stderr as text |
| `--port <PORT>` | `BOT_PORT` | `47821` | Port `serve` binds on `127.0.0.1`; clients connect to it |
| `--url <URL>` | `BOT_URL` | | Full daemon URL for client commands (overrides `--port`) |
| `--remote <ENDPOINT>` | | | Peer endpoint ID of a paired daemon (iroh). Conflicts with `--url` |
| `--version`, `--help` | | | |

`keychain` is accepted as an alias of `keyring` for `--secrets-backend`.

Commands that talk to a running daemon (`status`, `chat`, `bots`, `answer`,
`approve`, `grants`, `schedules`, `workflow`, `email`, `mcp add`, `token`,
`channels telegram pair`, and every `host` command except `join`) read the
API token from `BOT_API_TOKEN` when that variable is set, otherwise from
`<data dir>/api-token`. Use the same data directory as `bot serve`.
`--remote` reaches a paired daemon for `status`, `proxy`, `chat`, `bots`,
`answer`, `approve`, `grants`, `schedules`, `workflow`, `email`,
`channels telegram pair`, and `host invite`, `status`, `devices`, `revoke`,
`standby` and `activate`.
`bot --remote <ENDPOINT> proxy` opens the tunnel, prints its loopback `url` and `token` and keeps it
open until Ctrl-C (used by the [link measurement guide](../design/live-view-link-measurement.md)).
`host join`, `screen`, `admin`, `add`, `repair`, `upgrade` and `remove`,
plus `token`, `mcp`, `doctor` and `setup`, stay on this machine.
`screen`, `admin` and the SSH host commands still use the local daemon.

## Exit codes

Documented in `bot --help` and stable across client commands:

| Code | Meaning |
| --- | --- |
| 0 | OK |
| 1 | General error (also returned by `bot doctor` when any check fails) |
| 2 | Usage error |
| 3 | Daemon unreachable |
| 4 | Authentication rejected (or missing API token, or Host/Origin refused) |
| 5 | Not found |
| 6 | Request rejected (invalid input, conflict, `setup_required`, or a failing `bot email test`) |
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

Run the local daemon API on `127.0.0.1` until Ctrl-C or SIGTERM. Starts even
before a provider is configured, reporting `setup_required` until you run
`bot setup` or `bot providers add <ID>`.

| Option | Meaning |
| --- | --- |
| `--host` | Run as an always-on host (peer networking defaults on) |
| `--peer` | Enable paired remote API access over iroh |
| `--shutdown-grace <SECONDS>` | Time running tool steps get to finish on shutdown (default 10); then live runs are paused for the next start |
| `--owner-key-stdin` | Read the owner key (one line) from standard input, so it never appears in argv, a file or the environment |
| `--safe` (alias `--safe-mode`, or env `BOT_SAFE_MODE=1`) | Safe mode: chat answers without tools; tools, schedules, channels, MCP servers, apps, memory work, desktop and peer networking stay off. Nothing is deleted or changed. Cannot be combined with `--host` or `--peer`. See [Safe mode](troubleshooting.md#safe-mode-start-with-everything-off) |
| `--fake-provider` | Only in builds with the `fake-provider` feature: use a built-in echo provider (demos, tests) |

### `bot doctor`

Check installation health (data dir writability and free disk space, config,
providers, daemon, browser, and more). Never prompts. Exits 1 when any check
fails.

| Option | Meaning |
| --- | --- |
| `--online` | Also test provider connections and peer relay reachability over the network |
| `--report <FILE>` | Write a redacted diagnostic bundle (JSON, no secrets) to this file |

`--json` prints the checks as JSON.

### `bot backup create | verify | restore`

`bot backup create <FILE>` writes one passphrase-encrypted file with your bots, chats,
memory, skills, workflows, schedules and provider settings. It never overwrites an existing
file. `verify` decrypts and checks the file without touching the data directory. `restore`
unpacks it into this installation; stop the daemon first (it refuses while `bot serve` holds
the data directory or a daemon answers on `--port`). See
[Encrypted backup and restore](backup.md).

The passphrase comes from a hidden prompt, `--passphrase-file <FILE>` or
`--passphrase-env <VAR>` (the variable name); it is never a command-line value.

| Option | Commands | Meaning |
| --- | --- | --- |
| `--passphrase-file <FILE>` | all | Read the passphrase from the first line of a file |
| `--passphrase-env <VAR>` | all | Read the passphrase from this environment variable |
| `--include-secrets` | `create` | Also store API keys and tokens (encrypted) in the backup |
| `--include-secrets` | `restore` | Write the backup's API keys into the secret store (default: skip) |
| `--replace` | `restore` | Swap existing bots and chats for the backup (the old state goes to a `backup-*` folder) |
| `--allow-policy-relaxation` | `restore` | Accept a backup whose permission policy is more permissive |

### `bot restore <FILE>`

Restore a plaintext archive made by an older version. Stop the daemon first;
it refuses while `bot serve` holds the data directory or a daemon answers on
`--port`. Encrypted backups use `bot backup restore`. See
[Restoring older plaintext backups](backup-and-restore.md).

| Option | Meaning |
| --- | --- |
| `--replace` | Swap existing bots and chats for the backup; the old state goes to a `backup-*` folder inside the data directory |
| `--allow-policy-relaxation` | Accept a backup whose permission policy is more permissive than the current one |

### `bot status`

Show daemon health and version, uptime, and the number of pending approvals, and
verify the API token. Before a provider is configured, `bot status` reports
`setup required` and prints the next command to run (`bot setup` or
`bot providers add <id>`).

### `bot proxy`

Open a loopback HTTP proxy tunnel to a paired remote daemon and keep it open
until `Ctrl-C`. Requires `--remote <ENDPOINT>` (conflicts with `--url`):

```sh
bot --remote <ENDPOINT> proxy
```

`bot proxy` waits up to 20 seconds for the iroh tunnel to connect, then prints
the loopback `url:` and `token:`. **Security:** the printed token grants
desktop-role access to the paired host on every route (including `/v1/secrets`);
do not share or log it, pass it to tools via `BOT_API_TOKEN` rather than a
command-line argument, and press `Ctrl-C` as soon as you finish. Used by the
[live-view link measurement guide](../design/live-view-link-measurement.md)
(`BOT_API_TOKEN=<token> node scripts/tools/measure-live-link.mjs --url <url> --seconds 30`).

### `bot chat <BOT_ID> <MESSAGE>`

Send a message and stream the answer.

| Option | Meaning |
| --- | --- |
| `--chat <CHAT_ID>` | Continue an existing chat instead of starting one |
| `--mode <steer\|queue>` | What to do if the chat has a running turn (default `queue`) |

### `bot chats`

List and export saved chats. Export is owner-only and uses the same Markdown
and JSON files as the desktop app.

| Subcommand | Meaning |
| --- | --- |
| `bot chats list [--bot <BOT_ID>]` | List chats; `--json` prints the API array |
| `bot chats export <CHAT_ID>` | Write one chat to stdout. Options: `--format md\|json` (default `md`; global `--json` forces JSON), `-o, --output <FILE>` writes a private file (mode `0600` on Unix; an existing path or a symlink is refused) and `--force` replaces an existing regular file |

### `bot bots`

| Subcommand | Meaning |
| --- | --- |
| `bot bots list` | List bots |
| `bot bots add <FILE>` | Create a bot from a profile TOML file |

### `bot approve <TOOL_CALL_ID>`

Approve or deny a pending tool call.

| Option | Meaning |
| --- | --- |
| `--scope <once\|this_chat\|always\|always_category\|run_control>` | How far the approval reaches (default `once`); `run_control` only where the card offers it |
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
| `bot schedules add <BOT_ID> <WHEN> <PROMPT>` | Add a schedule. `<WHEN>` is a phrase such as "every 15 minutes", "in 2 hours" or "her gün 09:00" (English or Turkish, parsed by the daemon). Options: `--name <NAME>` (default: the start of the prompt), `--tz <IANA_TZ>` (default UTC), `--chat <CHAT_ID>` (default: a chat named after the schedule) |
| `bot schedules rm <ID>` | Delete a schedule |
| `bot schedules run-now <ID>` | Fire a schedule now, in the background |

### `bot email`

Manage the e-mail accounts of the running daemon (see [email](email.md)). Changes apply to the next tool call.

| Subcommand | Meaning |
| --- | --- |
| `bot email list` | List accounts (never shows passwords) |
| `bot email add <NAME> <ADDRESS>` | Add an account (`--provider` defaults to `imap_smtp`). The name may instead be `--name <NAME>` (not both). Options: `--preset gmail\|outlook\|yahoo\|icloud`, `--imap-host` (port 993, TLS), `--imap-port`, `--smtp-host`, `--smtp-port` (465 with implicit TLS, 587 with `start_tls`), `--smtp-security implicit\|start_tls`, `--username` (when the login differs from the address), `--bot <NAME_OR_ID>` (only this bot) or `--all-bots` (one of the two is required), `--allowed-sender <ADDR>` (repeatable), `--password-env <VAR>` (hidden prompt when omitted) |
| `bot email add --provider agentmail --name <NAME> --bot <NAME_OR_ID>\|--all-bots` | Create an AgentMail inbox with **your own** AgentMail API key and add it as an account. Options: `--name-hint <TEXT>` (part before the `@`; default: the account name), `--domain <VERIFIED_DOMAIN>` (default: the shared `agentmail.to`), `--allowed-sender`, `--api-key-env <VAR>` (hidden prompt when omitted; the key is never an argument) |
| `bot email rm <NAME>` | Remove an account and its stored password or key. `--delete-inbox` also deletes the inbox at AgentMail (irreversible; off by default) |
| `bot email test <NAME>` | IMAP login and SMTP authentication, or the AgentMail key. Nothing is sent. Exit 6 when the check does not pass |

### `bot providers`

| Subcommand | Meaning |
| --- | --- |
| `bot providers list` | List configured providers (`*` = global default) |
| `bot providers detect-local` | Look for a local model server (Ollama, LM Studio, llama.cpp) on loopback and print what it found as JSON. Contacts only `127.0.0.1` |
| `bot providers add <ID>` | Add or update a provider. Built-in catalog IDs: `anthropic`, `deepseek`, `gemini`, `gemini-openai-compat`, `lmstudio`, `ollama`, `openrouter`. Options: `--base-url <URL>` (custom OpenAI-compatible provider), `--key-env <VAR>`, `--model <MODEL>`, `--no-test` |
| `bot providers test [ID]` | Test a provider's connection (the default provider when omitted) |
| `bot providers default <ID>` | Set the global default provider. Option: `--model <MODEL>` |

### `bot catalog`

Validate the curated model catalog, check its freshness, or compare it with a
provider model list (see [model catalog](model-catalog.md)). Offline by
default; `--live` is the only network call and is opt-in.

| Subcommand | Meaning |
| --- | --- |
| `bot catalog check` | Validate `model_metadata.json`, the provider catalog entries, and optionally the docs table. Options: `--metadata <FILE>`, `--docs <FILE>`, `--max-age-days <DAYS>` (default 90), `--strict` (exit non-zero when stale) |
| `bot catalog diff <PROVIDER> [SNAPSHOT]` | Compare the committed catalog for a provider with a JSON or TOML model-list snapshot. Options: `--live` (query the configured provider with its stored key), `--metadata <FILE>` |
| `bot catalog render-docs` | Print the Markdown provider table generated from `model_metadata.json`. Option: `--metadata <FILE>` |

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

### `bot completions <SHELL>`

Print a completion script to standard output for `bash`, `zsh`, `fish`,
`powershell`, or `elvish`. After installing the script, open a new shell session,
type `bot` followed by a space, and press Tab.

#### Bash

```bash
mkdir -p "$HOME/.local/share/bot"
bot completions bash > "$HOME/.local/share/bot/bot.bash"
```

Add this line to `~/.bashrc`. If you use a login shell, make sure your login
profile sources `~/.bashrc`.

```bash
. "$HOME/.local/share/bot/bot.bash"
```

#### Zsh

```zsh
mkdir -p "$HOME/.zfunc"
bot completions zsh > "$HOME/.zfunc/_bot"
```

Add `fpath=("$HOME/.zfunc" "${fpath[@]}")` to `~/.zshrc` before `compinit`
runs. If your configuration does not yet initialize completions, add these
lines after setting `fpath`.

```zsh
autoload -Uz compinit
compinit
```

#### Fish

Fish loads `bot.fish` on demand from its completions directory. If you set
`XDG_CONFIG_HOME`, use `$XDG_CONFIG_HOME/fish/completions` in both commands.

```fish
mkdir -p ~/.config/fish/completions
bot completions fish > ~/.config/fish/completions/bot.fish
```

#### PowerShell

```powershell
$profileDir = Split-Path -Parent $PROFILE
New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
bot completions powershell | Set-Content -Encoding utf8 (Join-Path $profileDir 'bot-completions.ps1')
```

Open the file at `$PROFILE` in your editor, creating it if needed, and add this
line.

```powershell
. (Join-Path (Split-Path -Parent $PROFILE) 'bot-completions.ps1')
```

#### Elvish

```elvish
bot completions elvish > ~/.bot-completions.elv
```

Add this line to your `rc.elv`. For its location, see the
[Elvish startup file reference](https://elv.sh/ref/command.html#rc-file).

```elvish
eval (slurp < ~/.bot-completions.elv)
```

### `bot man-pages <OUTPUT_DIR>`

Generate section 1 manual pages for `bot` and its visible subcommands, including
nested commands. Command names are joined with hyphens, as in
`bot-channels-telegram-setup.1`.

```sh
bot man-pages target/man
man -l target/man/bot-channels-telegram-setup.1
```

On macOS, use `man ./target/man/bot-channels-telegram-setup.1` to preview the file.

When building from source, run `cargo run -p cli -- man-pages target/man` from
the repository root to generate the same pages. For a package, set `DESTDIR`
to your staging directory and install the files under `usr/share/man/man1`.

```sh
install -d "${DESTDIR:?Set DESTDIR to the package staging directory}/usr/share/man/man1"
install -m 0644 target/man/*.1 "$DESTDIR/usr/share/man/man1/"
```

Once the package is installed, open the pages with `man 1 bot` or
`man 1 bot-channels-telegram-setup`.

### `bot skills`

Review, install and manage skills. These are local, explicit commands on this
machine; they are not daemon commands and never agent tools, so a bot cannot
install a skill.

| Subcommand | Meaning |
| --- | --- |
| `bot skills review <NAME>` | Print every file in full and its review fingerprint. Option: `--bot-id <BOT_ID>` |
| `bot skills trust <NAME> --reviewed <FINGERPRINT>` | Trust exactly the content `review` printed. Option: `--bot-id <BOT_ID>`. A later change needs another review |
| `bot skills install <SOURCE>` | Install from an `https://`/`ssh://` git URL, an https index (`--name <SKILL>`) or a local folder. Prints the risk report first. Options: `--rev`, `--subdir`, `--name`, `--accept <FINGERPRINT>` |
| `bot skills update [NAME]` | Re-fetch hub-installed skills, show what changed (new findings, risk, commit) and ask before replacing. `--accept <FINGERPRINT>` with a NAME |
| `bot skills uninstall <NAME>` | Remove the skill folder, its lock entry and its usage counters |
| `bot skills list` | Name, version, source, pinned commit, risk, usage; flags missing or locally modified skills |
| `bot skills sync` | Reinstall locked skills whose folder is missing, verifying the locked hash |

See [Skills: install a skill](skills.md#install-a-skill).

### `bot token`

Create or revoke scoped MCP credentials on the local daemon (see
[MCP server](mcp-server.md)). Prints JSON. Not available with `--remote`.

| Subcommand | Meaning |
| --- | --- |
| `bot token create --bot <UUID>[,UUID...]` | Store a credential and print its public id (never the bearer). `--bot` is required (comma-separated). `--scope <mcp:tasks\|mcp:computer>[,...]` (default `mcp:tasks`). `--ttl <DURATION>` (default `30d`; for example `12h` or `600s`; from 60 seconds to 30 days) |
| `bot token revoke <ID>` | Revoke a credential and cancel its running tasks |

### `bot mcp`

Manage the daemon's MCP servers (see [MCP servers](mcp-servers.md) and
[MCP server](mcp-server.md)).

| Subcommand | Meaning |
| --- | --- |
| `bot mcp add [OPTIONS] <PASTE>...` | Add servers from a paste: a server URL, a `claude mcp add ...` line, an `npx` / `uvx` command line, or MCP JSON (`{"mcpServers": ...}`). Quote the paste, or pass `-` to read it from stdin. Values in the paste (`-e KEY=value`, headers, `env`) go to the secret store, never the config. Put flags before the paste. `--name <NAME>` (default: derived from the URL or package). `--dry-run` shows what would be added and contacts nothing |
| `bot mcp serve --bot <UUID>` | Expose that bot as a scoped Praxi MCP server on stdio, using a token from `bot token create`. Loopback daemon only (not `--remote`). `--allow <tasks\|computer>[,...]` (default `tasks`; `computer` must be requested explicitly). `--token-id <UUID>` is the public id from `token create` when more than one credential matches (never a bearer token) |

### `bot host`

Manage the active host and paired devices (see [remote host](remote-host.md)
and [host operations](host-operations.md)).

| Subcommand | Meaning |
| --- | --- |
| `bot host join <TICKET>` | Pair this fresh installation with a desktop before starting the service. Local only |
| `bot host invite --role <phone\|desktop\|node\|host>` | Create a single-use invite and print its QR and confirmation code. `--role` is required |
| `bot host status` | Show host status (JSON) |
| `bot host devices` | List paired devices (JSON) |
| `bot host revoke <KEY>` | Revoke a paired device |
| `bot host standby` | Stop automatic components and persist standby ownership |
| `bot host activate` | Take ownership with an epoch above every known epoch. `--force` uses this installation's last local data; the command warns that newer changes on the other leader may be missing and that missed schedules can duplicate actions |
| `bot host screen [KEY]` | Let a paired desktop watch, and take over, this host's screen. Run it on the host; the grant survives restarts. Give a device key from `bot host devices`, or `--all-desktops` (exactly one of the two). `--view-only` watches without takeover or input. `--revoke` removes the grant |
| `bot host admin [KEY]` | Let a paired desktop send model keys, bots and settings to this host. Run it on the host. Give a device key, or `--all-desktops`. `--revoke` withdraws it. New desktop invites already grant this |
| `bot host add <TARGET>` | Install Praxi on your own Ubuntu/Debian server over SSH and pair it with this computer (idempotent; run it again to resume). `<TARGET>` is `user@server` or a saved name. `--ssh-port <PORT>` (default 22; not the daemon `--port`). `--key <FILE>` (otherwise the ssh-agent; passwords are not supported). `--name <NAME>` (default: derived from the address). `--yes` accepts a new host key without asking; a changed key is never accepted. `--no-desktop` skips the virtual display and desktop session |
| `bot host repair <TARGET>` | Re-run the install, migrating a plain `bot serve` install to host mode in place (data is kept and snapshotted first). Same target options as `add`: `--ssh-port <PORT>`, `--key <FILE>`, `--name <NAME>`, `--yes`, `--no-desktop` |
| `bot host upgrade <TARGET>` | Install this app's version on a server that runs an older one. Same target options as `add`: `--ssh-port <PORT>`, `--key <FILE>`, `--name <NAME>`, `--yes`, `--no-desktop` |
| `bot host remove <NAME>` | Forget a server and revoke this computer on it. `<NAME>` is a saved name or `user@server`. `--uninstall` also uninstalls Praxi on the server and keeps its data. `--purge` uninstalls and deletes the server's data, config and praxi user (conflicts with `--uninstall`); `--yes` skips the typed confirmation for `--purge`. `--ssh-port` and `--key` are the same as on `add` |

`add`, `repair`, `upgrade` and `remove` run in this terminal over SSH. They are
not daemon subcommands of their own, and `--remote` does not apply to them.

### `bot workflow`

Record, review, save, and replay workflows from successful runs (see
[workflows](workflows.md)). Prints JSON.

| Subcommand | Meaning |
| --- | --- |
| `bot workflow list --bot <BOT_ID>` | List saved workflows for a bot |
| `bot workflow show <NAME> --bot <BOT_ID>` | Show every step and parameter of a saved workflow |
| `bot workflow propose <RUN_ID>` | Print an unsaved workflow proposal from a completed run for review |
| `bot workflow save <FILE>` | Save a reviewed proposal JSON file (`--name <NAME>`, `--description <TEXT>`) |
| `bot workflow delete <NAME> --bot <BOT_ID>` | Delete a saved workflow |
| `bot workflow run <NAME> --chat <CHAT_ID>` | Replay a saved workflow in an existing chat (`--params-stdin` reads a JSON parameter object from stdin) |

### `bot self-update`

Update the Linux installation. Restart is separate; this command does not
restart a service. On other operating systems it prints a note to use the
platform installer and exits 0.

| Option | Meaning |
| --- | --- |
| `--check` | Show the available release without downloading or installing it. Conflicts with `--rollback` |
| `--version <TAG>` | Release tag (a leading `v` is optional). Conflicts with `--rollback` |
| `--rollback` | Switch to the previously installed version |
| `--prefix <PATH>` | Installation prefix (normally inferred from the installed executable) |

## Other binaries

- `bot-node`: the node-agent for controlling another machine (experimental, see
  the [FAQ](faq.md)). Commands: `join <bot-pair:...> [--name]`, `run`, `devices`;
  global `--data-dir`, `--relay <default|none|URL>` (default `default`) and
  `--display <X11>` (for example `:99`).
- `bot-desktop`: the Windows desktop app.
- `bot-eval`: offline and live evaluation runner (`crates/evals`, subcommands
  `run`, `compare`, `report`, `list`), including the
  [prompt-injection evaluation pack](security.md#prompt-injection-evaluation-pack)
  (`bot-eval run --tier 1 --filter inj- --repeat 3`; see
  [`evals/baselines/README.md`](../../evals/baselines/README.md)).

## The HTTP API

Everything the CLI and desktop app do goes through the daemon's HTTP API. The
OpenAPI description is in [`docs/api/openapi.json`](../api/openapi.json). Notable
routes without a CLI command: policy (`/v1/policy`), usage and cost
(`/v1/usage`), run control (`/v1/runs/{id}/cancel`, `/resume`, `/remember`),
provider reload, bot update/delete, and schedule update.
