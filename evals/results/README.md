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

## Regression run after the provider-robustness fixes (task 030)

Model: `gemini/gemini-3.5-flash-lite` through the **native** Gemini provider (the `gemini` preset, see below),
throttled to one request per 6.5 s. File: `gemfix-run1.jsonl`. About 59 model requests in total.

| Task | Before (024) | After | LLM calls | Wall |
|---|---|---|---|---|
| desktop-notepad-write-save | FAIL (HTTP 400, then 26 calls) | run 1: FAIL (tool calls stalled, wall cap 240 s, 7 calls); run 2: **pass** | 7 / 14 | 240 s / 90 s |
| desktop-calculator-compute | FAIL (intermittent 400), pass on re-run | **pass** | 11 | 87 s |
| desktop-settings-open-page | FAIL (model opened Explorer) | FAIL (no Settings window, judge fail): model weakness | 16 | 114 s |
| browser-example-read | pass | **pass** | 5 | 30 s |
| browser-httpbin-read | pass | **pass** | 5 | 32 s |

Pass rate on this subset: before 2/5 (3/5 counting the calculator re-run); after 4/5 tasks passed at least once
(4 of 6 attempts). No HTTP 400, no `MALFORMED_FUNCTION_CALL`, no thought leakage and no stalled retry occurred.
The one unexplained failure (notepad run 1) was in tool execution, not the provider: 5 `use_tool` calls consumed the
whole 240 s; it did not recur.

### Running Gemini evals

Use the native provider; no `providers.toml` entry is needed. The key is read from `GEMINI_API_KEY`:

```
BOT_EVAL_LIVE=1 bot-eval run --tier 2 --model gemini/gemini-3.5-flash-lite --min-interval-ms 6500
```

Failed HTTP 400 request bodies (redacted, never the key) are written to `<data_dir>/debug/last-failed-request-<provider>.json`.

## Round 2 (task 035): native Gemini, 15 tasks, slow-call diagnosis

Model: `gemini/gemini-3.1-flash-lite` (native provider), throttled to one request per 6.5 s (about 9 RPM).
Fallback `gemini/gemini-3.5-flash-lite` for three tasks (see below). About **239 model requests** in total.
Files: `baseline-gemini-3.1-flash-lite.jsonl` (one run per task, 15 records: 14 from the full sweep plus the first
notepad run), `rerun-gemini-3.1-flash-lite.jsonl` (re-runs after fixes, 3.1), `fallback-gemini-3.5-flash-lite.jsonl`.
Records now carry `tool_time` (per tool: calls, total, max ms) and `model_time` (model steps, rate-limit waits and
provider retries included); `bot-eval run --trajectory-dir <dir>` writes each task's calls and results.

| Task | 3.1-flash-lite baseline | LLM calls | 024 / 030 | Final (best result) |
|---|---|---|---|---|
| browser-example-read | pass | 2 | pass / pass | pass |
| browser-follow-link | pass | 3 | pass | pass |
| browser-wikipedia-search | pass | 9 | pass | pass |
| browser-httpbin-form | pass | 7 | pass | pass |
| browser-httpbin-read | pass | 3 | pass / pass | pass |
| desktop-notepad-write-save | FAIL (26 calls, never saved) | 26 | FAIL / FAIL, pass | pass on 3.5-flash-lite (15 calls, 92 s) |
| desktop-notepad-read-file | pass | 4 | pass | pass |
| desktop-explorer-create-folder | pass | 11 | pass | pass |
| desktop-calculator-compute | pass | 7 | FAIL (400) / pass | pass |
| desktop-settings-open-page | **pass** | 3 | FAIL / FAIL | pass (was model weakness; see fixes) |
| browser-httpbin-multifield-form (new) | FAIL (my final_text check was case-sensitive; page checks passed) | 16 | - | pass on 3.5 (23 calls) |
| browser-wikipedia-second-result (new) | pass | 3 | - | pass |
| desktop-explorer-rename-file (new) | pass | 17 | - | pass |
| desktop-notepad-font-size (new) | FAIL (setup: leftover Notepad held settings.dat) | 0 | - | pass on 3.1 re-run (7 calls) |
| mixed-web-value-to-notepad (new) | FAIL (model launched chrome.exe/Opera, see below) | 14 | - | pass on 3.5 (15 calls, 92 s) |

Baseline pass rate on 3.1-flash-lite: 11/15 (all 10 old tasks except notepad-write-save: 9/10, plus 2/5 new). After fixes
and re-runs every task passed at least once (15/15), but three of them only on `gemini-3.5-flash-lite`: the 3.1 model
returned HTTP 503 "high demand" during the re-runs (calls of 100-200 s each), so the fallback was used once.

### Slow-call diagnosis (the 030 "240 s notepad stall")

Per-tool timing says the desktop tools are not slow: in every desktop run the tool bodies summed to 2-7 s
(`desktop_key` max 0.4 s, `desktop_type` max 0.6 s, `desktop_get_state` max 0.35 s, `desktop_launch` 1.6 s), and a
model-free timing of launch / snapshot / click / type / Ctrl+S was under 0.1 s per call. The wall time is **model
steps**: `model_time` of the 240 s notepad runs was 233 s, with single calls of 107-182 s. Those were Gemini
`503 high demand` upstream errors retried inside the provider (plus the 6.5 s throttle). Not a tool hang.

Two real desktop problems did turn up and are fixed:

1. **Notepad session restore.** The Store Notepad restores the tabs of a killed instance on its next launch. Killing
   Notepad with `taskkill /F` after a task left tab state for `secret.txt` in deleted temp dirs; the next launch
   opened extra windows and tabs for them, stealing focus and keystrokes. Eval cleanup now removes the Notepad
   session-state files a task created (`kill_new: [notepad.exe]` also forgets the new `TabState`/`WindowState`
   files), and the leftover state of earlier eval runs was deleted by hand.
2. **Leaked windows poison later runs.** The first sweep left a Notepad (opened by the rename task) and a Chrome plus
   Opera window (opened by the mixed task through `desktop_launch`); the next task's first `desktop_get_state`
   then saw and typed into the leftover Opera window. Cleanup of the rename and mixed tasks now kills the new
   processes, and `desktop_launch` says web pages go to the browser tools.

Hardening that costs nothing: UIA transaction timeout 8 s (default 20 s) so a frozen app cannot block the UIA thread
for long; keyboard input re-activates the last snapshot's window when another *process* owns the foreground; tier-2
tool calls are capped at 60 s (the loop default is 120 s).

### Settings page (model weakness)

Two defects made it fail: the task prompt itself suggested `explorer.exe ms-settings:display`, and `desktop_launch` on a
URI returned an *error* ("no process handle") even though the shell had succeeded. Now `desktop_launch` treats a launch
without a process handle as success (`pid` 0 -> "launched ... look for the app's window"), its description names
`ms-settings:` URIs and tells the model never to open Explorer for them, and the prompt no longer mentions Explorer.
Re-run: pass in 3 calls.

### Notes

- `desktop-notepad-font-size` changes a persistent Notepad setting: setup copies `settings.dat*` into the task's
  workdir and cleanup restores it after Notepad was killed (`settings.dat` was byte-identical afterwards, `LOG1` differs
  because Notepad rewrites its transaction log).
- `browser-wikipedia-second-result` is judged (the search ranking is live data).
- Eval runs on a real desktop with the user's own browsers installed: a model that launches `chrome.exe` opens the
  user's Chrome. Tasks now clean up the processes they started (never pre-existing ones).
