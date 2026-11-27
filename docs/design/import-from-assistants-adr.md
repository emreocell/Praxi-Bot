# ADR: importing from other assistants

Status: accepted. PRs: #620 (apps and skills), #550 and #651 (chat exports), #574 (more apps, the
desktop Import screen, daemon routes).
Code: `crates/assistant-import` (scan and plan), `crates/daemon-api/src/assistant_import_routes.rs`
(`/v1/import/*`), `apps/cli/src/import_cmd.rs`; chat import in `crates/daemon-api`.

## Context

People switching from Claude Desktop, Claude Code, OpenClaw or Hermes expect
their servers, skills and chats to come along. Everything in those files was
written by software we do not control, and some of it runs code.

## Decision

- `bot import detect` needs no daemon. `bot import run` shows a preview (what
  will be imported, what is skipped and why) and writes nothing without `--yes`.
- **Sources:** Claude Desktop and Claude Code (servers, skills, agents, `CLAUDE.md`), Cursor and
  VS Code `mcp.json`, Codex (`config.toml` servers, prompts, skills, `AGENTS.md`), OpenClaw,
  Hermes, and a folder the owner names. Nothing is read from a folder until it is named, and the
  desktop screen scans only after the owner pressed "Look for setups".
- **The daemon does the work** (`POST /v1/import/scan`, `POST /v1/import/run`, owner only), so the
  desktop screen and a remote server work the same way: a server imports from its own files.
  `run` scans again, so what is written is what is on disk now; values never travel back.
- **MCP servers arrive disabled.** The owner enables each one after reading it.
  Unpinned `npx`/`uvx` specs, never-list commands and SSE servers are skipped
  with a plain reason. A `env`/`headers` value goes to the secret store only after
  the owner confirmed that one value; unconfirmed values are left out and reported. It
  is never written in plaintext or printed. A value that is only a variable reference
  (`${env:TOKEN}`) is not copied, and a command line or URL that holds a key is not imported.
- **Skills:** only Low-risk ones, installed through the skill hub with the scan
  fingerprint. Others are skipped with the `bot skills install` command.
- **Instructions** (`CLAUDE.md`, `AGENTS.md`): scanned like a skill, never ticked by default, and
  added to the end of one bot's instructions through the normal bot edit route (all its checks
  apply). Agent and prompt files become skills (name, description and text only; tools and
  model settings are not copied).
- API keys, channels, notes and other config are listed as skipped, never copied.
- **Chat exports** go through the owner-only `POST /v1/chats/import`: only
  `user` and `assistant` roles, hard size and count limits, new ids, and every
  message is marked untrusted (`exposure_untrusted`, source `imported`). Steps
  and approvals are counted but never written, so an import cannot forge the
  history of an approved action.

## Consequences

- Import cannot start a program or contact a server by itself.
- Imported chat text is data, never instructions, for later runs.
- Asking per secret keeps the owner in charge; the cost is more clicks for a server with
  several keys (the screen has one box per value).

## Alternatives rejected

- Importing servers enabled, for convenience: they could run on first start.
- Copying API keys and approvals: it would carry trust we cannot check.
- Replaying imported steps as real history.
