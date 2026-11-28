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
| `/missions`, `/mission <id>`, `/pause <id>`, `/resume <id>`, `/stop <id>`, `/mission new <text>` | Manage your missions from the phone (see [below](#manage-missions-from-your-phone)) |
| `/answer <text>` | Answers the questions in the latest "needs you" message (see below) |
| `/status` | One glance: server health, your bots, runs working now and missions by state |
| `/help` | The commands, in English and Turkish |
| `/lang tr` or `/lang en` | Sets this chat's language to Turkish or English (`/lang` alone shows the current one) |
| anything else | Sent to the selected bot as a message |

When a bot needs approval or asks a question, you get a message with buttons. The
run waits until you tap one (or, for scheduled runs, until the 30-minute timeout
pauses it).

## Manage missions from your phone

A bot that works for you around the clock needs a remote control. These commands work
in your own private chat with the bot, typed by you:

| Message | Does |
| --- | --- |
| `/missions` | Lists your missions: open ones first, each with its 8-character id, state and `done/total` |
| `/mission <id>` | One mission: state, counts, what waits for you and the last report (or, before there is one, the latest results) |
| `/pause <id>` | Pauses it: a session in flight stops and no new one starts |
| `/resume <id>` | Continues a paused or blocked mission (the bot has to still be on the Full level) |
| `/stop <id>` | Stops it for good. The bot **asks first**; nothing happens until you tap **Stop it** (or **Keep going**). The question expires after 5 minutes and works once |
| `/mission new <text>` | Starts a mission with `<text>` as the goal, on the bot this chat talks to (see `/bots`, `/use`). That bot has to be on the **Full level**, like any mission |
| `/status` | Server health, your bots, how many runs work now, missions running, waiting for you and paused |

Turkish names work everywhere: `/gorevler`, `/gorev <id>`, `/gorev yeni <metin>`,
`/duraklat`, `/surdur`, `/durdur <id>`, `/durum`, `/yardim`. A bare `/stop` (or `/durdur`)
still stops the run of this chat; `/stop <id>` is the mission command.

The **id** is the 8 characters `/missions` shows (the end of the mission's id: the start
is a timestamp, so missions started together look alike). Case and dashes do not matter,
and a longer part of the id or the whole id works too. If it fits two missions you are
asked for more characters.

How it is kept safe:

- **Only you.** The command has to come from the paired Telegram account, in its own
  private chat. A message from anyone else, from a group, or **forwarded** from somewhere
  else is refused (a forward carries someone else's words under your name). A button
  only counts when pressed by you on a question this bot asked a moment ago.
- **Nothing is stopped by a command alone.** Stopping cannot be undone, so it needs the
  button. Pause and resume change nothing that cannot be undone and run at once.
- **Limits.** At most 20 commands a minute, and 3 new missions per 10 minutes; past that
  the bot says so once and ignores the rest until the window has passed.
- **A new mission is chat text like any other.** Its goal is labelled as typed in
  Telegram by the paired owner (the same standing as a chat message from here, below a
  mission started in the app), a pasted secret is cut out before it is sent, and the
  mission runs under the bot's own limits, budgets and approval rules. Money, mail sending
  and secrets still ask **in the app** (a payment can also be approved
  [from your phone](#approving-a-payment-from-your-phone), with your PIN).
- **Escaping.** Mission titles, reasons and results are the bot's words (and, through it,
  web pages'). They are quoted with links, mentions and `/commands` made harmless.
- Answers never repeat the daemon's own error text; you get a short line that says what
  to do.

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
answer questions and say "try again"; it can never approve anything. (A payment
can also be approved on your phone, with a PIN, through the link described in
[Approving a payment from your phone](#approving-a-payment-from-your-phone).)

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

## Approving a payment from your phone

A payment is the one thing the bot still asks about at the Full level. Its card in
the chat never has a button: **the chat alone can never approve money**, not even
from your own paired account. If you are away from the desktop, you can still
decide on your phone, with a second factor that never goes through Telegram.

Turn it on in **Settings > Notifications > Approve payments from your phone**:

1. Choose a **PIN** of 6 to 12 digits (not your phone's unlock code). The daemon
   keeps only a hash of it.
2. Enter the public **https address of a tunnel** to this computer's daemon, the
   same kind of tunnel as for [incoming webhooks](incoming-webhooks.md) (for example
   a Cloudflare Tunnel). The daemon still listens only on `127.0.0.1`, and it only
   answers requests whose `Host` is a local name: set the tunnel to send
   `Host: 127.0.0.1` (for Cloudflare Tunnel, `httpHostHeader: 127.0.0.1`).

From then on, a payment card in the chat carries a link instead of a button:

- The link works **once**, for **10 minutes**, and only for that one payment. A new
  card for the same payment replaces the old link.
- The page first shows the merchant, the site, the amount and what the bot asked
  for, then asks for your **PIN**. Only after the right PIN does it show a
  screenshot of the checkout page and the **Approve this payment** and **Deny**
  buttons. Approving allows this one payment; it is never a standing permission.
- Merchant, amount and the screenshot are what the checkout page itself shows, and a
  page can lie. The **site** is the address the bot's browser is on: check it.
- Three wrong PINs burn a link. Five wrong PINs within 15 minutes lock phone
  approval for 15 minutes (the settings show until when).
- A link stops working when it is used, when it expires, when you decide in the
  app, and when you change the PIN or turn phone approval off.
- **Check the address before you type the PIN.** Someone who controls the chat could
  send a link to a look-alike page. Your PIN on the real page is what approves.

Without a PIN (the default), payments are approved only in the desktop app, as
before. The design and its threat model are in
[the phone approval ADR](../design/phone-money-approval-adr.md).

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
- The channel's own words change with the language: replies, button labels,
  card headings and notices. What a button does, the information on the card and
  what is masked stay the same.
- The detail block of an approval card (what exactly will happen, ending with
  the line that starts with `DAEMON SUMMARY:`) is written by the daemon in
  English and is not translated yet. If you cannot follow it, decide in the
  desktop app.
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
