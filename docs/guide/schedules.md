# Scheduled jobs, in plain words

A **scheduled job** lets a bot do something on its own, at the times you choose:
"every weekday at 9:00, summarise my unread email". You set it up once, and it
runs by itself. (The technical reference is [Scheduled tasks](scheduled-tasks.md).)

## Make your first job

1. Open **Schedules** and press **New**.
2. Write the whole job in **one sentence** at the top, for example *"Every weekday
   at 9:00 check my inbox and summarise it"*, and press **Fill in**. It fills in the
   timing, the task and a name. Or click one of the small **example** chips under it
   (Daily briefing, Weekly review, Inbox triage, Price watcher, Backup reminder).
   Everything can be changed afterwards.
3. Check **when**: *Once*, *Every day*, *Weekdays*, *Every week*, *Every month* or
   *Other*, and the time (and the day, if it asks). One line sums it up, for example
   *"Every weekday at 09:00, Istanbul"*, with the first run under it; **Next 5 runs**
   unfolds the next times. **Change time zone** opens the zone field.
4. Pick the **bot** and give the job a **name**.
5. Write **what the bot should do**, in your own words.
6. Press **Create**.

### Typing it in words

Choose **Other** to describe the timing yourself. English and Turkish work, for
example `every weekday at 9`, `her gün 09:00`, `hafta içi her sabah 8`,
`in 30 minutes`. Nothing is guessed by an AI model: a fixed set of rules reads your
words. You always see what was understood and the next run times first; press
**Create** again to confirm. If the words are not understood, you get a message
saying so; use the chips instead.

### More options (collapsed)

*More options* holds when you want to be **told** about a run (every time, only if
something goes wrong, never), the **time zone**, the on/off switch, the raw **cron
expression** for people who know cron, and the **missed-run** setting. Its title
line shows the current notification choice and zone, so nothing is hidden. Most
people never need to open it.

## If the computer was off

Jobs only run while Praxi-Bot is running on an awake computer. For times that
passed while it was off or asleep you choose:

- **Skip it** (the default): wait for the next planned time.
- **Run once when the computer is back**: one catch-up run, however many were missed.
- **Catch up**: run every missed time, at most 10.

Missed times are shown in the job's history. A restart never runs the same time
twice.

## Run now, pause, edit, delete

- **Run now** starts the job right away; the planned times stay unchanged.
- **Pause** stops it without losing it; **Resume** turns it back on.
- **Edit** changes the timing, the text or the notification choice.
- **Delete** asks you to confirm first. Deleting a bot also deletes its jobs.

## History and problems

Each job lists its recent runs with a plain status (done, failed, running, missed)
and a link to the chat where it ran. A failed or missed run has a **Retry** button.
Open the run to see what happened; if it waited for an approval that never came,
you can resume it from there.

## Telling you about it

For each job: **Every time**, **Only if something goes wrong** (the default) or
**Never**. This is saved on the computer you are using and only controls what the
app announces. If a job needs your OK, you are always told and the request waits
in **Needs you**.

## Safety

A job has exactly the permissions of its bot, never more. Anything that needs your
approval waits for you instead of going ahead, and expires safely if nobody
answers. Web pages, emails and other outside text a job reads are treated as
information, never as orders. A monthly spending limit (Settings) also applies to
scheduled runs. Creating, changing and deleting jobs is owner-only.
