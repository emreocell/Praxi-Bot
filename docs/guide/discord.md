# Discord

Talk to your bots from Discord, in a private chat with your own bot account or
in one server channel you allow. Only **your** Discord account can use it:
everyone else is ignored (in a private chat they get one polite line).

## Connect it (about 5 minutes)

```sh
bot channels discord setup     # explains the steps, asks for the token (hidden), then tests it
bot serve                      # the channel starts with the daemon
bot channels discord pair      # prints a one-time 6-digit code (valid 10 minutes)
```

1. Open <https://discord.com/developers/applications> and press **New Application**.
2. Open **Bot**, press **Reset Token** and copy the token.
3. On the same page, under **Privileged Gateway Intents**, switch **Message Content Intent** on. Without it Discord sends the bot empty messages.
4. Run `bot channels discord setup` and paste the token. It goes into your secret store and is never shown again.
5. The setup ends with a connection test and an invite link. Use the link to add the bot to a server you manage, then open its profile in the member list and choose **Message**.
6. In Discord, open a **private message** with your bot and send `!pair 123456` (your code).

Use `bot channels discord setup --lang tr` or `--lang de` for Turkish or German setup steps and default replies. You can change replies later with `!lang`.

`bot channels discord test` repeats the connection test at any time.

## Who can talk to the bot

- Pairing only works in a private message with the bot, and only with a code you created on your computer. A code typed into a server does nothing; create a new one.
- The paired account is the **owner**. Nobody else is ever served, not even in a server channel you allowed. A new valid code from another account replaces the owner and forgets the allowed server channels.
- Bots and other apps are ignored.
- Wrong codes are refused and, after a few, locked for a while.

## In the chat

Commands start with `!` (a `/` also works if you send it as plain text).

| Message | Does |
| --- | --- |
| `!bots` | Lists your bots |
| `!use <name>` | Chooses the bot this chat talks to |
| `!stop` | Stops the current task |
| `!missions`, `!mission <id>`, `!pause <id>`, `!resume <id>`, `!stop <id>`, `!mission new <text>` | Manage your missions (see [below](#manage-missions-from-your-phone)) |
| `!answer <text>` | Answers the questions in the latest "needs you" message (see below) |
| `!status` | Server health, your bots, runs working now, missions by state and tasks waiting for your OK |
| `!lang en`, `!lang tr`, `!lang de` | Language of the bot's own messages |
| `!allow` / `!disallow` | In a server channel: let the bot answer you there / stop (owner only) |
| `!help` | Short help |
| anything else | Sent to the selected bot |

## Manage missions from your phone

In your **private chat** with the bot, only you can list, read, pause, resume and stop
missions and start new ones. They are the same commands as on [Telegram](telegram.md#manage-missions-from-your-phone),
with `!` in front:

| Message | Does |
| --- | --- |
| `!missions` | Lists your missions with their 8-character ids, state and `done/total` |
| `!mission <id>` | Details and the last report |
| `!pause <id>`, `!resume <id>` | Pause or continue |
| `!stop <id>` | Stops it for good; you confirm with the **Stop it** button (or **Keep going**) |
| `!mission new <text>` | Starts a mission on the bot this chat uses; it has to be on the Full level |
| `!status` | Health, bots, runs, missions |

Turkish names work too: `!gorevler`, `!gorev <id>`, `!gorev yeni <metin>`, `!duraklat`,
`!surdur`, `!durdur <id>`, `!durum`, `!yardim`. A bare `!stop` still stops the current task.

These commands do **not** work in a server channel you allowed (you are told to use the
private chat), and Discord forwards are ignored. At most 20 commands a minute and 3 new
missions per 10 minutes. A goal you start from Discord counts like a Discord chat
message (not as typed in the app): the sites you name in it do not become pre-approved.
What a mission wrote is shown with Discord's formatting, links and mentions made harmless.

## When a bot reports back on its own

You do not have to be in the chat for your bots to reach you. The daemon watches
your **missions** and your **scheduled and background tasks** and sends your private chat with the bot a
short message when something needs your attention:

- **Finished:** a few lines on what was done, with the counts.
- **Failed:** the reason, and a **Try again** button (for a scheduled task it runs
  that occurrence again; for a mission that stopped on a limit it continues it).
- **Needs you:** every open question in **one** message, numbered. Answer them
  right there:
  - one question: `!answer Webioo Ltd` (write `!answer skip` to skip it);
  - several: one numbered line each, for example `!answer 1: Webioo Ltd`, then
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

In an allowed server channel **every** message you write goes to the bot, so pick
a private channel that is only for this. Anyone who can read that channel can see the bot's replies.

## Approvals

When a bot needs your OK you get a message with **Approve once** and **Deny**
buttons. Only you can press them, only in the chat where the card appeared,
once, within 10 minutes.

- Buttons appear only for low-risk requests whose full text fits in the message. Everything else (risky actions, requests with hidden secrets, very long requests) says so and waits for the app on your computer.
- There is no "always allow" here. Standing permissions are decided in the app.

## Messages and attachments

- Long answers are split into several messages and updated about once a second. The last unfinished word stays hidden until it is safe to redact.
- The bot never pings anyone: `@everyone`, `@here` and user or role mentions in an answer are made harmless.
- Attachments are **not downloaded or opened**. The bot only learns that a file with a certain name and type was sent, marked as untrusted. Send the text instead.
- Everything sent to Discord goes through the secret redactor, which hides secrets it knows, including the bot token.
- The bot stays within Discord's rate limits and waits when Discord asks it to.

## Good to know

- You create your own Discord application, so the token is yours. The bot connects out from your computer; no open port is needed.
- Discord messages travel through Discord's servers. Do not use it for anything you would not send through Discord.
- `bot channels discord status` shows whether the channel is configured, enabled and running and whether an owner is paired. `bot channels discord disable` turns it off at the next `bot serve`; the token stays stored.
- If the bot says nothing: check `bot channels discord test` (token and Message Content Intent), and that `bot serve` was restarted after setup.

- Keep `bot serve` running. Messages sent while it is off are not collected later.
