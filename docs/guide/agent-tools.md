# Notes

Every bot has a small built-in scratchpad between runs: a plan for a long task, a checklist, facts it will need tomorrow. Tools: `notes_read` (list notes, read one, or search) and `notes_write` (replace, append or delete a note).

* Notes belong to one bot; other bots never see them. They stay on your computer.
* Notes never store passwords or keys: text that looks like a credential is refused.
* Limits: 32 KB per note, 100 notes, 512 KB per bot.
* Safety: after the bot has read a web page, a mail or other outside content, saving or deleting a note asks for your approval first, because a note shapes what the bot reads later. A note saved in such a run is remembered as untrusted and is shown to the bot as data, not instructions.

Notes are different from memory: memory holds facts you told the bot; notes are the bot's own working space.
