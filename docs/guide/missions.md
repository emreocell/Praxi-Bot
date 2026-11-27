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

In the bot's chat, type **`/mission`** (in Turkish **`/görev`**) followed by the
task, for example `/mission Sign up on these 20 directory sites and report which
ones worked`, and send it. The app starts the mission at once, without a
confirmation: you typed the command, that is the go-ahead. The bot has to be on
the **Full** level (a mission works without asking, so that is the level where it
can); below Full the chat says why it cannot start. "Start a mission" in the
bot's panel types `/mission` into the message box for you. The **Automation** tab explains how missions differ from scheduled
jobs and links here.

A compact **mission card** appears in the chat where you started it: the title,
its status, a progress bar, **Pause**/**Resume** and **Stop**, and a button that
opens the mission's own chat (**Answer** when it waits for you, **See the
report** when it ended). The card is remembered on this computer.

The bot may ask its questions once, in one message, at the start. After that it
decides for itself and writes the decisions down; you read them in the report.

### Or start from a goal

The **Start from a goal** recipes that describe long work (get backlinks, collect leads into a
spreadsheet, watch competitor pages, a weekly report from your files) run as a mission when you
set the bot's level to **Full access** in the review step. The bot is created at the level you
confirm (a recipe never asks for Full itself), the mission gets its own chat, and the app opens
it, with the progress at the top. On a lower level the same goal is one ordinary chat task, and
the review step says so. A recipe that repeats (competitor watch, weekly report) starts the
mission as the first big pass; the schedule then repeats the check as a normal run. Short recipes
(a daily brief, inbox triage) are never missions.

### Or just ask in the chat

On a bot at the **Full** level you can also write the job in your own chat (typed
in the app or sent from your paired Telegram chat), for example *"Sign up on
backlink sites with your own e-mail address."* When the job is a long batch that
cannot fit one chat turn, the bot starts a mission by itself with its
`mission_create` tool and tells you what it started. You are not asked again:
asking at the Full level is the go-ahead. The mission gets its own chat
("Mission: ..."), where the report is posted.

What the bot cannot change: the goal is **your own message**, copied by the daemon
(the bot cannot rewrite it), the budgets are the defaults, and a chat in which the
bot has already read a web page or a mail cannot start a mission any more (a page
could plant the request). A bot has at most three open missions, and the same
request never starts a second one. The chat you asked in also shows the mission
card. Below the Full level the tool does not exist, and `/mission` is refused with
the reason: raise the bot to Full first.

## What you see

The bot panel's **Activity** tab lists the missions one line each: the title, a
status chip and how many items are done ("7/15"). Click a line to open the
mission's chat. Its header shows the progress bar (**done**, **waiting**,
**blocked** and **left**), what the bot works on now, and **Pause**/**Resume** and
**Stop**.

An item that needs you (a 2FA code, a question only you can answer, a payment, an
approval nobody gave) is **parked**: it is marked blocked with the reason and the
bot carries on with the rest. All blocked items appear in **one list**; type an
answer for each (or skip it) and send them all with one button. When nothing
else is left to do, the mission waits for you and the report lists what is
blocked.

