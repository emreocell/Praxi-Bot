# Troubleshooting

Start with `bot status`. It checks that the daemon is up and that your token is
accepted, and shows how many approvals are waiting.

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

## Windows SmartScreen blocks the installer

Builds are not code-signed yet. Choose **More info**, then **Run anyway**, only if
you built or trust the installer.

## Collecting information for a bug report

Include `bot --version`, your OS, the exact command and error, whether you use
the desktop app or CLI, and the relevant daemon output. Do not paste your
`api-token`, API keys or the contents of your data directory.
