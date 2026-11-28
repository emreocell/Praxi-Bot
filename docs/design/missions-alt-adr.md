# Missions: durable work across bounded runs (alternative)

Status: proposed, independent design for comparison. Date: 2026-11-26.

This document was developed from the code and existing decisions, without reading worker c880's design or PR. It proposes architecture; it does not implement or authorize a permission change.

## Decision and product contract

A **Mission** is an owner-authorized objective, its durable work ledger, and the rules under which the daemon may keep advancing it. A **Run** remains one bounded execution attempt. A mission can outlive hundreds of runs, context windows, mailbox waits, and daemon restarts. Do not turn one run into an immortal conversation, and do not make a new agent framework.

The owner gives one substantial task. The bot gathers genuinely missing information in one intake batch, chooses sensible defaults for the rest, and proceeds without asking whether to start or continue. Later discoveries accumulate in one owner digest; independent work continues. Timers, cron, incoming mail, signed webhooks, file changes, child completion, and owner replies can advance the same mission. A wake is evidence to consider, never new authority.

Success means the agreed acceptance criteria have evidence, not that the last model reply sounds finished. Failed targets and uncertain actions remain visible. The system must say "17 verified, 2 refused, 1 awaiting your help" rather than claim 20 registrations. A finite mission has a deadline or a finite completion condition. An ongoing service is an explicitly renewable mission with periodic checkpoints and budgets; it is never silently infinite.

**Opinion:** the backbone is a small transactional coordinator plus typed work records. Increasing `max_steps`, periodically asking an LLM what to do, or saving a model summary alone cannot provide this contract.

## Audit: what exists and what does not

