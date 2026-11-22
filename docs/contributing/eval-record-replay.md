# Turn a model run into a regression scenario

`cargo xtask record` and `cargo xtask replay` use the existing standalone
`bot-eval` command through a Cargo alias.
Recording uses a real configured chat provider with the tier-1 harness's isolated
fixture tools. The real permission engine, loop guards and taint tracking run in
both modes. Fixture tools never perform the model's requested external actions.
No daemon route or API change is required.

Start with a scenario template containing `name`, `user`, `tools`, `policy` and
any explicit owner `reactions`. A template's `script` is ignored during live
recording; `expect` is replaced by observations from that run. Choose tool
`content`, `executes`, category and effect as carefully as in a handwritten eval.
Approval reactions are decisions supplied by the owner of the template; recording
does not automatically approve calls. Unanswered approvals hit the turn timeout.
Real filesystem fixtures, workflows and subagents are refused by this initial
recording entry point to keep external effects and concurrent model calls out.

On the owner's machine, configure the provider as usual, then use PowerShell:

```powershell
$env:CARGO_TARGET_DIR = 'D:/praxi-builds/<worker-id>'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '3'
$env:BOT_EVAL_RECORD = '1'
cargo xtask record evals/scenarios/02-approval-deny.yaml --model gemini/<model> --out evals/scenarios/recorded-my-bug.yaml
Remove-Item Env:BOT_EVAL_RECORD
cargo xtask replay evals/scenarios/recorded-my-bug.yaml
cargo test -p evals
```

`--data-dir` / `BOT_DATA_DIR` and `--catalog-dir` / `BOT_CATALOG_DIR` select the
existing provider configuration. Recording requires `BOT_EVAL_RECORD=1` and
refuses **any** set `CI` variable, including `CI=false`. Replay never constructs
a live provider, reads credentials or invokes network tools. All recorded YAML
files in `evals/scenarios` also run in the ordinary tier-1 suite.

The output is one self-contained YAML scenario with an inline `transcript`
instead of `script`. Keeping the fixtures and transcript together avoids dangling
sidecar files. Format version 1 stores:

- `synthetic`: whether the source was scripted rather than a live provider;
- `source_model`: original provider/model for provenance, ignored by replay;
- `calls`: one entry per provider invocation, in order;
- `request`: message roles (excluding system and runtime action-ledger messages),
  sorted offered tool names, and the tool results exactly as supplied by the loop,
  subject to privacy filtering;
- `request.fingerprint`: a stable FNV-1a checksum of that sanitized summary,
  for change detection; this is not a cryptographic prompt hash;
- `chunks`: ordered text deltas, argument deltas, complete tool calls, usage and
  the final finish reason.

Raw system prompts and request bodies are not exported. The initial user turn
lives in the scenario's `user` field. Provider-specific opaque signatures are
removed. A credential spanning text chunks is masked as assembled text; affected
argument fragments are cleared while sanitized complete arguments are retained.
Other chunk boundaries are preserved. Incomplete/error streams are refused at
export rather than silently converted to successful replies.

Before the output file is opened, the privacy pass uses the existing secret-store
redactor/detector, vault values, credential environment values, registered scenario
secrets, email addresses and home paths. It also scans embedded JSON and applies a
second privacy lint. Existing files are never overwritten. Original registered
secrets are removed from the exported scenario's secret-check configuration;
add synthetic secret assertions when needed. Review the sanitized artifact before
committing: arbitrary personal prose is not automatically identifiable as private.
The recording command immediately replays its sanitized output. If masking changes
a gate decision, the command fails and prints the assertion failures; review the
fixture and generated assertions before accepting it as a regression.

The generated `expect` block checks run state, model-call count, requested calls,
execution counts, ordered permissions, approval cards, absence of unexpected
approval cards, and final text. Edit these assertions to express the intended
regression. Do not regenerate them merely to accept a changed security decision.
Replay independently requires every recorded call to be consumed and compares the
request summary before yielding each response. Changed tool results, missing or
extra calls fail with the zero-based invocation and the divergent summary fields.
Unknown versions and streams without a final `done` are rejected at load time.

`recorded-owner-denial.yaml`, `recorded-malformed-recovery.yaml` and
`recorded-tainted-executor.yaml` are explicitly synthetic examples, captured through
the same wrapper and then replayed in tests. To regenerate them deliberately on a
developer machine, set `UPDATE_EVAL_RECORDINGS=1` and run
`cargo test -p evals --test transcripts synthetic_recordings_round_trip_and_generate_examples`.
Normal tests only write temporary files and require no provider key.

The three `live-recorded-*.yaml` examples were captured locally with
`gemini/gemini-3.1-flash-lite` against synthetic fixture data, then replayed without
network access. They cover an allowed read, an owner-denied write and a denied
executor request after untrusted content. They are marked `synthetic: false`;
all tool implementations and data remain fixtures.
