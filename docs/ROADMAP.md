# Roadmap

Praxi-Bot is **alpha**. This roadmap is a product-level picture of where the project is and where
it is heading, in three horizons. It is **not a promise and has no dates**: priorities change with
what we learn and with who contributes. Open work is tracked in
[issues](https://github.com/emreocell/Praxi-Bot/issues) and pull requests; starter tasks are listed
in [docs/contributing/starter-issues.md](contributing/starter-issues.md).

## Now (being built or hardened)

- **Bots that live on a server.** Pair an always-on Linux host, move a bot's setup (keys, bots,
  schedules, chats) to it, and let it work while your computer is off. Includes installing coding
  CLIs on the server and signing them in from the desktop app, and choosing per request whether a
  bot uses your computer or the server's.
- **A clearer desktop app.** The bot panel, the computer card, per-run status and queued messages,
  and a more reliable Stop.
- **Connecting apps with less friction.** One-click connections for remote apps (OAuth) and
  Composio apps, with real logos, instead of API-key chores.
- **Trust and safety.** Approvals, the egress gate, protected paths, secrets handling and the
  owner-only route tables keep getting tests and reviews; releases are signed and verified.
- **Reliability.** Fixing flaky steps in the real-app smoke run, and closing documentation gaps.

## Next (we want this, and contributors can help)

- **More platforms.** A real macOS desktop backend (screenshots, accessibility, input), Wayland on
  Linux, Windows on ARM64, and desktop bundles for Linux and macOS.
- **Packaging.** winget, Homebrew, an AUR package and a container image for the host.
- **More engines on the server.** Signing in to Antigravity, Cursor and OpenCode from the desktop
  where their tools allow it.
- **More connectors and providers.** More ready-made remote app connectors and model-provider
  presets.
- **Languages.** A third UI language, and a translation-friendly path for the CLI and the schedule
  parser.
- **Accessibility.** A keyboard-only and screen-reader pass over the main flows.
- **Quality.** The real-app smoke run in CI, more hermetic evaluation scenarios, more UI unit tests.

## Later (ideas, no commitment)

- Recording a workflow by demonstration and a workflow editor in the desktop app.
- More messaging channels beyond Telegram.
- A plugin API for tools, providers and channels for end users.
- Smarter model use: tiered models with escalation, and a pluggable decision layer before the main
  model is called.
- A bot sharing format (import/export of bot profiles).

## How to influence it

Open an issue with the *feature request* template, or comment on an existing one. If you want to
build something that is not listed, say so in an issue first: we will tell you whether it fits and
which parts of [CONTRIBUTING.md](../CONTRIBUTING.md) matter for it.
