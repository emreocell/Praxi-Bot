# Bot knowledge and retrieval

Status: implementation in three dependent `rag/*` PRs, all targeting `main`.
Baseline inspected: `a5a4f51bff99a75f566651a64c1ad240b9a0158c`.
Nothing in this design authorizes a merge or changes the agent-router workstream.

## Existing behavior and the problem to solve

[The baseline investigation](../discovery-current-memory.md) records exact source
anchors and read/write timing. `memory-store` already has learned memories,
FTS5, sqlite-vec, RRF, trust labels, undo journaling, session search and an
optional ONNX embedder. It does not have a document source catalog or a shipped
knowledge surface. Its synchronous embedder is mandatory even when only lexical
retrieval is wanted; a failed query embedding aborts lexical retrieval too.

A loop built with `memory: None` (safe mode, library use) adds
**zero learned-memory tokens and zero memory-extraction calls**. `bot serve`
(not in safe mode) opens `memory.db` and configures recall for every bot unless the
owner sets that bot's memory to Off (per-bot setting, see `docs/guide/memory.md`).
Legacy recall prepends a block to the first **user** message, once
per root turn, with an approximate 800-token content budget. It is included again
in later model calls in that run. Wrappers/escaping are not in that old budget.
Background extraction adds one model call plus a consolidation call for each
candidate with similar memories; those calls have no fixed prompt or candidate
budget and are not included in normal usage accounting. This proposal does not
enable that background extractor.

Other existing building blocks:

| Area | Existing behavior | Knowledge integration |
| --- | --- | --- |
| `storage` | Owned bots/chats/messages/runs and durable tool steps in `bot.db` | Resolve the authenticated bot and owner; select authorized conversation/email snapshots |
| `doc-tools` | Pure-Rust PDF extraction and ZIP/XML DOCX parsing; callers authorize paths | Read once through an authorized directory handle, then parse bounded bytes |
| `web-tools` | Bounded HTTP fetch and HTML-to-Markdown; heading, code and table preservation; untrusted output | Index saved HTML/text; adding a saved page makes no new network request |
| `skills` | Layered human-readable `SKILL.md` instruction library and bot-scoped prompt index | Keep the brain outside skills; retrieved notes never become skill instructions |
| `state-bundle` | Validates/snapshots three top-level SQLite databases; nested bot files were ordinary blobs | Explicitly snapshot and validate each knowledge database, omit WAL/SHM, revoke imported grants |
| API/UI | `rememberRun` saves a reusable workflow; no semantic-memory CRUD or knowledge settings | Bot-scoped sources/settings/search/indexing UI and generated API client |

PDF extraction has no OCR dependency: scanned/image-only PDFs can produce no
text. File citations use a `#extracted-text` locator and line ranges within the
indexed, extracted/redacted text, including Markdown and code. The live secret
redactor can replace multiline spans, so these ranges must not be presented as
original filesystem, PDF page or Word line numbers.

## Boundaries and storage

Knowledge is a module of `memory-store`, reusing the SQLite and ranking stack,
but a separate model/database from learned conversational facts. Learned-memory
soft deletion and undo snapshots cannot satisfy source erasure. Knowledge uses
hard source/document/chunk deletion and cascade cleanup of derived vectors.

Each physical index is `data_dir/bots/<canonical bot UUID>/knowledge.db`.
Its metadata permanently binds both `UserId` and `BotId`. Opening it with another
scope fails. The model cannot choose a bot ID, database path, owner or arbitrary
SQL in `knowledge_search`; execution supplies the bot from `ToolContext`, and the
host resolves its owner from `Storage`. Missing/invalid context fails closed.
All API routes independently check `bot_of_user` and source ownership.

Core entities:

| Entity | Stored fields / invariant |
| --- | --- |
| Source | Generated UUID, immutable kind/locator/provenance, label, enabled state, monotonic revision, optional explicit embedding grant, creation/update times |
| Document | Generated UUID, source UUID, stable source-relative locator, title, decoded format, content hash, mtime, revision |
| Chunk | Generated UUID, source/document UUIDs, ordinal, section ancestry, extracted line range, text and content hash |
| FTS | FTS5 `unicode61 remove_diacritics 2`; title/section/body weighted BM25; synchronized in the same transaction |
| Vector/cache (PR2) | Bot-local chunk/content identity plus full immutable embedding-profile fingerprint, validated normalized vector |
| Quota ledger (PR2) | Host-wide `knowledge-runtime.db`, keyed by provider/account group; no text, API keys or model output |

