# Missions: one big task, finished without check-ins

A secretary who asks "shall I open the file? shall I pick up the pen? this page
is done, shall I go to the next?" is not much help. Praxi works the other way
round: you give **one** big task, the bot asks what it needs **once** at the
start, then works alone until the task is done and reports.

Example: *"Get backlinks for my site: find sites that give backlinks, check
where we already have an account, register on the others with your own e-mail
address, and move on to the next site."*

## Two things that changed

1. **A bot no longer stops to ask "should I continue?".** Every bot is told to
   work through the whole task, to ask all its questions together before it
   starts, and to decide small choices itself and tell you in the final report.
   If a reply still ends with a request for permission to carry on ("I did
   one, shall I do the next?", "want me to also research other companies?"),
   the daemon answers "yes, continue" itself on the **Full** level. A turn that
   ends while its plan still has open steps is continued at every level. This
   is bounded: a limit on automatic continuations per turn, the step and token
   budgets and a time window. Asking about money is never answered
   automatically, and a real question (which e-mail address?) still ends the
   turn. The permission engine is unchanged: every tool call is still decided
   by it, so a bot below Full still asks before it acts.
2. **Missions** make long work durable. A mission is a list of items (sites,
   leads, documents) that lives in the daemon's database, not in the chat. The
   bot works through it in **sessions** (ordinary runs). Each session starts
   from a short "where am I" brief that the daemon builds from the list, so a
   restart, a crash or a full context window changes nothing, and the 40-step
   limit of one run no longer ends the work: the next session just goes on.

## Starting a mission

Open the bot's right-hand panel, **Activity**, **Start a mission**, describe the
task once and press **Start**. The bot has to be on the **Full** level (a mission
works without asking, so that is the level where it can).

The bot may ask its questions once, in one message, at the start. After that it
decides for itself and writes the decisions down; you read them in the report.

## What you see

The Activity tab shows each running mission with a progress bar: **done**,
**blocked** and **left**. You can pause, resume or stop it there.

An item that needs you (a CAPTCHA, a 2FA code, a question only you can
answer, a payment) is **parked**: it is marked blocked with the reason and the bot
carries on with the rest. All blocked items appear in **one list**; type an
answer for each (or skip it) and send them all with one button. When nothing
else is left to do, the mission waits for you and the report lists what is
blocked.

## When a mission stops

* **Done:** the goal you set is met. The daemon checks it from the list; the
  bot cannot declare it done by saying so.
* **Needs you:** only blocked items are left.
* **Budget:** sessions (default 60), model calls (default 1500), time (default
  14 days) or an optional spend cap are used up. The report says which, with the
  numbers. The bot cannot raise a budget.
* **No progress:** four sessions in a row changed nothing, or three in a row
  failed (a provider outage, for example).
* **Paused / stopped:** you did it. Stopping ends the run in flight.

At the end (and whenever it blocks) the mission posts a short report in its own
chat: what was done, what is blocked and why, what was given up, and the
decisions the bot made.

## Limits of this first version

Missions are delivered in waves (see the [design](../design/missions-adr.md)).
What works today: the item list, session chaining, budgets, parking of CAPTCHA
and 2FA items, the report, pause/resume/stop, restart recovery and the Activity
panel. What does **not** exist yet: waking a mission by a timer, an incoming
mail or a webhook (the bot cannot "check the verification mail in 10 minutes"
without holding a run), a separate permission scope per mission, mail pacing and
opt-out handling for outreach, sub-missions for helper bots and push reports to
Telegram. A mission session is the owner's own run: it asks for what the Full
level asks for (money, secrets, messages to other people), so an unattended
mission can sit on an approval card; it then blocks after ten minutes with a
note.

The laptop sleeping pauses a mission (the daemon stops with it). For work that
should run all night, use an [always-on host](remote-host.md).
