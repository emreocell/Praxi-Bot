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
| `ollama` | Local models via Ollama at `http://localhost:11434/v1` | none |
| `lmstudio` | Local models via LM Studio at `http://localhost:1234/v1` | none |

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
- `classifier`: an optional small, fast model used by the decision ladder.
- `planner` and `fallback` are recorded for the future and not used by the loop
  yet.

A good pairing is a capable model as `executor` and a cheap one as `classifier`.

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
