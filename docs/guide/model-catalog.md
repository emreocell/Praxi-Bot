# Model picker metadata

The daemon annotates the model ids returned by each configured provider. The
curated table is presentation data: it does not limit custom ids, alter a bot's
saved model, change capability overrides, or change pricing.

## API

`GET /v1/providers` keeps `configured[].models` as a string array. Its ids are
now filtered to chat candidates and ordered by recommendation rank, then display
name. The additive `configured[].model_details` array uses the same order:

```json
{
  "id": "claude-sonnet-5-5",
  "display_name": "Claude Sonnet 5.5",
  "kind": "chat",
  "recommended": true,
  "recommended_rank": 1,
  "deprecated": false,
  "note": null
}
```

For a compact picker, show entries with `recommended: true` first and put the
remaining entries behind an **All models** expander. The backend does not cap the
rest of the list. The existing desktop picker consumes the display names and
rank ordering; layout and the expander belong to the separate UI redesign.

Use `GET /v1/providers?chat_only=false` to receive non-chat and deprecated models
as well. `chat_only=true` is the default. The endpoint and peer permissions are
unchanged. No new Tauri command is introduced.

`kind` is the primary use: `chat`, `reasoning`, `vision_capable`, `embedding`,
`image`, `audio`, `video`, or `other`. The first three are chat candidates. A
recommended model has kind `chat` even when it also accepts images or reasons;
execution capability flags remain their own source of truth. Unknown ids get
a humanized display name and never get a recommendation. Conservative inference
hides names indicating image/video/audio/music/embedding/realtime generation,
specialized deep-research, computer-use, moderation, and reranking endpoints.

The API lists only ids actually returned by the provider. It does not inject a
curated model absent from the live listing. Local models therefore need to be
installed and visible to the local server first. A failed listing returns no
model candidates and a redacted error. Malformed or secret-bearing ids are
omitted rather than changed into unusable ids. A bot's current or manually
entered custom model remains usable even when absent from this list.

## Curated data and maintenance

`crates/provider-catalog/src/model_metadata.json` contains the checked-in table,
`last_reviewed` date, and official source URLs. `providers/catalog/*.toml` holds
provider endpoints, context limits, and reference pricing. The validator checks:

- required fields and valid ISO `YYYY-MM-DD` review/pricing dates,
- unique provider ids, aliases, and model ids, with every alias resolving to its
  canonical provider and every shipped TOML provider/model covered by the table,
- unique display names per provider (case-insensitive),
- sane context windows, output limits (`max_output_tokens <= context_window`),
  and non-negative finite pricing (`cached_per_mtok <= input_per_mtok`),
- unique recommendation ranks in 1–4, no more than four recommendations per
  provider, no `:free` recommendations, and no deprecated or non-chat
  recommendations,
- deprecated models explicitly flagged with both `"deprecated": true` and a
  human note (such as `"legacy"` or `"retired"`),
- zero drift between `model_metadata.json` and the summary table below.

Changing `model_metadata.json` does not change inference or execution.

<!-- BEGIN:model-catalog-table -->
Last reviewed on 2026-11-15:

