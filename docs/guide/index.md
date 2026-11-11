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
> API and no releases yet. Expect breaking changes. This guide documents what
> the code does today and marks everything else as **Planned**.

## What is where

| You want to... | Read |
| --- | --- |
| Install Praxi and run it the first time | [Install and first run](install.md) |
| Create a bot and understand its profile file | [Bots and profiles](bots-and-profiles.md) |
| Talk to a bot, approve or deny actions | [Chatting and approvals](chatting-and-approvals.md) |
| Run something every morning | [Scheduled tasks](scheduled-tasks.md) |
| Replay a recorded sequence of steps without a model | [Workflows](workflows.md) |
| Teach a bot how to handle a tricky web page or app | [Skills](skills.md) |
| Add external tool servers | [MCP servers](mcp-servers.md) (Planned in the daemon) |
| Pick a provider, use the Gemini free tier | [Providers and models](providers-and-models.md) |
| Talk to your bots from your phone | [Telegram](telegram.md) |
| Read, search, draft or send e-mails | [Email integration](email.md) |
| Let a bot search the web and read pages without a browser | [Web tools](web.md) |
| Look up a command or flag | [CLI reference](cli-reference.md) |
| Understand what protects you | [Security model](security.md) |
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