Source kinds are selected files/folders, saved web pages, conversations, email
snapshots, bot notes, and the shared brain. A shared source is explicitly attached
to each bot that may search it. There is no implicit all-bots index or fallback.
Private API credentials remain `SecretRef`s resolved through `secret-store`.

Portable knowledge indexes must have exactly the schema produced by the current
migrations, including triggers, foreign keys, indexes and FTS shadow tables.
Validation rejects additional or altered schema objects before import resets
source authority; an imported trigger cannot restore consent or suppress deletion.
Data and SQLite page layout may differ without changing the supported schema.
State-bundle import also attests the complete migrated `bot.db` schema before
sanitizing settings. This import-only check retains the canonical audit triggers
and rejects additional or altered objects, including a dormant trigger that
could restore an imported embedding configuration later. Ordinary runtime
storage initialization is unchanged.

## Source acquisition, incremental work and limits

The source catalog is a grant to a specific source, not a grant inferred from
document frontmatter. Files/folders are human-selected roots. Authorized handles
constrain reads below those roots; symlink/reparse-point escapes, device/network
paths and nonregular inputs are refused. Parsing receives the authorized bytes
instead of reopening an unchecked path. Saved web pages accept saved content and
a validated HTTP(S) locator; they do not crawl or refresh that URL. A conversation
snapshot requires owner **and** bot/chat equality. Email snapshots come from an
authorized read by this bot and are rechecked against the account's current
binding. Only a successful account binding change or removal invalidates related
sources; password rotation, unrelated edits and failed updates preserve them.

The daemon resolves its own data directory once at startup as a trusted root.
Source selection rejects that directory, its descendants, and any ancestor that
would include it. Only an explicit shared-brain attachment may read the exact
`data_dir/brain` directory. Credential roots (`.kube`, `.docker`, cloud credential
stores and browser profiles) and protected filenames remain excluded after
canonicalization, including Windows short-name aliases. No-follow handle checks
still apply below the selected root. Paths and labels are checked against live
registered secret values; normal long paths are not treated as base64 secrets.
Saved URLs also reject userinfo and secret-bearing query/fragment values.

Source mutations, embedding configuration and owner approval/reset routes reject
all paired peer roles, including Desktop, Phone, Node and Host. Generic bot
JSON/TOML editors reject peer changes to the decoded knowledge subtree while
preserving ordinary name/profile edits. The owner uses dedicated knowledge
settings so stale generic profile snapshots cannot restore saved consent.

The email selector only offers a successful, persisted `email_read` result with
an explicit account argument from an owned run of this same bot. Failed or
spilled results are excluded, the matching call/result is bounded before it is
copied from storage, and indexing rechecks current account ownership. A
conversation snapshot accepts at most 2,000 messages and 1 MiB of text; an
incomplete oversized conversation is refused. All source metadata and document
text are treated as private, untrusted data. Known credential files are refused;
credential-shaped text and the daemon's currently registered exact secret values
are redacted before indexing or embedding.

Markdown chunking preserves heading ancestry, paragraphs, fenced code and table
headers. Large blocks split on complete lines where possible; pathological long
lines still have a hard bound. Code is indexed as code, with line citations.
Each content hash includes normalized format/chunker version as well as text;
mtime is metadata, not proof that bytes are unchanged. A changed document replaces
its previous chunks atomically. An unchanged document preserves chunk identity.
Source and document mutations require the expected source revision. A stale job
cannot overwrite a concurrent source change or resurrect a deleted source.

Defaults are deliberately finite: 4 MiB decoded document input, 1,600 characters
per chunk, 4,096 chunks per document, 256 sources, 50,000 documents and 100,000
chunks per bot. Source adapters impose their own input/extraction/scan limits
before invoking parsers. Indexing publishes one document at a time, reports a
cursor for bounded folder pages, and can restart safely through hash comparison.
Individual folder files that fail reading, extraction or indexing are recorded
with safe failure codes; the cursor continues past them. Missing files are pruned
only once discovery reaches the end, including when other entries were skipped.
A partial scan must not delete unseen documents. A fatal initial scan rolls back
the new source. Oversized or incomplete extraction is explicitly refused rather
than saved as a complete-looking truncated document.

