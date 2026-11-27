# Telegram

Talk to your bots from your phone. The daemon runs a Telegram channel: you
message your bot, watch the answer stream in, and approve or deny gated actions
with inline buttons. Only Telegram accounts you **pair** can use it; everyone
else is refused.

The complete setup reference is [docs/telegram.md](../telegram.md). In short:

```sh
bot channels telegram setup     # paste the token from @BotFather (hidden input)
bot serve                       # the channel starts with the daemon
bot channels telegram pair      # prints a one-time 6-digit code (valid 10 minutes)
```

Then open your bot in Telegram and send `/pair 123456` (your code).

## In the chat

| Message | Does |
| --- | --- |
| `/bots` | Lists your bots |
| `/use <bot name or id>` | Chooses the bot this chat talks to |
| `/stop` | Stops the current run |
| `/answer <text>` | Answers the questions in the latest "needs you" message (see below) |
| `/status` | Shows status |
| `/lang tr` or `/lang en` | Sets this chat's language to Turkish or English (`/lang` alone shows the current one) |
| anything else | Sent to the selected bot as a message |

When a bot needs approval or asks a question, you get a message with buttons. The
run waits until you tap one (or, for scheduled runs, until the 30-minute timeout
pauses it).

## When a bot reports back on its own

You do not have to be in the chat for your bots to reach you. The daemon watches
your **missions** and your **scheduled and background tasks** and sends your paired chat a
short message when something needs your attention:

- **Finished:** a few lines on what was done, with the counts.
- **Failed:** the reason, and a **Try again** button (for a scheduled task it runs
  that occurrence again; for a mission that stopped on a limit it continues it).
- **Needs you:** every open question in **one** message, numbered. Answer them
  right there:
  - one question: `/answer Webioo Ltd` (write `/answer skip` to skip it);
  - several: one numbered line each, for example `/answer 1: Webioo Ltd`, then
    `2: skip` and `3: use the shop name` on the next lines.
- **Progress** (off by default): a short note at 25%, 50% and 75% of a mission.

Work you start from this chat reports in this chat as before; these messages are
for work that runs without you. A task that runs often is announced at most once
every 15 minutes (a failure, every 5), so a schedule that fires every minute cannot
flood your phone.

**Money and approvals stay in the app.** A purchase, a one-time code or any other
approval is listed as "waiting in the app", with no button here. The chat can only
answer questions and say "try again"; it can never approve anything.

**What is in a message is not trusted.** Titles, reasons, results and questions come from
the bot, and through it from web pages. They are shown as quoted lines with links,
mentions, commands and formatting made harmless, so a page cannot put a live link or a
`/command` button into your chat. What a task said after it read private data (mail,
files, accounts) is not sent here at all: the message says to open the app. Only you,
typing in your own chat, can answer or retry; a forwarded message cannot.

In **Settings > Notifications > Telegram, Discord and other channels** you choose
which of these you want (finished, failed and needs-you are on; progress is off) and
set **quiet hours**. Messages that arrive during quiet hours wait and are sent when
the window ends, marked as held back; a progress note that was missed is dropped.
The same digests appear in the app's notifications panel. The settings live on the
daemon, so they also apply when it runs on a server and the app is closed.

## Language

Commands, approval cards, buttons and notices from the channel itself are in
English or Turkish. Each chat has its own language and keeps it across restarts;
`/lang` changes it at any time. What your bots write, tool output and
daemon messages are not translated.

- A chat that was paired **before** chat languages existed, or a user listed in
  the configuration, stays in English until someone in that chat sends `/lang`.
  An upgrade never changes the language of an approval card on its own.
- A chat paired with `/pair` from now on starts in Turkish or English depending
  on the Telegram app's language; any other app language falls back to
  English. (The channel can also fall back to the desktop app's language, but
  the desktop app does not pass it to the daemon yet.)
- Only the button labels change with the language. What a button does, the
  information on the card and what is masked stay the same.
- Before a chat is paired it has no language, so `/pair` errors are in English.
  The reply to a successful `/pair` already uses the chat's new language.
- The refusal sent to accounts that are not paired is **English on purpose**:
  the channel knows nothing about a stranger's language and does not guess.
- Text that comes from elsewhere keeps its own language: bot names, error
  details reported by the daemon, the daemon's status value and transcribed
  voice notes.

## Good to know

- Create your own Telegram bot with BotFather; the token is yours. Praxi polls
  Telegram from your machine, so no inbound port or public address is needed.
- Wrong pairing codes are refused, and after a few wrong guesses pairing is
  locked for a while. Messages from unpaired accounts are refused with a fixed
  text and rate-limited.
- Everything sent to Telegram passes through the secret redactor, which masks
  secret values it knows (including your bot token).
- `bot channels telegram status` shows whether the channel is configured,
  enabled and running and how many users are paired. `bot channels telegram
  disable` turns it off at the next `bot serve`; the token stays stored.
- Runs started from Telegram are ordinary runs of the selected bot and follow
  that bot's permission rules like runs you start at your desk. (A separate
  `channel` initiator exists in the policy model but is not assigned to Telegram
  runs yet.) A message from your own paired chat counts as **you** for the Full
  level (no extra cards for web browsing and plain shell commands, see
  [Chatting and approvals](chatting-and-approvals.md)): it needs the paired
  Telegram account and the daemon's owner key. Money, mail sending, secrets and
  the never-list still ask, and other senders never get this.
- Telegram messages travel through Telegram's servers. Do not use it for
  anything you would not send through Telegram.
