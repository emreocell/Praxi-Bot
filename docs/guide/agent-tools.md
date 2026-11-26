# Notes

Every bot has a small built-in scratchpad between runs: a plan for a long task, a checklist, facts it will need tomorrow. Tools: `notes_read` (list notes, read one, or search) and `notes_write` (replace, append or delete a note).

* Notes belong to one bot; other bots never see them. They stay on your computer.
* Notes never store passwords or keys: text that looks like a credential is refused.
* Limits: 32 KB per note, 100 notes, 512 KB per bot.
* Safety: after the bot has read a web page, a mail or other outside content, saving or deleting a note asks for your approval first, because a note shapes what the bot reads later. A note saved in such a run is remembered as untrusted and is shown to the bot as data, not instructions.

Notes are different from memory: memory holds facts you told the bot; notes are the bot's own working space.

## Interactive terminal sessions

`shell_run` runs one command to the end and cannot answer a question. For an installer, a login or a setup wizard (`vibe --setup`, `claude auth login`, `codex login --device-auth`) the bot has `shell_interactive`: the program runs on a real terminal (a PTY) and the bot can type into it.

* `start` runs the command and returns the first output and a session id. `read` waits (up to a timeout you can see) for more output. `write` types text, with `enter` to press Enter, or presses one named key (`enter`, `tab`, `esc`, `space`, the arrows, `backspace`, `ctrl_c`, `ctrl_d`). `screen` shows the whole screen, `resize` changes its size, `kill` ends the program and `list` shows the open sessions.
* Every answer names the state: `running`, `waiting_for_input`, `finished` (exit code 0) or `failed` (another exit code, a signal or a kill). A program that waits for you never hangs the bot silently: when it shows a link or a code that **you** must act on (a browser sign-in, a device code), the bot shows it to you and waits.
* **Same rules as `shell_run`.** It is a Shell tool: it asks for approval like `shell_run`, the never-list sees the command when it starts and every line the bot types, the environment is the same short allow-list (no API keys or vault), and the working directory is the bot's workspace.
* **Not a shell.** A program that is itself a shell or an interpreter (`sh`, `bash`, `powershell`, `python`, `node`, `sudo`, `ssh`, `docker` and the like) is refused: typing into one would run commands one keystroke at a time, around the command check. Use `shell_run` for scripts.
* **Bounded.** At most 4 sessions at a time, 8 KiB of text per read, 4 KiB per write, 10 minutes without a call, 1 hour in all. A bot sees only its own sessions.
* **Secrets.** Output is masked with the same secret detector as the rest of the app before the bot sees it, and neither the typed text nor the output is ever logged. A password or a code you type for the bot is still shown on its approval card: give secrets through the secret request, not in chat.

## Memory tools

A bot with memory switched on has four tools: `memory_search` (look up what it remembers), `session_search` (find something said in an earlier chat), `memory_save` and `memory_forget`. A bot whose memory is **Off** does not get them. See [Memory](memory.md).

* `memory_save` only keeps what you said in this turn (it must quote your message), refuses secrets, questions and sub-agent runs, and asks for approval if the run has read outside content. It follows the bot's memory setting: with **Ask before saving** the note waits for your OK, and the "Share what I learn" switch decides whether facts about you are shared with your other bots or kept by this bot.
* Background learning is separate from these tools: after a turn the bot may also keep notes automatically. That includes turns that used tools, child runs or outside content (web pages, files, mail, other bots); notes from such turns are labelled **untrusted**, wait in **Needs your OK** and are not used until you approve them. They are shown to the model only as quoted data, never as instructions.

## Clock and calculator

Two small helper tools are always available to the bot, but are only loaded when it needs them (it finds them by searching its tools):

* `time_now` gives the current date and time in any time zone and does date arithmetic: add or subtract days, months or hours, count how long until a date, or show the same moment in other zones. This stops the bot from guessing today's date.
* `calculate` does exact arithmetic (`+ - * / % ^`, brackets, a few functions such as `round` and `sqrt`) so sums and percentages are not worked out "in the head".

Both only compute on the text the bot gives them and read the clock. They cannot open files, use the network or run programs, so they never ask for approval and never count as sending anything out. Expressions are limited in length and depth, and anything that is not plain arithmetic is refused.

Reading a CSV, JSON or PDF file (`csv_read`, `json_query`, `pdf_extract`) works only inside the folders you allowed and is treated as reading private data: if the bot has just read a web page or mail, anything that would send data out asks you first.
