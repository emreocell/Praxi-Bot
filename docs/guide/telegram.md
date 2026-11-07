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
| `/status` | Shows status |
| anything else | Sent to the selected bot as a message |

When a bot needs approval or asks a question, you get a message with buttons. The
run waits until you tap one (or, for scheduled runs, until the 30-minute timeout
pauses it).

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
  runs yet.)
- Telegram messages travel through Telegram's servers. Do not use it for
  anything you would not send through Telegram.
