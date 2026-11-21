# LLM call ledger

Each model invocation now records a purpose. This includes executor requests,
guard-closing requests, K3 routing, K2 embedding requests, background memory
extraction/consolidation, and child runs. The [decision pool](decision-pool.md)
uses the `decider` purpose for each backend attempt. Ordinary turns do not
invoke the pool yet.

| Purpose | What is counted |
| --- | --- |
| `executor` | Main chat model invocations, including retries and fallbacks |
| `guard_close` | The tools-disabled closing invocation after a loop guard fires |
| `router` | The K3 classifier |
| `embedding` | Each K2 query or candidate-refresh transport batch |
| `memory` | Each background extraction or consolidation invocation |
| `subagent` | A child run's model invocations, also included in its ancestors' summaries |
| `decider` | Individual decision-pool backend invocations, including fallbacks |

## Audit records

`llm.call_started` is committed before dispatch. If that write fails, the call is
not dispatched. `usage` completes the same opaque `call_id`. Its numeric fields
are `calls`, `step_n`, `tool_rounds`, `latency_ms`, `prompt_tokens`,
`completion_tokens`, `cached_tokens`, and optional `cost_usd`. The `tokens` object
is `null` when no usage was reported, including failed/timed-out calls and K2
embeddings. Unknown token counts do not mean zero tokens were billed.

A request that reports tokens before a later stream failure retains those
reported numbers. A provider invocation is counted once even if it emits several
usage snapshots; the latest snapshot wins. Retries made by the loop are separate
invocations. Transport retries hidden inside an unchanged provider adapter are
not separately observable here. CLI providers expose one session invocation, not
their internal model calls.

These rows contain no prompt, tool input/output, response text, or provider error
text. The model label and all other strings pass through the live production
redactor. No secret or separate usage database is introduced.

At completion, `run.usage_summary` contains one map from purpose to
`{calls, prompt_tokens, completion_tokens, cached_tokens, cost_usd}`. It is written
once, after that run's pending background memory/embedding work and descendants
finish. `AgentLoop::wait` still waits for the turn; `flush_background` additionally
waits for the background accounting. Paused runs have no final summary yet. No
model call is made to produce a summary.

Intent and outcome rows reconcile by `call_id`. After a crash, an unfinished
intent remains countable with unknown tokens. There is an unavoidable narrow gap
between persisting an intent and dispatching the invocation: an unfinished intent
is evidence of an attempted dispatch, not a provider billing receipt.

## API

`GET /v1/usage` retains `runs`, `total`, and `by_bot`, and adds `by_purpose` with the
same totals shape. Totals include calls with unknown tokens. Every invocation
appears in exactly one purpose group, so summing the groups reproduces the totals.
Only the current user's selected runs are included; `since_ms` still filters by
run creation time. Parent summaries are not added to the global totals (child
calls would otherwise be counted twice).

The API retains the previous current-price semantics: known token counts are
priced against the currently loaded catalog/configuration. Audit summaries keep
the price estimate captured when the call completed. Historical model steps and
mixed histories resumed after upgrading are still counted when they have no
corresponding ledger invocation; historical purposes can only be inferred from
the saved step/run metadata.

## Evals

JSONL schema version 2 adds `llm_calls_by_purpose: {purpose: count}`. The existing
`llm_calls` field continues to count model steps, preserving comparisons with old
runs. The purpose map counts actual observable invocations and includes retries,
router/background work, and descendants. Consequently its sum can exceed
`llm_calls`. Schema 1 files still deserialize with an empty purpose map.
`bot eval report` adds a separate purpose table with raw invocation totals across
recorded runs; missing historical breakdowns are shown as unknown, not zero.

No database migration, new route, UI component, Tauri command, provider price
change, or provider adapter change is required.
