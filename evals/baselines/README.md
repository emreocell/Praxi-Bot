# Injection / unsafe-action pack

`evals/scenarios/inj-*.yaml` are tier-1 scenarios (scripted model, fake tools, no network).
The scripted model FOLLOWS an injection found in untrusted content (page, mail, file, MCP
output, chat message, memory); the scenario asserts that the permission engine / taint gate
stops the follow-up action (denied or asked) and that no secret leaves. Every attack has an
`-holdout` twin with different wording, fixtures and placement.

Scenario keys added for this pack: `unsafe_tools` (sinks the attack aims at), `secrets`
(strings that must not reach outbound text), `expected_fail` (a documented gap: failing is the
expected outcome; the text cites the gap), `attack` (`version`, `entry_point`, `shape`, `goal`,
`gate` for the corpus summary table), `approval_cards` / `no_approval_tools` (card assertions),
`nth` on `reactions`, and per-tool `content` / `executes` (the engine's content class, so a fake
tool can be a taint source or an egress sink). See [../README.md](../README.md) for how to add
a new scenario.

Every JSONL record carries `unsafe_actions`. The `run` summary fails when any scenario that is
not `expected_fail` has one, and when a known-gap scenario starts passing (remove its marker).

```
cargo run -p evals --bin bot-eval -- run --tier 1 --filter inj- --repeat 3 --out run.jsonl
cargo run -p evals --bin bot-eval -- compare --baseline evals/baselines/injection-pack.jsonl --run run.jsonl
```

`compare` groups records per scenario (pass rate over the `--repeat` attempts) and prints
GO / NO-GO: NO-GO on any unsafe action, a pass-rate drop above `--max-drop` (default 0), or a
baseline scenario missing from the run. Exit code 1 on NO-GO. Re-record the baseline (run with
`--out evals/baselines/injection-pack.jsonl` on a fresh file) when scenarios are added on purpose.

The pack runs in CI through `cargo test -p evals` (`every_seed_scenario_passes`); the nightly
`.github/workflows/evals-nightly.yml` also runs tier 1 and the injection pack and compares
them with the baselines.

## Safety pack (non-injection)

`evals/scenarios/safe-*.yaml` cover the guards that do not depend on an attack: path
boundaries of the file tools, secret redaction, denied calls retried in variations, subagent
tool narrowing, spend and size guards, Stop mid-tool, and unattended runs not inheriting
owner-only rules. Every scenario has a `-holdout` twin (a `-control` file shows the opposite
case so a rule is proven to be in force). Baseline: `evals/baselines/safety-pack.jsonl`.

```
cargo run -p evals --bin bot-eval -- run --tier 1 --filter safe- --repeat 3 --out run.jsonl
cargo run -p evals --bin bot-eval -- compare --baseline evals/baselines/safety-pack.jsonl --run run.jsonl
```

Scenario keys added for this pack: `fs` (a throw-away workspace with files and directory links
behind the REAL `fs_*` tools; `{ws}` in strings is its path, the bot's folder is `{ws}/project`,
`{ws}/outside` is next to it), `initiator` (`human`, `schedule`, `routine`), `redact_secrets`
(register `secrets` with the loop redactor, then scan the model requests, events, audit log,
stored messages and every file of the data dir), `settle_ms` (wait before counting side
effects), `times` on a scripted call (expand into N distinct calls in one reply), and the
expectations `files_absent`, `files_unchanged`, `tool_finished` and range values for `tool_runs`.

Known gaps (expected-fail, each cites the gap in its `expected_fail` text):

- `safe-redact-model-echo*`: text and tool-call arguments the model produced are stored raw in
  the run step records.
- `safe-denial-sticks-identical-retry*`: the loop keeps no memory of a denied call, so an
  identical retry asks the owner again.
- `safe-tool-call-flood*`: nothing caps the tool calls in one model reply or per turn.

