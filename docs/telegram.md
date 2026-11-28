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

In the chat with your bot: `/bots`, `/use <bot>`, `/stop`, `/status`,
`/answer <text>` (answer the questions of a "needs you" message the daemon sent on its
own, see [When a bot reports back on its own](guide/telegram.md#when-a-bot-reports-back-on-its-own)), and
`/lang tr` or `/lang en` to pick the chat's language (see
[Language](guide/telegram.md#language)). Missions can be steered from the chat too:
`/missions`, `/mission <id>`, `/pause <id>`, `/resume <id>`, `/stop <id>` (confirmed with
a button) and `/mission new <text>`, owner only, in English or Turkish names (see
[Manage missions from your phone](guide/telegram.md#manage-missions-from-your-phone)).
Any other text is sent to the selected
bot. Voice messages, photos and files are not
read yet: the bot answers that it only understands text.

## Notes

- The pairing code is created by the running daemon, so `bot channels telegram
  pair` needs `bot serve` to be up and the channel running.
- Paired users, the chat mapping and each chat's language (`chat_languages`;
  a chat without an entry is English) are kept in `<data dir>/telegram_state.json`;
  the on/off switch is `<data dir>/channels.toml`.
- Everything the bot sends to Telegram is passed through the secret redactor.
- The daemon pushes mission and background-run digests to every paired chat from
  `GET /v1/owner-notices/stream` (owner key required); the push settings and quiet
  hours are `GET`/`PUT /v1/owner-notices/settings`.
- The daemon API exposes `GET /v1/channels` and
  `POST /v1/channels/telegram/pair-code` (bearer token required); see
  `docs/api/openapi.json`.
