# Praxi user guide

Praxi (the repository is called Praxi-Bot, the command is `bot`) is a
self-hosted AI agent. It runs on your own computer, talks to the language-model
provider you choose (a cloud API or a local model), and can work for you:
read and edit files, run commands, drive a browser, operate desktop apps, run
on a schedule, and report to you on your phone through Telegram.

Three ideas run through the whole product:

- **You stay in control.** Every action that changes something is checked by a
  permission system. By default reads are free and anything that writes, clicks,
  types, runs a command or sends a message asks you first. See
  [Chatting and approvals](chatting-and-approvals.md) and
  [Security model](security.md).
- **Local first, no telemetry.** The daemon only listens on `127.0.0.1`. Your
  chats, bots and settings live in a data directory on your machine, and API
  keys go to your OS keychain (or an encrypted file). The project's design
  decision is that there is no telemetry.
- **Use the model as little as possible.** Before the main model is called, a
  small "decision ladder" can answer a slash command, reuse a route you told it
  to remember, or pick a saved workflow. Recorded workflows replay without any
  model calls. See [Workflows](workflows.md).

> **Status: early development.** The README says it plainly: there is no stable
> API and only pre-releases (`v0.1.0-alpha.2` latest) so far. Expect breaking changes. This guide documents what
> the code does today and marks everything else as **Planned**.

## What changed recently

Notable additions in the `0.1.0` desktop, CLI, and evaluation surface:

