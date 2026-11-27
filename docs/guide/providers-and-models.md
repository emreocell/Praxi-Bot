# Providers and models

A **provider** is a service (or local program) that runs a language model for
you. Praxi talks to the providers you configure; nothing is sent anywhere else.
A **model** is one of the models a provider offers. In bot profiles a model is
written `provider/model`, for example `gemini/gemini-2.5-flash-lite`.

## Built-in providers

| Id | What it is | Needs a key |
| --- | --- | --- |
| `gemini` | Google Gemini, native API (has a free tier) | `GEMINI_API_KEY` |
| `gemini-openai-compat` | Gemini through its OpenAI-compatible endpoint | `GEMINI_API_KEY` |
| `anthropic` | Anthropic Claude, native Messages API | `ANTHROPIC_API_KEY` |
| `openrouter` | One key, many upstream models (OpenAI-compatible) | `OPENROUTER_API_KEY` |
| `deepseek` | DeepSeek (OpenAI-compatible) | `DEEPSEEK_API_KEY` |
| `groq` | Groq (OpenAI-compatible, low-latency open models) | `GROQ_API_KEY` |
| `mistral` | Mistral AI: Large, Medium, Small, Codestral, Pixtral (OpenAI-compatible) | `MISTRAL_API_KEY` |
| `together` | Together AI (OpenAI-compatible serverless open models) | `TOGETHER_API_KEY` |
| `meta` | Meta Model API: Muse Spark and Muse Wonder (OpenAI-compatible) | `META_API_KEY` |
| `ollama` | Local models via Ollama at `http://localhost:11434/v1` | none |
| `lmstudio` | Local models via LM Studio at `http://localhost:1234/v1` | none |

**Mistral:** you need an API key from La Plateforme
(<https://console.mistral.ai/api-keys>). A Le Chat chat subscription is not an
API key and cannot be used here. Mistral only accepts tool-call ids of exactly
nine letters/digits, so Praxi-Bot rewrites other ids (for example after you
switch a conversation from another provider) to a stable nine-character id.
Mistral's Voxtral speech-to-text is not wired in as a dictation backend: see
the dictation notes.

**Meta (Muse models):** you need an API key from the Meta Model API dashboard
(<https://dev.meta.ai/>); keys start with `LLM_`. The preset points at
`https://api.meta.ai/v1` and lists `muse-spark-1.3` (the everyday model) and
`muse-wonder-1.0` (the most capable one, still in preview at Meta). Use the full
`muse-wonder-1.0`: the short name `muse-wonder` does not exist. Both models read
images, stream, call tools and can return JSON; Meta's limits per team are 3,000
requests and 4 million tokens per minute and model. Meta accepts only automatic
tool choice (it rejects "must call a tool"), which Praxi-Bot never asks for. The
`-contributor` variants Meta also offers are cheaper but let Meta train on your
prompts and answers, so they are not in the catalog; pick them by hand only if
that is acceptable. Availability depends on your country (see Meta's Geographic
Use Policy). Muse Image, Muse Voice and Segment Anything show up in the key's
model list but are not chat models, so the picker hides them.

Any other OpenAI-compatible server can be added as a **custom provider** with a
base URL (see below). The catalog also carries a list of models with USD prices
per million tokens, which is how the app can show cost estimates; the live model
list is fetched from the provider when available.

Anthropic models in the catalog are `claude-opus-5-5`, `claude-sonnet-5-5` and
`claude-haiku-5-5`; Gemini ones include `gemini-3.1-flash-lite`,
`gemini-2.5-flash-lite`, `gemini-3-flash-preview` and `gemini-3.1-pro-preview`.
Prices and model names change; the catalog files in `providers/catalog/` are the
reference.

## Add and manage providers

```sh
bot providers list                         # * marks the global default
bot providers add gemini                   # asks for the key (hidden input)
bot providers add gemini --key-env GEMINI_API_KEY --model gemini-2.5-flash-lite
bot providers add mylocal --base-url http://localhost:8000/v1   # custom
bot providers test [id]                    # test the connection
bot providers default gemini --model gemini-2.5-flash-lite
```

- `add` tests the connection first; `--no-test` skips that.
- The first provider you add becomes the global default.
- API keys are stored in the secret store (OS keychain, or an encrypted file),
  under the name `provider.<id>.api_key`. They are never written to
  `providers.toml` and never printed. Prefer `--key-env` or the hidden prompt
  over typing a key on a command line.
- `bot secrets list|set|delete` manage secrets directly; values are never shown.
- A running daemon picks up a new provider after `POST /v1/providers/reload`
  (runs already streaming keep their old provider).

## Which model runs what

A bot's profile names models by **role** (see
[Bots and profiles](bots-and-profiles.md)):

- `executor`: runs the agent loop (required).
- `light`: optional model for simple first turns; see [model tiering](model-tiering.md).
- `classifier`: an optional small, fast model used by the decision ladder.
- `fallback`: alternate models after retryable errors or missing capabilities.
- `planner`: recorded for the future; it does not split the loop into a planner
  and executor.

A good pairing is a capable model as `executor` and a cheap one as `classifier`.

To see how the model picker gets display names, recommendation rank and
`model_details`, read [Model picker metadata](model-catalog.md).

## Gemini free tier

Google offers a free API tier for Gemini through
[Google AI Studio](https://aistudio.google.com/): create an API key there, no
credit card needed. This is the cheapest way to try Praxi.

```sh
export GEMINI_API_KEY=...        # PowerShell: $env:GEMINI_API_KEY = "..."
bot providers add gemini --key-env GEMINI_API_KEY --model gemini-2.5-flash-lite
```

What to expect (limits are set by Google, differ per model and account, and
change; look at the rate-limit page in AI Studio for your key):

- Each model has its own limits on requests per minute, tokens per minute and
  **requests per day**. Some Flash-Lite models allow hundreds of requests a day,
  while other models may allow only about twenty, and the most capable (Pro)
  models may not be available on the free tier at all.
- An agent makes several model calls per task, so a small daily cap is used up
  quickly. Pick a Flash-Lite model for `executor` and `classifier`, keep tasks
  small, and avoid running many schedules on the free tier.
- When the limit is hit the provider answers with an error (HTTP 429). Praxi
  retries a model step a few times with backoff (3 attempts), but a daily cap
  does not recover by waiting a few seconds; wait for the reset or switch
  provider.
- Free-tier terms differ from paid terms, including how Google may use your
  prompts. Read Google's current terms before sending anything sensitive.
- A Gemini key also enables the **embedding** search used by the decision ladder
  (K2), so requests can match your workflows and skills semantically.

## Local models

Install [Ollama](https://ollama.com/) or LM Studio, pull a model, then:

```sh
bot providers add ollama --model <model-name>
```

No key is needed and nothing leaves your machine. Pick a model that supports
tool calling, or the bot will not be able to use its tools well.

## Custom OpenAI-compatible providers

```sh
bot providers add myserver --base-url https://my.host/v1 --key-env MY_KEY --model my-model
```

Use this for vLLM, LiteLLM, a company gateway and the like.
