# Memory baseline before first-class knowledge retrieval

> **Historical snapshot.** The statements below describe commit `a5a4f51b`. Since then
> `bot serve` opens `memory.db` and enables recall and background extraction for every
> bot (not in safe mode), the memory tools (`memory_search`, `session_search`,
> `memory_save`, `memory_forget`) are registered, and each bot has an owner-only
> Memory setting (On / Ask before saving / Off), a recall budget and a "Needs your OK"
> review list for untrusted-origin notes. For what ships today read
> [the Memory guide](guide/memory.md).

Inspected commit: `a5a4f51bff99a75f566651a64c1ad240b9a0158c`. This records
source behavior at that commit, rather than capabilities inferred from names or
design intent. Source references below use paths and baseline line numbers.

## What is actually connected

The normal CLI daemon builds `AgentLoop` with a `LoopConfig` containing only
`redactor`, `max_computer_runs`, and remaining defaults
(`apps/cli/src/serve.rs:237–247`). `LoopConfig::default()` sets `memory: None`
(`crates/agent-core/src/config.rs:324–354`). Peer and host constructors also use
the default configuration (`apps/cli/src/peer.rs:104`, `host.rs:221`). A repository
search found `MemoryConfig` constructed only in agent-core tests, not shipped
application initialization. Therefore **the deployed default adds zero learned
memory tokens and makes zero memory extraction calls**.

There are no registered memory-management, memory-search, or session-search tools
in `builtin-tools` or `tool-registry`. `BuiltinConfig` has no memory store field;
its construction/registration covers browser, desktop, files, shell, script,
meta, email and web tools (`crates/builtin-tools/src/lib.rs:64–184`). The memory
store's management and session APIs are useful library capabilities, not an
end-to-end shipped memory feature.

## Persistent models

`storage` owns `<data_dir>/bot.db`, SQLite WAL with foreign keys, and migrations
1–3: users, bots, chats, messages, runs, steps, approvals, append-only audit log,
event index, and subagent links. Messages contain UUID, user/chat/run identifiers,
role, text, and creation time. Bot ownership is available through
`Storage::get_bot`; `config_json` is free-form bot configuration. See
`crates/storage/migrations/0001_init.sql`, `0002_event_index.sql`,
`0003_subagents.sql`, and `src/records.rs:21–68`.

`memory-store` separately owns `<data_dir>/memory.db`, schema version 1, WAL and a
single mutex-protected connection (`src/store.rs:31–36,113–165`). It contains:

| Entity | Existing representation |
| --- | --- |
| Learned memory | UUID; user; optional bot; kind; content; supporting quote; source chat/message; trust; importance; creation/last-use/expiry times; soft-delete flag |
| Kind | `profile`, `bot`, `episodic`, `procedural`, `lesson` |
| Scope | `bot_id = NULL` means shared across the owner's bots; otherwise one bot |
| Trust | `user` or `untrusted`; provenance does not carry a source registry or document permission |
| Lexical index | `memories_fts`, FTS5 `unicode61 remove_diacritics 2`, indexed memory content |
| Vector index | `memories_vec`, sqlite-vec `vec0`; one embedding dimension recorded in `meta` |
| Journal | Every add/update/delete, full before/after JSON snapshots, reason, time and undo state |
| Session search | `session_msgs` + `session_fts`; message/user/chat/role/content/time, no bot column |

Source: `crates/memory-store/migrations/0001_memory.sql` and `src/types.rs`.
The stores use plain UUID strings across databases, with no cross-file foreign
keys. A memory delete removes its lexical/vector entries but retains its body in
the soft-deleted row and journal for undo (`src/store.rs:343–360`). That retention
model is unsuitable for the new requirement to delete a knowledge source and its
derived content.

## Read timing and precise token accounting

With a caller-provided, enabled `MemoryConfig`, top-level turns recall against
the initial user text. Recall is applied after the existing routing decision;
scheduled directives can bypass that path, and subagents do not receive recall.
Normal entry is `exec.rs:190–212`; empty-history recovery is `242–247`; the
enabled/root guard and recall call are `333–354`. Steering messages do not run
another recall (`274–275`).

The recalled block is prepended to the **user message, never the system prompt**.
The initial augmented user step is persisted and reconstructed for subsequent
model calls in the same run (`history.rs:153–158`). Thus the same recall block is
charged again for every model request/retry that includes it; there is no fresh
retrieval per model step. Raw chat messages remain unaugmented and are used for
future-turn history (`tests.rs:2938–2975`). Older augmented run steps are not
automatically copied into later chat history.

`MemoryConfig::new` defaults to an 800-token recall budget
(`config.rs:93–110`). This is **an estimate over selected raw content**, calculated
as `ceil(Unicode scalar count / 4) + 4` for each memory
(`memory-store/src/recall.rs:41–45,225–233`). It excludes section wrappers,
category labels, JSON quoting, HTML escaping, and the separating newline in the
rendered result. Consequently 800 is not a hard bound on final serialized output
or actual provider tokens. The selection stops when the first candidate does not
fit, instead of considering smaller later candidates.

