# Troubleshooting

Start with `bot status`. It checks that the daemon is up and that your token is
accepted, and shows how many approvals are waiting.

## Diagnostics (Settings → Diagnostics, `bot doctor`)

**Reporting a bug:** open Help and use *Copy diagnostic info*. It first shows exactly
the text it would copy (versions, platform, check results; keys, passwords, e-mail
addresses and your user folder are masked, no messages are included) and copies it
only when you press *Copy*. Nothing is sent anywhere.

The desktop app has a **Diagnostics** page in Settings. It lists the same
checks as `bot doctor`: provider keys and coding-CLI sign-in, loaded
providers, data folder (including a file name with Turkish letters),
browser, desktop automation, peer / VPS link, disk space and clock.
Something the platform does not offer is shown as *not available*, not as an
error.

- **Reading the page changes nothing.** A few problems have a repair button
  (create the data folder, reload the providers). A repair runs only when you
  press its button, and every attempt is written to the audit log. Nothing
  else is offered as a one-click fix; the API token is never regenerated.
- **Copy support report** copies a Markdown summary (versions, platform,
  check results, config file *names*). Your user folder is shown as `~`;
  secrets the daemon knows (also URL-encoded, base64 or JSON-escaped), the
  API token, key-like strings, credentials and host names in URLs, the lines
  a config parse error quotes, your user and computer names and e-mail
  addresses are masked. If the secrets cannot be read, no report is made.
  Read it before you share it.
- **Peer / VPS link** is checked against the daemon's live connection: a
  configured host that is offline, unpaired, or a daemon without peer
  networking is reported as a problem. A coding CLI whose sign-in could not
  be confirmed is a warning, never "ready".
- **Host display (`host_display`)** on an always-on Linux host checks the
  requirements in order and reports the first missing piece instead of a generic
  warning: whether `DISPLAY` is set, whether the display answers (and whether
  `XAUTHORITY` is set and readable), and whether a usable non-snap browser is
  present (`BOT_BROWSER_PATH` or `/opt/praxi-browser/chrome`). On a systemd host,
  run `sudo praxi doctor` so the check runs with the service's own user,
  environment (`/etc/praxi/praxi.env`), and mount namespace rather than your
  interactive shell.
- **Free RAM (`free_ram`)** on Linux reads `MemAvailable` from `/proc/meminfo`
  and warns below 512 MiB (suggesting lowering `limits.max_computer_runs` in
  `host.toml`).
- The daemon routes behind the page are `GET /v1/doctor`, `POST
  /v1/doctor/repair/{id}` and `GET /v1/doctor/report` (bearer token
  required). They are local to the computer: paired phones and desktops
  cannot call them.
  `bot doctor` runs more checks that need a terminal (port probe, bounded
  headless browser launch with a 5-second startup / 8-second overall timeout,
  `--online` connection tests).

## The always-on host

Logs, restart, upgrade, backup and the host-offline / slow-start / peer table
are in [Host operations](host-operations.md).

## `daemon not reachable at http://127.0.0.1:47821` (exit code 3)

The daemon is not running, or it listens on another port.

- Start it: `bot serve` (or launch the desktop app, which starts it).
- Used `--port` or `BOT_PORT` for `serve`? Use the same for the client command,
  or pass `--url http://127.0.0.1:<port>`.
- Port already in use: start with `bot serve --port 47822` and point clients at it.

## Authentication rejected (exit code 4)

The token or the Host/Origin check failed.

- The client reads `<data dir>/api-token`. If you started the daemon with a
  different `--data-dir` or `BOT_DATA_DIR`, give the client the same one.
- The API only accepts loopback hosts. Do not put a proxy with another Host name
  in front of it.
- Developing a web UI against the daemon? Add its origin to
  `BOT_ALLOWED_ORIGINS` (comma separated) before `bot serve`.

## `no usable provider; run bot setup or bot providers add`

`bot serve` found no enabled provider it can build.

- `bot providers list` shows what is configured. A provider whose key is missing
  is skipped with a warning at startup (`provider <id> skipped: ...`).
- Add one: `bot providers add gemini --key-env GEMINI_API_KEY`.
- Keys in environment variables are read when the command runs; set the variable
  in the same shell, and for the desktop app make sure the app was started after
  setting it (or store the key with the hidden prompt instead).

## `connection test failed`

`bot providers add` and `bot providers test` call the provider. Check the key,
the network or proxy, and for local providers that Ollama or LM Studio is
running and the model is pulled. `--no-test` saves the provider without testing.

## A bot answers "has no usable profile"

The bot's stored profile is not a valid profile. This happens with the first bot
saved by the CLI wizard (`bot setup`). Create a bot from a profile file with
`bot bots add` instead; see [Bots and profiles](bots-and-profiles.md). A profile
needs at least `[bot]` with `id` and `name`, and `[models]` with `executor`.

## `bot bots add` is rejected (exit code 6)

The message names the problem: invalid TOML, a missing `executor`, an unknown
permission preset (use `read-only`, `read-and-draft`, `approve-destructive` or
`full`) or a bad `[tools]` glob.

## The bot stops and waits

It is waiting for you: an approval or a question. Run `bot status` for the
count, or watch the chat output for `[approval needed]` / `[question]` and answer
with `bot approve <id>` / `bot answer <id> "..."`. Approvals for scheduled runs
time out after 30 minutes, after which the run is paused.