The shipped file adapter admits at most 4 MiB of input bytes and 1 MiB of
extracted UTF-8 text per document. Saved-content source requests allow a JSON
body of `6 × 4 MiB + 64 KiB`: the sixfold allowance covers JSON Unicode/control
escaping of the decoded byte budget, and the remainder bounds metadata overhead.
The decoded content still has its own 4 MiB limit. Complete PDF/DOCX extraction
must fit the 1 MiB text limit; interactive document-tool preview limits do not
silently shorten indexed content. Folder discovery is bounded by 4,096 directory
entries and a depth of 24; a single indexing page handles at most 32 files.
Hidden entries, dependency/build directories and known credential files are
excluded. An opened file must be regular, have one hard link and remain within
the byte limit. On Unix it is opened without blocking before type validation,
so replacement with a FIFO cannot leave an indexing worker waiting for a writer.

PDF knowledge ingestion uses a deliberately conservative subset, independently
of the interactive `pdf_extract` tool:

| Admission | Knowledge source limit |
| --- | --- |
| Decoded stream / page extraction stream | 1 MiB per stream |
| Stream markers and pages | 64 each; marker counting is conservative |
| Retained object accounting | 16 MiB, 25,000 nodes, nesting depth 64 |
| Cross-reference entries | 25,000 declared entries before allocation |
| Compressed object work | 64 MiB of decoded-length × member-count accounting |
| Fonts | 32 per page |
| Unicode character maps | 4,096 mappings per document, at most 8 UTF-16 code units per destination |
| Character-map decoding / scanning | 4 MiB / 8 MiB aggregate work |

Encrypted PDFs and indirect stream-length references are refused before parsing,
including escaped PDF names and comment-separated references. Compressed object
headers are checked before expansion, and Unicode map ranges are counted before
the library builds its reverse map. These checks prevent small files from
bypassing the output limit through stream/object or character-map expansion.
Uncommon or complex PDFs may need a simplified export. The parser remains
in-process: these are input, retained-object and work admission limits, not an
OS-enforced RSS/CPU ceiling or a claim about transient allocator overhead.

CPU-heavy parsing/indexing and source/configuration writes run off async executor
threads with one bounded mutation worker per trusted data directory. The permit stays with blocking work if its awaiting request is
cancelled. This avoids releasing capacity while a parser is still running.
Read-only source/settings metadata uses a separate bounded read queue. Lexical
search admits two concurrent blocking operations; indexing admits one.
An occupied indexing slot returns a busy response instead of accumulating an
unbounded queue of parser tasks.
No GPU, remote vector server, daemon-managed model download or telemetry is
required. These bounds are design constraints, not a benchmark claim for the
owner's ARM host; real throughput remains to be measured there.

## Retrieval and the agent loop

`knowledge_search` takes a plain query, optional source IDs within the current
bot, top-k and output budgets. FTS syntax is never accepted directly: query words
are quoted/normalized and joined as bounded alternatives. English/Turkish
stopwords are removed. Questions and multi-term searches require at least two
meaningful matching terms; an explicit single keyword remains supported. Term
coverage precedes BM25 within the bounded candidates. Empty, weakly matching or
unmatched questions return an empty result. Tool discovery checks for a live
enabled source, and read-only search never creates a missing index.

Default search returns at most five chunks with 600 **estimated** tokens and an
independent 8 KiB byte cap. Hard ceilings are 20 hits, 2,000 estimated tokens,
32 KiB response bytes and 256 lexical candidates. Budgeting serializes the
**complete** JSON response, including citations, escaping, truncation notices,
trust marker and the estimate field. It may shorten an excerpt or omit a hit
whose citation overhead alone cannot fit. Citation fields are source ID/label,
document locator, section and extracted line range. Approximate tokens are
labelled as estimates; they are not provider billing data.

PR2 adds bounded embedded vector retrieval and RRF. Vectors are scoped before
ranking; the old memory-store global top-N/post-filter limitation is not copied.
Vector storage is plain SQLite BLOBs with a bounded streaming cosine scan and
bounded top-k heap. This avoids another native ANN dependency and is appropriate
for a deliberately capped first implementation. A scan ceiling is explicit and
reported rather than pretending a capped scan searched the entire corpus.
The lexical index is authoritative and remains usable with no configured
embedder, absent consent, an exhausted quota, a timeout, malformed vectors or a
provider failure. RRF combines rank, not incomparable raw BM25/cosine scores.
A bounded local reranker may reorder the existing candidate set; a remote
reranker requires a separate egress authorization and is not enabled implicitly.