| Provider | Aliases | Last reviewed | Candidate recommendations | Sources |
| --- | --- | --- | --- | --- |
| `gemini` | `gemini-openai-compat` | 2026-11-15 | Gemini 4 Carbon (`gemini-4-carbon`), Gemini 4 Argon (`gemini-4-argon`), Gemini 4 Hydrogen (`gemini-4-hydrogen`), Gemini 3.5 Flash-Lite (`gemini-3.5-flash-lite`) | <https://ai.google.dev/gemini-api/docs/models>, <https://ai.google.dev/gemini-api/docs/deprecations> |
| `openai` | — | 2026-11-15 | GPT-6.1 Sol (`gpt-6.1-sol`), GPT-6 Luna (`gpt-6-luna`), GPT-6 Astra (`gpt-6-astra`), GPT-6.1 Terra (`gpt-6.1-terra`) | <https://developers.openai.com/api/docs/models> |
| `anthropic` | — | 2026-11-15 | Claude Sonnet 5.5 (`claude-sonnet-5-5`), Claude Opus 5.5 (`claude-opus-5-5`), Claude Haiku 5.5 (`claude-haiku-5-5`), Claude Fable 5.2 (`claude-fable-5-2`) | <https://platform.claude.com/docs/en/models/overview> |
| `openrouter` | — | 2026-11-15 | Claude Sonnet 5.5 (`anthropic/claude-sonnet-5.5`), GPT-6 Luna (`openai/gpt-6-luna`), Gemini 3.8 Flash (`google/gemini-3.8-flash`) | <https://openrouter.ai/anthropic/claude-sonnet-5.5>, <https://openrouter.ai/openai/gpt-6-luna>, <https://openrouter.ai/google/gemini-3.8-flash> |
| `xai` | — | 2026-11-15 | Grok 4.7 (`grok-4.7`) | <https://docs.x.ai/developers/models> |
| `deepseek` | — | 2026-11-15 | DeepSeek V4.1 Flash (`deepseek-flash`), DeepSeek V4 Pro (`deepseek-v4-pro`) | <https://api-docs.deepseek.com/quick_start/pricing/> |
| `ollama` | — | 2026-11-15 | Qwen 3 · 8B (`qwen3:8b`), Qwen 3 · 4B (`qwen3:4b`), Qwen 3 · 30B (`qwen3:30b`), Llama 3.3 · 70B (`llama3.3:70b`) | <https://ollama.com/library/qwen3>, <https://ollama.com/library/llama3.3> |
| `lmstudio` | — | 2026-11-15 | None (depends on installed local models) | <https://lmstudio.ai/docs/developer/openai-compat/models> |
| `cli:claude` | — | 2026-11-15 | Claude Code default (`default`), Claude Sonnet (`sonnet`), Claude Opus (`opus`), Claude Haiku (`haiku`) | <https://code.claude.com/docs/en/model-config> |
| `cli:codex` | — | 2026-11-15 | Codex default (`default`) | <https://developers.openai.com/codex/models> |
| `groq` | — | 2026-11-15 | GPT-OSS 120B (`openai/gpt-oss-120b`), GPT-OSS 20B (`openai/gpt-oss-20b`) | <https://console.groq.com/docs/models>, <https://console.groq.com/docs/rate-limits> |
| `mistral` | — | 2026-11-15 | Mistral Large (`mistral-large-latest`), Mistral Medium (`mistral-medium-latest`), Mistral Small (`mistral-small-latest`), Codestral (`codestral-latest`) | <https://docs.mistral.ai/models> |
| `together` | — | 2026-11-15 | DeepSeek V4.1 Flash (`deepseek-ai/DeepSeek-V4.1-Flash`), Llama 3.3 70B Instruct Turbo (`meta-llama/Llama-3.3-70B-Instruct-Turbo`), GPT-OSS 120B (`openai/gpt-oss-120b`) | <https://docs.together.ai/docs/serverless/models>, <https://docs.together.ai/docs/serverless/rate-limits> |
| `meta` | — | 2026-11-27 | Muse Spark 1.3 (`muse-spark-1.3`), Muse Wonder 1.0 (`muse-wonder-1.0`) | <https://dev.meta.ai/docs/models>, <https://dev.meta.ai/docs/pricing-rate-limits> |
<!-- END:model-catalog-table -->

### Freshness tooling and staleness reminders

Praxi works offline by design, so catalog freshness is checked against the
recorded `last_reviewed` date rather than fetched at runtime:

- **Offline validation & freshness check:**
  ```sh
  bot catalog check --docs docs/guide/model-catalog.md
  ```
  Validates `model_metadata.json`, `providers/catalog/*.toml`, and the docs
  table above, then prints each provider's review date and age. Validation
  errors exit non-zero; staleness (older than 90 days) prints a warning and
  exits 0 unless `--strict` is passed. Add `--json` for machine-readable output.
- **Offline diff against a provider snapshot (JSON or TOML):**
  ```sh
  bot catalog diff anthropic ./anthropic-models.json
  bot catalog diff gemini ./gemini-models.toml
  ```
  Compares the committed catalog for a provider (or alias such as
  `gemini-openai-compat`) against a saved listing snapshot and prints added,
  removed, and changed models plus likely renames. Read-only: never edits files
  and makes no network calls. Pass `--live` instead of a snapshot path to
  opt-in query your configured provider using its stored key (also read-only).
- **Regenerate the Markdown table above:**
  ```sh
  bot catalog render-docs
  ```
- **Staleness reminders:** `bot doctor` and **Settings → System** report a
  non-blocking `catalog_freshness` warning when `last_reviewed` is older than
  90 days. A weekly GitHub Actions workflow
  (`.github/workflows/catalog-freshness.yml`) runs the offline check and writes
  a non-blocking warning to the job summary when a review is due.

