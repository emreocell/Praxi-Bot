# Embedding provider layer (`embedding-hub`)

One provider-agnostic layer for text embeddings. Consumers: the decision router
(K2, wired), memory recall (c611, to consume this API) and the knowledge index
(keeps its own per-source approval flow today; adoption is future work).

## Stable API for consumers

```rust
use embedding_hub::{EmbeddingHub, EmbedItem, Purpose, Task, VectorSpace};

// Agent-wide hub: `agent.providers().embedding_hub()` (None only in test hosts).
let hub = providers.embedding_hub().unwrap();

// Which space would the next call use? Compare with the space your store holds.
let space: VectorSpace = hub.preferred_space(Purpose::Memory);

// Embed. `pin = Some(&space)` guarantees the answer is in exactly that space
// (Err(Unserved) otherwise); `None` accepts whatever the chain gives and tells
// you the space in `Embedded::space`.
let out = hub
    .embed(Purpose::Memory, &items, Task::Document, Some(bot_id), None, max_wait)
    .await?;
```

* `EmbeddingConfig` (`embeddings.json` in the data dir, no secrets): providers,
  per-purpose chains (`router`, `memory`, `knowledge`), `remote_denied_bots`.
  Default = no providers = offline hash embedder (384 dims), nothing leaves the PC.
* `EmbedItem { text, private }`: a batch containing a private item is served by
  non-egress backends only.
* `VectorSpace { provider, model, dimensions }` (`key()` = `provider/model@dim`):
  stores key their vectors by it and re-embed in the background when the preferred
  space changes (see "Not done" below for the memory store).
* Redaction: the hub redacts every text with the agent's live `SharedRedactor`
  before a remote call. No redactor installed = no remote call.
* Opt-in: a provider is only called when `allow_remote` is true (set by the
  owner-only `PUT /v1/embeddings`).

## Behaviour

* Chain: first usable provider wins. A provider rests after 401 (10 min), 429
  (Retry-After, default 60 s), network error (30 s), wrong vector size (10 min).
* Limiter: rolling windows for RPM, TPM, RPD at 90% of the catalog/override limit;
  waits up to `max_wait` (router 250 ms), otherwise degrades to the next provider.
  In memory only.
* Catalog: `crates/embedding-hub/src/embedding_catalog.json`, every entry has
  `source` and `last_reviewed`.
* Router adapter: `agent-core/src/embedding.rs` pins to the preferred space so a
  query vector is never compared with another provider's index vectors.

## Routes

`GET/PUT /v1/embeddings`, `POST /v1/embeddings/{id}/test`, `GET/PUT
/v1/decision-helper`. Writes and the test are in `owner.rs`; paired peers are
refused in `peer.rs`. API keys are write-only and stored as `embedding-<id>` in the
secret store; the client cannot choose another secret name (no exfiltration of other
secrets to a custom URL). A key may be copied from the chat provider `gemini` only.

## Decision helper

`gemini_decisions` already exists in the decision pool (`agent-router/src/decider`);
its wire shape was checked against Google's Decisions API docs on 2026-11-24
(`POST /v1beta/decisions`, `input`, `questions`, `answers`, `usage.total_*_tokens`).
Settings writes a single-backend `decider.toml` (rpm 180 = 90% of the 200 free tier)
and refuses to overwrite a hand-written file.
