# Talking to your bots from Telegram

The daemon (`bot serve`) can run a Telegram channel: you message your bot from
your phone, watch the answer stream in, and approve or deny gated actions with
inline buttons. Only Telegram accounts you pair can use it.

## Set it up

1. In Telegram, talk to [@BotFather](https://t.me/BotFather), create a bot and
   copy the token it gives you.
2. Store the token (it is asked with hidden input, kept in the secret store and
   never printed):

   ```
   bot channels telegram setup
   ```

   For scripts, `--token-env VAR` reads it from an environment variable instead.
   The token is never accepted as a command-line argument.
3. Start (or restart) the daemon: `bot serve`. It starts the channel
   automatically when a token is stored and the channel is enabled, and stops it
   on shutdown.
4. Pair your account:

   ```
   bot channels telegram pair
   ```

   This prints a one-time 6-digit code that is valid for 10 minutes. Open your
   bot in Telegram and send `/pair <code>`. A wrong code is refused; after a few
   wrong guesses pairing is locked for a while.

## Commands

| Command | What it does |
| --- | --- |
| `bot channels telegram setup` | Store the BotFather token and enable the channel |
| `bot channels telegram pair` | Ask the running daemon for a pairing code |
| `bot channels telegram status` | Show configured / enabled / running and the number of paired users |
| `bot channels telegram disable` | Turn the channel off (the token stays stored); takes effect at the next `bot serve` |

In the chat with your bot: `/bots`, `/use <bot>`, `/stop`, `/status`. Any other
text is sent to the selected bot.

## Notes

- The pairing code is created by the running daemon, so `bot channels telegram
  pair` needs `bot serve` to be up and the channel running.
- Paired users and the chat mapping are kept in `<data dir>/telegram_state.json`;
  the on/off switch is `<data dir>/channels.toml`.
- Everything the bot sends to Telegram is passed through the secret redactor.
- The daemon API exposes `GET /v1/channels` and
  `POST /v1/channels/telegram/pair-code` (bearer token required); see
  `docs/api/openapi.json`.
