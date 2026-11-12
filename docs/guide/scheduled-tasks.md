# Scheduled tasks

A **schedule** makes a bot do something on its own: every weekday morning, every
15 minutes, once in two hours. When it fires, the daemon starts a normal run
(same events, same approvals, same history) in a chat. The daemon must be
running (`bot serve`, or the desktop app) for schedules to fire.

## Create one

```sh
bot schedules add <bot-id> "every weekday at 9" "Summarize my unread notifications"
bot schedules add <bot-id> "her gün 09:00" "Günlük raporu hazırla" --tz Europe/Istanbul
bot schedules add <bot-id> "in 2 hours" "Remind me to stand up" --name stretch
bot schedules list
bot schedules run-now <schedule-id>     # fire it now; the timer is unchanged
bot schedules rm <schedule-id>
```

Arguments of `add`: the bot id, a **when** phrase, and the **prompt** the bot
runs each time. Options: `--name` (default: the start of the prompt), `--tz`
(IANA timezone such as `Europe/Istanbul`; default UTC), and `--chat` (post the
runs to this chat; by default a chat named after the schedule is created and
reused).

The desktop app has a Schedules screen for the same thing.

## When phrases

English and Turkish phrases are understood; the daemon parses them
(`POST /v1/schedules/parse` previews the result). Examples taken from the
parser's tables:

| Kind | Examples |
| --- | --- |
| Interval | `every minute`, `every hour`, `every 15 minutes`, `saat basi` |
| Daily and weekly | `every day`, `every morning` (09:00), `every evening` (18:00), `every night` (22:00), `at midnight`, `at noon`, `every weekday`, `every weekend`, `every week`, `every monday at 8`, `her gün 09:00` |
| Monthly | `every month` (1st, 09:00) |
| One-shot, relative | `in a minute`, `in half an hour`, `in 30 minutes`, `2 saat sonra`, `yarim saat sonra` |

If a phrase is not understood the command fails with an error rather than
guessing. The API also accepts raw 5-field **cron** expressions (for example
`0 9 * * 1-5`, optionally with a timezone), fixed intervals in seconds and exact
one-off timestamps.

Calendar phrases use the timezone you give with `--tz`; without it, UTC.

## What a schedule can run

The CLI creates schedules that send a **prompt** to the bot. Through the API
(and so the desktop app) a schedule's payload can also be:

- a **workflow** (replayed without a model, see [Workflows](workflows.md)), or
- a direct **tool** call.

All of these go through the normal permission gate.

## Webhooks

A schedule can have a webhook trigger so something outside can fire it:
`POST /v1/webhooks/{id}` with the header
`X-Signature-256: sha256=<hex HMAC-SHA256 of the body with the schedule's webhook secret>`.
No bearer token is used there; the signature is the authentication. The body
(at most 256 KB) replaces `{input}` in the schedule's prompt. The route is still
loopback-only like the rest of the API, so reaching it from outside your machine
needs a tunnel or reverse proxy you set up yourself. Webhook secrets are set
through the API (`webhook_secret` on create or update); the CLI has no flag for it.

## Missed runs

If the daemon was stopped or the computer asleep when a schedule was due, the
schedule's **misfire policy** decides: `skip` (default: ignore missed runs),
`run_once` (run once for the missed window) or `run_all` (run every missed
occurrence, at most 10). The policy is set through the API; the CLI always uses
the default.

## Approvals in unattended runs

A scheduled run behaves like you for permissions unless the bot's rule has a
separate `schedule` setting. If it needs approval and you do not answer within 30
minutes, the run pauses. Money and never-list actions always ask. Tip: give
unattended schedules a bot whose policy only allows what the job needs.

A schedule runs one run at a time: if the previous firing is still in flight
(for example waiting for an approval), a new firing is skipped.

## Letting a bot manage its own schedules

Bots whose profile allows `schedule_*` (the General preset does; existing
profiles are not changed) can ask for schedules in chat ("remind me every weekday
at 10:30"). The tools are `schedule_list`, `schedule_create`, `schedule_update`,
`schedule_pause`, `schedule_resume` and `schedule_delete`. A bot sees only its own
schedules and can change (update, pause, resume, delete) only the ones it created
itself; schedules you made in the app, CLI or API are read-only to it (shown as
"Made by bot" in the list when a bot made them). A bot can never edit or resume a
schedule that has a webhook. A new schedule posts into the chat it was asked in.

A schedule is authority you hand over for later, so **create, update and resume
always ask you**, whatever the policy says (a policy that denies them still
denies). The card shows the exact prompt, the timing, the next three firing times
in your timezone and what the bot may do unattended (for an update, old and new
side by side). What you approve is exactly what is stored: a time like "in 2 hours"
is fixed when the card is shown, and if the schedule changed meanwhile the change is
refused. Prompts are limited to 4,000 characters and names to 80. Control
characters, text-direction overrides and invisible characters (zero-width,
soft hyphen, line/paragraph separators, variation selectors, tag characters) are
rejected, so no hidden text can ride along in a prompt that runs unattended.
**Look-alike letters (homoglyphs) are not filtered**: read the prompt on the card.
"Always allow" and "this chat" grants never apply to these three, and the daemon
refuses such a request from any client (desktop, HTTP, Telegram, CLI). A request
that contains something that looks like a secret (a key or token) is refused, since
the card would mask text the schedule would keep. Telegram and the CLI show the
full details too (the prompt quoted between begin/end lines, followed by a summary
line the daemon writes); if a client cannot show them, approve in the desktop app.

Pause and delete of a schedule a bot created follow the normal approval rules.
When you edit a bot-made schedule in the app, it becomes yours: from then on the
bot can read it but no longer change it. A bot can still pause or delete the ones
it made and you have not rewritten.

Limits (in `<data dir>/schedule.toml`, all optional):

```toml
max_per_bot = 20          # schedules per bot, whoever made them
min_interval_secs = 300   # shortest gap between firings of a bot-made schedule
timezone = "Europe/Istanbul"   # default for "9:00"; else the computer's, else UTC
```

`max_per_bot` must be 1 to 1000 and `min_interval_secs` 1 to 604800. If the file is
broken or has a nonsensical value, the schedule tools are switched off (they never
fall back to the defaults) and `bot doctor` reports it.

Every create, update, pause, resume and delete by a bot is written to the audit
log with the bot id and run id (updates also record the old prompt's length and
SHA-256). The entry is written before the change; if the change then fails, a
`schedule.failed` entry follows, and if the audit write itself fails nothing is
changed.

