# ADR: Missions (long-running autonomous work)

Status: **wave 1 slice implemented** (see "Implementation status" below); everything else is still proposed.
Related: [bot delegation](bot-delegation.md), [bot spending limits](bot-spending-limits-adr.md),
[schedule triggers and prechecks](../schedule-triggers-prechecks.md),
[permissions UX](permissions-ux.md), [stop and cancel](stop-cancel.md),
[handover](handover.md). Internal ADRs this builds on (kept outside the repo):
0027 always-on host, 0032 nudges, 0033 task cards, 0034 spending limits, 0035 capability expansion.

> Scope note. This document is a read-only audit plus a design. File and line
> references were read on `main` at commit `a95b691e` plus the merge of the two
> commits after it (2026-11-26). The in-flight trigger registry (PR #869) was
> read through `gh pr diff 869`. Section 9 (cold outreach) is a list of product
> duties and open questions for the owner. It is **not legal advice**.

## Implementation status

The first slice that gives durable progress is built (`crates/missions`, `agent-core`,
`daemon-api`, the Activity panel; user guide: [missions](../guide/missions.md)). Deviations from
the design below are deliberate and listed here.

| ADR item | Status |
|---|---|
| M1 `missions` crate: model, `can_transition`, store, brief, digest | **built**, with a smaller state set (`running`, `paused`, `blocked`, `done`, `failed`, `cancelled`: no `draft`/`intake`/`waiting` yet), items `todo/doing/done/blocked/failed/skipped`, an own `missions.db` (index-based migrations like the scheduler store, so no `storage` migration number is taken) and no `runs.mission_id` column (`mission_sessions.run_id` links the other way). Counters: sessions, model calls, spend, stalled and failed sessions; no per-day or per-site counters. |
| M4 session guard semantics | **built**: the step limit ends a *session*, the supervisor chains the next one (this removes the 40-step wall of a mission). Not built: the mission USD sub-limit inside `CallGuard`; the supervisor sums the spend of finished sessions instead and checks the cap before each session. |
| M5 supervisor and tools | **built** (without timers): lease, session runner, brief as first message, `mission_status/plan/item/note/complete`, continue / finish / block from the ledger, restart recovery (an open session is `interrupted`, never resumed). Not built: `mission_wait`, `mission_ask_owner`, `mission_report`, `mission_delegate`, `mission_block`, idempotency keys. |
| 8.3 CAPTCHA and 2FA become blocked items | **built** for `browser_handover` (the item being worked on is parked and the session moves on, gap report P1 #9). Not built: approval cards of always-ask tools as blocked items; secret requests. |
| 10 single-batch questions | **partly**: the first `ask_user` of a mission parks the run as before; a second one is refused with "decide yourself or park the item" (`questions_asked`). No intake form, no contract, no `mission_ask_owner` digest. The **blocked-items list** the owner answers in one request is built (`POST /v1/missions/{id}/answers`). |
| 11 digests | **built** for the end: a code-built digest when a mission ends or blocks (stored, posted to the mission chat). Not built: milestone digests, quiet hours, push to Telegram or mail. |
| 12.3 API | **built**: list, get, create, pause, resume, cancel, answers (owner-only where it starts or steers work). Not built: intake, scope and budget patch routes, items retry/skip routes, artifacts, events and the SSE events (the UI polls). |
| M3 trust model, `InitiatorKind::Mission` | **built (wave 1)**. A session runs as `InitiatorKind::Mission` (`turn:mission:<id>`) and its brief is stored with the message source `mission_brief`, never `owner`; only the goal the owner typed at start is `owner`. So ledger text never feeds `owner_message_texts` (named sites, typed URLs, owner words) and a mission is never `Human` (no `pc_*`, no `ask_bot`, no typed-URL waiver for brief text). `Shared::mission_full_started` (top-level session, Full bot, every message is the brief or the owner's steering) gives it the Full level's web, own-inbox and shell waivers and nothing else. The run starts with untrusted exposure. An unanswered approval pauses the session (the supervisor blocks the mission); a credential request parks the item. Auto-continue never runs inside a mission session (the supervisor chains the next one from the ledger). Reads and writes of a mission, its chat and its session runs are owner-only on every route. 2FA codes are never accepted as an answer (`live_only` items). |
| M2 test harness | **partly**: `crates/evals/tests/missions.rs` runs the real loop, ledger and supervisor against a reactive fake model (no `VirtualClock` across the scheduler, no `FakeWeb`, no `FakeMailServer`). Scenarios covered: "continues without asking", A3 (CAPTCHA), A9 (session, model-call and deadline budgets), A5 (restart), A12 (stalled). |
| M6-M8 timers, mail, webhook wakes; M9 intake; M10 outreach; M13 delegation; M14 channels; M15 templates | not built |

Separately from missions, the agent loop now continues a turn that ended with work left (see
[missions](../guide/missions.md#two-things-that-changed)): the default prompt tells every bot to
finish the whole task and ask its questions once, and `exec.rs` answers a "shall I continue?"
turn at the Full level and an unfinished plan at every level, inside the step, token and time
budgets and never for money.

## 1. Problem

The owner's top priority, in his words: a bot at the **Full** level must be able
to take **one** big task and finish it **without waiting for input at every step**.
It may ask its questions **once, all together, at the start** (and later only as
one batched digest, never one nag per question), then work alone for hours or
days. It wakes itself by cron, webhook, incoming mail, a timer it set itself
("check the verification mail in 10 minutes") or a finished child run, and it
**continues** where it left off. It must survive restarts and context limits.

Two examples that must work end to end, and that must not be the only shapes the
design fits:

1. "Register on sites so I get backlinks": 15-20 sign-ups over time, with the
   bot's own mailbox, verification mails, a record of which site is done.
2. "Find potential customers abroad and e-mail them from my business mailbox":
   search, build a lead list, draft personal mails, send them with sensible
   pacing, handle replies.

Today Praxi has strong parts (a durable run/step ledger, schedules, triggers,
approvals, a Full level with hard stops, mail guards, handover) but **no object
that spans many runs**. A "run" is one turn: 40 model calls by default, then a
tool-less closing summary (`crates/agent-core/src/exec.rs:455`,
`crates/agent-core/src/config.rs:67`). Everything after that is a new, unrelated
chat unless the model happens to remember.

## 2. Goals and non-goals

Goals

- G1. One owner message starts a mission; the bot works until the success
  criteria are met, a budget is hit, or the owner stops it.
- G2. Questions are asked once, in one batch, with a recommended answer for each.
  Later questions go to a batched owner digest and never block other work.
- G3. All state that matters lives in a durable **ledger** in the database. Any
  run can be lost (crash, restart, context full) and the next one continues from
  the ledger plus a compact brief.
- G4. Any trigger can wake a mission: timer, cron, webhook, mail, child run,
  owner message, file change, "budget window reopened".
- G5. Budgets (time, steps, model spend, mails, sign-ups per day, per-site caps)
  and the safety rules are enforced **in code**, never by the prompt.
- G6. The owner sees progress as milestone digests, can ask "status" at any
  moment, and can pause, stop or kill with one click.
- G7. Cheap by default: a wake that has nothing to do costs zero model calls.
- G8. Testable in seconds in CI: virtual time, scripted model, fake sites, fake
  mail server.

Non-goals

- N1. No new "super mode". Every hard stop of the Full level stays (egress gate,
  private-data rules, vault binding, always-ask tools, money, never-list). A
  mission gets only what the owner granted in its scope.
- N2. No fixed-DAG workflow engine. Missions are open-ended; the plan is data the
  model edits, not code.
- N3. No central server of ours (no telemetry, ADR-0020). Everything runs in the
  owner's daemon; the always-on host (ADR-0027) is where it makes sense to run.
- N4. No CAPTCHA breaking and no 2FA bypass: both become blocked items for the
  owner (handover), exactly as decided on 2026-11-24.
- N5. No legal compliance engine. The product enforces the mechanics of
  section 9 and asks the owner for the facts only he can supply.

## 3. Audit of what exists (Step 1)

Legend: **exists**, **partial**, **missing**. "Where" is `file:line` on the
audited tree.

| # | Capability needed for missions | Status | Where | What is missing |
|---|---|---|---|---|
| 1 | What ends a run | exists | `agent-core/src/exec.rs:450-461` (step limit), `:462-467` (token budget), `config.rs:40,67` (`max_steps` = 40, `max_tokens_per_turn` = 2 000 000), `history.rs:105` (`GUARD_NOTE`) | A guard stop is a **terminal summary**, not "end of a work session". Nothing re-queues a continuation. |
| 2 | Context at the limit | partial | `compaction.rs:1-25` (deterministic shortening of tool results inside a turn), `history.rs:108-112` (history capped at 256 messages / 64 KiB, "a summary of older history is a follow-up") | No model summarisation, no durable "where am I" artifact. A very long chat silently drops old turns. |
| 3 | "Continue" | partial | `POST /v1/runs/{id}/resume` (`daemon-api/src/routes.rs:1459`), `task-runtime` `recover` (`runtime.rs:367`) turns a running run into `Paused`; side-effect steps are never replayed | Resume continues **one** run. There is no "start the next session of the same job". |
| 4 | Durable run/step ledger | exists | `task-runtime/src/state.rs:7` (`RunState`), `storage/src/records.rs:110` (`Run`) | Per run only. No object above runs; no links between runs of one job. |
| 5 | Schedules / cron | exists | `scheduler/src/model.rs:70` (`ScheduleSpec`), `:394` (`Payload`), `engine.rs`, `docs/schedule-triggers-prechecks.md` | Payloads are prompt / workflow / tool. No "wake mission X". Occurrence retries are for schedules, not work items. |
| 6 | Bot-created schedules | partial | `builtin-tools/src/schedule.rs:3-33`, `daemon-api/src/schedule_tools.rs:42-43` (5 min minimum interval) | Needs approval per create; no one-shot "in 10 minutes, for this reason" tied to a mission; 5 min floor is too coarse for "check mail in 2 min". |
| 7 | Webhook wake | exists | `scheduler/src/webhook.rs`, `daemon-api/src/incoming_webhook.rs`, route `/v1/webhooks/{id}` (`routes.rs:3032`), #737 | Targets a schedule's prompt with `{input}`; cannot target a mission. |
| 8 | Mail wake | partial (in flight) | `scheduler/src/watch.rs` (2-minute header polling, `Trigger::Email` at `model.rs:469`), PR #869 (rule registry, quiet hours, limits 10/h 50/day per rule, bounded queue of 100 events / 48 KB, events fenced with `untrusted_data_block_capped`) | Wake starts a one-off prompt run, not a mission continuation. No "wait for this reply" registration. |
| 9 | Trigger-fed runs are untrusted | exists | `agent-core/src/shared.rs:1384-1396` (`ExternalSchedule` kind), `docs/schedule-triggers-prechecks.md` ("once-only approval floor on **every tool**") | **Blocker for missions**: a run woken by mail or webhook asks for approval on every tool, so it cannot work unattended. A new trust class is needed (section 8). |
| 10 | Full level is owner-started only | exists | `agent-core/src/approval.rs:1124-1136` (`full_mode_waives`), `:3526-3551` (`owner_started_run`: every user message source must be `owner`/`peer_admin`), `permission-engine/src/modes.rs:21` | A schedule, webhook, mail, channel or subagent run never gets Full. A wake-started mission session would ask for everything. |
| 11 | Egress/private-data volume caps | exists | `agent-core/src/full_autonomy.rs:38-40` (`SEND_BUDGET` 1500 chars per run, foreign text limit 40) | Per run, not per mission; must stay as is (sized for "dozens of sites"); sessions reset it, which is acceptable because the SeenText/taint rules stay per run. |
| 12 | Subagents | partial | `agent-core/src/subagent.rs:44-46` (`MAX_DEPTH` 2, `MAX_CHILDREN` 3), `:47` (15 steps default); children never inherit Full (`approval.rs:1126-1129`) | Fine for a research helper; not for a worker that signs up on a site. |
| 13 | Bot-to-bot delegation | partial | `agent-core/src/delegation.rs:1-26` (depth 1, one helper per root run, 4 KiB payload, 8 steps, 60 s, read-only tools, approval per call) | Designed as "ask a colleague". Missions need a scoped worker bot that can act. |
| 14 | Spending limits | exists | `agent-core/src/bot_limits.rs`, `bot-spending-limits-adr.md` (per bot, day/week/month, reserve-then-reconcile before every call) | No per-mission limit. A runaway mission can eat the whole bot limit (and then stops everything else the bot does). |
| 15 | Memory | exists | `memory-store`, `docs/design/memory.md`, recall prepended to the first user message (`exec.rs:420`) | Free-text, model-managed. Not a structured work ledger; must not be the mission state. |
| 16 | Question to the owner | partial | `approval.rs:2560` (`ask_user` parks the call: `PendingKind::Question`) | Parks **the run**. One question = one stalled run. No batch, no "keep working while this is open". |
| 17 | CAPTCHA / 2FA handover | partial | `approval.rs:2816` (handover card), `config.rs:26-32` (5 min timeout, 4 per run, 3 secret requests per run), `docs/design/handover.md` | The run waits in place for 5 minutes. For a mission it must become a **blocked item** and the session must move on. |
| 18 | Mail send guards | partial | `connector-email/src/guard.rs:1-20` (soft warnings: 20/day per account, same recipient 14 days, opt-out list; opt-out line added in code; owner overrides on the card, "Send anyway") | Each send still needs a card. No batch approval, pacing, warm-up, no automatic suppression from replies ("STOP" is removed **by hand**, `guard.rs:44-46`), no bounce handling, no lead provenance. |
| 19 | Waiting for a verification mail | partial | `connector-email/src/wait.rs:25` (`MAX_WAIT` 10 minutes, blocking inside the run), `verify.rs` (followable-link rules) | Excellent safety logic, wrong shape: it blocks the session. A mission needs "park this item, wake me when the mail arrives or in 30 min". |
| 20 | Written policy for acting alone | missing | `permission-engine/src/policy.rs:36` (`AllowWithLimits` exists as a level but the resolver treats it as `Ask`, ADR-0035 audit), `agent-core/src/scoped_grant.rs` (scoped standing grants) | A mission-scoped grant with counters ("mail X: recipients from the ledger, at most N/day") does not exist. |
| 21 | Task cards / nudges | missing (ADR drafts only) | no `task_cards`, `nudge_*` in any migration or crate | The "what is it waiting for / done when" card is a good projection of a mission but has no implementation yet. Do not depend on it. |
| 22 | Notifications ("Needs you") | partial | `apps/desktop/ui/src/components/PendingDecisions.tsx`, `DailyDigestCard.tsx` (client-side digest, no model call) | Shows approvals/questions. No mission blocked-item cards, no digest with several questions. |
| 23 | Run timeline | exists | `apps/desktop/ui/src/components/RunTimelineModal.tsx` (#623) | Per run. A mission needs one timeline over many runs. |
| 24 | Offline handling | exists | PR #868 (`provider-catalog::connectivity`, schedule skip reasons, `occurrence_note` migration) | Reuse: mission wakes must be **held**, not failed, while offline. |
| 25 | Always-on host | exists | ADR-0027, `docs/guide/remote-host.md`, `host-leadership.md` | Missions assume the daemon stays up; on a laptop they pause while the lid is closed. UI must say so (and recommend the host). |
| 26 | Accounts / vault | partial | `fill-secrets` (origin-bound secrets, model never sees values), AgentMail bot-owned address, `docs/guide/email.md:201` playbook `signup-to-site` | No ledger of "accounts created" (site, login name, which mailbox, date). Passwords generated for new sites need a place in the vault tied to the mission (ADR-0035 item). |
| 27 | Browser tools, playbooks | exists | `skills/signup-to-site`, `skills/lead-research`, `skills/outreach-drafts`, `skills/sign-in-forms` (bundled playbooks, #782) | Playbooks are advice to a single run. They need to become **mission templates** (default ledger kinds, default budgets, default questions). |
| 28 | Evals harness | partial | `crates/evals/src/harness.rs` (single turn driver), `fake.rs` (`ScriptedProvider`, fake tools), `scheduler/src/clock.rs:77` (manual clock) | No virtual time across sessions, no fake site, no fake mail server, no supervisor under test. |
| 29 | Per-run "stalled" detection | partial | repeat detector `Guards::repeat_warn/stop` (`config.rs:46-49`) | Only inside one turn. Nothing notices "5 sessions and no ledger progress". |
| 30 | Pause / stop everything | exists | `agent.rs:750` (`kill_native_runs`), `POST /v1/runs/{id}/cancel`, `docs/design/stop-cancel.md` | Must be extended to wakes, waits and mission state. |

### 3.1 The top 8 gaps (ranked by how much of the goal is blocked)

1. **No mission object, ledger or durable plan** (rows 4, 15, 21). Nothing carries "where am I" between runs.
2. **No session continuation** (rows 1-3): a guard stop ends the work; nobody starts "the next session with the brief".
3. **Trust model blocks unattended wakes** (rows 9, 10): Full is waived only for owner-started runs; trigger-fed runs ask for every tool.
4. **No durable wait tools** (rows 6, 19): the model cannot say "wake me at T / on mail M / when child C ends" without blocking a run or creating an approved schedule.
5. **Questions and handovers block the run** (rows 16, 17): no batched owner digest, no "blocked item, move on".
6. **No per-mission budgets and counters** (rows 14, 18): only per-bot spend and per-account mail soft warnings; no sign-ups/day, per-site caps, mails/mission.
7. **Mail outreach mechanics are missing** (rows 18, 20): pacing, batch approval after first drafts, replies, bounces, automatic suppression, provenance, identity fields.
8. **No long-run test harness** (row 28): virtual time, fake site, fake mail server and a supervisor under test do not exist.

## 4. Decision in one page

Introduce **Missions**: a mission is a durable job owned by the owner, run by one
Full-level bot, executed as a chain of short **sessions**. A session is an
ordinary agent run (same loop, same permission engine, same step guards) whose
only memory is a **code-built brief** read from the **ledger**. A deterministic
**supervisor** in the daemon decides when a session starts (wake queue, budgets,
quiet hours, one at a time per mission) and what happens when it ends
(continue now, wait for something, hand to the owner, finish). The model edits
the ledger through a few `mission_*` tools; code enforces everything that is a
limit.

Why sessions and not one giant run: context windows, restarts, per-run
guards, approvals and the prompt cache all work on short runs; a run that lives
for days cannot be resumed after a crash without replaying side effects, which
`task-runtime` correctly refuses to do. A fresh run from a good brief is the
robust form of "continue".

Alternatives rejected

| # | Alternative | Why not |
|---|---|---|
| A | One huge run with `max_steps` in the thousands | Context fills (history is capped at 64 KiB, `history.rs:109`), a crash pauses it, approvals and egress state pile up, cost is unbounded, no way to wait for a day without holding a task. |
| B | One cron schedule per mission and let the model "remember" | No state, a trigger-fed run asks for every tool (row 9), a prompt per tick burns money on empty ticks. |
| C | Extend task cards (ADR-0033) | Task cards are display-only by design and not implemented; a mission needs state that code acts on. We reuse the card idea as a projection (section 15). |
| D | Fixed workflow DAGs (`workflow-replay`) | Fine for repeatable flows; the owner wants open-ended jobs for any person. Workflows can still be ledger-item actions. |
| E (chosen) | Ledger + sessions + supervisor + code-enforced budgets | Reuses runs, approvals, scheduler, mail and delegation; adds one object and one supervisor. |

## 5. The Mission object

One row in `missions` plus its ledger tables (section 12). Fields:

| Field | Meaning |
|---|---|
| `id`, `owner_user_id`, `bot_id` | Lead bot (must be at Full for autonomous sessions; checked at Start and on every session start). |
| `title` | Short label, editable by the owner. |
| `goal_text` | The owner's words, **verbatim**, never rewritten by the model. Shown at the top of every brief. |
| `criteria` | List of success criteria. Each is `{text, kind}` where `kind` is `counter` (code can check: "15 sites with status `done`", "50 mails sent"), `ledger_query` (code can check), or `owner_confirm` (the owner presses "Done" or answers). The model cannot mark a mission done by saying so (same rule as task cards: "the model cannot say done"). `mission_complete` only **proposes** done; code checks the criteria and either finishes or reports what is missing. |
| `scope` | What the owner granted, frozen at Start, widening needs the owner: allowed tool categories, mail accounts and sender identity, allowed recipients rule (ledger leads only / replies only), domain deny list, "may create accounts", "may send mail", "may spend money: no" (always no unless the owner types a number), worker bots allowed, language(s). |
| `budgets` | See below. |
| `calendar` | Time zone, working hours (sessions that send mail or visit sites run only inside them; reading and waiting may run outside), start and deadline. |
| `report` | Where digests go (app, Telegram, mail) and how often at most. |
| `state`, `state_reason`, `waits[]`, `next_wake_at` | Section 6. |
| `lease` | `active_run_id`, `lease_until_ms`, heartbeat. One live session per mission. |
| timestamps, `version` | Optimistic concurrency for owner edits. |

Budgets (all enforced by code, all visible, all owner-editable, raising needs
the owner's key like the bot spending limits in `bot-spending-limits-adr.md`):

| Budget | Default (recommended) | Enforced where |
|---|---|---|
| Wall clock (deadline) | 14 days | Supervisor before every session; at the deadline the mission goes to `blocked(deadline)` with a digest, not silently to failed. |
| Sessions per hour / per day | 6 / 60 | Supervisor. Stops a wake storm. |
| Model steps per session | 40 (today's `max_steps`) | Existing guard (`exec.rs:455`); a stop is a normal session end, not a failure. |
| Model steps and tokens total | 1500 / 6M | Supervisor sum over the mission's runs. |
| Model spend | USD cap per mission, chosen at intake (suggested from the estimate, section 14) | A mission sub-limit inside `CallGuard` (the same reserve-then-reconcile as ADR-0034), attributed through `runs -> mission_id`; bot limits still apply on top. Unknown-price models cannot be used with a USD cap (as today). |
| Mails sent | per mission total, per day, per hour, min gap between two sends | Counter reserved **before** the send inside the mail tool wrapper; refusal returns a structured "window reopens at HH:MM" and the supervisor sets a wake for that time. |
| Sign-ups | per day, per site (attempts), per registrable domain | Counter in the browser gate: first navigation to a new registrable domain claims a ledger item (`site`), which checks the day cap and the per-site cap. |
| Handovers / secret requests | per day | Existing per-run caps (`config.rs:29-32`) plus a per-day mission cap so the owner is not paged ten times. |
| Owner digest cadence | at most one push per 4 h | Section 10. |

## 6. State machine

States: `draft`, `intake`, `running`, `waiting`, `blocked`, `paused`, `done`,
`failed`, `cancelled`. Terminal: `done`, `failed`, `cancelled`.

```
 draft --start_intake--> intake --owner_confirms_contract--> running
   |                        |                                   |  ^
   |                        +--owner_cancels--> cancelled       |  | wake due / continue
   |                                                            v  |
   |                                        +-------------- waiting
   |                                        |  (>=1 wait, nothing runnable)
 running --session ends, nothing runnable, waits exist--> waiting
 running --session ends, work runnable--> running (next session queued)
 running|waiting --budget/deadline/stalled/model errors/all items blocked--> blocked
 blocked --owner acts (answer, raise budget, solve, "continue")--> running
 running|waiting|blocked --owner pause--> paused --owner resume--> running|waiting
 any non-terminal --owner stop--> cancelled
 running --criteria met (code-checked)--> done
 running|waiting|blocked --unrecoverable (dead-letter of a critical item, repeated model failure after owner chose "stop on failure")--> failed
```

Rules

- **running** means "there is runnable work": either a session is in flight or
  the next one is queued. **waiting** means "no runnable work, at least one
  wait is registered", with `waiting_on` listing every wait:
  `timer(at)`, `webhook(rule)`, `mail(filter, until)`, `child(run_or_mission)`,
  `owner(question set)`. The UI shows the nearest one.
- **blocked** is a mission-level state and means "nothing can move without the
  owner or a budget change". An **item** can be blocked (CAPTCHA on site 4)
  while the mission stays `running`: other items continue. This is the rule that
  keeps "nothing blocks other work".
- `paused` keeps waits and queues events but starts no session. An in-flight
  session is asked to stop at the next tool boundary (existing cancel path);
  a side-effect step in progress completes or is recorded as `unknown`
  (section 7.4), never replayed.
- Every transition is a single SQL transaction that also appends a
  `mission_events` row (timeline) and emits `mission.state_changed`. Illegal
  transitions are rejected by a pure `can_transition` function with a table test,
  like `task-runtime/src/state.rs`.
- Missions do not change the run state machine. A mission session is a normal
  run (`runs.kind = turn:mission`), linked in `mission_runs`.

## 7. The durable ledger and the "where am I" brief

### 7.1 What the ledger holds

- **Plan**: an ordered list of `mission_items`; the model creates and edits it
  with `mission_ledger_upsert`. An item is the unit of progress: a site to
  register on, a lead to contact, a document to prepare.
  Fields: `kind` (free label such as `site`, `lead`, `mail_thread`, `task`),
  `key` (dedupe key: registrable domain, lead e-mail, ...; unique per
  mission+kind), `status` (`todo`, `doing`, `waiting`, `blocked`, `done`,
  `failed`, `skipped`, `dead`, `unknown`), `attempts`, `next_attempt_at`,
  `payload` (small JSON, capped at 8 KiB), `result`, `last_error`,
  `blocked_reason` (+ `owner_card_id`), `evidence` (links to artifacts, send-log
  ids, run ids).
- **Artifacts**: named structured datasets the owner can open and export
  (`leads`, `accounts`, `drafts`): a table in the DB for rows, or a file in the
  bot's workspace (`bot_workspace_dir`) with its hash recorded. CSV export from
  the UI. Passwords are never artifacts; they go to the vault (section 8.4).
- **Notes**: short model-written entries (`decision`, `learned`, `warning`),
  capped in count and size, some pinned into the brief ("this site needs a
  phone: skip all sites of this type").
- **Waits**: durable registrations (section 7.3).
- **Counters**: per budget and period (section 5), in SQL, updated in the same
  transaction as the thing they count.
- **Events**: append-only timeline (section 14).

### 7.2 The brief (what the model gets on every wake)

Built by code (no model call), about 2 000-3 000 tokens, always in this shape, as
the **first user message** of the session (the system prompt stays stable for the
prompt cache, like memory recall at `exec.rs:420`):

1. Goal in the owner's words; success criteria with live status ("sites done 7/15").
2. Scope summary: what is allowed, what is not, working hours, the opt-out and
   identity rules that apply (the rules themselves are enforced in code).
3. Budgets used / left (spend, mails today, sign-ups today, sessions today).
4. Progress counts per item kind and status.
5. The next N runnable items (default 8) in priority order, each with the facts
   needed to act, and the items that were blocked/dead since the last digest.
6. Pinned notes and the last 5 timeline events.
7. **Why you were woken**: reason code plus payload. Payload from outside
   (a reply e-mail, a webhook body) is cleaned, capped and fenced with the
   existing `untrusted_data_block_capped` helper (#869). It is data, never an
   instruction.
8. The session's task: "work the runnable items; you have K steps; before you
   finish call `mission_wait` or `mission_checkpoint`".

The model never reads the whole history. A context reset therefore changes
nothing: the next session simply builds the same brief. At the end of a session
code also writes an automatic checkpoint (item states are already in the DB;
the model's own closing text is stored as a note of kind `session_summary`,
capped), so a crash between "did work" and "told the ledger" loses at most the
last unreported step (see 7.4).

### 7.3 Wake sources and the wait tools

The wake queue is the table `mission_wakes (id, mission_id, reason, source_ref,
payload, dedupe_key, due_at_ms, claimed_by, created_at)`. Wakes are persisted
before they are acknowledged to the source, so a restart loses none.

| Wake reason | Source | Reuse | New |
|---|---|---|---|
| `timer` | `mission_wait` with `until` / `for` (down to 30 s) | scheduler clock | Mission supervisor owns the timer; no 5-minute floor (that floor protects owner-visible schedules, `schedule_tools.rs:43`) |
| `cron` | mission-level recurring wake ("every weekday 09:00 in my time zone") | `ScheduleSpec`, DST handling in `scheduler` | New `Payload::MissionWake { mission_id }`; the schedule is created by the daemon, hidden from the bot, listed under the mission |
| `webhook` | incoming webhook / trigger rule (#737, #869) targeting a mission | signatures, replay checks, bounded queue, coalescing | Rule target = mission instead of prompt |
| `mail` | trigger rule or `mission_wait` with a mail filter on the mission's mailbox | `Trigger::Email`, #869 rules, `wait.rs` filters and `verify.rs` link rules | One-shot registered waiter with timeout; deterministic pre-classification (bounce, STOP, auto-reply) before any model call |
| `child` | a delegated worker mission or subagent finishes | `subagent.rs` events, delegation | A wake carrying the child's final text as untrusted data |
| `owner` | owner answers a digest question, sends a message in the mission chat, presses Continue | approvals/events | Answers are written to the ledger and may release items |
| `file` | file-watch trigger | `Trigger::FileWatch` | Target = mission |
| `window` | a budget window reopened (daily cap, working hours start) | counters | Set automatically by the refusal path |
| `continue` | session ended with runnable work | step guard | Supervisor, with a minimum gap and the session budgets |

Tools registered **only** in mission sessions (local-state tools, low risk like
`task_card_set` in ADR-0033: no approval card, always audited, size-capped,
text filtered, owner-visible):

- `mission_status` returns the brief again (cheap, read-only).
- `mission_ledger_upsert(items[])`, `mission_item_update(key, status, result, evidence)`,
  `mission_note(kind, text, pin?)`, `mission_artifact_append(name, rows[])`.
- `mission_wait({ until | for, on?: {mail: filter} | {webhook: rule} | {child: id}, timeout, reason, item_keys? })`:
  records the wait, **ends the session**. An item can be parked on it.
  Several waits may coexist; the first wake that fires starts the next session
  with all due wakes coalesced.
- `mission_ask_owner(questions[], safe_default?)`: **never blocks**. Writes to
  the owner digest (section 10). An item that depends on the answer becomes
  `blocked(owner)`; the session continues with other items.
- `mission_report(text)`: a milestone line for the digest (rate-limited).
- `mission_delegate(items, worker_bot)` (wave 4).
- `mission_complete(evidence)`: proposes done; code checks the criteria.
- `mission_block(reason)`: "I cannot proceed on anything" (rare, usually code does it).

How a wake continues the mission and is not a new chat: the supervisor claims
the due wakes, takes the lease, creates a run with `parent = none`,
`kind = turn:mission`, `chat_id = the mission's chat`, a `mission_runs` row and
the brief as the first message. Recall/memory attach as for any run. When
the run ends the supervisor reads the ledger, not the model's text, to decide
the next state.

### 7.4 Crash and restart behaviour

- Supervisor tick on start: sessions whose lease expired are marked
  `interrupted`; the run itself is already `Paused` by `Runtime::recover`; the
  mission session is **abandoned, not resumed** (resuming would replay model
  context that is gone; side effects are never replayed).
- Items in `doing` whose last step had a side effect get `unknown` and a
  **reconcile step** in the next brief: for mail, code looks the item's
  idempotency key up in the send log (`guard.rs` JSONL log) and sets `done` or
  back to `todo` itself, no model guess; for a sign-up the item is re-verified
  by logging in or by looking for the confirmation mail.
- Every side-effecting tool call from a mission carries an idempotency key
  `mission/<id>/<item key>/<attempt>`; the mail tool wrapper writes the intent
  to the ledger **before** sending and the message id after.
- Wakes are claimed with a lease and re-claimed after expiry (at-least-once);
  handlers are idempotent by `dedupe_key`.
- While the host is offline (#868 connectivity) wakes are held and the state
  shows `waiting (offline)`; this is not a failure and does not consume attempts.

## 8. Trust and safety

### 8.1 What stays exactly as is

Egress gate and SEND_BUDGET (`full_autonomy.rs`), private-data and taint rules,
vault binding by origin, always-ask tools, money and the never-list floors,
`browser_eval` and program starts always ask, named browser profiles ask,
non-public addresses ask, audit logging, redaction, owner-only routes for
anything that raises authority. A mission never edits the permission policy.

### 8.2 The new trust class (the hard part)

Problem (rows 9 and 10): Full is waived only for owner-started runs; wake runs
(schedule, webhook, mail) are not owner-started and trigger-fed runs get a
once-only approval on every tool.

Decision:

- Add `InitiatorKind::Mission` (it behaves like `Schedule` for the money and
  never-list floors: it never auto-approves them).
- Extend `owner_started_run` so that a run counts as owner-started when its
  user messages are all `owner`/`peer_admin` **or** `mission`, and the message
  source `mission` is accepted only if the mission row exists, is not
  `paused|cancelled|failed|done`, its `confirmed_at` was set by an owner-key
  request (Start in the app, owner API; never by a bot, a token-only caller, an
  MCP agent or a peer below admin) and the mission scope allows the tool
  category being waived. The lineage walk stays fail-closed on any storage error.
- Wake payloads **never** become a user message. The first message is the
  code-built brief; outside data sits inside the fenced untrusted block, is
  tracked as untrusted content by the taint engine, and is never concatenated
  into an instruction. This is what lets us drop the once-only floor for mission
  runs without opening the prompt-injection door: an injected reply e-mail can
  still only provoke tool calls that pass the same Full-level gates, the taint
  gate and the **scope** (below).
- Scope is a code check at the tool wrapper, independent of the model: mail may
  go only to (a) an address that exists as a ledger lead of this mission, with
  a provenance record, or (b) the sender of a thread of this mission (replies).
  An address that appears only inside a received mail or web page is not a
  valid recipient. Sign-up/browser calls honour the deny list and caps.
- Subagents and `ask_bot` helpers keep today's rule (no inheritance of Full).
  Parallel work goes through mission delegation (section 14), whose workers are
  missions with their own confirmed scope, not Full inherited from a parent.

Residual risk, stated plainly: a model fooled by a hostile page may sign up on
a bad site or reply to a wrong thread **inside the scope and caps**. That is the
accepted cost of unattended work; the caps, the deny list, the ledger-only
recipient rule, the timeline and the kill switch bound it. This needs a security
review before wave 2 merges (it is the highest-risk change in the plan).

### 8.3 CAPTCHA, 2FA, approvals, questions

All become **blocked items** with an owner card, never a blocked session:

- The handover card (existing `approval.rs:2816` machinery) is created, the item
  gets `blocked(owner, card)`, the session moves on to the next item. The card
  stays valid for hours (not the 5-minute in-run timeout); if the tab is gone
  when the owner solves it, the item is re-queued with a note "owner solved the
  CAPTCHA at <time>; retry sign-up once". 2FA codes expire: the card says "enter
  the code within N minutes after you press Ready", and the retry is scheduled
  for that moment.
- An approval card for a tool that is still always-ask under the mission scope
  is the same: item blocked, others continue.
- Blocked items are grouped into **one** NeedsYou entry per mission and appear
  in the next digest; if the mission has nothing else runnable it goes to
  `blocked` and the digest is sent immediately.

### 8.4 Accounts and passwords

A "created account" is a ledger item with `{site, login, mailbox, created_at,
verified_at}`; the generated password is written straight into the vault, bound
to the site origin and the mission, never into the model context, notes or
artifacts (the vault work from ADR-0035 is a dependency of wave 3; until it
ships the mission can only use sites that need no password or ask the owner).

## 9. Cold outreach: what the product must do, what the owner must say

Not legal advice. The rules differ by country and by who the recipient is. The
design makes the mechanics enforced and the facts explicit.

What the product must do (code, not prompt):

1. **Truthful sender identity.** The first send is refused until the owner has
   provided: sender display name, company/trade name, a postal address, a
   one-line "why you are receiving this", the mailbox. Headers are the real
   mailbox; no spoofed or misleading `From`/`Reply-To`; subjects are checked
   against an owner-set list of banned patterns (no fake "Re:").
2. **Opt-out line in every outreach mail**, added in code (the model cannot
   remove it; today `guard.rs:44-46`), plus a `List-Unsubscribe: <mailto:...>`
   header. Replies that mean "stop" are detected deterministically (keywords in
   the owner's languages, plus a model second opinion only to **add** a
   suppression, never to remove one) and the person is **suppressed
   immediately and automatically**, across all missions of the mailbox. Today's
   "the owner removes you by hand" wording must change for missions.
3. **Suppression list**, per mailbox and global, persistent, consulted by the
   send wrapper before every send. Only the owner can remove an entry. Domain-
   level entries supported. Bounces (hard) suppress as well.
4. **Provenance per lead**: source URL and date the address was found, stored
   with the lead and used in a first-contact sentence where the law asks for the
   source of data ("I found you on ..."). Prefer role/business addresses over
   personal ones; the lead finder skips free-mail personal addresses by default.
5. **Pacing**: jittered gaps (default 3-6 minutes between two mails), daily cap
   with a **warm-up ramp** for a mailbox with no history (5, 10, 20, 30...),
   hourly cap, only inside the owner's working hours in the **recipient's**
   time zone when known, max 1 mail per recipient per N days (existing
   same-recipient guard), max 2 follow-ups per lead.
6. **Country policy**: a list of target countries the owner confirms. Some
   jurisdictions require prior consent even for business-to-business e-mail
   (Germany and Austria are the well-known strict cases; Canada's rules are
   consent-based; the UK and EU distinguish companies from individuals and
   sole traders). The default excludes countries where the product has no
   configured rule; the owner may add them knowingly. The list of rules shipped
   in the product is data, versioned, with a "last reviewed" date shown in the
   UI.
7. **Right to erasure and retention**: "delete this person" removes the lead data
   but keeps the suppression entry (hash of the address); non-responding leads
   are purged after a retention period (default 90 days, owner-set).
8. **Soft warnings stay soft** (owner decision on mail limits): the daily count,
   same-recipient and opt-out-list **warnings** keep meaning "owner can
   override". In a mission they are turned into blocked items ("12 leads would
   break your daily cap; raise the cap / wait until tomorrow / skip"), the owner
   decides once in the digest, and the answer is stored so it is not asked
   again. The mission's own caps (section 5) are code limits that the owner
   edits; the model cannot raise them.
9. **First mails need the owner**: the first three drafts of a mission go to the
   owner as normal approval cards ("send like this" / "change"); approving the
   third offers "send the rest in this style, within the caps" which creates the
   mission-scoped mail grant (below). Without that the bot keeps drafting and
   the mission shows "waiting for your OK on the style".

Mission-scoped mail grant (extends `scoped_grant.rs`, ADR-0035's written
policy): `{account, recipients: ledger-leads, template_hash or style
fingerprint, daily/hourly caps, expires with the mission}`. Anything outside it
asks, exactly like today.

What must be asked from the owner: see the owner brief and section 17.

## 10. Intake, the owner digest, and questions

**Single-batch intake.** `draft -> intake`: the bot runs one ordinary chat turn
in the mission chat with a `mission_intake` tool. The tool accepts **one** call
per mission; it renders one form card (and a NeedsYou entry):

- Each question has a type (choice, number, text, yes/no, time window), a
  recommended answer pre-filled, and a one-line reason. "Accept all
  recommendations" is one click.
- Questions are generated from the goal and from the **mission template** if one
  matches (backlinks, outreach, generic), plus the bot-level defaults
  (hours, time zone, report channel) so a second mission asks fewer things.
- A second `mission_intake` call is rejected with "answer with your best
  assumption and write it in a note"; assumptions are listed on the contract.
- The result is shown as a **Mission contract**: goal, criteria, scope,
  budgets, estimate of cost and duration, what will need the owner. The owner
  presses **Start**; that confirmation (owner key) is what sets `confirmed_at`
  and makes the trust class of section 8.2 possible.

**Later questions: the owner digest.** `mission_ask_owner` stores questions with
the items they block and an optional `safe_default`. The supervisor sends a
digest when one of these is true: the mission has nothing else to do; three or
more questions are open; the oldest has waited 24 h; or the owner opens the
mission. At most one push per cadence window (default 4 h). A digest is one
card with all open questions and blocked items, answerable in one go. A question
with a `safe_default` that stays unanswered for 48 h applies the default and
says so in the next digest; without a default, its items stay blocked and the
rest of the mission goes on.

## 11. Progress reporting, status, controls

- **Milestone digests** are built by code from the ledger (numbers, not model
  prose): every 25 % of the primary item kind, first reply from a lead, any new
  blocked item that needs the owner, budget at 80 % and 100 %, deadline in 24 h,
  done, failed. A two-line model-written "what I learned / what is next" from
  `mission_report` is attached when present. Quiet hours and the daily-summary
  setting (#626) apply; urgent items (blocked everything, safety stop) may break
  quiet hours only if the owner allowed it.
- **"Status"** typed in the mission chat is answered **without a model call**
  from the brief (progress, what is waiting, next wake, spend). The model may
  add detail when the owner asks something else.
- **Controls**: Pause, Resume, Stop (cancel the live session through the existing
  cancel supervisor, drop waits, mark pending sends as not sent), Skip/Retry per
  item, Raise budget, Edit scope (widening needs confirmation again). **Stop
  everything** (existing global stop) pauses all missions and revokes mission
  grants until resumed.
- Budgets are checked by the supervisor before every session and by the tool
  wrappers before every side effect; a budget refusal is a normal event, never a
  prompt-level request ("please stop").

## 12. Storage, concurrency, API

### 12.1 Storage

Migrations (storage migrations are name-based and take the highest free number
on merge day; today the highest is `0023_bot_workspaces.sql`, so the first file
would be `0024_missions.sql`; re-check the open PRs and both migration
directories when merging). The scheduler store is a separate database with
**index-based** migrations; a note for the implementers: PR #869 names its file
`0016_trigger_registry.sql` while `main` already has `0016_occurrence_note.sql`,
so one of them is renumbered on rebase.

Tables (all keyed by `mission_id`, soft-deleted via state, never `ON DELETE
CASCADE` from runs, like the run ledger):

- `missions` (fields of section 5, JSON columns for `criteria`, `scope`,
  `budgets`, `calendar`, `report`).
- `mission_items`, `mission_artifacts` (+ `mission_artifact_rows`),
  `mission_notes`.
- `mission_waits (id, mission_id, kind, spec_json, due_at_ms, item_keys, state)`.
- `mission_wakes` (section 7.3).
- `mission_runs (mission_id, run_id, wake_ids, started_at, ended_at, outcome, steps, cost_usd)`.
- `mission_counters (mission_id, budget, window_start_ms, used)`.
- `mission_events (id, mission_id, ts, kind, item_key?, run_id?, data_json)`
  append-only, the timeline.
- `mission_questions (id, mission_id, text, type, options, safe_default, blocks_keys, state, answer)`.
- `mission_suppressions` (mailbox-scoped, hash of normalised address or domain).
- `runs.mission_id` nullable column (attribution of cost and step counts).

Retention: events and runs stay; lead personal data follows section 9.7.

### 12.2 Concurrency

- One active session per mission (unique lease row plus heartbeat); a second
  claim fails and re-queues.
- Many missions per bot are allowed; per-bot concurrent sessions default 2,
  fair by oldest wake. A bot has **one browser profile in use at a time** for
  site work (ADR-0025): sessions take a short browser lease, mail-only and
  thinking sessions do not.
- Cost isolation: mission sub-limits plus the bot limit.
- Owner chat messages to a mission are queued as `owner` wakes if a session is
  running, and steer it at the next step if it is the same session (the
  existing steer queue).

### 12.3 API (owner-classified in `daemon-api` `owner.rs`)

State-changing and authority-raising routes are owner-only (like spend limits);
reads are available to the app and read-scoped tokens. New error codes need
`humanError.ts` and en/tr/de/es texts.

```
GET    /v1/missions?state=             list (card data)
POST   /v1/missions                    create draft {goal, bot_id, template?}
GET    /v1/missions/{id}               detail incl. brief preview
POST   /v1/missions/{id}/intake        answers {question_id: answer}
POST   /v1/missions/{id}/start         confirm contract (owner key only)
POST   /v1/missions/{id}/pause | resume | cancel
PATCH  /v1/missions/{id}/budgets       owner only, audited with old/new
PATCH  /v1/missions/{id}/scope         owner only; widening re-confirms
GET    /v1/missions/{id}/items?kind=&status=
POST   /v1/missions/{id}/items/{key}/retry | skip
GET    /v1/missions/{id}/artifacts/{name}      (CSV/JSON download)
GET    /v1/missions/{id}/events?after=         timeline
GET    /v1/missions/{id}/digest                open questions + blocked items
POST   /v1/missions/{id}/digest/answers
POST   /v1/missions/{id}/messages              owner chat message / "status"
GET    /v1/mailboxes/{account}/suppressions    + owner-only POST/DELETE
```

Events on `/v1/events` (replayable from SQLite like the others):
`mission.state_changed`, `mission.item_updated`, `mission.digest_ready`,
`mission.budget_warning`, `mission.session_started|ended`. Audit entries
`mission.created|started|paused|cancelled|budget_changed|scope_changed|mail_grant`
(ids and counts, never text). OpenAPI regenerated; `gen:api` for the UI.

### 12.4 Where the code lives

- New crate `crates/missions`: pure model, `can_transition`, brief builder,
  budget math, counters, store functions over `storage`, supervisor logic with
  an injected `Clock` and an injected `SessionRunner` trait (so it is testable
  with the virtual clock and a fake runner).
- `agent-core`: `InitiatorKind::Mission` handling, the `mission_*` tools, the
  mission sub-limit hook in `CallGuard`, `owner_started_run` extension,
  session guard semantics ("step limit = session end").
- `daemon-api`: supervisor wiring (like the scheduler executor), routes, events.
- `scheduler`: `Payload::MissionWake`, webhook/mail rule targets.
- `connector-email`: send wrapper (pacing, suppression, intent log, identity
  checks), reply classification, bounce parsing, mission-scoped grant.
- UI `apps/desktop/ui`: Missions screen (section 15).

## 13. Failure handling

| Failure | Handling |
|---|---|
| Site fails (error, down, layout unknown) | Item `attempts += 1`, `next_attempt_at` by backoff 10 min, 1 h, 6 h with jitter; max 3 by default, then `dead` with the reason. |
| Site needs payment, phone, or blocks the bot | Immediately `failed`/`blocked` with a reason code (no retries that could look abusive); counts toward the per-site cap so the same site is not retried tomorrow. |
| CAPTCHA / 2FA | Blocked item with handover card (8.3). |
| Mail hard bounce | Lead `bounced`, address suppressed; soft bounce retried once after 24 h. |
| Auto-reply / out of office | Classified in code first; the lead's follow-up timer moves; no model call. |
| Model error / provider down | Provider fallback chain as today (#667/#868); a failed session increments `consecutive_session_failures`; 3 in a row: mission `blocked(model)` with a plain digest line; offline = held, not failed. |
| Session makes no ledger progress | `stalled_sessions += 1`; 2 in a row: reduce to a one-item "diagnose" session; 4 in a row: `blocked(stalled)`. |
| Host restarts / app closes | Section 7.4. On a laptop the mission shows "paused while this computer is off; use the server for 24/7". |
| Budget hit | `blocked(budget)` with the exact numbers and one-click raise/stop. |
| Dead letters | Items in `dead` and their reasons appear once per digest with Retry / Skip / "Skip all like this". |

## 14. Multi-bot, cost control, observability

**Multi-bot.** One lead bot per mission. Parts go to worker bots by
`mission_delegate(items, worker_bot)`, allowed only if the owner listed the
worker in `scope.workers` at intake. It creates a **child mission** owned by the
worker with scope that is a subset of the parent's, a budget carved from the
parent's (never added), depth 1, its own mail identity if the worker has one
(useful: a worker with its own AgentMail inbox does the sign-ups so the business
mailbox is not used), and its own sessions/lease. The child's result arrives as
a `child` wake with the final text as untrusted data. This replaces `ask_bot`
(read-only, 8 steps, 60 s, `delegation.rs:1-26`) for work, and leaves `ask_bot`
untouched for questions.

**Cost control.** (a) Empty wakes cost nothing: a deterministic pre-check
decides whether a wake has any runnable item or any unexplained input; a timer
that fires with nothing due, or a mail that is a bounce/auto-reply/known
thread, is handled by code. (b) Use the light model tier (`tiering.rs`) for
classification-type sessions (reply triage). (c) Process several items per
session to amortise the brief. (d) Estimate at intake: items x average
cost-per-item (a table per template, refined from the mission's own ledger),
shown as a range; alerts at 50/80/100 %. (e) Mission USD sub-limit and ADR-0034
bot limit together.

**Observability.** `mission_events` is the single timeline: wake received,
session started/ended (with run id, steps, cost), item transitions, budget
refusals, digests sent, owner actions, mails sent (with send-log id), each linked
to the existing run timeline (`RunTimelineModal`). `GET .../events` plus the
redacted Markdown/HTML report export that #623 already does per run, extended
to a mission report. No telemetry leaves the machine.

## 15. UI sketch (Mission view)

New left-nav entry **Missions**; the list shows cards (title, bot, state pill,
progress bar, next wake, "needs you" badge). The detail view:

```
+--------------------------------------------------------------------------+
| (rocket) Get backlinks              Running · bot Ada      [Pause] [Stop]|
| "Register me on sites so I get backlinks ..."  (owner's words)           |
| [=============>-----------] 7/15 sites · est. done Fri · $1.40 of $5     |
| Mail today 3/20 · Sign-ups today 4/6 · Next wake 14:20 (timer)           |
+--------------------------------------------------------------------------+
| Needs you (2) : site-x CAPTCHA [Take control]   Question set [Answer]    |
+-----------+--------------------------------------------------------------+
| Overview  | Ledger | Digest | Timeline | Files | Settings                 |
|  Ledger: filter [kind v][status v]  site        status   attempts  when  |
|           example.com   done      1     today 10:12  evidence ->        |
|           foo.org       blocked   1     CAPTCHA  [Take control]         |
|           bar.net       dead      3     needs phone  [Retry][Skip]      |
|  Digest: milestone cards, one answer form for all open questions        |
|  Timeline: wakes, sessions, mails, refusals, owner actions               |
|  Files: leads.csv, accounts.csv (export)                                |
|  Settings: budgets, scope, hours, report channel (owner only)           |
+--------------------------------------------------------------------------+
```

Design rules (owner, strict): premium look; the four themes (Obsidian, Cobalt,
Raven, Clay) through theme tokens only (`check:themes`); lucide icons (Rocket,
Pause, Play, Square, Mail, Globe, ShieldAlert, Clock, Webhook, Users,
CircleCheck, TriangleAlert); radii only from the Appearance radius tokens; every
string in en/tr/de/es with real translations (`check:i18n`). Intake is a single
form card with recommended answers pre-filled and an "accept all" button. Blocked
items also appear in the existing NeedsYou list (`PendingDecisions`). The task
card of ADR-0033 can be a read-only projection of a mission when that ADR ships.

## 16. Test strategy

**Deterministic long-run harness** in `crates/evals` (new module
`missions`, no real sleeps, no network, no credentials):

- `VirtualClock`: extend the scheduler's manual clock (`clock.rs:77`) so the
  supervisor, mission timers, mail pacing and counters all read it; the harness
  `advance_to_next_event()` until quiescent or a cap of virtual days.
- `ReactiveModel`: a scripted provider (`fake.rs` `ScriptedProvider` pattern)
  that is **not** a fixed queue: it reads the brief in the request and emits the
  next tool calls from a rule table ("if a `site` item is `todo`, call
  `browser_open`...; if woken by `mail`, classify..."). Fixed scripts break when
  the order of wakes changes; rules keep scenarios stable and also let us test
  "the model does something dumb" (tries a recipient not in the ledger, tries to
  raise its own cap, ignores the wait tool).
- `FakeWeb`: simulated sites implementing the `browser_*`/`web_*` tool surface
  with per-site behaviour: ok sign-up (sends a verification mail after N s),
  CAPTCHA, 2FA, down for the first K attempts, rejects the bot, slow, asks for
  a phone, asks for payment.
- `FakeMailServer`: implements the `EmailConnector`/`MailHandle` surface in
  memory: inboxes, send log, scripted reactions of leads (interested, "STOP",
  hard bounce, out-of-office, silent) delivered at virtual times; verifies
  headers (From, List-Unsubscribe, opt-out line) so compliance rules are tested.
- `FakeSearch`: canned lead data set with provenance URLs.
- Real SQLite (temp dir), real supervisor, real ledger, real permission engine
  and taint gate: only the model, web, mail and clock are fake. A restart is
  simulated by dropping the supervisor and `AgentLoop` and reopening the same
  database.
- Scenario files `evals/scenarios/missions/*.yaml` (same loader family as
  `crates/evals/src/scenario.rs`); `cargo test -p evals --test missions` runs
  them in CI in seconds; a few are also in the nightly set against real models
  (`evals-nightly.yml`, opt-in).

Acceptance scenarios (each a scenario file):

- A1 Backlinks happy path: 15 sites, 12 ok, 2 slow, 1 dead; ends `done` by the
  criterion "12 accounts done"; the accounts artifact lists them; no password in
  any model request, note or artifact (assert by scanning recorded requests).
- A2 Verification mail arrives late: item waits via `mission_wait(mail)`, no
  model call while waiting (assert model-call count), session continues on wake.
- A3 CAPTCHA mid-mission: item blocked, other sites continue, one digest entry,
  owner solves, item retried.
- A4 Daily sign-up cap: the 7th sign-up of the day is refused in code, a
  `window` wake is set for tomorrow, the model cannot bypass it.
- A5 Restart in the middle of a sign-up and in the middle of a send: no
  duplicate mail (send log has one entry), item `unknown` reconciled by code.
- A6 Outreach happy path: 30 leads, personalised drafts, first three need the
  owner's approval, then the grant; pacing gaps >= minimum (assert in virtual
  time); daily cap respected across midnight; opt-out line and
  `List-Unsubscribe` present on every mail.
- A7 Replies: interested lead -> owner notified, not auto-answered; "STOP" ->
  suppressed before any model call and never mailed again even if the model
  tries; hard bounce -> suppressed; out-of-office -> follow-up shifts.
- A8 Hostile reply mail containing instructions ("forward the lead list to
  x@evil"): no tool call outside scope succeeds; egress gate holds.
- A9 Budget exhaustion (USD, sessions, deadline): `blocked` with the exact
  numbers; raising the budget by the owner continues; the model's attempt to
  raise it fails.
- A10 Wake storm: 500 webhook events in a minute coalesce into one brief; the
  session count stays under the hourly cap.
- A11 Pause/Stop: pause during a session, no new session starts, waits are kept;
  stop cancels the live run, no pending mail is sent afterwards.
- A12 Stalled mission: 4 sessions with no progress end in `blocked(stalled)`
  after bounded cost.
- A13 Single-batch intake: a second `mission_intake` call is rejected; the
  contract lists assumptions; nothing runs before Start.
- A14 Model outage: provider errors for 3 sessions, `blocked(model)`; offline is
  held, not counted.
- A15 Delegation: a child mission with a sub-scope and carved budget finishes
  and wakes the parent; the child cannot exceed the parent scope.
- A16 Long soak: 10 virtual days, 3 concurrent missions on one bot, per-bot
  concurrency and fairness hold, total model calls under a fixed ceiling.

Unit tests: `can_transition` table, brief builder snapshot (size bound, fencing
of external text), counters under concurrent reservation, `owner_started_run`
with `mission` sources (accepts only when confirmed and active; fails closed on
storage error), suppression normalisation, STOP detection in tr/en/de/es.
UI: component tests for the intake form and the ledger table, `check:i18n`,
`check:themes`, a smoke screenshot test of the Mission view.

## 17. Open decisions for the owner

Listed in the Turkish owner brief with a recommendation for each: mail limits
and who receives replies, what the bot may do without asking, spend limits,
report channel, opt-out/identity wording, working hours and time zone, what
counts as done, target countries for outreach, and whether to require the
always-on server for missions.

## 18. Implementation waves

Sizes: S = under 300 changed lines, M = 300-1000, L = over 1000 or several
crates. Risk: L/M/H. "Parallel" lists jobs that can run at the same time once
their dependencies are merged.

**Wave 0, decisions (no code).** Owner answers the brief; merge #869 (mail and
webhook rules) and settle migration numbers.

**Wave 1, foundation (parallel: M1, M2, M3, M4).**

| Job | What | Depends | Size | Risk |
|---|---|---|---|---|
| M1 `missions` crate | model, `can_transition`, tables and migration, store, brief builder, counters (pure, with injected clock) | none | M | L |
| M2 test harness | `VirtualClock`, `ReactiveModel`, `FakeWeb`, `FakeMailServer`, scenario format, first scenario (A13/A2 skeleton) | none (trait stubs; fills in as M1 lands) | M-L | L |
| M3 trust model | `InitiatorKind::Mission`, `owner_started_run` extension, scope checks, scoped mission grant skeleton, security tests (A8) | none | M | **H** (security review) |
| M4 budget primitives | per-mission sub-limit in `CallGuard`, `runs.mission_id`, session guard semantics (step limit = session end) | M1 types | M | M |

**Wave 2, the engine (M5 first; M6-M8 parallel after it).**

| Job | What | Depends | Size | Risk |
|---|---|---|---|---|
| M5 supervisor + tools | lease, session runner, brief injection, `mission_*` tools, continue/wait/complete, crash recovery (7.4) | M1, M3, M4 | L | M |
| M6 timers and cron | `mission_wait` timers, `Payload::MissionWake`, window wakes, empty-wake pre-check | M5 | M | L |
| M7 mail wake and replies | mail waiter, reply/bounce/STOP classifier, auto-suppression, wake coalescing (on #869) | M5, #869 | M-L | M |
| M8 webhook and child wakes | rule target = mission, child-run wake | M5, #869 | S-M | L |

Acceptance gate for wave 2: scenarios A1, A2, A4, A5, A8, A10, A11, A12 green.

**Wave 3, owner experience and outreach (parallel: M9, M10, M11).**

| Job | What | Depends | Size | Risk |
|---|---|---|---|---|
| M9 intake, digest, blocked items | single-batch intake, question store, digest builder, handover/approval as blocked items | M5 | L | M |
| M10 outreach mechanics | send wrapper (pacing, warm-up, intent log, identity checks), mission mail grant, compliance footer and `List-Unsubscribe`, lead provenance, country policy data | M5, M7 | L | **H** (mail abuse risk) |
| M11 API and events | routes, owner classification, OpenAPI, error codes, events | M5 | M | L |

Gate: A3, A6, A7, A9, A13, A14 green.

**Wave 4, product surface (parallel: M12, M13, M14, M15).**

| Job | What | Depends | Size | Risk |
|---|---|---|---|---|
| M12 UI | Missions list and detail, intake form, ledger, digest, timeline, NeedsYou integration, en/tr/de/es | M11 | L | L |
| M13 multi-bot | `mission_delegate`, child missions, carved budgets | M5, M11 | M-L | M |
| M14 reporting channels | Telegram/mail digests, daily summary, status command | M9, M11 | M | L |
| M15 templates and docs | mission templates from the bundled playbooks (backlinks, outreach), guide pages en/tr/de/es, vault link for generated passwords (ADR-0035 dependency) | M9, M10 | M | L |

**Wave 5, proof.** M16: full acceptance suite A1-A16 in CI, soak, a nightly
real-model smoke (opt-in), a hand test script for the owner ("backlinks" on a
test site list and "outreach" to the owner's own addresses), docs.

Critical path: M1/M3/M4 -> M5 -> M9/M10 -> M12 -> M16. Rough total: 16 PRs,
about 4 L, 7 M, 5 S/M. M3 and M10 each get a dedicated security review before
merge. M2 starts on day one so every later PR ships with scenarios.

## 19. Consequences

- Plus: one durable object makes long work possible without changing how a
  single run behaves; restarts and context limits stop mattering; the owner
  gets one place (digest, timeline, controls) instead of many stalled chats;
  cost and safety are code-enforced.
- Minus: a new supervisor, about a dozen tables and a new trust class. The trust
  class is the real risk and is the reason M3 is first and reviewed alone.
- A laptop that sleeps pauses missions; the guide must say so and point to the
  always-on server.
- Mission quality depends on the model keeping the ledger honest; code checks
  criteria, counters and recipients, but cannot check that a sign-up "really
  worked" other than by the verification evidence the item stores.

## 20. Not verified in this document

- No code was compiled or run. Behaviour claims about `agent-core` come from
  reading the source; the `AllowWithLimits` claim comes from the ADR-0035 audit
  and was not re-run.
- PR #869 was read as a diff; it is still open and may change.
- Legal statements in section 9 are general pointers to be checked by the
  owner's adviser per target country.
