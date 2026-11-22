# Notes, clock and calculator

Besides files, web, mail and the browser, every bot has three small built-in helpers.

## Notes (a scratchpad between runs)

A bot can keep short named notes: a plan for a long task, a checklist, facts it will need tomorrow. Tools: `notes_list`, `notes_read` (one note, or a search across notes), `notes_write` (replace or append), `notes_delete`.

* Notes belong to one bot; other bots never see them. They stay on your computer.
* Notes never store passwords or keys: text that looks like a credential is refused.
* Limits: 32 KB per note, 100 notes, 512 KB per bot.
* Safety: after the bot has read a web page, a mail or other outside content, saving or deleting a note asks for your approval first, because a note shapes what the bot reads later. A note saved in such a run is remembered as untrusted and is shown to the bot as data, not instructions.

Notes are different from memory: memory holds facts you told the bot; notes are the bot's own working space.

## Clock

`time_now` answers with the computer's current date and time, in any time zone, or converts a given moment. No approval needed.

## Calculator

`calculate` evaluates arithmetic exactly (`+ - * / % ^`, brackets, `sqrt`, `round`, `min`, `max`, ...). It does not run code. No approval needed.
