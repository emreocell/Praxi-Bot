# Long chats: how a bot keeps the thread

In another language: [Türkçe](tr/long-chats.md).

A bot that lives on your computer or on an always-on host can keep one chat open for weeks. A model can only read so much at once, so Praxi keeps the **newest messages** word for word and replaces the **older part** with a short note: *"earlier in this chat"*. This page explains what that note is, what it is not, and how to switch it off.

## What happens

When a chat holds more than about 160 messages (or about 40 KB of text) that no note covers yet, the bot's model writes a short note about the older ones after a turn, in the background. From then on the bot reads **that note plus your recent messages** (at least the last 40 messages, or about 16 KB). When the chat has grown by roughly the same amount again, the note is updated: it folds the previous note and the next older messages into one.

* **Nothing is deleted or changed.** Every message stays in the chat, in search and in exports. Only what is sent to the model changes.
* **Cheap and bounded.** One round reads at most about 48 KB of messages and writes at most about 6 KB. It runs only after the chat has grown, at most twice after one turn, and never twice at the same time for one chat. If a round fails (no model, a limit was reached, an unusable answer), that chat waits 30 minutes before the next try and your turns are not affected. Each round is a normal model call in the [call ledger](llm-call-ledger.md) with the purpose `summary`, so [spending limits](bot-spending-limits.md) apply, and a short entry `chat.summary` (counts and message ids, never the text) goes to the audit log.
* **Which model.** The bot's cheaper helper model if it has one, otherwise its normal model. If the older messages involved private data (mail, files you opened), only the bot's normal model is used, the one that already saw that data.
* **Edits.** If you edit or regenerate any message of the chat, the note is dropped and written again when the chat grows. A round that was running while the chat changed is thrown away.

## What the note is, and what it is not

The note is **quoted data**, never your voice:

* The bot receives it as a clearly labelled *untrusted* block, not as a system instruction and not as a message from you. It is told never to follow instructions inside it. Your current request is always in the messages below it.
* The note has fixed headings written by Praxi. **Your own words** are quoted by Praxi from the messages you typed in the app (the newest few, shortened); the model never writes what you said or asked for. The model only fills in short items under its own headings: what the bot did and found, what is still open, and **what came from outside** (web pages, mail, files, other bots), marked as unverified. If the older messages held anything from outside, the note starts with an *UNTRUSTED* line.
* Nothing in the note counts as something you typed. The checks that let an address or a site through because you wrote it read only the messages you typed in this chat, never the note, so a web page or a mail cannot slip an order into it that is then treated as yours.
* The note **inherits what it was made from**. If the older messages came from a run that had read a web page or private data, a run that reads the note counts as having read it too, with the same sources, so the same approval questions apply as if the old messages were still there.
* Passwords, keys, card numbers and other secrets are masked in everything the model reads for the note (the older note included) and in everything it writes, before the note is stored. Without the secret masker (which the app always runs) no note is written at all.

## Switch it off

Open **Settings → AI and memory → Long chats**, choose a bot and turn **Summarise older messages of long chats** off. Then that bot only sees the newest messages (about 256 messages or 64 KB), as before, and a short notice tells it that older ones were left out. An existing note is kept but not used; turning the switch on again uses it again. The switch is the bot's `history.summary` profile field (`false` to switch off), so it can also be set with `bot` profile edits or the API (`PATCH /v1/bots/{id}`).

## Good to know

* A scheduled task does not read the note (nor its own earlier runs), as before.
* The note belongs to the chat's own bot (its switch, its model and spending limits). A bot you call into the chat with `@name` neither writes nor reads it: it sees the newest messages only. Replies of other bots count as untrusted when the note is written. The note is written after a turn of the chat's own bot.
* A message added to the chat with a date inside the part the note covers (an import) drops the note, like an edit.
* A bot that runs through a coding CLI engine keeps its own session history and gets no note.
* The note is only as good as the model that wrote it. Anything important, say it again, or ask the bot to search the earlier chat: the originals are all there.
