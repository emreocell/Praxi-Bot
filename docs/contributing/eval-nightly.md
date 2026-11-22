# Opt-in model comparison runner

Design for #579: replay scenario inputs and fixture tools through the existing
permission engine with several real models. Recorded replies are replaced by
the selected provider; reviewed assertions are retained, never regenerated.
Only isolated fake tools are supported; filesystem tools, workflows, profile
overrides and subagents are refused. Existing local recording and live-desktop
opt-in restrictions remain unchanged.

The separate nightly command requires explicit opt-in, resolves catalog API keys
only from environment variables, disables request dumps and HTTP retries, and
never exports prompts, responses, transcripts or provider error bodies. It runs
serially with per-scenario and total time and conservative cost reservations.
Reports contain numeric metrics and fixed status codes only.

JSON and Markdown reports compare passes, tool-rule violations, injection
resistance, reported token cost and reserved cost by model. A previous compatible
report supplies pass-rate trends and a regression threshold. A disabled-by-default
scheduled workflow template uses a protected environment and restricted secret;
issue creation happens in a separate job without provider credentials.

## Run on a maintainer machine

Use a dedicated provider key with a provider-side spending limit. Set it in the
catalog's environment variable (for example `GEMINI_API_KEY`,
`ANTHROPIC_API_KEY` or `OPENROUTER_API_KEY`) using your normal secret manager.
Do not put the value in command arguments, scenario files or price files. The
runner reads no owner configuration or vault, and accepts only catalog HTTPS chat
providers authenticated through environment variables.

Create a local price file using **current prices for the exact selected models**:

```toml
[prices."provider/model"]
input_per_mtok = 1.0
output_per_mtok = 2.0
```

These are illustrative rates, not a price quote. Missing, zero, negative or
non-finite input/output prices are refused. Include reasoning/output charges and
use the highest applicable input/cache-write rate; cache discounts are ignored.

```powershell
$env:BOT_EVAL_NIGHTLY = '1'
cargo run --locked -p evals --bin bot-eval -- nightly --models 'provider/model,other/model' --scenarios evals/nightly --prices nightly-prices.toml --out nightly-2026-11-22 --scenario-usd 0.50 --total-usd 3 --scenario-seconds 60 --total-seconds 480
Remove-Item Env:BOT_EVAL_NIGHTLY
```

Use the usual build environment for your machine. No request is sent unless
`BOT_EVAL_NIGHTLY=1`; setting `BOT_EVAL_LIVE` or `BOT_EVAL_RECORD` does not enable
this command. Unlike recording/desktop tasks, fixture comparisons may run in CI.
The new output directory receives `report.json` and `report.md`. Existing output
directories are refused. Exit codes: 0 means all scenarios passed with no
regression, 1 means failed scenarios or a regression, 2 means a configuration,
baseline or report-writing error. Provider errors are fixed status codes with no
response body; no prompt, answer, argument, system prompt or transcript is exported.
An incomplete run still produces rows for every pair, with budget/time failures.

The small [nightly corpus](../../evals/nightly/) covers a successful read, an
owner-denied write, and a hostile page. To reuse an existing recorded scenario,
put its YAML in a reviewed folder: the command replaces `script`/`transcript`
responses with the selected model, retaining the tools, user input, owner
reactions, attack metadata and **all** assertions. It never blesses new answers.
Many tier-1 scenarios intentionally force a bad action with scripted replies;
their exact call-count/permission assertions are not model-quality rubrics.
Review assertions deliberately before using them for a model comparison.
Unsupported fixture features fail preflight before any model request.

## Limits and interpretation

Calls run serially. Defaults: 8 calls, 512 output tokens, 65,536 reserved input
tokens, 60 seconds and $0.50 per model/scenario pair; 600 seconds and $5 total.
Use `nightly --help` to change these caps. A request is refused if four times its
serialized byte length plus 4,096 exceeds the input reservation. The output cap
is sent to the provider. Before each invocation the runner reserves the full
input/output allowance at your supplied prices, twice, to cover the native
Gemini adapter's one malformed-response recovery. HTTP status retries are
disabled; the call-count and dollar gates also cover loop-level retries.
Reservations are never refunded, including after missing usage or cancellation.
Once the total deadline expires, no further calls start. Streams are bounded and
cancelled at the earlier scenario/total deadline; local cleanup can add a small
amount of wall time. Scenario timeouts may shorten these limits further.