A **CAPTCHA** is not parked: nobody can solve it from that list and a mission has
no live hand-over, so the item is **given up** with the reason ("a CAPTCHA on
site X") and the bot goes on to the next site. The report lists the given-up
sites, so you know where to try by hand.

An **approval card** nobody answers (for example for money) stays open for five
minutes. After that only the item it belongs to is parked ("needs your approval")
and the session carries on; the mission is not paused. Allow the action in the
bot's permissions, then answer the item to retry it, or skip it.

A long item may take more than one session: when the step limit cuts a session
while an item is half done, that does not count as a failed attempt. An item is
given up, with the reason, only when the bot ends three sessions itself with it
still unfinished, or when it eats four whole sessions (cut by the step limit, or
interrupted by a crash, a restart or an error) without being settled.

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
  The bot gives a timeout (60 minutes by default, 3 days at most). A mail that
  arrives is a **hint, not proof of who sent it**: anyone can forge a From line and
  the mailbox does not say whether the sender passed SPF or DKIM, so the wake
  says "not verified". Next to a sender domain the bot should give a keyword or
  subject word it expects (a token from the sign-up); the keyword is checked on
  the mail itself.
* **A webhook you created** for that bot (Settings, incoming webhooks). The bot
  names its id. A signed call that a mission waits for wakes the mission and does
  **not** start a separate run. The webhook is checked again when the call comes
  in: one you turned off since, or that belongs to another bot, wakes nothing. A
  computer in standby answers "standby" and never starts a run for it; the sender
  retries at the active one. Bodies over 64 KB are refused before anything reads
  them, and the mission keeps a 4,096-character copy.

The mission chat's header lists each parked item under **Waiting**: *waiting for a mail from example.com*,
*waiting until 14:00*. If nothing happens in time, the item is **parked for you**
(blocked, with what was missing) and the mission goes on without it; it never
fails the mission. Waits are stored in the daemon's database, so a restart keeps
them (a wait that ran out while the daemon was off times out at the next start).
Only the leader of a host runs the wake loop. A mission is woken at most six times
an hour: a flood of calls stays recorded and is delivered in the next window. A
mission keeps at most one undelivered wake per item (a newer one replaces it) and
50 in all; finished waits and delivered wakes are deleted after 30 days.

**What a wake brings is untrusted data.** A mail summary or a webhook body is
capped, cleaned and shown to the next session only inside a fenced data block,
like any text from outside. It never becomes your message, never carries your
authority, and links in it are not judged against a page: the bot opens the site
and uses `email_wait_for`, which finds the mail at once and lists the links it
may follow.

**A code is never an answer.** A 2FA or verification code is single-use, so it
is never stored in the list and never shown to the bot. An item that waits for a
code only offers "Skip this one": enter the code on the site yourself, then skip
the item. The same holds for passwords: never write one in an answer. When the
bot needs a login or key it asked for on a site, that item shows a secure form in
the app (see [security](security.md)): the value goes straight to the vault, the
bot never sees it, and your answer releases only that item. The rest of the
mission keeps working while the form is open (24 hours).

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
* **A question:** the bot's one batch of questions at the start is the only thing
  that still pauses the whole mission when you do not answer in time.
* **Paused / stopped:** you did it. Stopping ends the run in flight.

At the end (and whenever it blocks) the mission posts a short report in its own
chat: what was done, what is blocked and why, what was given up, and the
decisions the bot made.

The same digest is **pushed to you**: it appears in the app's notifications panel
and, if you paired Telegram or Discord, arrives there too, with every open
question in one message that you can answer from the chat. See
[Telegram](telegram.md#when-a-bot-reports-back-on-its-own) for what is pushed, the
quiet hours, and why approvals (anything involving money) always stay in the app.
A short progress note at 25%, 50% and 75% is available but off by default.

## Limits of this first version

Missions are delivered in waves (see the [design](../design/missions-adr.md)).
What works today: the item list, session chaining, budgets, parking of 2FA and
approval items, giving up CAPTCHA items, starting a mission from a Full-level chat,
waking by a timer, a mail in the bot's own inbox or a webhook, the
report, pause/resume/stop, restart recovery, the `/mission` command with its
chat card, and the Activity list. What does **not** exist yet: a recurring ("every weekday") wake, a wake
when a helper bot finishes, mail reply classification (bounce, STOP), a separate permission scope per mission, mail pacing and
opt-out handling for outreach and sub-missions for helper bots. A mission session is **not** your own typing, because its first
message is built from a list that earlier sessions filled with text from web
pages and mails. It runs as its own kind (`mission`): that text never counts as
your words (the sites you named, the URLs you typed), it never reaches your own
computer (`pc_*` tools) or other bots, and it gets the Full level's web,
own-inbox and shell waivers with nothing of yours but the goal: only the goal
you wrote when you started the mission is your text. It still asks for what the Full
level asks for (money, secrets, messages to other people): an approval nobody
answers parks that one item with a note instead of running past the card. Everything about a mission (the goal, the
list, your answers, its chat and its sessions) can be read or changed only with
the owner key: the app, or the administrator desktop of a host. A paired phone, a
script holding the API token or another desktop gets a refusal.

The laptop sleeping pauses a mission (the daemon stops with it). For work that
should run all night, use an [always-on host](remote-host.md).
