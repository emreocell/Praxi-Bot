# ADR: importing from other assistants

Status: accepted. PRs: #620 (apps and skills), #550 and #651 (chat exports).
Code: `apps/cli/src/import_cmd.rs`; chat import in `crates/daemon-api`.

## Context

People switching from Claude Desktop, Claude Code, OpenClaw or Hermes expect
their servers, skills and chats to come along. Everything in those files was
written by software we do not control, and some of it runs code.

## Decision

- `bot import detect` needs no daemon. `bot import run` shows a preview (what
  will be imported, what is skipped and why) and writes nothing without `--yes`.
- **MCP servers arrive disabled.** The owner enables each one after reading it.
  Unpinned `npx`/`uvx` specs, never-list commands and SSE servers are skipped
  with a plain reason. Every `env`/`headers` value goes to the secret store via
  the daemon; it is never written in plaintext or printed.
- **Skills:** only Low-risk ones, installed through the skill hub with the scan
  fingerprint. Others are skipped with the `bot skills install` command.
- API keys, channels, notes and other config are listed as skipped, never copied.
- **Chat exports** go through the owner-only `POST /v1/chats/import`: only
  `user` and `assistant` roles, hard size and count limits, new ids, and every
  message is marked untrusted (`exposure_untrusted`, source `imported`). Steps
  and approvals are counted but never written, so an import cannot forge the
  history of an approved action.

## Consequences

- Import cannot start a program or contact a server by itself.
- Imported chat text is data, never instructions, for later runs.
- Not built yet: a desktop onboarding step for app import (needs a daemon
  route, owner classification and three-locale strings).

## Alternatives rejected

- Importing servers enabled, for convenience: they could run on first start.
- Copying API keys and approvals: it would carry trust we cannot check.
- Replaying imported steps as real history.
