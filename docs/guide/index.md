# Praxi user guide

Praxi (the repository is called Praxi-Bot, the command is `bot`) is a
self-hosted AI agent. It runs on your own computer, talks to the language-model
provider you choose (a cloud API or a local model), and can work for you:
read and edit files, run commands, drive a browser, operate desktop apps, run
on a schedule, and report to you on your phone through Telegram.

In another language: [Türkçe](tr/index.md) · [Deutsch](de/index.md). Pages that are not translated yet open in English.

Three ideas run through the whole product:

- **You stay in control.** Every action that changes something is checked by a
  permission system. By default reads are free and anything that writes, clicks,
  types, runs a command or sends a message asks you first. See
  [Chatting and approvals](chatting-and-approvals.md) and
  [Security model](security.md). The [Settings guide](settings.md) explains the
  three safety levels in plain words.
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

- **Settings redesign (eight pages, Vault and Updates).** **Settings** is grouped into You, Your bots, and App pages: **General**, **Appearance**, **AI and memory**, **Connections**, **Safety**, **Vault**, **System**, and **Updates**. Expert cards sit under **More options**; many cards have a **`?` popover**. See [Settings](settings.md).
- **Profile menu and bot panel.** The profile menu (bottom of the left rail) opens Settings, Vault, Schedules, Updates, and Help. Each bot's **Activity**, **Automation**, **Library**, and **Tools** tabs live in the right-hand panel. See [Bots and profiles](bots-and-profiles.md) and [Managing chats](chatting-and-approvals.md#managing-chats-desktop-app).
- **Import from other assistants.** **Settings → General** or `bot import` can copy setups from Claude, Cursor, VS Code, Codex, and others. See [Import from other assistants](import-from-assistants.md).
- **Apps, MCP Registry, and `.mcpb` bundles.** **Apps** covers Composio, custom MCP, REST APIs, and registry search. See [Connecting apps](apps.md) and [MCP servers](mcp-servers.md).
- **Missions and schedules in the bot panel.** Long jobs start from the **Activity** tab; scheduled jobs and webhooks are edited under **Automation**, and the full list stays in the left-rail **Schedules** item. See [Missions](missions.md) and [Automate things](automate.md).
- **Bot-only left rail and chat management in the bot panel.** The left rail lists bots only; each bot's chats live in its right-hand panel (**Library → Chats**) with an overflow menu for rename, pin, archive, and delete. See [Managing chats](chatting-and-approvals.md#managing-chats-desktop-app).
- **Message copy and Markdown export.** Hover or focus any message to copy its visible text, or type `/export` in the composer to save the open chat as a `.md` file. See [Copying messages and Markdown export](chatting-and-approvals.md#copying-messages-and-markdown-export).
- **Keyboard shortcuts dialog (`?`) and "What's new" card.** Press `?` (outside text fields) or use the command palette (`Ctrl/Cmd+K`) to view all shortcuts. Bundled release notes appear once per version after an update with no network fetch. See [Keyboard shortcuts and "What's new"](chatting-and-approvals.md#keyboard-shortcuts-and-whats-new).
- **Unread markers.** Bots with unseen replies or pending decisions show a dot in the left rail, and the open chat marks the first unseen reply with a **New messages** divider. See [Unread markers](chatting-and-approvals.md#unread-markers).
- **Praxi as the default main bot and per-computer "Make main bot".** Fresh desktop setups default to **Praxi** (white ghost) as the main bot. Each computer (This computer and every paired host) keeps its own bot order and main bot, set with the star button (**Make main bot**) in the bot panel header. See [Main bot and bot order](bots-and-profiles.md#main-bot-and-bot-order-desktop-app).
- **Hands-free voice call mode.** Tap the phone button in the desktop chat composer to talk to a bot with automatic voice activity detection, sentence-by-sentence speech, barge-in, push-to-talk, and click-only approvals. See [Voice call mode](voice-call.md).
- **Live voice conversation (Talk).** Press **Talk** in the desktop composer for a low-latency spoken conversation (Gemini Live): interrupt by speaking, the bot does the work through the normal approvals, and the conversation lands in the chat as text. Audio is not stored unless you turn it on. See [Live voice conversation](voice-live.md).
- **Integrated window controls and complete ghost app icon.** On Windows, the desktop app hides the native window frame by default and draws its own minimize, maximize/restore, and close controls in a reserved title-bar strip above the panels, alongside the complete Praxi ghost icon. See [Window controls and appearance](install-windows.md#window-controls-and-appearance).
- **Refined satin-matte bot avatars.** Bots can use one of 16 procedural SVG shapes with layered lighting, living eyes, state motion (respecting reduced motion), or a custom uploaded image/GIF. See [Bot avatars](bots-and-profiles.md#bot-avatars-desktop-app).
- **Host-status commands without approval fatigue.** Fixed read-only status commands (`uptime`, `df -h`, `free -m`, `uname -a`, and similar) on an always-on host's own computer no longer taint the run or trigger repeated executor asks. See [Host-status commands on an always-on host](chatting-and-approvals.md#host-status-commands-on-an-always-on-host).
- **Paired server as a bot's browser computer.** A local bot with `[runtime] computer = "server:<key>"` keeps its model calls, files, shell, and approvals on your computer while running `browser_*` inside an isolated browser on the paired server. See [Computer target](bots-and-profiles.md#computer-target) and [Using a paired server as a bot's browser computer](remote-host.md#using-a-paired-server-as-a-bots-browser-computer).
- **Bounded browser startup and clearer `bot doctor` checks.** `browser_open` now bounds launch and session waits with actionable failure messages, Linux hosts re-probe late displays after 15 seconds, and `bot doctor` pinpoints the first missing display, `XAUTHORITY`, or browser piece. See [Browser does not open or times out](troubleshooting.md#browser-does-not-open-or-times-out).
- **Auto-starting the local host on send.** Sending a message while this computer's daemon is offline restarts it automatically and delivers the message once it is up; for an offline paired server, the draft is kept with a Retry prompt. See [Sending while the host is offline](chatting-and-approvals.md#sending-while-the-host-is-offline).
- **Host-admin pause recovery.** A paired host's admin desktop can clear a paused screen takeover via `POST /v1/control/recover` without losing its screen-input right, and **Stop** aborts runs parked behind a pause. See [Live screen privacy and human control](security.md#live-screen-privacy-and-human-control).
- **Prompt-injection eval pack.** Offline tier-1 injection scenarios with an `unsafe_actions` gate and baseline comparison (`bot-eval run --tier 1 --filter inj- --repeat 3`). See [Prompt-injection evaluation pack](security.md#prompt-injection-evaluation-pack).
- **`bot proxy` and live-view link measurement.** `bot --remote <ENDPOINT> proxy` opens a loopback tunnel to a paired host for measuring direct vs. relay live-view performance. See [`bot proxy`](cli-reference.md#bot-proxy) and [Measuring the live-view link](../design/live-view-link-measurement.md).
- **Host operations guide.** Day-to-day Linux host logs, service control, upgrades, backups, and troubleshooting now live in [Host operations](host-operations.md).

## Start here (no technical knowledge needed)

Seven short pages in plain words. Read them in this order:

| Page | What it is |
| --- | --- |
| Wake a bot when mail or a signed call arrives | [When something happens](triggers.md) |
| [Getting started in 5 minutes](getting-started.md) | Install, get a free key, have your first chat, and learn what a bot is. |
| [Keep your bot safe](keep-your-bot-safe.md) | The permission levels, approvals, and what leaves your computer. |
| [Run it on this computer or on a server](local-or-server.md) | Why you do not need a server, and how to add one if you want. |
| [Automate things](automate.md) | Scheduled jobs, with three examples you can copy. |
| [Incoming webhooks](incoming-webhooks.md) | Start a bot from GitHub, a form, or home automation. |
| [Connect your apps](connect-your-apps.md) | Apps, what each permission means, and who may use them. |
| [Fix common problems](fix-common-problems.md) | The 12 most likely problems and what to do next. |
| [Quick answers](quick-faq.md) | Short answers to the questions people ask first. |

## What is where

| You want to... | Read |
| --- | --- |
| Install Praxi and run it the first time | [Install and first run](install.md) |
| Windows installer details, upgrade, uninstall | [Installing on Windows](install-windows.md) |
| Check that a Windows download is genuine (checksum, build proof, parts list) | [Check your download](verify-download.md) |
| Set up an always-on Linux host (VPS / VM) | [Running on a remote host](remote-host.md) |
| Run the always-on host in Docker | [Running Praxi in Docker](docker-host.md) |
| Copy-paste commands for a Linux always-on host: virtual display, update, uninstall | [Linux always-on host](linux-host.md) |
| Run the always-on Linux host: logs, restart, upgrade, backup | [Host operations](host-operations.md) |
| Add a server so bots run around the clock (guided, no jargon) | [Adding a server](servers.md) |
| Understand what a host does when it is demoted or hears another active host | [Host leadership limits](host-leadership.md) |
| Import setup from Claude, Cursor, VS Code, or another assistant | [Import from other assistants](import-from-assistants.md) |
| Create a bot and understand its profile file | [Bots and profiles](bots-and-profiles.md) |
| Talk to a bot, approve or deny actions | [Chatting and approvals](chatting-and-approvals.md) |
| Talk to a bot hands-free by voice (desktop app) | [Voice call mode](voice-call.md) |
| Have a live, interruptible spoken conversation (desktop app) | [Live voice conversation](voice-live.md) |
| Run something every morning | [Scheduled jobs in plain words](schedules.md), [Scheduled tasks](scheduled-tasks.md) (reference) |
| Give one big task and let the bot finish it | [Missions](missions.md) |
| Replay a recorded sequence of steps without a model | [Workflows](workflows.md) |
| Teach a bot how to handle a tricky web page or app | [Skills](skills.md) |
| Create, import, update or delete shared skills through the local API | [Managing shared skills](skill-management-api.md) |
| Connect external apps (OAuth, Composio, and MCP servers in the desktop app) | [Connecting apps](apps.md) |
| See what each app needs to connect (sign-in, where it runs, licence) | [What you need to connect each app](app-catalog.md) |
| Connect Gmail, Drive and Calendar with your own Google client, also on a server | [Google Workspace](google-workspace.md) |
| Connect any REST API from its OpenAPI document and choose what each bot may call | [Connect a REST API](rest-apis.md) |
| Configure external MCP servers by hand (`mcp.toml` and API) | [MCP servers](mcp-servers.md) |
| Let another MCP agent use a selected Praxi bot; client recipes and troubleshooting | [Praxi as an MCP server](mcp-server.md) |
| Pick a provider, use the Gemini free tier | [Providers and models](providers-and-models.md) |
| Understand how the model picker names and ranks models | [Model picker metadata](model-catalog.md) |
| Let a lighter model answer simple first turns | [Optional model tiering](model-tiering.md) |
| Configure bounded advisory decisions | [Optional decision pool](decision-pool.md) |
| See what each model call in a run was for and what it used | [LLM call ledger](llm-call-ledger.md) |
| Cap what a bot may spend per day, week or month | [Bot spending limits](bot-spending-limits.md) |
| See, correct, forget and switch off what your bots remember | [Memory](memory.md) |
| Keep one chat going for weeks: the rolling "earlier in this chat" note, and how to switch it off | [Long chats](long-chats.md) |
| Use a local coding CLI (Claude Code, Codex, Cursor, ...) as the engine | [Coding CLI engines](cli-engines.md) |
| Talk to your bots from your phone | [Telegram](telegram.md) |
| Talk to your bots from Discord | [Discord](discord.md) |
| Connect my e-mail and calendar step by step (Gmail, Outlook, iCloud, Yahoo, Fastmail, others) | [Connect my email and calendar](connect-email-calendar.md) |
| Read, search, draft or send e-mails | [Email integration](email.md) |
| Read and add calendar events | [Calendar (CalDAV)](calendar.md) |
| Set up Home Assistant from the app, so a bot can read your entities and call services | [Home Assistant](home-assistant.md) |
| Let a bot search the web and read pages without a browser | [Web tools](web.md) |
| Add the optional page reader for cleaner text and small crawls | [Web scraping add-on](web-scraping.md) |
| Give a bot a scratchpad between runs | [Notes](agent-tools.md) |
| Let a bot work in apps on your own PC | [Computer use](computer-use.md) |
| Set up knowledge embeddings (optional; knowledge search works without them) | [Set up knowledge embeddings](knowledge-embeddings.md) |
| Add optional helpers: embedding providers, free-key limits, decision helper | [Helpers (optional)](embeddings.md) |
| Look up a command or flag | [CLI reference](cli-reference.md) |
| Back up and restore your data (encrypted) | [Encrypted backup and restore](backup.md) |
| Restore an older plaintext backup | [Restoring older plaintext backups](backup-and-restore.md) |
| Understand what protects you | [Security model](security.md) |
| Look up a term | [Glossary](glossary.md) |
| See keyboard shortcuts | [Keyboard shortcuts](keyboard-shortcuts.md) |
| Use Praxi with a keyboard, screen reader, high-contrast theme, or reduced motion | [Accessibility](accessibility.md) |
| Something does not work | [Troubleshooting](troubleshooting.md), [Test connection](connection-test.md), [FAQ](faq.md) |

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
