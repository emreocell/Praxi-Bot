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
   one, shall I do the next?"), the daemon answers "yes, continue" itself on the
   **Full** level, but only for your own turn (typed in the app, from the admin
   desktop or from your paired Telegram chat) and only when the bot has not read
   a web page or a mail in that turn: a page can plant that very question. A
   turn that ends while its plan still has open steps is continued the same way
   at every level. This is bounded: a limit on automatic continuations per turn,
   the step and token budgets and a time window. A turn that a safety limit
   closed is never re-opened. Anything about money (amounts, payment, banking,
   fees, prices, orders, crypto), anything about deleting, a generic consent
   question ("shall I go ahead?") and an offer of more work ("want me to also
   research other companies?") is never answered automatically: the reply is
   read as a whole, in English, Turkish, German and Spanish, and a real question
   (which e-mail address?) still ends the turn. The permission engine is
   unchanged: every tool call is still decided by it, so a bot below Full still
   asks before it acts.
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
**waiting**, **blocked** and **left**. You can pause, resume or stop it there.

An item that needs you (a CAPTCHA, a 2FA code, a question only you can
answer, a payment) is **parked**: it is marked blocked with the reason and the bot
carries on with the rest. All blocked items appear in **one list**; type an
answer for each (or skip it) and send them all with one button. When nothing
else is left to do, the mission waits for you and the report lists what is
blocked.

**A code is never an answer.** A 2FA or verification code is single-use, so it
is never stored in the list and never shown to the bot. An item that waits for a
code only offers "Skip this one": enter the code on the site yourself, then skip
the item. The same holds for passwords: add a credential in the app, never write
it in an answer.

## Waiting for a mail, a timer or a webhook

Some steps cannot go on at once: a site sends a verification mail, an account is
reviewed "within a few hours", a form tool calls you back. The bot does not hold a
session open for that. It **parks the item** with `mission_wait` and takes the next
one; when nothing else is open, the session ends and the mission shows
**Waiting**. Three things can wake it:

* **A timer** (`after_seconds`, 30 seconds to 7 days). The item continues when it is due.
* **A mail in the bot's own inbox** (a sender domain, a word in the subject or a
  keyword). The daemon looks into the inbox and junk folder of the bot's **own**
  mail identity about once a minute; the owner's mailbox is never read for this.
  The bot gives a timeout (60 minutes by default, 3 days at most).
* **A webhook you created** for that bot (Settings, incoming webhooks). The bot
  names its id. A signed call that a mission waits for wakes the mission and does
  **not** start a separate run.

The Activity tab lists each parked item: *waiting for a mail from example.com*,
*waiting until 14:00*. If nothing happens in time, the item is **parked for you**
(blocked, with what was missing) and the mission goes on without it; it never
fails the mission. Waits are stored in the daemon's database, so a restart keeps
them (a wait that ran out while the daemon was off times out at the next start).
Only the leader of a host runs the wake loop. A mission is woken at most six times
an hour: a flood of calls stays recorded and is delivered in the next window.

**What a wake brings is untrusted data.** A mail summary or a webhook body is
capped, cleaned and shown to the next session only inside a fenced data block,
like any text from outside. It never becomes your message, never carries your
authority, and links in it are not judged against a page: the bot opens the site
and uses `email_wait_for`, which finds the mail at once and lists the links it
may follow.

## When a mission stops

* **Done:** the goal you set is met. The daemon checks it from the list; the
  bot cannot declare it done by saying so.
* **Needs you:** only blocked items are left (items that wait for a wake are not
  finished: the mission waits for them).
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
and 2FA items, waking by a timer, a mail in the bot's own inbox or a webhook, the
report, pause/resume/stop, restart recovery and the Activity
panel. What does **not** exist yet: a recurring ("every weekday") wake, a wake
when a helper bot finishes, mail reply classification (bounce, STOP), a separate permission scope per mission, mail pacing and
opt-out handling for outreach, sub-missions for helper bots and push reports to
Telegram. A mission session is **not** your own typing, because its first
message is built from a list that earlier sessions filled with text from web
pages and mails. It runs as its own kind (`mission`): that text never counts as
your words (the sites you named, the URLs you typed), it never reaches your own
computer (`pc_*` tools) or other bots, and it gets the Full level's web,
own-inbox and shell waivers with nothing of yours but the goal: only the goal
you wrote when you started the mission is your text. It still asks for what the Full
level asks for (money, secrets, messages to other people): an approval nobody
answers pauses the session, and the mission then waits for you with a note
instead of running past the card. Everything about a mission (the goal, the
list, your answers, its chat and its sessions) can be read or changed only with
the owner key: the app, or the administrator desktop of a host. A paired phone, a
script holding the API token or another desktop gets a refusal.

The laptop sleeping pauses a mission (the daemon stops with it). For work that
should run all night, use an [always-on host](remote-host.md).