## A tool call is refused

- The policy says **deny**: `read-only` preset or a `deny` rule. Look at the
  bot's profile, and `GET /v1/bots/{id}` for its effective policy.
- Desktop tools: you took control, or paused the bot, in the desktop app. Hand
  control back.
- No desktop tools at all: they exist on Windows only, and `BOT_DESKTOP=off`
  disables them.

## "Always allow" does not stick, or never asks

`bot grants list <bot-id>` shows the persisted grants. Revoke one to make the
bot ask again. Money and never-list actions ask regardless of any grant, by
design.

## Rate limit errors from Gemini (HTTP 429)

You hit a free-tier limit, most likely the daily request cap of that model. Wait
for the reset, switch the model to one with a higher cap, reduce schedules, or use
a paid key or another provider. See
[Gemini free tier](providers-and-models.md#gemini-free-tier).

## Safe mode: start with everything off

Use it when something that acts by itself gets in the way of fixing it: a schedule
that starts a failing run on every launch, an MCP server that hangs at startup, a
bot that keeps controlling the screen.

Safe mode is chosen **only when the service starts**, by you:

- **Desktop app:** Settings → Diagnostics → **Restart in safe mode**, or **Start in
  safe mode** on the screen shown when the service does not come up.
- **Terminal:** `bot serve --safe`, or set `BOT_SAFE_MODE=1` before `bot serve`.

While it is on, the app shows a banner ("Safe mode. Tools, schedules and channels are
off.") and `bot doctor`, Settings → Diagnostics and `GET /v1/health` (`safe_mode`) say
so. You can still chat (the model answers, with no tools; any tool call is refused)
and read and change bots, schedules and settings.

What is off: every tool, schedules and workflows (nothing fires), the Telegram bridge
and other channels, MCP servers and apps, email/calendar/web connectors, memory
recall and write-back, the desktop backend, and peer or host networking. Saved workflows
never replay (a matching chat line is answered by the model, and the run endpoint is
refused), and coding-CLI executors are refused. Interrupted runs are not resumed:
starting the service marks runs that were running or waiting for approval as paused,
as after any restart, and a later normal start does not resume them either; they stay
visible. Settings pages of the parts that
are off answer "not configured" until you leave safe mode.

Nothing is deleted or changed: bots, chats, schedules, channels and settings stay
exactly as they were, and `mcp.toml` is not even opened. To leave, press **Restart
normally** in the banner (or restart the service without `--safe` and without
`BOT_SAFE_MODE`); everything is back. No tool, bot or paired device can switch safe
mode on or off while the service runs. It is a debugging aid, not a sandbox against
malware.

## Schedules do not fire

- The daemon must be running at the due time. Missed firings are skipped by
  default.
- `bot schedules list` shows the next run time. If the phrase was not understood
  `add` fails; try a form from [Scheduled tasks](scheduled-tasks.md) or preview
  with `POST /v1/schedules/parse`.
- Calendar phrases use UTC unless you pass `--tz`.
- A new firing is skipped while the previous run of the same schedule is still
  waiting or running.

## Telegram

- `pair` says it needs the daemon: start `bot serve` first; the code is created
  by the running daemon and expires after 10 minutes.
- Nothing happens after `setup`: restart `bot serve`; `bot channels telegram
  status` shows configured / enabled / running.
- Two programs polling the same bot token conflict (Telegram answers 409). Use a
  separate token per running daemon, and make sure no webhook is set on the bot.
- You are told to pair: send `/pair <code>` to the bot with a fresh code.

## The secret store asks for a passphrase

With the encrypted-file backend the passphrase comes from `BOT_SECRETS_PASSPHRASE`
or a prompt. Force the OS keychain with `--secrets-backend keyring` if you have one.

## Browser does not open or times out

- **Bounded startup and session waits**: `browser_open` bounds starting the
  browser (spawning Chrome/Chromium, waiting for `DevToolsActivePort`, and
  opening the first tab) to **40 seconds** per attempt, and waiting on another
  browser call holding the session to **45 seconds**. If it times out, do not
  loop `browser_open`; run `bot doctor` on that computer (`sudo praxi doctor` on
  a Linux host) to see what is missing.
- **Early Chrome exit messages**: If the browser process exits during startup,
  the error includes the last lines of Chrome's output (`Browser said: ...`)
  and highlights known causes such as a missing X display, missing or
  unreadable `XAUTHORITY`, a blocked Chrome sandbox (missing AppArmor profile on
  Ubuntu 24.04), or missing shared libraries.
- **Snap browsers**: Snap-wrapped Chromium/Chrome is unsupported under the
  hardened Linux service. Install a non-snap browser or set `BOT_BROWSER_PATH`.
- **Slow Xvfb startup on Linux**: If probing the X11 display fails during a
  busy boot, the daemon re-probes after **15 seconds** instead of staying stuck
  in headless mode until restart.

## Windows SmartScreen blocks the installer

Builds are not code-signed yet. Choose **More info**, then **Run anyway**, only if
you built or trust the installer.

## Collecting information for a bug report

Include `bot --version`, your OS, the exact command and error, whether you use
the desktop app or CLI, and the relevant daemon output. Do not paste your
`api-token`, API keys or the contents of your data directory.