For the maintainer review routine, see
[Keeping the model catalog fresh](../contributing/MAINTAINING.md#keeping-the-model-catalog-fresh).

OpenAI and xAI currently use custom OpenAI-compatible provider configuration on
main; this change supplies metadata, not new provider adapters. Only Claude and
Codex have runtime engines; Cursor/OpenCode/Gemini CLI are detection-only on
main, so no new executable provider or guessed model ids are introduced.

CLI engines have no live model-list API in Praxi. They retain the configured
model or the existing `default` option; other CLI aliases in the table are not
injected as claims of account entitlement. xAI and DeepSeek intentionally have
fewer than three recommendations because their verified current chat lineups
are shorter. OpenRouter's three candidates use its verified exact ids, not
first-party ids. No `:free` model is pinned as a recommendation or default. A
`free` note is added only when an OpenRouter `:free` id appears in the live list.

## Automatic catalog updates (no app update needed)

New models appear all the time. So that the picker does not wait for an app
release, Praxi-Bot can fetch a newer model list on its own.

- **What it does:** at most once a day the daemon downloads two small files
  (`provider-catalog.json` and its signature `provider-catalog.json.minisig`)
  from the project's GitHub release. Both are plain `GET` requests to fixed
  `https://` addresses. **Nothing about you, your bots or your chats is sent.**
- **Only signed files are used.** The signature must match the public key built
  into the app. A changed, unsigned, too-large or *older* file is thrown away
  and the model list you already have keeps working. Unknown extra fields in a
  newer file are ignored.
- **How it is merged:** for each provider, the downloaded entry replaces the
  built-in one when its review date is the same or newer. Providers the file
  does not mention stay as they are.
- **See it:** `bot doctor` shows "Catalog updated 2026-12-01" and, once the list
  is older than 30 days, a hint to update.
- **Turn it off:** `bot catalog auto-update off` (check with
  `bot catalog auto-update status`, back on with `on`). Setting the environment
  variable `BOT_CATALOG_UPDATES=off` also stops it.
- **Update by hand:** `bot catalog update` installs a newer catalog now;
  `bot catalog update --check` only tells you whether one exists.

Downloaded files live in `<data dir>/provider-catalog/` and are re-verified on
every start, so a hand-edited copy is ignored.

### Maintainers: publishing a catalog

The signing key is **never** committed. Generate it once and keep the file
private (a password manager or an offline disk):

```sh
python scripts/release/sign_provider_catalog.py --generate-key ~/catalog-key.b64
```

It prints the public key; that string must equal `PINNED_PUBLIC_KEY` in
`crates/provider-catalog/src/catalog_update.rs` (changing it needs an app
release). For each refresh, update `crates/provider-catalog/src/model_metadata.json`
(bump `last_reviewed`), then:

```sh
PRAXI_CATALOG_SIGNING_KEY="$(cat ~/catalog-key.b64)"   python scripts/release/sign_provider_catalog.py --out dist/
```

and upload `dist/provider-catalog.json` and `dist/provider-catalog.json.minisig`
as release assets (the app reads `releases/latest/download/...`). The script
needs the Python `cryptography` package.

## Local models: hardware and recommendations

`GET /v1/local-models` (owner only) reports what this computer can run with Ollama or
LM Studio. It is best effort and read-only:

- **Hardware probe.** RAM, logical cores, free disk in the data directory, and the GPU name and
  video memory from `nvidia-smi` when it is installed (fixed arguments, no shell, 1.5 s limit,
  16 KiB output cap). Apple silicon counts its unified memory. Anything that cannot be read is
  reported as unknown, never guessed.
- **Recommender.** Candidates are the curated `ollama` rows of the model table; a model's size
  class is the parameter count in its own id tag (`qwen3:8b` is 8 billion). Each gets a verdict
  (runs well, runs slowly, too large) from conservative 4-bit estimates. `recommended` is the
  largest model that fits and has room on disk; with unknown memory or an empty catalog it is
  null ("no recommendation"). The `ollama pull <id>` command is only built from plain ids and is
  shown for copying; Praxi never runs it.
- **Where it shows.** First-run wizard (when Ollama is chosen), Settings > AI models > Local models,
  and `bot doctor` (an informational line computed in-process). `GET /v1/doctor` deliberately
  omits it: the hardware fingerprint is owner-only.
- **Privacy.** The hardware summary stays on this computer: it is never sent to a model, a
  provider or telemetry.

## Limits of a model listing

A live model list and curated recommendation cannot prove that an account has
remaining quota, sufficient credit, or permission to invoke every returned id.
No paid completion or real-account probe is made by this feature. Provider
errors and per-account quota remain runtime concerns. Unknown model-name
inference is conservative, not a replacement for future provider-supplied
modality/entitlement metadata.

Gemini 2.5 Flash-Lite is **not deprecated** according to Google's current list;
it carries `note: "existing users only"`. This distinction matters even though
new projects should select a current recommended model. Existing bots keep
their configured id.

## Owner verification

1. Open a configured provider in the app and confirm human names are shown;
   selecting a name still saves the original provider/model id.
2. Compare `GET /v1/providers` with `?chat_only=false`: the default omits media,
   embeddings, specialized endpoints, and known deprecated models.
3. Stop a local model server and refresh: the API reports its listing failure,
   and no uninstalled recommendations appear.
4. Keep a bot on a custom model absent from the curated table and run a normal
   turn; metadata does not change its executor.

No real provider completion, installed desktop launch, or account quota check
is part of the automated tests; all model and route fixtures are local fakes.
