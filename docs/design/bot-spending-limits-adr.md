# ADR-0034: per-bot spending limits

Status: accepted. User guide: [bot spending limits](../guide/bot-spending-limits.md).

## Decisions

- **Defaults.** A new bot gets a daily hard cap of 5 USD and 10M tokens.
  Existing bots are migrated with `{}` (still unlimited, migration `0020`);
  a bot with no stored row gets the new-bot default. Limits per period (day, week, month): soft (notify at 80% and
  100%) and hard (stop), in USD and in tokens.
- **Enforcement point.** In `agent-core`, before every model call
  (`CallGuard::start`), for every run of the bot (chat, schedules, channels,
  subagents, background memory work). Usage comes from the LLM call ledger, so
  it survives restarts; an in-process reservation covers parallel runs.
  Unreadable limits fail closed. A blocked call ends the run with a
  `bot_limit_reached` card (raise / wait / stop).
- **USD caps and unknown prices.** A USD limit cannot count a model without a
  known price; the token cap does. This is documented in the guide.
- **Owner only.** Changing limits is local-owner only: refused for token-only
  callers and MCP (`OWNER_ROUTES`, `require_owner` in the handler) and for
  every paired peer role (`host_local_denial`), so nothing that a bot or a
  remote device controls can lift its own cap. Every change is audited as
  `bot.spend_limits_changed` with the old and new values. Reading limits stays
  allowed.

## Reserve, then reconcile, and the stream cut

- **Admission.** Before a call, the guard reserves an estimate (the larger of
  the run's last prompt or a default, and a conservative size of the request
  itself, plus the completion cap) in `BotLimitState`. Parallel calls are
  admitted against ledger usage plus everything reserved, so a big first prompt
  or several calls at once cannot slip under a cap.
- **Reconcile.** When the call ends the reservation is released
  (`release`); the real usage is then written to the LLM call ledger, which is
  the durable total. A cut call is recorded with the larger estimate.
- **Stream cut.** While a reply streams, `stream_progress` (text plus tool-call
  argument bytes) re-checks the hard limits and ends the call when what it has
  produced would break one. Growth beyond the admission reservation is added
  to the in-flight total. CLI sessions (`cli:` models) are stopped the same way.

## Consequences

- A hard cap can be exceeded only by the part of a reply that arrives between
  two progress checks; soft limits only notify.
- Estimates are conservative on purpose: a call can be refused slightly early
  rather than overspend.

## Alternatives rejected

- Counting only after the call: parallel runs and one long reply overshoot.
- Enforcing in the UI or per channel: runs started elsewhere would bypass it.
- Letting a peer or a bot raise its own cap: see "Owner only".

Links: PRs #524 (limits), #621 (size-aware estimate, stream cut), #372
(reservation, fail closed). Code: `crates/agent-core/src/bot_limits.rs`,
`crates/agent-core/src/exec.rs`.