Baseline: `origin/main` at `869f3e9b`; open trigger registry [PR #869](https://github.com/emreocell/Praxi-Bot/pull/869) inspected separately at `566ac83e29083295e37f688ea07c42a99b2e8852`. An open PR is a dependency candidate, not released behavior. Paths below describe the baseline unless marked otherwise.

| Area and evidence | Existing behavior | Consequence for missions |
| --- | --- | --- |
| [Loop guards](../../crates/agent-core/src/config.rs), [executor](../../crates/agent-core/src/exec.rs) | Defaults: 40 model calls, 2,000,000 turn tokens, 120-second tool timeout, 300-second model timeout, repeated-call warning at 3 and stop at 5. Guard exhaustion produces a closing tool-less model step. | Keep turn guards. Add a structured slice outcome; parsing the closing prose or checking unfinished todos is insufficient. |
| [Run state](../../crates/task-runtime/src/state.rs), [runtime](../../crates/task-runtime/src/runtime.rs) | Queued/Running/WaitingApproval/Paused/Succeeded/Failed/Cancelled. Startup recovery pauses Running runs and marks unfinished intents Interrupted. Non-idempotent interrupted steps require a decision; `resume_skipping_interrupted` does not establish their outcome. | Mission recovery must preserve uncertainty. A second run is not permission to repeat the action. |
| [Storage](../../crates/storage/src/lib.rs), [records](../../crates/storage/src/records.rs) | SQLite WAL, runs/steps, approvals, append-only audit, event index and per-run event logs. | Use application SQLite for mission control and links to existing steps. Do not store authority in editable workspace Markdown. |
| [Scheduler engine](../../crates/scheduler/src/engine.rs), [store](../../crates/scheduler/src/store.rs), [watcher](../../crates/scheduler/src/watch.rs), [trigger contract](../schedule-triggers-prechecks.md) | Cron/interval/one-shot, misfire handling, occurrences, per-schedule concurrency, file metadata and two-minute mail-header polling, prechecks. Separate `schedules.db`; unresolved dispatches are missed/interrupted, not an exactly-once delivery queue. Mail search windows are bounded. | Reuse clocks and adapters, but per-schedule concurrency cannot serialize several wake sources for one mission. Durable handoff is needed across databases. |
| [Incoming webhooks](../../crates/daemon-api/src/incoming_webhook.rs) | HMAC timestamp/signature validation, persistent replay protection, ingress limits, owner-only management, fresh run/chat for accepted calls. | A mission binding must enqueue into an existing mission instead of starting an unrelated run. Keep signing, replay and ingress checks. |
| Trigger registry #869 | Proposed owner-visible mail/webhook/time rules over schedules; filters, quiet hours, persistent coalescing and rule/bot limits. Its bounded queue is 100 events/48 KiB. External-source isolation and once-only approval floors remain. | Add a destination adapter and durable handoff to this registry if it lands. Do not fork its rule editor or claim it already resumes missions. |
| ADR-0032 nudges; ADR-0033 task cards (external decision records, read locally) | Proposed proactive read-only discovery and a display summary of a run. Neither is a mission executor. | A nudge may suggest a mission; cannot authorize it. A task card can display one slice; cannot represent the full lifecycle. This proposal intentionally adds a mission lifecycle beyond ADR-0033's rejected second run ledger, while reusing its actual run/step ledger. |
| ADR-0034; [usage guard](../../crates/agent-core/src/usage.rs), [bot limits](../../crates/agent-core/src/bot_limits.rs), [spending](../../crates/agent-core/src/spend.rs) | CallGuard covers model calls with usage auditing, installation and bot limits, and in-process reservations. Unknown prices are not billed as known money; bot token limits still apply. | Add mission attribution at this choke point and durable reservations; a run-local cap resets on continuation and is not a mission budget. |
| ADR-0035 capability expansion (including its later owner decisions) | Recommended playbooks, accounts, mail, handover, then continuation. Its original capability inventory is older than main. Owner says mail count/cooldown warnings are overridable by the owner, and payments retain today's approval behavior. | Do not copy the stale inventory or turn every soft warning into a global hard prohibition. This design keeps owner overrides and separates safety floors from mission budget choices. |
| [Playbooks](../../skills/README.md), [signup](../../skills/signup-to-site/SKILL.md), [outreach drafts](../../skills/outreach-drafts/SKILL.md), [workflows](../guide/workflows.md), [meta tools](../../crates/builtin-tools/src/meta.rs) | Skills explain goals and steps; workflows record/replay reviewed operations; `todo_write` renders a replaceable model-authored list; `ask_user` currently asks one question. No dedicated durable goal-recipe executor was found in the audited router/core/meta sources. | Treat goal recipes as optional, versioned intake/acceptance templates, not another execution language. Existing skill goals and todos confer no authority or durable completion proof. |
| [Subagents](../../crates/agent-core/src/subagent.rs), [delegation](../../crates/agent-core/src/delegation.rs) | Same-bot child runs, spawn-key reuse after restart, depth/step/token caps; parent tool waits for children. `ask_bot` is a narrower owner-started, approved, read-only helper with its own restrictions. | Preserve ancestry and charge all children. Child-finished wakes are coordinator events, not a reason to grant asynchronous children broader rights. |
| [History](../../crates/agent-core/src/history.rs), [compaction](../../crates/agent-core/src/compaction.rs), [memory](../guide/memory.md) | Rebuilds run history; deterministic observation shortening/spill preserves stored steps and taint. Recall/extraction is a separate memory system. | Mission truth must be explicitly loaded at each slice. Vector recall and a generated summary cannot recover an exact work cursor. |
| [Full-mode approvals](../../crates/agent-core/src/approval.rs), [#859](https://github.com/emreocell/Praxi-Bot/pull/859), [security](../guide/security.md) | Full public-web waivers are limited to owner-started runs. Private-source comparisons, unhashable-source fail-closed behavior, always-ask tools and absolute floors remain. External triggers, children and helpers do not inherit that waiver. | Never forge `Human` initiator on a webhook or continuation to obtain Full. Authority continuity is an explicit security work item below. |
| [API/run history](../../crates/daemon-api/src/routes.rs), [event journal](../../crates/storage/src/journal.rs) | Run details, steps, approvals, usage, cancel/resume and replayable events already exist. | Add mission events with stable sequence numbers and links into those timelines, not a second copy of every tool result. |
| [Always-on host](../guide/linux-host.md), [leadership limits](../guide/host-leadership.md) | A service and virtual browser can stay alive without desktop login. Leadership epochs do not fence already admitted runs; partitions can produce concurrent owners. | First release binds a mission to one host and forbids live migration. A local SQLite lease cannot fix distributed split brain. |
| [Accounts](../../crates/builtin-tools/src/account_register.rs), [fill secrets](../../crates/secret-store/src/fill.rs), [mail wait](../../crates/connector-email/src/wait.rs), [mail guard](../../crates/connector-email/src/guard.rs) | Daemon-generated account passwords, bot/origin scope, identity resolution, verification mail matching. `email_wait_for` is an in-run bounded wait (up to ten minutes). Mail daily/cooldown/opt-out warnings are currently soft, owner-overridable guards; frozen overrides are short-lived. | Store vault/identity references only. Replace long in-run waiting with durable subscription state. Do not confuse existing mail logs or a prepared vault entry with proof of successful delivery/registration. |
| [Browser handover](../design/handover.md), [implementation](../../crates/builtin-tools/src/handover.rs) | Managed-browser CAPTCHA/code handover, live view, fresh origin checks, ephemeral owner codes, limited simple CAPTCHA attempts across a run family. Current owner-started admission and short timeouts apply. | Mission-scoped challenge accounting and durable nonsecret request metadata are needed across slices. Codes and stale DOM refs must never be persisted for later replay. |

The external ADR files are not repository artifacts and are not copied here. Application migrations currently reach `0023_bot_workspaces`; scheduler has its own ordered migration list including `0016_occurrence_note`. #869 also proposes an `0016`-named migration. Allocate filenames and append order against the actual merge base when implementing; do not reserve old ADR numbers now.

## Mission model and authority

Proposed record, with bounded structured fields rather than an arbitrary agent scratchpad:

```text
Mission {
  id, owner_id, bot_id, chat_id, execution_host_id,
  revision, state, pause_reason?, created_at, updated_at, deadline?,
  owner_objective_ref, acceptance_criteria[], scope_revision,
  intake_batch_id?, authority_ref, policy_revision,
  limits_ref, notification_preferences, retention_policy,
  active_root_run_id?, execution_generation,
  checkpoint_seq, next_eligible_at?, terminal_outcome?
}
```

The owner objective, intake answers and approved scope are immutable revisions. The bot can propose additions, never broaden its authority, change sender identity, raise limits, rewrite acceptance criteria, or extend an expiry. Current policy, current bot mode and revocation are checked at execution time; taking a snapshot must not preserve an allowance the owner removed.

`authority_ref` identifies a daemon-created, owner-confirmed scope: permitted action families and targets, mail identities, data classes/destinations, expiry, and revocation generation. A model cannot supply an owner identity or authority reference as a tool argument. The original trigger and current wake cause are stored separately from this authority. An authenticated webhook only proves which source sent data; it cannot enlarge the scope.

**Permission compatibility is a release gate, not a prompt fix.** The present Full and external-source rules cannot deliver unrestricted unattended sending and account creation. Implement two clearly separated pieces:

1. A typed continuation authority for a previously approved owner mission, persisted in run metadata and reconstructed from the mission ledger. Its resolver intersects mission scope with current permissions and all existing source, taint, egress and absolute floors. It must not relabel the run as Human, inherit a helper's powers, discard external-source restrictions, or treat a cron instruction as owner speech. If the intersection asks, the mission collects that request.
2. A batch decision mechanism that approves **each exact action** in a finite selected set. One owner interaction can produce many single-use decisions without a blanket waiver. Bind each to mission revision, tool, canonical target, arguments/content hash, source/taint generation, identity and expiry. Recheck current gates before consumption; changed content, destination, secrets, new reads invalidating approval, or warnings require a fresh decision. `Deny` always wins. A batch is never an "always allow" grant.

Batch decisions require a separately reviewed extension of the existing approval pipeline; until it exists, retain individual cards and honestly report the limitation. For Full browsing across slices, any extension of #859's owner-started eligibility needs explicit security review and regression tests. External-data floors remain effective. A new worker run cannot launder a prior external run into a less restricted one.

Fully automatic **newly personalized** outreach without later owner review is a further policy decision: precise standing orders and destination/data controls may be proposed, but this ADR does not waive mail's existing always-ask floor. It is outside the first release. This leaves a real gap against the eventual owner contract; hiding it behind "Full" would be worse than naming the prerequisite.

## State machine: lifecycle separate from work availability

Use six mission lifecycle states. Store reasons and outstanding dependencies separately rather than inventing a state for every kind of wait.

| State | Meaning | Legal exits and actor |
| --- | --- | --- |
| `intake` | Objective recorded; one consolidated missing-information batch may be open. Safe preliminary reads can run within existing authority. | `active` when required intake/scope is satisfied; `paused` or `cancelled` by owner. |
| `active` | Coordinator may advance eligible work. It can be running or asleep. | `paused` by owner or deterministic safety/budget/stall rule; `succeeded` or `failed` by validated completion; `cancelled` by owner. |
| `paused` | No ordinary dispatch; explicit reason such as owner pause, unsafe uncertainty, expired scope, or hard budget. | `active` after owner action or a previously authorized deterministic recovery condition; `failed` after an explicit unrecoverable outcome; `cancelled`. |
| `succeeded` | Acceptance record, evidence and final report committed. | None. A new objective creates a new linked mission. |
| `failed` | Mission cannot meet its contract; partial results and reasons retained. | None. Owner may clone/revise into a new mission. |
| `cancelled` | Admission closed, in-flight run family settled, pending effects/wakes disabled. | None. Cancellation does not undo sent mail or remote accounts. |

`active` has a **derived** display phase: working, ready, waiting on time/source/child, or needs owner. An owner question blocks only dependent work items. The mission is paused only when no safe path remains or a global gate closes. UI may show "Needs you; 8 other sites continuing." No model-authored status can mark a run complete or enable dispatch.

Pause/cancel first closes admission and increments the execution generation in SQLite, signals the run family, and waits for it to settle before acknowledging completion. A side effect already accepted remotely can remain uncertain; display that explicitly. `cancel_requested` is an operation marker while draining, not a successful cancellation claim. Late child/event callbacks can add evidence but cannot reopen a terminal mission.

## Durable ledger and a trustworthy "where am I" brief

Add tables to **application SQLite**, next to runs and steps:

| Table | Essential content / invariant |
| --- | --- |
| `missions` | Lifecycle, immutable scope revision reference, generation, active root slot, checkpoint/event cursors. |
| `mission_events` | Append-only `(mission_id, seq)`; typed event, time, actor, source identity and evidence reference. Payload bounded/redacted. Commit event and affected projection in one transaction. |
| `mission_work_items` | Generic item key, dependencies, status (`ready/working/waiting/needs_owner/done/failed/unknown`), next action proposal, attempts, evidence refs, revision. Optional site/lead metadata lives in versioned payloads, not special core tables. |
| `mission_runs` | Run/mission/parent link, slice number, claim generation, input checkpoint and event watermark, structured outcome. Unique root slot per mission; descendants explicitly linked and budgeted. |
| `mission_waits` | Predicate, subscription source/ref, event cursor, absolute deadline, status, deterministic idempotency key and work-item reference. |
| `mission_effects` | Stable logical operation id, item, tool/target/identity, content hash, approval ref, attempt, status, existing run-step ref and external receipt. Uncertain operation blocks a new equivalent attempt. |
| `mission_owner_batches` | Intake/digest ids, stable question/action ids, dependency refs, answers/decisions, delivery state, version and notified watermark. |
| `mission_reservations` | Model-call/action reservation id, mission and existing usage/effect link, maximum reserved amount/tokens, settlement state; no parallel independent spend ledger. |

Indexes enforce unique source event ids and logical effect ids. The event ledger explains mission decisions; the existing run ledger remains the only detailed tool-attempt history. Model usage remains the existing usage ledger, joined through mission attribution. Materialized counters must be rebuildable from ledger references and unresolved reservations; never charge a child twice through both its root total and its own calls.

Checkpoint after every completed effect, work-item transition and wait registration, not just when the model feels like summarizing. `mission_checkpoint` accepts a revision-checked item delta and evidence references. It cannot assert an unknown remote side effect succeeded, alter authority, or rewrite a prior receipt. An interrupted model call may lose a proposed plan; committed effects and item states survive.

At **every** new slice, the daemon constructs a bounded brief with:

1. Owner objective and acceptance/scope revision, separately identified as owner material.
2. Current constraints, remaining budget, pacing, deadlines, host and selected identities.
3. Verified completed items and artifacts, with evidence references; failed and uncertain operations first.
4. Eligible frontier: item ids, dependencies, pending waits/owner requests, next legal action.
5. Unconsumed wake facts and their event ids, labeled untrusted with their source metadata.
6. Last slice outcome, execution generation and a small optional model-written orientation note.

The orientation note is a cache, never the cursor or proof. Derived counts and authorization come from typed records. External quotes remain external when copied into a summary. Reconstruct content provenance/taint from durable labels and source hashes; unavailable or unhashable provenance stays fail closed. No password, OTP, webhook secret or mailbox credential appears in the brief. Large artifacts are retrieved by validated, scoped references, not arbitrary model-provided paths. If the brief is too large, page the ready frontier; never truncate uncertainty or safety constraints.

Context rollover creates a fresh slice with that brief and selected evidence, not the entire old chat. Recall can help with background preferences but is not needed to know which of twenty sites is complete. Immutable recipe/skill versions are recorded so a later playbook update cannot silently change the mission contract.

## Wake sources, durable waits and single-run admission

One coordinator lives inside the existing daemon. Scheduler and connector adapters call `enqueue_mission_event`; they never call `AgentLoop::start` directly for a bound mission. Other existing schedules/webhooks retain their behavior. First-class sources:

| Source | Deduplication / continuation behavior |
| --- | --- |
| Cron, interval, one-shot | Existing timezone/misfire logic; key `(binding, definition_revision, due_at)`. Default coalesce missed checks into one current-state check, not a burst of old actions. |
| Mission timer / retry / pacing deadline | Durable wait id plus deadline generation. Daemon scans due waits at startup and on scheduler ticks; a lost in-memory timer cannot lose the wake. |
| Signed webhook | Existing signature/replay/size checks first; validated binding selects mission. Payload cannot choose another mission or overwrite objective. Provider delivery id or persistent ingress id deduplicates retries. |
| Incoming mail / reply / verification | Account+mailbox+UIDVALIDITY+UID or provider id; match the bound identity and expected operation/thread, not just subject text. Retain a cursor and bounded catch-up. |
| File change / connector event | Source-specific cursor; metadata/payload is untrusted. The adapter interface supports future sources without a new runner. |
| Child completion | Persist terminal child outcome, then append completion keyed by child run id. Startup reconciliation synthesizes a missing event once. |
| Owner answer / approval / manual continue | Owner-authenticated revisioned decision; retries return the same accepted result. No raw public ingress may emulate it. |

Mission tools are loop-interpreted control operations, subject to scope and bounds:

```text
mission_wait({item_id, source_binding, predicate, deadline, on_timeout})
mission_sleep_until({item_id, at, reason})
mission_checkpoint({expected_revision, item_deltas, evidence_refs})
mission_request_owner({questions_or_actions, dependency_ids, urgency})
mission_finish({acceptance_evidence, partial_failures})
```

Predicates use a small typed comparison schema, not arbitrary scripts or executable text. Tools can reference existing authorized bindings or narrow their filters; creating a new mailbox/hook, opening ingress or widening a source subscription requires owner management authority. These tools cannot create recurring permission to do arbitrary work.

Register wait + checkpoint + structured `yield(waiting)` outcome atomically before releasing the root slot. A model proposing a wait ends that slice after the control operation; it does not sleep a thread for days. The run can finish successfully **as a slice**, while `mission_runs.outcome=yielded` and the mission stays active. UI must not present that RunState as mission success. Reuse short browser waits for page rendering; `email_wait_for` may remain for short interactive use, but is not the durable mission waiter.

Avoid the lost-event race: record an observation watermark when starting the logical operation; registering a wait scans already committed matching events after that watermark in the same application transaction. Never discard events merely because no waiter exists yet. Fresh mailbox subscriptions need initial catch-up after the operation's timestamp, not the watcher's current "first poll is only baseline" behavior. Account replacement/UIDVALIDITY changes invalidate the cursor and trigger bounded resynchronization. Overflow records a gap and pauses dependent work for reconciliation; it must not silently pretend every mail was seen.

**Cross-database handoff:** `schedules.db` and application storage are separate. Add a durable source outbox keyed by source occurrence/delivery id, insert the application inbox event idempotently, then acknowledge the outbox. Replaying delivery is safe; dropping an occurrence before inbox commit is not. Webhook replay-marker acceptance and its mission inbox entry should be one application transaction before returning 202. A 202 means durably accepted, not action completed. Keep source acknowledgements until handoff is certain; bounded retention and visible overflow apply.

**Admission transaction** (`BEGIN IMMEDIATE`, conceptual):

```text
read current mission, authority generation, host admission, and due work
if inactive / paused / expired / already owns root slot: retain event; return
reserve the unique root slot; allocate a run id and slice number
persist queued Run + mission_runs link + checkpoint/event watermark
commit
start this prepared run id (idempotent); reconcile unstarted queued rows on boot
```

This needs a prepared-start API in agent-core/storage; "set active_run_id, then create a random run" has a crash hole. A partial unique index on open root claims plus revision CAS prevents simultaneous cron/mail/webhook/manual dispatch. While a run is active, additional events remain pending; it can consume a new batch at a checkpoint or the next slice can. Events are acknowledged only when their effects on mission state are committed.

One **root** controller per mission; children belong to that run family. Initially keep existing synchronous child behavior and disable detached children. A parent waiting for a child still owns the root slot; child completion unblocks that parent, not a second parent. Later detached fan-out can park a controller, but outstanding child ownership still prevents competing writers to the same item. Child results are untrusted, permissions cannot expand, and all descendants share mission deadlines and budget reservations.

Within one host use one owning daemon process, a process lifetime lock, and monotonically increasing execution generations checked before every tool effect and durable mutation. A lease timeout alone must **not** start a replacement while an old process can still send mail. Boot recovery first establishes that the old process is gone, runs existing runtime recovery, and inspects open effects/children. An interrupted non-idempotent step remains quarantined. Read-only recovery can resume that same run through a reviewed recovery path; ordinary new slices wait until the run family is settled. Multi-host failover requires external fencing beyond this local protocol and is cut from v1.

## Intake and one later owner digest

During intake inspect available profiles, identities, capabilities and already supplied facts. Ask one structured batch containing only consequential missing choices: objective/done condition, identity, required owner-only information, budget/deadline, and action boundaries. Show defaults alongside optional fields. If everything is available, skip questions. A non-answer is never consent to send, spend or broaden scope.

After activation `ask_user` in a mission is adapted to the owner-request accumulator; children cannot notify the owner directly. Duplicate questions collapse by stable question/dependency id. It returns `recorded; dependent item blocked`, not an indefinite interactive wait. Model output saying "please answer" cannot emit a separate notification. A capability failure is recorded with a real error/evidence and proposed remedy, not an invented outage.

The digest contains newly blocked questions, exact action approvals, hard-stop requests, uncertain effects and a compact progress/budget summary. Owner can answer/approve/skip all selected entries in one submit; answer handling atomically revises dependencies and enqueues a wake. Keep the digest in chat/Needs You even if notification delivery fails. One pending digest per mission is updated in place. Notification idempotency is `(mission, digest, notified_version)`; an ambiguous outbound notification is not blindly resent.

Default: notify once when safe independent work is exhausted, or at the owner's chosen digest interval; coalesce updates and honor quiet hours. Urgent expiry/credential/OTP/CAPTCHA items can surface the **same digest** immediately under a rate cap; no per-question ping. Owner can open it anytime. No arbitrary "one later question only" quota: new blockers may arise days later, but they are always collected and notified as a batch. Completion gets one final report with actual receipts and remaining partial work.

CAPTCHA is an exception to asynchronous convenience, not a reason to store a code. Persist the site/item and "human needed" reason, release browser control, and request fresh observation when the owner returns. An expired challenge or OTP is reacquired; no automatic resubmission from stale state. Carry the simple-challenge attempt budget by mission+site+challenge identity across slices, so continuation cannot reset the two-attempt cap. Keep immediate handover for complex challenges, no solver/evasion shortcut, and current origin/secret protections. Extending the current owner-started handover admission is another reviewed prerequisite; timers must not spoof it.

## Budget, pacing and progress enforcement

Enforce these in code at admission and at every actual call/effect, not in a system prompt:

| Control | Enforcement |
| --- | --- |
| Mission total model money/tokens, expiry, optional elapsed-active-time and continuation caps | Add mission context to CallGuard for executor/router/fallback/memory/embedding/children. Intersect installation, bot and mission limits. Recheck deadline/revocation on each tool and call. |
| Concurrent calls and outstanding estimates | Persist reservations before external invocation under a transaction. Settle with usage through an idempotent ledger link; after crash keep unknown calls conservatively charged until reconciled, never simply release them. |
| Unknown price or missing usage | Automated USD-bounded execution pauses on an unpriced remote model unless owner explicitly selected a token-bounded alternative; zero-priced local models are identified explicitly, still token/step bounded. Missing usage retains the conservative reservation. Do not change all interactive unknown-price behavior as a side effect of this feature. |
| Estimate versus hard bill ceiling | Use the actual constructed prompt's conservative token bound plus enforced output cap and known price. If a provider cannot enforce/report a bound, describe money limits as conservative admission controls with possible final-call overrun; never promise exact billing caps. Token/time bounds remain compulsory. |
| Outbound actions, sites/accounts, recipients, shared identities | Transactional reservation at the common effect boundary, across missions using the same account/origin; existing guards still execute. Budget cannot be reset by new runs, new children, fallback model or another mission. |
| Rate / cooldown / working hours | Per-account and per-origin persisted next-eligible times/token buckets; respect provider Retry-After and site refusal. Queue a timer; no repeated model polling. Due timers coalesce after downtime. |
| Owner overrides | Preserve existing soft warnings and explicit "send anyway" decisions. A model cannot invoke the override. A mission's explicitly selected hard envelope needs an owner revision; warnings and hard limits are different UI/API fields. |
| Stalls | Across slices count repeated equivalent attempts and slices with no durable evidence/item advancement. Proposed default: 3 no-progress slices or 5 repeats of one logical action pauses with one digest. Waiting for time/source is not a stalled slice. Only owner or real new relevant evidence resets a stall gate. |

Per-run 40-step exhaustion with unfinished eligible work yields a structured `continue` only if checkpoint, authority, budget, no-progress checks and pacing permit. Token/cost/deadline/denial/repeat-stop exhaustion are distinct outcomes, not generic permission to make another run. Preserve a capped continuation count and active-time budget even for a free local model. A final model answer with pending work yields `incomplete`, never automatic mission success.

## Side effects, safety and outreach

The hardest crash is "server accepted it, client died before saving the answer." Before an irreversible operation persist a stable logical effect id and `prepared` intent; transition to `dispatching` before network I/O, then `confirmed` with a receipt or `unknown` on ambiguity. Link to the existing step; do not reexecute it through a new model tool call merely because its run id changed. Operation identity is assigned from mission/work item/action generation, not a fresh model UUID per retry.

Use a provider idempotency key only where its real contract supports it. For SMTP, a stable Message-ID is useful evidence but does **not** guarantee deduplication; absence in Sent does not prove non-delivery. `unknown` sends require positive reconciliation or an explicit owner decision acknowledging duplicate risk. Account creation reconciles with a read-only sign-in/account check and expected verification thread; a vault entry only proves password preparation. Intent without dispatch is retryable; dispatch without receipt is not. Never automatically "compensate" by deleting a remote account or emailing a correction.

All permission, redaction, sandbox/path, egress and owner-only route checks remain. Denied work is recorded and skipped/blocked, not retried through shell, MCP, another browser profile or a helper to evade the gate. Imported recipes and site/email/MCP text remain data. Model-produced acceptance evidence is a claim until checked against actual tools/receipts. Reject hidden-control text and validate/redact text before cards, logs, events and notifications. Use current origin checks again immediately before typing secrets. Persist only vault reference/account id, never secret values or OTPs.

For commercial outreach, the runtime needs an **eligibility record**, not just `consent_basis="yes"` written by a model: source/provenance, country and recipient type (unknown allowed only for research), basis evidence/ref and owner confirmation, sender identity, purpose, suppression status, last contact, and retention/deletion schedule. Personalization must be supported by sources; no fake prior relationship or fabricated findings. Workspaces hold scoped artifacts; third-party addresses/bodies must not be automatically extracted into general bot memory. Minimize data sent to remote model providers and offer the existing local-model path; provider/data-transfer suitability remains an owner deployment decision.

The following are product requirements for **unattended** outreach, not a silent rewrite of today's manual mail policy:

- Check current provider eligibility and suppression immediately before each send, including after an old batch approval. Opt-outs, complaints and hard bounces cancel queued follow-ups across all missions sharing the sender. An ambiguous opt-out reply conservatively stops that contact pending review. Never let a bot suppress unrelated contacts based on arbitrary email instructions.
- Owner overrides of today's soft guards remain manual, visible and audited. An overridden suppress-list warning does not become recurring automated consent. No unattended sends to known opt-outs or through a provider that prohibits the campaign; owner can handle a lawful exception outside the automatic queue.
- Require a clear sender identity, truthful subject/content, working opt-out route and configured required footer/address details. Recheck recipient-type/jurisdiction requirements; an owner approval and a public email address do not establish legal eligibility. Fail to drafts when evidence is unknown. Initial autonomous reply handling classifies/routes replies and drafts answers; accepting contracts or widening the campaign needs scope/approval.

Primary guidance checked on 2026-11-26 illustrates why one global "B2B=true" flag is insufficient: the [US FTC CAN-SPAM guide](https://www.ftc.gov/business-guidance/resources/can-spam-act-compliance-guide-business) requires truthful sender/subject information, sender address and opt-out handling; the [UK ICO B2B guidance](https://ico.org.uk/for-organisations/direct-marketing-and-privacy-and-electronic-communications/business-to-business-marketing/) distinguishes corporate from individual subscribers and still applies data-protection duties to personal data; the [Turkish Ministry's commercial-message FAQ](https://usak.ticaret.gov.tr/sikca-sorulan-sorular/ticari-elektronik-ileti-ile-ilgili-sorular) describes consent/ret obligations. These are inputs to a jurisdiction-specific policy review, not a claim of worldwide legal compliance or a completed GDPR/KVKK/IYS integration.

**A current documentation mismatch:** the existing outreach playbook describes AgentMail's shared-domain restriction. [AgentMail's current primary guidance](https://www.agentmail.to/docs/knowledge-base/cold-outreach) says it does not support cold outreach, including scraped/purchased prospects and automated sales sequences; a custom domain is not presented as an exception. Do not route this mission through AgentMail on the assumption that buying a domain fixes eligibility. Account verification/transactional mail is a different use case. Other providers must be checked on their own terms. This discrepancy is noticed here; changing the playbook/provider code is outside this docs-only PR.

Registration missions likewise check site terms/eligibility and use the authorized identity; no impersonation, ban evasion or duplicate registration after refusal. Accepting paid plans or obligations is not implied by "get backlinks." A CAPTCHA, identity verification or payment can legitimately require one digest/handover; the bot continues other sites in the meantime.

## Failure handling and operations

| Failure | Deterministic response |
| --- | --- |
| Model/provider transient error | Existing bounded provider retries plus a durable retry deadline if still useful; count all calls. Cap attempts and show real errors, not invented explanations. |
| Restart before run starts | Reconcile the prepared queued run by id; do not allocate another. |
| Restart during tool/send/registration | Existing Interrupted step plus unknown effect; reconcile before retry, retain reservation and root-family ownership until settled. |
| Read-only interrupted step | Reuse existing safe resume rules, then fresh observation before effects. Never blindly trust a previous browser ref or login session. |
| Wait deadline expires | One timeout event; item policy selects retry, skip or owner digest. Deadline generation prevents an obsolete timer winning after success. |
| Mail/webhook flood or cursor gap | Bound ingress and coalesce redundant facts; preserve receipt identities and safety signals. Record overflow/gap, resynchronize or pause affected work; no silent loss. |
| Owner unavailable / card expired | Keep durable request metadata and work on independent items. No automatic consent. Recreate exact approvals when the owner returns and gates still permit; short-lived override tokens are not persisted as authority. |
| PC/browser unavailable | Durable capability wait with deadline and backoff. Always-on host availability and PC availability are separate. No background switch onto the owner's PC by spoofing initiation. |
| Storage full/corrupt or authority unavailable | Stop before new calls/effects, show a host health reason. Do not operate from an in-memory mission copy whose intent cannot be committed. |
| Host ownership conflict | Close admission; quiesce in-flight families. Do not automatically activate a stale imported mission on another host. |

A startup reconciler scans active missions, root claims, queued runs, unresolved effects, reservations, pending owner decisions and due waits. It is idempotent and bounded; a watchdog rechecks stuck coordinator records without invoking a model. Expose last checkpoint, last useful progress, next known wake, pending owner dependencies and host health. If no wake is scheduled, say "waiting for event" rather than inventing a time. No telemetry leaves the installation.

## UI, API and storage boundaries

Start in the existing chat and Needs You surfaces: a persistent mission card with objective, verified progress, derived phase, waiting reason, next wake, budget and Pause/Stop. Details expand into work items/artifacts, one owner digest and the linked run timeline. Schedules/trigger editor links show which mission a binding advances; changing a source cannot change mission scope. Keep large plans and verbose tool metadata out of the primary card.

Use the existing premium visual language: theme tokens for Obsidian/Cobalt/Raven/Clay and every other supported theme, Appearance-driven radius tokens, lucide icons, accessible status text rather than color alone, and real translations in en/tr/de/es **and all other locales required by the current repository**. No UI is added by this proposal; implementation gates include `check:themes` and `check:i18n`.

Proposed API (none exists yet):

```text
POST /v1/bots/{bot}/missions                    create intake; idempotency key
GET  /v1/missions?bot=&state=&cursor=             scoped, paginated summaries
GET  /v1/missions/{id}                           derived detail + revision
GET  /v1/missions/{id}/events?after=&limit=        bounded replay
POST /v1/missions/{id}/intake-answer             one revisioned batch
POST /v1/missions/{id}/owner-decisions           exact selected decisions
POST /v1/missions/{id}/pause|resume|cancel        owner lifecycle actions
PUT  /v1/missions/{id}/scope                     owner revision + audit
```

All management, lifecycle, scope and decision writes are owner-only and classified in `daemon-api/owner.rs`, including method aliases/HEAD treatment for sensitive reads. Reads use current owner/bot scoping and redact source payloads; no broad peer access by default. Tools use run-bound capabilities, not public management routes. Optimistic revision checks and idempotency keys make owner retries safe. Public webhook ingress stays on the existing signed endpoint, with an owner-created destination binding; never expose a generic unauthenticated "wake any mission" endpoint.

Event types include `mission.created`, `item.changed`, `wait.registered`, `wake.accepted`, `slice.started`, `slice.ended`, `owner_digest.updated`, `effect.unknown`, `mission.paused` and `mission.completed`. Events carry ids/counts/status, not raw mail or secret-bearing arguments. SSE uses existing event infrastructure; reconnect snapshots plus sequence replay must not duplicate decisions or notifications. Generated OpenAPI and UI types are regenerated in the implementation PR, and new error codes receive localized human titles/hints.

Use additive forward-only migrations, preserve existing runs, and back up mission tables with application storage. Source outbox migrations belong to scheduler's independent migration list. Backups/imports include authority revocation metadata and unresolved effects, but imported missions start paused and require owner host activation; copying a DB must not immediately resend work. Retention compacts bulky source data while preserving small effect receipts, unresolved records, decision hashes and deduplication tombstones needed to prevent repeats. Deleting artifacts must not remove the fact that an action was sent. Never place authoritative mission tables under a tool-writable workspace.

## Deterministic acceptance harness

Build this alongside implementation, not as a benchmark against a real LLM. Reuse the repository's scripted provider/agent eval style and scheduler's injectable clock. Every scenario runs with a temporary real SQLite store and production permission/CallGuard/coordinator code. The scripted model asserts the brief's scope, cursor, wake ids and taint, emits predetermined tool calls and may deliberately lie or disobey. Advance fake time explicitly; no real sleeps, paid credentials or external network.

Use loopback fake sites with fixtures for registration, delayed confirmation, duplicate POST, 429/Retry-After, CAPTCHA, stale selectors and a malicious page. A test-only host/transport fixture may target loopback; never relax production SSRF restrictions. A fake browser driver is sufficient for coordinator cases; a small CDP fixture test covers real navigation/secret/handover boundaries. Fake mail implements the connector trait plus a loopback IMAP/SMTP fixture for UIDVALIDITY, delayed mail, replies, bounces and "SMTP accepted DATA, connection lost before response." Record actual fake remote effects, independently of what the model claims.

Inject crashes at named boundaries (before/after intent, dispatch, remote acceptance, receipt, wait registration, inbox insert/outbox ack, prepared-run commit/start and usage settlement). Restart with a fresh coordinator and the same database. An external-effect test journal survives that restart so assertions do not mistake lost test memory for no send. Exercise only meaningful boundaries; avoid an exhaustive role/provider matrix.

Required focused scenarios:

1. **Long general mission:** 20 independent items exceed a run guard, two context rollovers and two restarts. Verify durable completion once per item, bounded slice history, and no "continue?" card. Run a non-mail document/research variant to prove the core is not a campaign engine.
2. **All wakes converge:** deliver cron, duplicate signed hook, mail, timer and child completion together. Assert one root controller, retained events, one logical effect, and no authority increase. Terminal mission ignores stale wakes.
3. **Early/late verification:** mail arrives before wait registration, then a timeout races its delivery; each resolves once. Change UIDVALIDITY and overflow a mailbox window; assert catch-up/gap visibility rather than silent success.
4. **Unknown send/registration:** remote accepts then process dies. Assert no automatic repeat after restart or in a new slice, stable operation id, and honest uncertain UI. Missing Sent entry does not release the barrier.
5. **Owner experience:** three intake unknowns create one batch. Later ten blocked items update one digest; other items finish. Quiet hours, notification retry, partial answers and expired approval all retain their exact dependencies and do not imply consent.
6. **Budgets and pacing:** children/fallback/router contend for the final reservation; restart during a call; use an unpriced model; hit account cooldown across two missions. Assert no unreserved invocation, no reset on continuation, and one timer instead of model polling. Check owner-only override and independent installation/bot limits.
7. **Injection and revocation:** page/mail/helper asks to change identity, increase budget, send secrets, create a new mission or forge a checkpoint. Use a disobedient scripted model. Production gates deny/ask correctly, copied summaries retain taint, and revoked scope defeats previously approved but unconsumed actions.
8. **Handover/stop:** CAPTCHA attempts cannot reset across slices/children; OTP never enters provider requests/events/database. Stop during dispatch closes admission before any next tool; an already accepted action remains honestly uncertain. Restart cannot treat an expired handover as done.
9. **Completion honesty and stalls:** model says "all done" with open/unknown items, or makes the same plan across slices. Assert evidence validation, stall pause, one digest and no infinite free-model loop.

Release assertions: fake remote effect counts match confirmed ledger receipts; one open root family per mission; every provider call has a reservation and attribution; no zero-time self-wake loop; every unresolved item has a dependency, next eligible time or actionable reason. Owner hand-testing on a consenting site/mailbox remains necessary: scripted models prove protocol invariants, not real-model judgment, deliverability or cross-site robustness.

## PR-sized waves and a two-week useful version

Each row is independently reviewable. Durability/security foundations precede wider autonomy; estimates assume existing tools/host are already usable and represent engineering days, not a guarantee.

| Wave / job | Scope and dependencies | Acceptance / approximate effort |
| --- | --- | --- |
| A1: mission records | Storage tables, lifecycle reducer, event/projection transactions, owner-scoped read API. No automatic execution. | Restart preserves work/dependencies; invalid transitions rejected. 1 day. |
| A2: prepared slices | Prepared-start API, unique root claim, structured slice outcomes, startup reconciliation. Depends A1. | Concurrent wakes and crash-before-start cannot create two roots; existing runtime recovery unchanged. 1–2 days. |
| A3: mission brief | Validated item checkpoints, evidence refs, taint-preserving brief and bounded continuation. Depends A2. | Over-40-call scripted task survives context rollover; lying completion/stall tests. 1 day. |
| A4: wait/inbox bridge | Durable timers, source inbox/outbox, mail/webhook/time/file adapters; reuse #869 when available, otherwise existing adapters. Depends A2. | Early-mail/lost-ack/restart tests; no first-baseline loss. 1–2 days. |
| A5: resource controls | Mission attribution and durable reservations at CallGuard/effect boundary; account pacing and unknown-effect reconciliation. Depends A2. | Crash-after-accept and budget race tests. 1–2 days. |
| A6: intake/digest/UI | One batch intake and later dependency digest, chat/Needs You card, linked timelines, owner-only APIs, generated schemas and translations/themes. Depends A1–A3; integrates A4/A5. | Owner answers once, sees true waits/results, and can reliably stop. 1–2 days. |
| B1: reviewed approval continuity | Explicit continuation authority, exact-action batch approvals, invalidation/revocation and Full/source eligibility review. Depends A2/A5/A6. | No forged Human, no floor/egress bypass; exact changed-action test. Separate security review; do not promise it inside the initial date. |
| B2: mission-aware accounts/handover | Vault refs and remote account reconciliation; challenge budget across slices; durable handover metadata with fresh codes/observations. Depends A4/A5/B1 for wider autonomy. | Consenting multi-site signup with verification and safe human takeover. |
| B3: eligible outreach | Eligibility evidence, reviewed sender/provider policy, suppression across missions, draft batch queue, receipt reconciliation and reply triage. Depends A4/A5/B1. | Approved exact messages paced once; opt-out cancels queued follow-ups. No general self-authorized sending. |
| C: advanced work | Detached children, new trigger adapters, reusable goal recipes, safely scoped standing orders, host transfer/fencing. | Separate designs where they change trust/ownership; no new framework needed for each recipe. |

**Cut to ship within about two weeks:** ship A1–A6 as one-host, sequential-root missions for research, workspace artifacts and drafts, durable timer/mail/webhook waits, one intake/digest, visible budgets, safe restart and Stop. A general 20-item research/drafting mission should finish across slices while the owner is away. Keep ordinary synchronous read-only children optional; they are not necessary to prove the backbone. Do not wait for a visual plan editor, a new sidebar, detached agents, automatic host migration, an open-ended recipe language, autonomous tool installation, mailbox universal catch-up, or new CAPTCHA solving.

Also cut unreviewed unattended account submission and newly personalized cold-mail sending. Existing approvals still apply and can be batched only after B1 lands. In the first version, produce useful artifacts and a single consolidated decision request; do not advertise that both original examples are already fully unattended. A manually started, already approved action can use durable follow-up checks where current source/permission gates allow. If A5's ambiguity handling or A2's single-root protocol is late, cut the side-effect surface further, not crash correctness. If #869 is unmerged, reuse current watcher/hook adapters and keep its future UI integration separate.

## The five hardest problems and proposed answers

1. **An external effect can succeed without a local receipt.** No universal exactly-once fix exists for SMTP or arbitrary sites. Persist logical intents before dispatch, use real provider idempotency where available, reconcile with positive evidence, and quarantine uncertainty across all later runs. Accept an honest owner decision rather than a duplicate action.
2. **Authority must survive time without washing away taint.** Store owner scope and wake provenance separately, intersect with live gates at every effect, preserve fail-closed labels/hashes through briefs, and bind batched decisions to exact inputs and source generations. Broad standing orders are deferred; existing floors are not silently weakened.
3. **Several sources and restarts compete to continue the same work.** One application transaction owns prepared-run creation and the root slot; source outbox/inbox handoff is idempotent; generation checks plus a real single-process host lock fence local executors. Cut automatic multi-host takeover until external fencing exists.
4. **A compressed plan can drift while the model confidently repeats or finishes early.** Typed items, immutable acceptance criteria, effect/evidence links and daemon-built wake briefs are authoritative. Summaries are replaceable notes. Measure durable progress across slices and stop stalls; require evidence for final success.
5. **Hours of autonomy can spend money, over-contact people, and still nag the owner.** Put durable reservations and shared-identity pacing below all calls, turn waits into events, enforce eligibility/suppression before sends, and route every nonurgent question/approval through one dependency-aware digest. Keep soft owner overrides explicit and bounded; never let the model approve itself.

The deliberate tradeoff is slower expansion of unattended side effects in exchange for a backbone that can explain where it is, what it actually did, and why it is safe to continue.