Automatic retrieval is **unavailable until native taint enforcement lands**.
The shared `knowledge_step` entry refuses both the heuristic and injected custom
deciders, even if bot JSON says `auto_retrieve=true`. `KnowledgeDecider` is our synchronous,
bounded hook for a heuristic or an already-computed decision from a future router
adapter. No `agent-router` code is changed. The shipped heuristic reads
`bot.config.knowledge.auto_retrieve` and requires an explicit document/notes
retrieval cue instead of every question mark. Once the native capability is
enabled, it can schedule one ordinary search before a relevant root-turn
question. It adds no decider model call. Subagents and later model
steps do not silently repeat it. The same tool visibility, permission, approval,
timeout, output and audit machinery applies to explicit and automatic searches.
Already persisted automatic retrieval is replayed as a delimited untrusted-data
User message. It never fabricates an assistant function call, provider signature
or Tool result. Real Gemini and Anthropic adapters are covered with local HTTP
shape tests. Explicit model-issued searches keep normal paired tool messages.

## Trust, poisoning and PR #46

All retrieved text is `content_trust: untrusted_data`, including user-authored
Markdown. `source_trust` records provenance only; it cannot elevate a chunk to
an instruction. Titles, headings, paths, saved emails, quotes and frontmatter are
also untrusted text. The brain is never scanned as a skills directory.

Trusted daemon tool code supplies `OutputProvenance` independently of result
bytes. After execution actually starts, the agent persists an execution fact
with `ran`, `ingests_untrusted`, `reads_private` and a fixed `knowledge` label.
A denied/unexecuted call creates no exposure fact. Result text resembling JSON,
an XML system tag or a fake `fact` field cannot override this sideband. It survives
process restart because it is stored in the durable tool step.

PR #46 is a separate, **unmerged** egress workstream at the inspected baseline.
Its current `ToolSpec.content`, `ToolOutput.exposure` and persisted `ExecFact`
wire shapes are supported by a narrow constructor bridge on the new knowledge
tools. Main ignores the extra spec/output fields. They are **inert compatibility
metadata**, not an active security boundary. Main's provenance
tests establish metadata integrity, **not** an egress gate that does not yet
exist on main. End-to-end poisoning/egress enforcement requires #46 integration
and its gate tests on the combined code; merge review must preserve that boundary.
No prompt label alone is claimed to make an LLM immune to prompt injection.

`Tool::egress_for(bot_id)` supplies an optional host-derived dynamic override;
wrappers must forward it and knowledge delegates to its bot-aware backend.
`KnowledgeBackend::search_for_run` carries the real execution run to a remote
backend's permission check. Neither hook alone authorizes network traffic. The
PR1 lexical backend declares no egress. Integration with #46 must consume the
override, replace the compatibility JSON with native types, keep exactly one
execution-fact writer, and pass durable taint/replay tests before opening the
automatic-retrieval capability. Ordinary wrappers no longer invent a private
read merely because they wrap another tool.

Standalone PR1/PR2 bundles exclude `knowledge.db`, `knowledge.db-wal` and
`knowledge.db-shm` at every depth. PR3 replaces this exclusion with its explicit
SQLite backup/validated import adapter; it must never restore generic file-copy
handling for these databases.

## Embeddings, caching and privacy

The only `provider-catalog` expansion is an embedding capability and factory,
using the existing Gemini/OpenAI-compatible providers, secret resolver and
sanitized HTTP error handling. Chat capabilities/model selection are unchanged.
Each embedding request has an explicit document/query role, model, task wire
format, dimensions and bounded input batch. Responses must match batch count,
indices, dimensions and finite nonzero values; invalid responses do not enter
the vector store. Embedding requests use one HTTP attempt and bounded response
bytes, refuse redirects, and never write failed document bodies to debug logs.

An embedding profile fingerprint binds provider, normalized destination,
model/revision, dimensions, task formatting and normalization version. Cache
identity also includes the exact formatted input/content hash and role. Models
with equal dimensions are not assumed compatible. Changing the profile requires
new consent and matching vectors; old vectors cannot silently answer queries.

