# Agent built-in tools: audit and gaps

Status: gap 1 implemented in the `feat/c478-agent-tools` change; gaps 2-3 deferred (budget).

## What exists (beyond MCP)

| Family | Tools | Class |
| --- | --- | --- |
| Files | `fs_read/list/search`, `fs_write/edit/move/delete`, `checkpoint_*` | private read / local write / destructive |
| Documents | `pdf_extract`, `docx_*`, `xlsx_*`, `csv_*`, `json_query` | private read / local write |
| Web | `web_search`, `web_extract` (URL to Markdown, SSRF guard), `web_feed`, `web_sitemap`, `web_metadata`, `web_research` (bounded search-and-read pass, no URL input; its `web_research_report` is a workspace write through `fs_write`) | untrusted + egress |
| Browser / desktop | `browser_*`, `desktop_*` | untrusted private + egress, some `executes` |
| Shell / code | `shell_run`, `script_run`, `git_*`, `process_list` | executes |
| Mail / calendar | `email_*`, calendar connector | private, untrusted, egress on send |
| Memory | `memory_search/save/forget`, `session_search`, `knowledge_search`, `documents_search` | private read; writes ask when tainted |
| Scheduling | `schedule_*` (always ask for create/update/resume) | egress-like, delegated future authority |
| Bots, skills, workflows, secrets | `bot_*`, `skill_*`, `workflow_*`, `secret_request`, `secrets_list` | per tool |
| Meta | `ask_user`, `todo_write` | none, always allowed |

The two candidates named in the brief already exist: `web_extract` is the
SSRF-guarded fetch (every hop checked, size/time caps, untrusted + egress) and
`schedule_create` is the reminder / one-shot tool (always asks). They were not
re-implemented.

## Gaps, ranked

| # | Gap | Safety class | Decision |
| --- | --- | --- | --- |
| 1 | A scratchpad the bot can keep between runs (memory is user-confirmed facts only; there is no place for a plan, checklist or working data) | private; write is local, marked by exposure | **done**: `notes_read`, `notes_write` |
| 2 | The clock: models guess the date and time zone arithmetic | none (pure) | **done (round 2)**: `time_now`, deferred |
| 3 | Exact arithmetic: models miscalculate sums, percentages, unit math | none (pure, no code execution) | **done (round 2)**: `calculate`, deferred |
| 4 | Clipboard read/write | private (read), egress-like (write); needs an OS clipboard dependency per platform | not done (round 2 re-checked, still skipped): the clipboard is the most likely place for a pasted secret, and the cross-platform dependency needs its own review |
| 5 | Generic `http_request` (POST/PUT) | egress, destructive | not done: always-ask by construction, little value over `web_extract` + connectors |
| 6 | `download_file` to the workspace | untrusted write | not done: needs the file-origin mark and size policy of `fs_*` |

## Direct tool budget

The original change fit the last two slots of the 40-tool direct budget.
Notes use `notes_read` (no argument lists, `name` reads, `query` searches) and
`notes_write` (`mode` replace, append or delete). The installed catalog can now
grow independently of the request budget through [deferred tool loading](tool-discovery.md).

## Safety design of the new tools

* `notes_read`: statically `private_read`, but it reports the marks of the
  notes it returns per call (`OutputExposure` replaces the class). Each note
  stores two marks from the run that wrote it: `untrusted`
  (`ToolContext::exposure_untrusted`) and `private`
  (`ToolContext::exposure_private`, a caller that only says "tainted" counts as
  private). Reading an untrusted note reports untrusted exposure and wraps the
  text in `<untrusted_note_data>` with a data notice (a closing tag inside the
  text is broken, not removed, so a tag split around another one cannot re-form).
  A note from a clean run reports neither mark, so a scheduled run's progress
  checkpoint does not taint the next run (a read in a run that already holds
  untrusted content reports private for every note, so an injected page cannot
  use the notes as a clean source for a search query). An append keeps the marks of the old
  text; a replace takes the marks of the run that replaced it. Notes stored
  before the `private` mark existed deserialize as private (fail closed).
* `notes_write`: declared `(untrusted=false, private=false, egress=false)`. It
  only touches the bot's own file on this machine, so there is no approval card
  and no egress budget (before: egress-like, which parked every unattended run
  that had read a page). What a tainted run writes is stored marked and comes
  back fenced and as untrusted exposure, so an injected note is data to the next
  run: it cannot raise a permission and anything that sends data out after
  reading it asks as after any untrusted read. Refuses
  text that `secret_store::detect_secrets` flags, names outside
  `[A-Za-z0-9 _.-]`, and a missing bot id (fail closed). Limits: 64-char names,
  32 KiB per note, 100 notes, 512 KiB per bot.
