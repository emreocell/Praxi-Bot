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
* **Same rules as `shell_run`, and stricter where a terminal needs it.** It is a Shell tool: it asks for approval like `shell_run`, and **every call is approved once** (there is no "allow for this chat": what the bot types next is its own choice). The never-list sees the command when it starts and the whole line the bot types into a session, also when it is typed in pieces; a line it flags is refused (an interactive session cannot ask, `shell_run` can). The environment is the same short allow-list (no API keys or vault), and the working directory is the bot's workspace. What the command downloads (`curl -o`, `wget`...) is recorded as untrusted, as for `shell_run`.
* **No shell runs the command.** `start` takes a program and its arguments, started directly: a pipe, a redirect, `$`, a backtick, a glob, `;` or `&&` is an error (quote the character to pass it as it is). Use `shell_run` for command lines.
* **Not a shell.** The program is found on the `PATH`, resolved to the real file it is (a link to `bash` is `bash`) and refused by that name when it is a shell or an interpreter (`sh`, `bash`, `powershell`, `python3.12`, `node`), a launcher of other programs (`sudo`, `env`, `nohup`, `timeout`, `ssh`, `docker`...), a terminal multiplexer, or a pager or editor with a shell escape (`less`, `vim`, `git`...). Typing into one would run commands one keystroke at a time, around the command check. This is damping, like the never-list: it stops the ways around the check that are known, not every program that could run another one.
* **Bounded.** At most 4 sessions per bot (16 on the computer), 8 KiB of text per read, 4 KiB per write, 10 minutes without a call, 1 hour in all. The limits hold in the background too, and everything a session started ends with it, also when the program ends on its own and when the app shuts down. A bot sees only its own sessions.
* **Secrets.** Output is masked with the app's secret detector, including the values of your stored secrets and also when a key is broken across lines, before the bot sees it, and neither the typed text nor the output is ever logged. A password or a code you type for the bot is still shown on its approval card: give secrets through the secret request, not in chat.

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

## Documents and code repositories

Bots can read and write Word, Excel and PowerPoint files, not only text. `docx_read`, `xlsx_read`, `pptx_read`, `pdf_extract`, `csv_read` and `json_query` read them; `docx_write`, `xlsx_write` and `csv_transform` create new files. Three tools change a file you already have without touching the rest of it:

* `xlsx_append_rows` adds rows after the last row of a sheet.
* `xlsx_update_cells` sets single cells by their address (`B2`). A cell keeps its own formatting.
* `docx_append` adds headings, paragraphs, lists and tables at the end of a Word document.

Other sheets, styles, column widths and images stay as they were. Limits: the tools do not extend Excel tables, do not recalculate formulas (Excel does that when you open the file) and refuse macro files (`.xlsm`, `.docm`) and old `.xls` / `.doc` files. Like `fs_write`, every change saves an undo checkpoint first.

All of these follow the same file access setting as `fs_read` and `fs_write`: a bot with "Folder only" access cannot open or change a document outside its folders, and nobody can reach credentials or Praxi's own data this way. `doc_convert` and `doc_preview` (Office to PDF, page images) also need LibreOffice and a network-isolated sandbox on the computer, and write only into the bot's workspace; without them they say so and do nothing.

Developer tools are available too: `git_status`, `git_diff`, `git_log`, `git_show`, `git_branch` and `git_commit` run `git` directly (never through a shell, with hooks switched off) and `process_list` shows running programs. With "Folder only" access the repository must be inside the bot's folders; with "This computer" access it can be any repository except one inside a credentials or settings folder. Reading a repository counts as reading private data, like reading a file.

On a computer with the OCR model files already installed (`PRAXI_OCR_MODEL_DIR`, or the default Praxi models folder), the desktop tools also read on-screen text when an app's accessibility tree is weak, and number what they find (`mark:3`) so the bot can click it. Without the model files nothing changes: the tools say that no OCR engine is configured.
