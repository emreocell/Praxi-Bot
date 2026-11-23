# Helpers (optional): embeddings and the decision helper

You do not need any of this to use Praxi. With no keys, Praxi uses a simple
offline method to match words, and **nothing leaves your computer**.

Open **Settings > Models > Helpers (optional)** to add more.

## What are embeddings?

Embeddings turn a text into a list of numbers so Praxi can find notes and
memories by meaning ("my flat in Berlin" matches "where do I live"). Praxi can
use several providers and is not tied to one:

| Provider | Good for |
| --- | --- |
| Google Gemini (`gemini-embedding-001`, `gemini-embedding-2`) | A free key is enough for personal use |
| OpenAI (`text-embedding-3-small`, `text-embedding-3-large`) | Paid, widely used |
| Any OpenAI-compatible server | Ollama, LM Studio, OpenRouter, Mistral, Together, Voyage-style gateways: give the address, the model, the vector size and (if needed) a key |
| The local model | Builds with local embeddings; the text stays on your computer |
| Offline method | Always there as the last fallback |

## Your text is only sent with your permission

Every provider except the local model and the offline method **receives the
text** of your notes, memories or document chunks. So:

- Adding a provider asks you to confirm in plain words that text will be sent to
  that provider. Without the tick, the provider is never called.
- Passwords and keys are removed from the text first.
- Anything marked private is never sent; that batch uses the offline method.
- You can keep chosen bots away from remote providers.
- API keys live in the secret store, never in the settings file, and are never
  shown again.
- Only the owner (the desktop app or the host's owner key) can change this.

## Free keys and limits

Free keys have limits per minute (requests and tokens) and per day. Praxi keeps
its own count and stays at about 90% of each limit, waits briefly, and otherwise
uses the next provider instead of risking a ban. The bars in Settings show
requests per minute, tokens per minute and requests per day. The counters are in
memory and start again when Praxi restarts.

The limits come from a dated list built into Praxi (see the "last reviewed"
date). Providers change limits; you can override them per provider. Reviewed on
2026-11-24 for the Google AI Studio free tier:

| Model | Requests/min | Tokens/min | Requests/day |
| --- | --- | --- | --- |
| Gemini Embedding 1 and 2 | 100 | 30,000 | 1,000 |
| Gemini Decision 1 | 200 | 50,000 | 10,000 |

## Fallback chain

For each purpose (choosing the right shortcut or tool, memory recall) Praxi
tries the providers in your order. A provider that is out of quota, refused the
key or is offline rests for a while and the next one is used. The offline method
is always last. Vectors from different providers are never mixed: a store keeps
vectors per provider, model and size.

## Decision helper (optional)

Praxi can use Google's Decisions model (`gemini-decision-1`) as an optional
helper for small yes/no and choice questions. It is **off by default**, needs a
Gemini key and is switched on in the same place. It only advises: a decision can
never grant a permission or approve a tool. Calls count in the LLM call ledger
and the spending limits like other model calls. If you wrote `decider.toml`
yourself ([Optional decision pool](decision-pool.md)), Settings leaves it alone.

## Problems

| You see | What to do |
| --- | --- |
| Key was refused | Check the key at the provider and paste it again |
| Free limit reached, resting | Wait, or add a second provider; Praxi continues by itself |
| Answers have the wrong size | The vector size does not match the model; use the suggested number |
| Not reachable | Check the server address and that the server is running |

Knowledge sources keep their own per-source approval described in
[Set up knowledge embeddings](knowledge-embeddings.md).
