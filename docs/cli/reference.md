# Generated CLI Reference (`bot`)

Generated from the `clap` command definitions in `apps/cli/src/main.rs`.
Do not edit by hand; regenerate with:

```sh
UPDATE_CLI_REFERENCE=1 cargo test -p cli --test cli_reference
```

For usage prose and examples, see [`docs/guide/cli-reference.md`](../guide/cli-reference.md).

## Global Options

- `--catalog-dir`
- `--data-dir`
- `--help`
- `--json`
- `--port`
- `--remote`
- `--secrets-backend`
- `--url`
- `--version`

## Environment Variables

- `BOT_CATALOG_DIR`
- `BOT_PORT`
- `BOT_SECRETS_BACKEND`
- `BOT_URL`

## Command Tree

| Command | Summary | Subcommands | Options |
| --- | --- | --- | --- |
| `bot` | Praxi-Bot agent CLI | `workflow`, `completions`, `man-pages`, `skills`, `token`, `host`, `backup`, `restore`, `setup`, `doctor`, `secrets`, `providers`, `catalog`, `serve`, `self-update`, `status`, `proxy`, `chat`, `bots`, `answer`, `grants`, `schedules`, `email`, `mcp`, `channels`, `approve` | `--catalog-dir`, `--data-dir`, `--json`, `--port`, `--remote`, `--secrets-backend`, `--url` |
| `bot answer` | Answer a pending question from a bot | — | — |
| `bot approve` | Approve or deny a pending tool call | — | `--deny`, `--reason`, `--scope` |
| `bot backup` | Encrypted backup of bots, chats, memory, skills, and settings: `create`, `verify`, `restore`. | `create`, `verify`, `restore` | — |
| `bot backup create` | Write an encrypted backup of the data directory to one file. | — | `--include-secrets`, `--passphrase-env`, `--passphrase-file` |
| `bot backup restore` | Restore an encrypted backup into the data directory (stop the daemon first) | — | `--allow-policy-relaxation`, `--include-secrets`, `--passphrase-env`, `--passphrase-file`, `--replace` |
| `bot backup verify` | Decrypt a backup and check every file against its manifest, without touching the data directory | — | `--passphrase-env`, `--passphrase-file` |
| `bot bots` | Manage bots on the daemon | `list`, `add` | — |
| `bot bots add` | Create a bot from a profile TOML file | — | — |
| `bot bots list` | List configured bots | — | — |
| `bot catalog` | Validate the curated model catalog, check freshness, or diff against a provider snapshot | `check`, `diff`, `render-docs` | — |
| `bot catalog check` | Validate the curated model metadata and provider catalog entries, and report freshness | — | `--docs`, `--max-age-days`, `--metadata`, `--strict` |
| `bot catalog diff` | Compare the committed catalog for a provider against a JSON/TOML model-list snapshot. | — | `--live`, `--metadata` |
| `bot catalog render-docs` | Print the Markdown provider table generated from `model_metadata.json` | — | `--metadata` |
| `bot channels` | Manage messaging channels (Telegram) | `telegram` | — |
| `bot channels telegram` | Manage the Telegram channel | `setup`, `pair`, `status`, `disable` | — |
| `bot channels telegram disable` | Disable the Telegram channel while keeping the stored token | — | — |
| `bot channels telegram pair` | Print a one-time 6-digit pairing code (valid for 10 minutes) from the running daemon | — | — |
| `bot channels telegram setup` | Store the `BotFather` token (hidden prompt, never printed) and enable Telegram | — | `--token-env` |
| `bot channels telegram status` | Show whether Telegram is configured, enabled, running, and how many users are paired | — | — |
| `bot chat` | Send a message to a bot and stream the answer | — | `--chat`, `--mode` |
| `bot completions` | Print a shell completion script to stdout | — | — |
| `bot doctor` | Check installation health (data directory, config, providers, daemon, browser, and more) | — | `--online`, `--report` |
| `bot email` | Manage daemon email accounts (IMAP/SMTP or AgentMail) | `list`, `add`, `rm`, `test` | — |
| `bot email add` | Add an IMAP/SMTP email account or an AgentMail inbox. | — | `--all-bots`, `--allowed-sender`, `--api-key-env`, `--bot`, `--domain`, `--imap-host`, `--imap-port`, `--name`, `--name-hint`, `--password-env`, `--preset`, `--provider`, `--smtp-host`, `--smtp-port`, `--smtp-security`, `--username` |
| `bot email list` | List configured email accounts (never shows passwords) | — | — |
| `bot email rm` | Remove an email account and its stored password or key | — | `--delete-inbox` |
| `bot email test` | Test IMAP/SMTP authentication or the AgentMail API key (sends no mail) | — | — |
| `bot grants` | List or revoke a bot's persisted "always allow" grants | `list`, `revoke` | — |
| `bot grants list` | List persisted grants for a bot | — | — |
| `bot grants revoke` | Revoke one persisted grant so future calls ask again | — | `--category`, `--tool` |
| `bot host` | Manage the active host and paired devices | `join`, `invite`, `status`, `devices`, `revoke`, `standby`, `activate`, `screen`, `admin`, `add`, `repair`, `upgrade`, `remove` | — |
| `bot host activate` | Claim active host ownership for this installation | — | `--force` |
| `bot host add` | Install Praxi on an Ubuntu/Debian server over SSH and pair it with this computer. | — | `--key`, `--name`, `--no-desktop`, `--ssh-port`, `--yes` |
| `bot host admin` | Grant or withdraw host administration permissions for a paired desktop (local only). | — | `--all-desktops`, `--revoke` |
| `bot host devices` | List paired devices (JSON) | — | — |
| `bot host invite` | Create a single-use invite and print its QR and confirmation code | — | `--role` |
| `bot host join` | Pair this fresh host with a desktop before starting the service | — | — |
| `bot host remove` | Disconnect a server and revoke this computer's pairing on it | — | `--key`, `--purge`, `--ssh-port`, `--uninstall`, `--yes` |
| `bot host repair` | Re-run installation on a server and migrate a plain `bot serve` setup to host mode. | — | `--key`, `--name`, `--no-desktop`, `--ssh-port`, `--yes` |
| `bot host revoke` | Revoke a paired device by its device key | — | — |
| `bot host screen` | Configure screen-viewing and control permissions for a paired desktop (local only). | — | `--all-desktops`, `--revoke`, `--view-only` |
| `bot host standby` | Stop automatic components and put this host in standby mode | — | — |
| `bot host status` | Show host ownership, peer status, and capabilities (JSON) | — | — |
| `bot host upgrade` | Upgrade Praxi on a paired server to match this client's version | — | `--key`, `--name`, `--no-desktop`, `--ssh-port`, `--yes` |
| `bot man-pages` | Generate section 1 man pages in the given directory | — | — |
| `bot mcp` | Manage daemon MCP servers and stdio exposure | `add`, `serve` | — |
| `bot mcp add` | Add MCP servers from a URL, launch command, or MCP JSON paste. | — | `--dry-run`, `--name` |
| `bot mcp serve` | Expose a selected bot as a scoped Praxi MCP server over stdio. | — | `--allow`, `--bot`, `--token-id` |
| `bot providers` | Manage LLM providers | `list`, `add`, `test`, `detect-local`, `default` | — |
| `bot providers add` | Add or update an LLM provider. | — | `--base-url`, `--key-env`, `--model`, `--no-test` |
| `bot providers default` | Set the global default provider | — | `--model` |
| `bot providers detect-local` | Look for a local model server (Ollama, LM Studio, llama.cpp) on loopback and print what it found as JSON. Contacts only 127.0.0.1 | — | — |
| `bot providers list` | List configured providers (`*` marks the global default) | — | — |
| `bot providers test` | Test a provider's connection (uses the default provider when ID is omitted) | — | — |
| `bot proxy` | Open a local loopback tunnel to a paired remote daemon (requires `--remote`). | — | — |
| `bot restore` | Restore a plaintext backup made by an older version (stop the daemon first). | — | `--allow-policy-relaxation`, `--replace` |
| `bot schedules` | Manage scheduled runs (cron, interval, or one-shot) | `list`, `add`, `rm`, `run-now` | — |
| `bot schedules add` | Add a schedule for a bot. | — | `--chat`, `--name`, `--tz` |
| `bot schedules list` | List schedules | — | — |
| `bot schedules rm` | Delete a schedule | — | — |
| `bot schedules run-now` | Trigger a schedule immediately in the background | — | — |
| `bot secrets` | Manage stored secrets (values are never printed) | `list`, `set`, `delete` | — |
| `bot secrets delete` | Delete a stored secret | — | — |
| `bot secrets list` | List stored secret names (never prints values) | — | — |
| `bot secrets set` | Store a secret from `--value-env` or a hidden prompt | — | `--value-env` |
| `bot self-update` | Update a Linux installation (does not restart the service) | — | `--check`, `--prefix`, `--rollback`, `--version` |
| `bot serve` | Run the local daemon API on 127.0.0.1 until Ctrl-C or SIGTERM. | — | `--host`, `--owner-key-stdin`, `--peer`, `--safe`, `--shutdown-grace` |
| `bot setup` | Set up a provider, default model, and first bot (interactive, or scripted with `--provider`) | — | `--base-url`, `--bot-name`, `--bot-preset`, `--key-env`, `--lang`, `--model`, `--permission`, `--provider`, `--yes` |
| `bot skills` | Review local skills and explicitly trust reviewed content | `review`, `trust` | — |
| `bot skills review` | Print every file in full (JSON-quoted whitespace) and its review fingerprint | — | `--bot-id` |
| `bot skills trust` | Trust exactly the content printed by `bot skills review`; changes require re-review | — | `--bot-id`, `--reviewed` |
| `bot status` | Show daemon status and verify the API token | — | — |
| `bot token` | Create or revoke scoped MCP credentials on the local daemon | `create`, `revoke` | — |
| `bot token create` | Create a scoped credential in the daemon's secret store and print its public ID | — | `--bot`, `--scope`, `--ttl` |
| `bot token revoke` | Revoke a credential and cancel its running tasks | — | — |
| `bot workflow` | Record, review, save, and replay workflows from successful runs | `list`, `show`, `propose`, `save`, `delete`, `run` | — |
| `bot workflow delete` | Delete a saved workflow | — | `--bot` |
| `bot workflow list` | List saved workflows for a bot | — | `--bot` |
| `bot workflow propose` | Print an unsaved workflow proposal for review (redirect JSON to a file to edit) | — | — |
| `bot workflow run` | Replay a saved workflow in an existing chat through the normal permission engine | — | `--chat`, `--params-stdin` |
| `bot workflow save` | Save reviewed workflow metadata and defaults (source steps are rebuilt and compared) | — | `--description`, `--name` |
| `bot workflow show` | Show every step and parameter of a saved workflow | — | `--bot` |
