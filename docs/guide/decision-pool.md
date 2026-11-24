# Optional decision pool

The decision pool is an opt-in facility for asking small, structured questions
about a user message. The existing K0–K3 router and
[model tiering](model-tiering.md) still choose how ordinary turns run. This
release prepares the pool but does not invoke it from ordinary turns.

## Quick switch for Gemini

If you only want the Gemini helper, open **Settings > AI models > Smart search & helper > Decision helper** and turn it on. It needs a Gemini key and writes a
`decider.toml` with one `gemini_decisions` backend for `gemini/gemini-decision-1`
at 180 requests per minute (90% of the free tier's 200; the free tier also allows
50,000 tokens per minute and 10,000 requests per day, last checked 2026-11-24).
Settings never overwrites a `decider.toml` you wrote yourself. See
[Helpers (optional)](embeddings.md).

## Configure the pool

Create `decider.toml` in the daemon's data directory, next to `providers.toml`.
Replace the model references with models available through your enabled
providers.

```toml
enabled = true
timeout_ms = 1500
max_calls_per_turn = 2

[[backends]]
api = "gemini_decisions"
model = "gemini/your-decision-model"
rpm = 20

[[backends]]
api = "chat"
model = "provider/your-small-json-chat-model"
rpm = 10
```

| Setting | Meaning |
| --- | --- |
| `enabled` | Defaults to `false`. When enabled, supply one to eight backends in priority order. |
| `timeout_ms` | One deadline for the entire chain. Defaults to 1,500 and accepts 1–1,500. |
| `max_calls_per_turn` | Defaults to 2 and accepts 1–8. Each attempt consumes a call, including failures and timeouts. |
| `backends[].api` | `chat` (default) uses the selected provider's JSON chat mode. `gemini_decisions` and `openrouter_decisions` use the respective built-in provider and its native Decisions endpoint. |
| `backends[].model` | Required full model reference. For OpenRouter include the upstream vendor, as in `openrouter/vendor/model`. A model may appear once in the chain. |
| `backends[].rpm` | Required rolling requests-per-minute limit for this backend, 1–10,000. A native HTTP 429 pauses that backend for one minute. |

Choose a small JSON-capable chat model from the
[provider catalog or provider's model list](providers-and-models.md). For native
Decisions, use a model listed by
[Gemini Decisions](https://aistudio.google.com/docs/decisions-api) or the
[OpenRouter Decisions catalog](https://openrouter.ai/models?categories=decisions).
The repository's curated catalog describes chat endpoints. OpenRouter's
[native Decisions API](https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request)
is alpha and has a separate model list. The pool supplies no
default model ID and discovers no free models automatically. Check the
provider's price, quota, and data-handling terms for the model you configure,
including whether a free tier lets the provider use prompts for training.

Configure credentials using [the usual provider setup](providers-and-models.md).
The pool uses the configured provider's secret reference or catalog environment
variable. Restart the daemon or reload providers to apply changes. RPM windows
are in memory and reset when the provider configuration is reloaded. An
unavailable backend is skipped. Invalid configuration disables the pool.
`bot doctor` reports the settings without probing the models.

## Input, answers, and accounting

The host only supplies the original top-level human message and trusted question
metadata. It runs both through the live redactor before dispatch. History,
attachments, and tool results stay out of the request. A question can return a
probability clamped to 0–1 or choose one of 2–255 offered IDs. Free text, unknown
IDs, and refusals are dropped. Answers carry no authority over tools,
permissions, approvals, or tool arguments. Unanswered questions leave the
caller's deterministic path available.

The host adapter records every dispatched attempt under `decider` in the
[LLM call ledger](llm-call-ledger.md). Reported token counts are used when
available. A failure to start the ledger entry prevents dispatch.
