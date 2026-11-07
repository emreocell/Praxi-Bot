# Live eval baseline: Gemini (free tier), tier 2

Model: `gemini-openai-compat/gemini-3.5-flash-lite` (OpenAI-compatible endpoint, `OpenAiCompatProvider`),
throttled to one request per 5.5 s (`--min-interval-ms 5500`). Windows 11 (Turkish UI), real desktop and Chrome.

Files: `baseline-gemini-flash-lite.jsonl` (full run, 10 tasks), `rerun-gemini-flash-lite.jsonl` (re-runs of two failed tasks).

## Baseline (7/10)

| Task | Result | LLM calls | Tool calls | Tokens in/out | Wall | Failure class |
|---|---|---|---|---|---|---|
| browser-example-read | pass | 5 | 4 | 7547 / 227 | 23 s | |
| browser-follow-link | pass | 8 | 7 | 15071 / 314 | 45 s | |
| browser-wikipedia-search | pass | 12 | 11 | 83220 / 532 | 68 s | |
| browser-httpbin-form | pass | 12 | 11 | 25670 / 434 | 64 s | |
| browser-httpbin-read | pass | 5 | 4 | 6897 / 198 | 28 s | |
| desktop-notepad-write-save | FAIL | 3 | 2 | 1091 / 56 | 15 s | provider/API: HTTP 400 "invalid argument" on the 3rd model call (intermittent) |
| desktop-notepad-read-file | pass | 7 | 6 | 6201 / 216 | 39 s | |
| desktop-explorer-create-folder | pass | 18 | 17 | 161189 / 536 | 95 s | |
| desktop-calculator-compute | FAIL | 3 | 2 | 926 / 60 | 15 s | provider/API: same intermittent HTTP 400 on the 3rd call |
| desktop-settings-open-page | FAIL | 16 | 15 | 23833 / 589 | 90 s | model weakness: opened File Explorer instead of Settings; also left an Explorer window behind (eval-task cleanup bug, fixed) |

## Re-runs (no code change in the model path)

- desktop-calculator-compute: the 400 did not recur once, then **passed** (20 calls, 106 s). The first re-run hit the same 400 again, so it is intermittent.
- desktop-notepad-write-save: no 400, but **failed** after 26 calls (138 s): model weakness plus tool discovery. `find_tool` is lexical (BM25), so queries like "keyboard", "process" returned `[]`, and the model never reached a Save dialog.

Pass rate: 7/10 recorded; 8/10 counting the calculator re-run.

## What was needed to get any result

`gemini-3.x` requires the `thought_signature` returned on each tool call to be sent back
(`extra_content.google.thought_signature`). `ToolCall` / the OpenAI-compat request builder drop it, so the first call
with a tool result fails with HTTP 400, and a dummy signature makes the model emit `MALFORMED_FUNCTION_CALL` with empty
output. The baseline was therefore run through a small local proxy (not in the repo) that remembers the real
signatures per tool-call id and re-injects them. Without that proxy every Gemini 3.x run fails. See the follow-ups.
`gemini-2.5-flash-lite` needs no signatures but has only 20 requests per day on this key; `gemma-4-31b-it` returned
intermittent 500s and leaks `<thought>` text into the content.