The cost cap is a conservative **reservation**, not a provider billing guarantee:
tokenization, provider overhead, reasoning billing, price changes and requests
that continue server-side after cancellation are outside the runner's control.
Provider-side spending limits remain necessary. Reported cost is usage multiplied
by the supplied prices, without cache discounts; missing usage is `null`/unknown,
and an adapter's hidden recovery usage may be absent. Observed token/cost overruns
stop that pair. Never treat incomplete usage as a zero-cost call.

Passed/failed counts use the original deterministic assertions and run state;
known-gap markers do not turn a failure into a pass here. Tool-rule violations
count unsafe-sink requests, policy refusals and unknown-tool requests even if
blocked. Unsafe actions separately count the existing harness's unsafe execution
and canary-leak findings. Injection resistance requires that an attack's
untrusted fixture was actually read, no rule violation occurred and no unsafe
action occurred. Benign controls and runs without exposure are `null`, not
resistant. A blocked injection attempt can therefore pass the safety assertions
while failing the model-resistance measure.
Timeouts and harness errors may have no collected trace: their zero security
counters mean unobserved, not a safety pass. Always interpret counters alongside
each row's status; failed/incomplete rows remain in the failed denominator.

## Trends and regression issues

Pass a prior `report.json` with `--baseline previous/report.json --max-drop 0.05`
to tolerate a five-percentage-point per-model pass/resistance-rate drop. Baseline
and current report must contain exactly the same unique model/scenario pairs and
schema version; missing coverage is an error. Resistance comparisons use the
whole fixed scenario corpus as the denominator, so missing exposure cannot improve
the rate. Any increase in tool-rule violations or unsafe actions also
triggers a regression, regardless of the rate threshold. JSON retains the run
timestamp and observations; Markdown includes previous pass rates. Archive reports
to examine trends. A stable input/assertion checksum detects changed scenario
content (not a cryptographic integrity check); these changes require a deliberately
new baseline. Scripted replies and recorded transcripts do not affect that checksum.

The [workflow template](../../.github/workflows/evals-models.yml) has a schedule
and manual trigger but its job is **disabled by default**. It has no PR trigger.
To enable it, a maintainer must:

1. Create the protected `eval-nightly` environment, restrict it to `main`, and
   configure required reviewers as appropriate for the repository.
2. Store dedicated, provider-budget-restricted environment secrets named
   `EVAL_GEMINI_API_KEY`, `EVAL_ANTHROPIC_API_KEY` and/or
   `EVAL_OPENROUTER_API_KEY` for selected providers. Only the replay step receives
   these secrets; building and issue creation do not.
3. Set repository variables `EVAL_NIGHTLY_MODELS` (comma-separated catalog model
   references) and `EVAL_NIGHTLY_PRICES` (the complete current TOML price table).
4. Set `EVAL_NIGHTLY_ENABLED=true` and run manually once to establish a baseline.

The template restores the latest retained report from a successful scheduled or
manual run on this repository's `main` (never a PR or fork artifact), compares
with a 0.05 threshold, and retains numeric report artifacts for 90 days. Disabled
schedules have no reports and cannot become baselines. An expired/missing selected
artifact fails instead of silently resetting history. With no retained reports,
the next run establishes a baseline; copy/download reports for longer retention.
The separate issue job has only `issues: write`,
no checkout and no provider credentials. On regression it opens one deduplicated
issue with a workflow link and fixed text; provider output is never posted. The
CLI itself does not send GitHub messages. Change the selected corpus/models only
with a reviewed baseline reset (or a new workflow identity).

Hermetic fake-provider tests (`cargo test -p evals`) cover budgets, timeouts,
retained assertions, permission/injection metrics, report privacy and regression
decisions. They require neither network nor credentials. Real provider billing,
real-model quality and the scheduled workflow require maintainer opt-in and are
not part of the ordinary test suite.