- **Bot-only left rail and chat management in the bot panel.** The left rail lists bots only; each bot's chats live in its right-hand panel (**Library → Chats**) with an overflow menu for rename, pin, archive, and delete. See [Managing chats](chatting-and-approvals.md#managing-chats-desktop-app).
- **Message copy and Markdown export.** Hover or focus any message to copy its visible text, or type `/export` in the composer to save the open chat as a `.md` file. See [Copying messages and Markdown export](chatting-and-approvals.md#copying-messages-and-markdown-export).
- **Keyboard shortcuts dialog (`?`) and "What's new" card.** Press `?` (outside text fields) or use the command palette (`Ctrl/Cmd+K`) to view all shortcuts. Bundled release notes appear once per version after an update with no network fetch. See [Keyboard shortcuts and "What's new"](chatting-and-approvals.md#keyboard-shortcuts-and-whats-new).
- **Unread markers.** Bots with unseen replies or pending decisions show a dot in the left rail, and the open chat marks the first unseen reply with a **New messages** divider. See [Unread markers](chatting-and-approvals.md#unread-markers).
- **Praxi as the default main bot and "Make main bot".** Fresh desktop setups default to **Praxi** (white ghost) as the main bot, and any bot's panel has a **Make main bot** star button. See [Main bot and bot order](bots-and-profiles.md#main-bot-and-bot-order-desktop-app).
- **Auto-starting the local host on send.** Sending a message while this computer's daemon is offline restarts it automatically and delivers the message once it is up; for an offline paired server, the draft is kept with a Retry prompt. See [Sending while the host is offline](chatting-and-approvals.md#sending-while-the-host-is-offline).
- **Host-admin pause recovery.** A paired host's admin desktop can clear a paused screen takeover via `POST /v1/control/recover` without losing its screen-input right, and **Stop** aborts runs parked behind a pause. See [Live screen privacy and human control](security.md#live-screen-privacy-and-human-control).
- **Prompt-injection eval pack.** Offline tier-1 injection scenarios with an `unsafe_actions` gate and baseline comparison (`bot-eval run --tier 1 --filter inj- --repeat 3`). See [Prompt-injection evaluation pack](security.md#prompt-injection-evaluation-pack).
- **`bot proxy` and live-view link measurement.** `bot --remote <ENDPOINT> proxy` opens a loopback tunnel to a paired host for measuring direct vs. relay live-view performance. See [`bot proxy`](cli-reference.md#bot-proxy) and [Measuring the live-view link](../design/live-view-link-measurement.md).
- **Host operations guide.** Day-to-day Linux host logs, service control, upgrades, backups, and troubleshooting now live in [Host operations](host-operations.md).

## What is where

| You want to... | Read |
| --- | --- |
| Install Praxi and run it the first time | [Install and first run](install.md) |
| Windows installer details, upgrade, uninstall | [Installing on Windows](install-windows.md) |
| Set up an always-on Linux host (VPS / VM) | [Running on a remote host](remote-host.md) |
| Run the always-on Linux host: logs, restart, upgrade, backup | [Host operations](host-operations.md) |
| Understand what a host does when it is demoted or hears another active host | [Host leadership limits](host-leadership.md) |
| Create a bot and understand its profile file | [Bots and profiles](bots-and-profiles.md) |
| Talk to a bot, approve or deny actions | [Chatting and approvals](chatting-and-approvals.md) |
| Run something every morning | [Scheduled tasks](scheduled-tasks.md) |
| Replay a recorded sequence of steps without a model | [Workflows](workflows.md) |
| Teach a bot how to handle a tricky web page or app | [Skills](skills.md) |
| Create, import, update or delete shared skills through the local API | [Managing shared skills](skill-management-api.md) |
| Add external tool servers | [MCP servers](mcp-servers.md) |
| Let another MCP agent use a selected Praxi bot | [Praxi as an MCP server](mcp-server.md) |
| Pick a provider, use the Gemini free tier | [Providers and models](providers-and-models.md) |
| Understand how the model picker names and ranks models | [Model picker metadata](model-catalog.md) |
| Let a lighter model answer simple first turns | [Optional model tiering](model-tiering.md) |
| See what each model call in a run was for and what it used | [LLM call ledger](llm-call-ledger.md) |
| Use a local coding CLI (Claude Code, Codex, Cursor, ...) as the engine | [Coding CLI engines](cli-engines.md) |
| Talk to your bots from your phone | [Telegram](telegram.md) |
| Read, search, draft or send e-mails | [Email integration](email.md) |
| Read and add calendar events | [Calendar (CalDAV)](calendar.md) |
| Let a bot read Home Assistant entities and call services | [Home Assistant](home-assistant.md) |
| Let a bot search the web and read pages without a browser | [Web tools](web.md) |
| Set up knowledge embeddings (optional; knowledge search works without them) | [Set up knowledge embeddings](knowledge-embeddings.md) |
| Look up a command or flag | [CLI reference](cli-reference.md) |
| Back up and restore your data | [Backup and restore](backup-and-restore.md) |
| Understand what protects you | [Security model](security.md) |
| Look up a term | [Glossary](glossary.md) |
| See keyboard shortcuts | [Keyboard shortcuts](keyboard-shortcuts.md) |
| Something does not work | [Troubleshooting](troubleshooting.md), [FAQ](faq.md) |

## The pieces

```
  Desktop app (Windows)        bot CLI              Telegram
          \                      |                    /
           +-------- local HTTP API on 127.0.0.1 -----+
                              |
                        daemon  (`bot serve`)
        bots, chats, runs, approvals, schedules, workflows, skills
                              |
        providers (Gemini, Anthropic, OpenAI-compatible, Ollama ...)
        tools (files, shell, browser, desktop, scripts, subagents, email, web)
```

- **The daemon** (`bot serve`) is the heart. It stores everything in SQLite plus
  event logs, runs the agent loop, enforces permissions and fires schedules.
- **The `bot` CLI** configures things (`bot setup`, `bot providers`,
  `bot secrets`) and talks to a running daemon (`bot chat`, `bot approve`, ...).
- **The desktop app** (Windows installer, Tauri) starts the daemon for you and
  gives you a graphical interface: onboarding wizard, chats, approval cards,
  schedules, usage, and an on-screen overlay while a bot controls your desktop.
- **Telegram** is an optional channel hosted by the daemon.
- **Node-agent** (`bot-node`) is an experimental helper to control another
  machine; see the [FAQ](faq.md).

## Conventions in this guide

- `bot ...` commands assume the `bot` binary is on your `PATH`. If you built it
  from source it is `./target/debug/bot` or `./target/release/bot`.
- "Data directory" means the folder Praxi keeps its files in. Override it with
  `--data-dir` or the `BOT_DATA_DIR` environment variable. Otherwise it is the
  per-user application-data folder of your OS for the app `praxi-bot`.
- **Planned** marks features that are described in the design documents or have
  library code but are not reachable from the shipped daemon or CLI yet.