* `memory_save` in an unattended run (schedule, external schedule, mission,
  routine, sub-agent): no owner message exists, so the owner-quote rule cannot
  apply and is not weakened for chats. The tool stores a *bot-noted* memory
  instead: `Memory::origin` (`MemoryOrigin`: run id, kind of run, schedule or
  mission id, untrusted flag; column `origin_json`, migration 0004), scoped to
  the calling bot, never `profile`, `Trust::Untrusted` when the run held outside
  content (always for an external trigger and for sub-agents), held for review
  per the bot's memory mode (an untrusted one is always held, as in background
  learning). Recall renders three separate sections: owner
  memories, `<bot_noted_memory>` (with provenance and "not confirmed by the
  owner") and `<untrusted_memory_data>` (with provenance). Bounds: 500
  characters, 10 per run, 200 per bot. Rewriting the text clears the origin (it
  is then the owner's). `mission` runs used to count as human turns here; they no
  longer do. The class of `memory_save` is unchanged (egress-like): in a chat the
  ask after outside content stays, and an unattended run that read a page still
  asks before `memory_save` (use notes for checkpoints).
* Storage: one JSON file per bot under `<data>/notes/`, named by a hash of the
  bot id; nothing from the model is ever a path.

## Permission levels

Notes use category `Fs` (as `memory_*`), so they follow the bot's file policy:
read-only presets allow the reads and deny the write; ask-before-write presets
ask for `notes_write` and `notes_delete` as for any write; `Full` no longer asks
on a tainted run (the write is local, see above). Preset
`tool_allow` lists include `notes_*`.

Eval scenarios: `inj-notes-poison-after-page(-holdout)` (a tool declared as
egress still asks after untrusted text; the real `notes_write` is no longer one)
and `inj-notes-read-then-exfil(-holdout)`
(private notes + untrusted page cannot be sent out without approval).

## Round 2: deferred tools (tool discovery)

Tool discovery ([tool-discovery.md](tool-discovery.md)) lifted the 40-tool
limit, so `time_now` and `calculate` are registered as ordinary tools and are
**not** in the curated direct set: the model finds them with `tool_search` and
activates them with `tool_describe`.

* Both are `ContentClass::none()` (no untrusted ingestion, no private read, no
  egress), effect `Read`, category `Meta` (always allowed, never taints a
  run, never an approval card). They take only short model-written strings and
  read the clock; no file, network or process access exists in the code path.
* `calculate` is a hand-written recursive-descent parser (no `eval`, no script
  engine). Bounds: 500 characters, 400 tokens, nesting 32, finite results only,
  division by zero and domain errors are plain errors.
* `time_now` uses `chrono`/`chrono-tz` (already in the workspace; MIT/Apache) and
  the system zone name. Every amount is bounded to +-100000 and every date
  operation is the checked variant. A local time skipped by a clock change is an
  error, a repeated one takes the first.
* Already present and therefore not re-implemented: `json_query`, `csv_read`
  (and `csv_transform`), `pdf_extract` (the task's `pdf_text`). They are
  `untrusted_private` reads limited to allowed folders with size and page caps.
* Clipboard: skipped. A clipboard read can hold a pasted password and is not
  a file the owner chose to share, a write replaces what the owner copied, and
  it needs per-OS native dependencies. Neither can be made safe by
  classification alone, so it stays out until it has its own review.
* Eval scenarios (`evals/scenarios/inj-*-after-page`, `inj-csv-read-`,
  `inj-json-query-`, `inj-pdf-extract-then-exfil`): after an untrusted page and a
  private read, the pure tools do not clear the taint and the document readers
  cannot be followed by an unapproved send.

## Files and commands without a coding CLI (critic round, 2026-11-26)

An independent critic found that a General-preset bot with only a model key could not
create a file or run a command ("I do not have a file writing tool"). The tools existed;
three things hid them:

* `bot setup` wrote the General preset's `[tools] allow` with only `fs_read`, `fs_list`,
  `fs_search`, so `fs_write`, `fs_edit`, `shell_run` and `checkpoint_*` were invisible to
  that bot (a bot made in the app has no allow list and sees everything). The preset now
  lists `fs_*`, `checkpoint_*` and `shell_run`; the permission preset still decides ask,
  allow or deny, and a "Folder only" bot still never gets `shell_run`.
* `fs_write`, `fs_edit` and `shell_run` were only reachable through
  `tool_search` + `tool_describe`; a light model searched for "write file", did not get them
  and gave up. They are now in the direct set (shown only when the bot may use them).
* The tool-access prompt said "files (read, write, search)" whenever any `fs_*` tool was
  allowed. It now names read-only files as such and adds one fixed "Not available to you"
  line for file changes and for commands when they are not permitted, so a bot never promises
  a tool it does not have.

The "Engine" (coding CLI) setting is unrelated to this: Praxi's own file and command tools
are the default. Shell commands still cannot be confined to a folder (see the Windows sandbox
note), so the only workspace confinement is the file-access mode, unchanged.