Remote document indexing requires an explicit **per-source** grant for that
profile fingerprint. The model cannot create it with a tool argument or a
document annotation. Query embedding has a separate owner grant: source consent does not authorize
sending arbitrary chat text. Portable bot/source JSON is never grant authority.
The host records explicit confirmations separately in `knowledge-authority.db`,
bound to owner, bot, profile, index epoch and source identity. Each five-minute
approval challenge binds the current source revision and is single use. Import
rotates the index epoch, including when bot/source IDs are unchanged. Reranking has no implied
remote grant. A missing/revoked grant leaves lexical retrieval available.
Indexing checks grant/revision before dispatch and again before publishing an
asynchronous result. Deletion/revocation prevents future publication; already
dispatched HTTP bytes cannot be recalled. No raw document cache is shared across
bots, and source deletion removes its chunks/vector associations and unused
derived cache records.

The daemon installs this runtime behind the same `knowledge_search` tool. It
reads the bot's current owner settings for every invocation and caches at most
16 bot/profile services. Each bot keeps only its current configuration snapshot;
changing or disabling settings releases the old service. Query-vector caches
hold hashes and at most 64 vectors for five minutes, never raw query text.
Before each remote attempt the host re-resolves the existing provider's current
`SecretRef` and destination. A cached service cannot send to a newly configured
endpoint under an old grant, and key rotation does not retain an old key.
The live secret redactor refreshes once before each outbound batch.

Source/settings mutations and dispatch admission share a **per-bot** lane;
network requests do not hold that lane. Other bots keep their hybrid path while
one provider request waits. Revocation cancels that bot's admitted generation,
and deletion waits for bounded workers to release SQLite/model handles, even
when the original HTTP/tool waiter was dropped. Publication also rechecks source
revision, current host authority and cancellation.

Both owner approvals and every outbound dispatch use the permission engine's
`Remote` category. This reuses the existing remote-computer category: an
effective `Remote: deny` also denies embedding egress, using the existing
global/bot override rules. The category is not an independent network
capability. A general tool/category grant never substitutes for explicit source
or query confirmation. Unsupported permission limits cannot be turned into a
standing embedding grant. The `Remote` decision at query dispatch uses the
**actual active run**, including its initiator and inherited subagent policy;
unknown, completed and cross-bot run IDs fail closed. The current bot policy is
also rechecked. Owner setup uses the Human initiator; a scheduled/channel query
cannot borrow that initiator. On native #46 integration the same final dispatch
guard must also consume the tainted-run egress decision, including an owner grant
that appears after the initial dynamic tool classification.

Automatic continuation preserves durable retry limits. Provider 429/cooldown
results never consume the terminal source-failure attempt budget, although the
HTTP attempt remains charged to quota. An explicit owner retry resets a source's
failed-job state. Owner cooldown recovery separately clears provider cooldowns
and corresponding job deadlines across bots on the same account, without
refunding quota reservations. Retry delays are capped at 48 hours; existing
unbounded values are capped persistently rather than extended on every poll.
Small wall-clock rollbacks remain conservative; a large rollback rebases future
reservations, logs the adjustment and recovers within one rolling budget day.

Input-size/provider validation failures reduce the durable batch size until a
single bad chunk is isolated. Its safe failure code is persisted and exposed as
`skipped_chunks`; healthy chunks continue and no failed document body is logged.
Explicit source retry clears these exclusions. A new profile keeps old paid
vectors until each source's replacement generation is complete without skipped
chunks. Inactive generations never answer queries under the new fingerprint.

