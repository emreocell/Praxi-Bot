# Hermetic retrieval fixtures

These are fictional engineering notes, not statements about deployed Praxi or
WPanel installations. The corpus deliberately mixes Turkish and English,
Markdown headings, fenced code, tables, overlapping vocabulary and irrelevant
documents. No private files, real mail, keys or network responses are used.

Run `cargo test -p memory-store --test retrieval_eval -- --nocapture`.

The test reports source-deduplicated recall@5 and reciprocal rank over positive
queries. The corpus has 15 sources, including same-vocabulary glossary, planning
agenda and keyboard-shopping distractors. `queries.json` preserves the original
labels; `ranking-cases.json` adds nine natural-language answer cases with exact
first-source assertions and six realistic negative questions. Including the
original no-match case, all seven abstentions must be empty. Aggregate gates are
recall@5 >= 0.95 and MRR >= 0.95; measured scores are printed separately.
Citation ranges and catalog scope are checked separately
in `knowledge.rs`. PR2 runs the same corpus through its deterministic fake
embedder and hybrid retriever; that is wiring evidence, not a benchmark of Gemini
or another semantic model.

The fake embedder constructs a sorted vocabulary from corpus text and returns
TF-IDF vectors with sublinear term frequency. Corpus document frequency lowers
the weight of shared boilerplate; repeated topical terms carry more evidence
than a glossary's one-off mentions. It does not read filenames, source IDs, expected answers or
query labels. An orthogonal unknown-token coordinate makes the no-match case
exercise actual zero-similarity abstention. The lexical path uses all 20 positive
queries with 0.95 recall/MRR gates. The hybrid path retains the original 11
positive queries, unchanged labels and 0.90 recall/MRR gates over all 15 sources.
The hybrid test additionally
checks durable indexing reuse and the bounded query-vector cache. Its scores
verify vector indexing, cosine ranking, fusion and result budgets; they do not
establish paraphrase or multilingual quality for a production embedding model.

`cargo test -p memory-store --test retrieval_semantic_eval -- --nocapture` runs a
separate semantic-path contract: eight held-out natural-language queries have
no lexical hits and must retrieve their exact matching document first. A small
concept-vector fixture supplies deterministic embeddings; it is not a learned
model. Recall@1 and MRR@1 must both equal 1.0, every complete JSON result must fit
600 actual cl100k tokens, and mean result size must be less than half of the
complete fixture corpus. An orthogonal query-vector negative control must
return no hits. Running with `PRAXI_EVAL_BREAK_QUERY_VECTORS=1` intentionally
fails the quality gate at 0/8; this prevents lexical results from hiding a
disconnected semantic channel. A local positive run measured 319–323 tokens per
result on average against a 1,329-token corpus. Dynamic IDs account for the
small tokenizer variation.

The expanded corpus's TF-IDF hybrid run measured Recall@5 **1.000000**, MRR
**0.909091**, and a mean **671** cl100k tokens per bounded result against **5,184**
tokens for the complete corpus. All original quality thresholds remain in place.

The token comparison counts **complete serialized search responses**, including
citations, provenance and escaping, with the embedded `cl100k_base` reference BPE.
It also reports the runtime `ceil(Unicode scalars / 4)` estimate and UTF-8 bytes.
The comparison baseline concatenates this whole corpus as hypothetical memory
context. A loop **without** a memory store (safe mode, or a library user who passes
`memory: None`) costs zero memory tokens; the shipped `bot serve` opens `memory.db`
and has recall on for every bot unless the owner sets that bot's memory to Off. The
benchmark must never be described as measured production savings over a no-memory
loop, or as the billed token count of a Gemini model. Agent-core
also measures complete serialized requests with `cl100k_base` and `o200k_base`:
a loop with no memory store, actual enabled `MemoryConfig` recall through
`with_recall`/`render_recall_block`, and both requests in an explicit retrieval
turn. The retrieval totals include tool schema, model-issued call, citations and
JSON escaping. Legacy background extraction/consolidation model calls are outside
the first-request recall measurement. A bot with no enabled source must produce
exactly the same request as a loop without the knowledge tool: zero added schema
or memory tokens. Automatic retrieval remains gated until native taint enforcement.

The corpus and thresholds are versioned. Changing expected sources to make a
failed search pass is not an acceptable fix without reviewing the query's
meaning. Duplicate chunks from one document do not count as independent relevant
sources. No request usage value from a scripted provider is used as measurement.

## Follow-up measurement

One local run of the expanded fixture produced recall@5 **1.000**, MRR **0.950**,
and all **7/7** negative-query abstentions. All nine natural-language ranking
cases selected their expected source first. These are lexical fixture results,
not semantic model accuracy or production quality estimates.

The actual request-path test for `Find atomic deployment symlink rollback
manifest` measured the following reference token counts. IDs are generated at
runtime, so repeated BPE counts can vary slightly; hard output bounds and the
quality gates are asserted independently.

| Request path | cl100k_base | o200k_base |
| --- | ---: | ---: |
| No memory store and no knowledge source (safe mode / library default) | 112 | 114 |
| Legacy enabled recall, actual first request, 800 estimated-token recall budget | 811 | 813 |
| Explicit knowledge retrieval, first planning request including tool schema | 308 | 318 |
| Explicit knowledge retrieval, second request including call and cited results | 1,038 | 1,052 |
| Explicit knowledge retrieval, total input across both requests | 1,346 | 1,370 |

**This run does not show token savings over either a no-memory loop or the
legacy recall path (which `bot serve` enables, 800-token default budget, adjustable per bot).** Explicit retrieval pays for a
model-issued tool call and a second request. Legacy background extraction and
consolidation are outside the first-request recall comparison, and provider
output tokens, caching and billing are not measured here. A missing/disabled
source contributes exactly zero additional tool-schema or memory tokens.

For the separate counterfactual of concatenating all 15 documents, the corpus
alone is 5,184 cl100k tokens. Across the 20 positive queries, complete bounded
search JSON averaged 652 cl100k tokens (maximum 926). That comparison describes
selective evidence size, and must not be presented as a saving over the real
legacy implementation or the default configuration.
