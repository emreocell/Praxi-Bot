# Optional model tiering

Model tiering lets a bot use a configured light model for a small class of
simple first turns. Add `light` to the bot's model configuration:

```toml
[models]
executor = "provider/your-main-model"
light = "provider/your-light-model"
```

Replace both references with models offered by your configured providers. No
provider or price is assumed. Choose a light model that supports your tools.
Both references must use the `provider/model` form; a `cli:` light model is
rejected. Remove `light` to restore the existing executor-only selection.
Setup presets leave tiering disabled.

## How the first model is chosen

Tier selection does not make another model request. This version uses a
deterministic rule after the existing routing ladder finishes: a user message
of at most 200 Unicode characters qualifies when the ladder returns a nonempty
tool hint and every hinted tool is visible and has `Effect::Read`. It also
accepts the exact greetings `merhaba`, `selam`, `hello`, and `hi`, ignoring case
and surrounding whitespace, when the route is `Agent` or routing is disabled.
Appending other instructions to a greeting does not qualify.

Other requests use the executor. The selection is stored as `tier` in the
first user step; `route_decided` carries the same optional field. Bots without
`light` keep the previous event and step shape, without a new `tier` field.
If the router reaches its existing time budget, the deterministic rule is
applied to its fallback route without another classifier request or timeout.

The optional [decision pool](decision-pool.md) is available for future routing
integrations. The tier selector continues to use the deterministic rule.

## When the executor takes over

The model remains light until one of these conditions applies:

- The light provider reports an unsupported capability. Two consecutive
  malformed provider responses also trigger escalation after one repair
  attempt. Existing retryable-provider fallback remains available.
- Two consecutive invalid tool calls are rejected by host validation, such
  as invalid JSON or an unknown tool name.
- The existing repeated-call warning is reached, or the next model call is
  the fourth model step.
- A light response asks for a non-read tool or a tool requiring approval.
  The entire light response is discarded before any tool is dispatched or
  approval card is created. The executor receives the preceding history and
  decides again. This check resolves the real target of `use_tool` too.
- A tool has actually executed, or a user steers the active turn. Tool errors
  after execution count as exposure too. A guard's final closing response
  always uses the executor.

The existing permission engine still decides whether the executor's resulting
tool call is allowed, denied, or needs confirmation. A model-tier decision
never grants permission. The tool preparation path rechecks the escalation
condition in case policy changed after the light response was generated.

Escalation emits `model_fallback` and records a redacted
`model.tier_escalated` audit fact before the executor request. If the state
cannot be read or a required fact cannot be persisted, the run fails before
the next model request. It is also the durable
latch on resume, including a light-provider failure followed by an interrupted
executor request inside one model step. A run never returns to its configured
light model after escalation, including through the fallback list. Existing
usage rows include the actual model and, when tiering is enabled, its tier.

Each completed model step also stores its actual tier and model. Pending tool
calls retain that producer's authority across restart: removing or replacing
the light model, or disabling tiering with a CLI fallback, cannot turn an old
light decision into an executor decision. Such a configuration change discards
pending light calls and asks the executor again. A response already produced by
the executor through fallback remains a standard response.

## Conservative context limits

The base branch has no general persisted run-taint gate. To avoid sending
untrusted results back to a light model, this implementation treats **every
executed tool output as requiring the executor**, without interpreting the
returned text. A fresh turn with any earlier chat message or enabled memory
recall starts at standard as well. The only exception to tool exposure is a
host validation or policy-denial result where no tool ran.

Only human-initiated top-level turns may start light. Scheduled and channel
turns start standard. Subagents retain their existing model-role selection.
Bots with a CLI executor, CLI fallback, or implicit CLI default provider do
not use tiering, because those providers manage a complete session.

These limits can be narrowed once the pending exposure and pooled-decider
features are integrated. They reduce the expected savings for read-heavy
work and ongoing conversations. Escalation can add a model call when a light
decision is discarded; tiering does not guarantee a lower call count.

## Validation and cost interpretation

The in-process fake suite exercises the enabled path, approvals, discovery,
read exposure, invalid calls, one-way recovery, and audit failures. Its mixed
20-task regression compares greetings, hinted reads, complex tasks, and
unsafe light proposals with an executor-only baseline. The gate requires all
20 scripted outcomes to pass on both sides and at least a 20% reduction in
assumed cost. It uses artificial prices and fixed token counts; it measures
routing/control behavior, not live model quality, live token consumption, or
real provider pricing. Tiering remains opt-in until you evaluate the selected
models on your own representative workload.

The checked fixture run used 28 calls without tiering and 32 with tiering:

| Fixture group | Tasks | Executor-only calls | Tiered calls |
| --- | ---: | ---: | ---: |
| Exact greetings | 8 | 8 | 8 |
| Hinted reads, including the shared routing calls | 4 | 12 | 12 |
| Complex requests starting at standard | 4 | 4 | 4 |
| Unsafe light write proposals discarded before execution | 4 | 4 | 8 |
| Total | 20 | 28 | 32 |

The 12 eligible happy-path fixtures therefore use 20 calls on either side
(16 main model calls plus four existing routing calls). The four additional
calls come from the four safety escalations: each discarded light response is
followed by an executor decision. Total invocations increase by 14.29%, despite
there being no extra tier-classifier request. Provider-retry regressions are
tested separately and are not included in these 20 fixtures. With the fixture's
artificial prices, estimated cost is $0.000366 versus $0.000234, a 36.07%
reduction, with all 20 scripted outcomes succeeding on each side.