Gemini 001 uses `RETRIEVAL_QUERY`/`RETRIEVAL_DOCUMENT`. Gemini Embedding 2 instead
uses documented task prefixes and document title/text formatting; its multiple
content parts form an aggregate embedding, not independent vectors. A batch must
therefore preserve one independent request per input. The two model families use
different embedding spaces. These choices come from the [Gemini embedding
documentation](https://ai.google.dev/gemini-api/docs/embeddings), not model-name
guessing. [OpenAI-compatible embedding requests](https://platform.openai.com/docs/api-reference/embeddings/create)
use arrays of inputs and validate returned indices before reordering vectors.

The owner's approximate 100 RPM / 1,000 RPD are configurable account budgets,
not advertised universal quotas. A host-wide durable ledger reserves each
attempt, including retries, across bots sharing an account group. The daemon
derives that group from the configured SecretRef/environment credential identity
or anonymous endpoint; a bot's `account_group` string is ignored. Batching,
cache hits, persistent backoff and `Retry-After` avoid retry storms. Conservative
rolling limits avoid assuming a provider's midnight boundary. The actual
[Gemini limits](https://ai.google.dev/gemini-api/docs/rate-limits) are project-scoped
and vary by account/model. A local ledger cannot coordinate unrelated machines
using the same external account; provider 429s remain authoritative.
The source settings UI uses conservative input units: each item in a batch counts
toward the configured minute/day budget. Provider aliases with the same saved
`SecretRef` share a default quota group; environment-only credentials use the
provider identity. Separate saved credentials can still belong to the same
upstream project, so the local grouping does not discover or replace provider
project limits. The owner can enter embedding model IDs manually; the chat model
picker's results are not used as an embedding-capability test. Gemini settings
accept the explicit supported 001, 2 and 2-preview identifiers, with no inferred
task format for an unknown model family.

The optional local path uses explicitly supplied model/tokenizer artifacts with
the existing permissively licensed fastembed/ONNX stack, where supported. It
must not infer permission to download missing artifacts. Multilingual E5 uses
different `query:`/`passage:` prefixes. Native artifact availability, model
license/checksums and Windows/ARM performance require separate validation; the
default build and CI retrieval eval never need a real model or a GPU.
Build this adapter with `cargo build -p cli --features local-embeddings`. The
operator installs six absolute, SHA-256-pinned files through the fixed
`data_dir/knowledge-host.json` registry. Bots can select only an installed
`local_profile_id`; they cannot supply paths, pins or executable bytes. See the
[host setup guide](../guide/knowledge-embeddings.md) for the file format.
The registry is opened without following a final symlink/reparse point and its
opened handle is checked before reading. On Unix it must belong to the daemon
user and disallow group/world writes.

The model/tokenizer assets are verified into owned byte snapshots. Linux loads
the runtime from a sealed memfd; Windows loads a private snapshot with an open
handle denying write/delete sharing. The process retains that handle for the
runtime lifetime. Replacing the original path after verification cannot change
the loaded bytes. Native loading fails closed on other platforms until an
immutable snapshot implementation is available. The local recipe uses up to
eight tokenizer-checked windows per chunk, covers the complete input, and pools
normalized vectors with token weights. Inference uses one CPU thread and one
global slot with batches of two windows. Windows/Linux CI compiles and tests
hermetic artifact/tokenizer fixtures; it does not run real model weights.

The local feature uses dynamic ONNX linking without model-hub/runtime-download
features. CI fetches Cargo dependencies first, verifies the resolved feature
graph (including negative controls), then runs tests with Cargo `--offline`,
`ORT_SKIP_DOWNLOAD=1` and `ORT_OFFLINE=1`. Cargo offline mode is not an OS network
sandbox and does not prevent arbitrary build-script sockets. The graph check
and ort-sys's disabled linking/download branch establish the narrower claim:
this optional embedding adapter has no enabled artifact download feature.

### Exact vector scan ceiling

A bounded SQL preflight checks eligible row count and dimensions before any
query quota reservation or HTTP request. The configured row ceiling is at most
100,000 (default 20,000), and the dimension-aware byte ceiling is the lower of
16 MiB or `query_timeout_ms × 32 KiB`. At the default 500 ms budget this permits
at most 10,666 vectors at 384 dimensions, 2,666 at 1,536, or 1,333 at 3,072.
Source filters apply before this count. A larger corpus or mismatched dimensions
returns `LexicalDegraded` without paying for a query embedding. These are
conservative admission ceilings, not throughput guarantees for every CPU.
Vectors are normalized at ingestion; scans borrow f32-LE SQLite blobs and compute
dot products without allocating and renormalizing each stored vector. The
runtime deadline remains a second bound. Large-corpus ANN indexing is outside
this exact-scan implementation.

## Shared Markdown brain and state transfer

The human-readable brain lives under `data_dir/brain`, outside `skills` and the
per-bot index. The first write operation creates a new Markdown note rather than
overwriting a human edit. `knowledge_brain_write` always asks for approval,
including under otherwise permissive policies. Before the card appears, it
freezes the exact rendered bytes, target relative path, source labels/revisions,
bot/run identity and hash in a bounded, one-use approval record tied to the live
approval guard. The
card shows the proposed content, not a vague summary. Changed arguments,
closed/reused approvals or changed source revisions fail. Capability-scoped
no-replace publication prevents traversal and accidental overwrite. Imported
frontmatter does not grant trust or write authority.

Atomic publication is implemented for Linux and Windows. Linux uses an unnamed
staging inode, requiring an `O_TMPFILE`-capable filesystem and accessible
`/proc/self/fd`. Windows also publishes from the held source and directory
handles, pins the staged file against concurrent writes/deletion, and requires a
local filesystem supporting hard links. Unsupported filesystems
or publication failures reject the write instead of falling back to a mutable
staging pathname. The bot tool advertises only these two supported platforms;
human editing and explicitly selected brain reads remain ordinary Markdown use.
The final publication holds a source-revision transaction and rechecks the
approved brain directory identity immediately before linking the new file.
A directory replaced while publication waits for that transaction cannot retarget
the approved note. Pending records follow the live approval guard, are bounded to 64 per bot, and
contain at most 64 KiB of rendered Markdown; the approval card shows those exact
bytes and their hash. Startup cleanup only removes ordinary files with an exact
`.praxi-brain-<canonical UUID>.tmp` name below the trusted brain handle. It unlinks
the staging name without following links; published Markdown and external
targets survive. Fake names, directories and symlinks are left untouched. Staging
names were already excluded from state bundles and remain excluded.

State bundles explicitly snapshot nested knowledge databases using SQLite's
backup API and validate scope/schema/foreign keys/index consistency before
installation. All imported vectors, including local `remote=0` vectors and
derived caches, are discarded; re-enabling a source cannot trust imported vector
geometry. WAL and SHM files are not ordinary bundle blobs. Import disables
all source bindings, clears remote document/query consent and automatic
retrieval, and clears the entire embedding configuration. File/folder locators
require explicit path selection on the receiving host before they can be enabled.
The same source identity and label survive that selection, while old indexed
bytes and document grants are invalidated. Installed local models are selected
only by a host profile ID; imported paths/hashes cannot choose native code. Pending approvals
and in-flight jobs are not transferable authority. The host quota ledger is
runtime state: it is excluded and an existing destination ledger must not be
rolled back by import. Shared Markdown files transfer as data and are only
indexed by bots that explicitly attach the brain.

## Evaluation, token accounting and delivery

The fixture corpus, queries and expected source IDs live under `evals/retrieval`.
Normal Rust tests run lexical recall@k/MRR and a deterministic fake-embedder
hybrid eval without network. Fake vectors test fusion, cache and isolation; they
are not evidence of Gemini/OpenAI semantic quality. A separate held-out concept
fixture uses eight natural-language queries with no lexical hit and asserts
Recall@1 = MRR@1 = 1.0, a 600-token serialized response ceiling, and at least 50%
savings against stuffing its complete corpus. Its broken-query-vector negative
control must drop recall to zero; the lexical channel cannot mask disconnection.
This deterministic semantic-path test is not a learned-model quality benchmark.
Provider wire tests use local
mock HTTP responses. Real paid provider calls and model downloads are unnecessary.

Token tests report both the lightweight output estimate and real reference BPE
counts from embedded tokenizer assets. They compare the complete bounded result
and agent-request overhead against stuffing the **same complete fixture corpus**
into a prompt. This is an explicit counterfactual benchmark: production recall is
bounded by the per-bot memory budget (default 800 estimated tokens), not by this corpus. They must not claim that adding retrieval
reduces a zero-token baseline. Actual Gemini/other-provider billing and the
effect of prompt caching need live measurements outside these hermetic tests.

The delivery sequence is: (1) this design, scoped storage/chunking/lexical search,
tool/runtime hook and eval; (2) provider capability, consent-bound embeddings,
cache/quotas/vector/RRF/local option; (3) source management/OpenAPI/generated
client/UI, approved shared-brain writes and state-bundle transfer. Dependent PRs
remain reviewable as separate commits while all target the requested `main`.
No PR is merged by this task. Linux and Windows Rust jobs, full workspace
clippy/fmt/tests, cargo-deny and UI typecheck/tests/i18n/themes/lint are the gates.
CI results apply only to the exact PR heads tested. Native desktop interaction,
real embedding quality and the owner's ARM throughput are reported separately
from those gates.