For an enabled turn with `N` model requests, the added prompt cost is approximately
`N × tokens(rendered recall block)`; provider caching and retries affect billing.
It cannot be stated as a fixed token total from this code. The shipped default
remains zero. There is **no current full-memory system-prompt stuffing path**.
A comparison against stuffing a complete fixture corpus must be labelled a
counterfactual benchmark, not a measured improvement over today's default.

Chat history is separately bounded to 256 messages / 64 KiB
(`agent-core/src/history.rs:75–93`). That window is not the memory recall budget.

## Writes and additional model calls

After a successful run with final assistant text, the loop persists the redacted
answer and queues memory extraction (`agent.rs:1077–1095,1171–1182`). The excerpt
contains this run's user messages and final answer, not a directly concatenated
dump of tool outputs (`memory.rs:139–175`). The classifier model is used when
configured, otherwise the executor (`181–207`).

Each exchange requires one extraction model call. Every validated candidate is
searched against the store; a candidate with existing similar memories causes
another consolidation call, with up to five similar memories. There is no bound
on candidate count, excerpt size, consolidation prompt size or response text in
this path (`memory-store/src/pipeline.rs:125–160`; `agent-core/src/memory.rs:27–51`).
It collects only text deltas and does not account the background calls' usage
against ordinary run token/cost metrics. Thus enabled extraction can add `1 + C`
model calls, where `C` is the number of valid candidates having similar memories;
the code provides no fixed token bound for these background calls.

Validation requires nonempty content, a quote found in the excerpt, no detected
secret, and no question/hypothetical according to current heuristics. Profile and
episodic facts are automatically user-shared; bot/procedural/lesson facts are
bot-scoped (`pipeline.rs:220–255`). A user edit promotes provenance to `user`
(`manage.rs:41–56`). The merged concurrency fix checks current trust again inside
the update transaction, preventing a stale untrusted consolidation from replacing
a concurrent user correction (`store.rs:274–305`, `pipeline.rs:174–205`).

Runs initiated by nonhumans, workflows, subagents, CLI sessions or any non-Meta
tool/unknown tool are conservatively untrusted for extraction
(`agent-core/src/memory.rs:103–175`). Untrusted candidates cannot update user
memories or delete memories. Rendered untrusted memories are escaped JSON strings
in a separate data-labelled section (`memory-store/src/render.rs:17–54`). These
labels are useful prompting boundaries; they are not executable enforcement of
instructions found in documents, and they are not a complete egress mechanism.

## Existing retrieval and weaknesses

Hybrid search already exists: FTS5 BM25 plus sqlite-vec nearest-neighbor search,
RRF (`k=60`), then importance and recency boosts. Defaults take 50 candidates per
retriever and filter vector similarity below 0.25 (`recall.rs:14–38,95–245`).
Lexical scope/expiry filtering happens before LIMIT. The vector index is global
and cannot apply scope in this query: it overfetches, filters after retrieval,
and doubles `k` up to 4096 (`119–162`). More than 4096 closer foreign-scope vectors
can still prevent a relevant in-scope vector being considered.

The current `Embedder` trait is synchronous and mandatory at store open. Its only
required methods are dimension and text batch embedding (`embed.rs:7–17`). Every
`gather` embeds the query **before** executing lexical search (`recall.rs:95–103`),
so embedding failure prevents a lexical-only fallback. Adds/updates also embed
content synchronously. Dimension identity alone cannot distinguish models with
the same dimensions, and there is no model fingerprint, cache, quota ledger,
source-specific network consent, async batching, or document/query task role.

Available implementations are deterministic feature hashing (default 256
dimensions, lexical-like similarity, not semantic evidence), and optional
fastembed/ONNX multilingual-e5-small (384 dimensions). `fastembed` is off by
default, involves native/model downloads, and currently prefixes both stored text
and queries with `query:` (`embed.rs:20–38,88–145`, `Cargo.toml:13–16`).

Session indexing is caller-fed, accepts user/assistant messages only, and is
idempotent by message UUID. Session search spans all the user's chats and truncates
each result to 500 characters (`sessions.rs:29–127`). No shipped caller feeds or
exposes these APIs. There is no source catalog, file/document chunking,
incremental document hash, source deletion cascade, resumable indexing cursor,
location citation, or bounded knowledge tool yet.

## Evaluation baseline

`crates/evals` already runs hermetic scripted agent scenarios in normal CI and
has separate opt-in live browser/desktop evaluations. It has no retrieval corpus,
recall@k, MRR or real token-count measurement. Fake model usage defaults to fixed
10 prompt / 5 completion tokens or explicit scripted values
(`crates/evals/src/fake.rs:104–118`), so those fields cannot measure token savings.
Existing memory tests check basic fusion, scope, deletion and approximate budget
selection (`memory-store/tests/memory.rs:289–369`), rather than retrieved source
quality or complete rendered prompt size.
