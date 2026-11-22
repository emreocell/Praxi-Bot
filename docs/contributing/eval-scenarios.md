# Adding prompt-injection & safety eval scenarios

Tier-1 evaluation scenarios live as YAML data files under [`evals/scenarios/`](../../evals/scenarios/) and run hermetically in CI (`cargo test -p evals`) with no network access and no real provider keys.

To capture a real model turn and replay its sanitized transcript through the same
loop, see [Record and replay](eval-record-replay.md).

For the full scenario schema, tool content classes, and baseline commands, see:

- [`evals/README.md`](../../evals/README.md) — scenario schema, `attack` metadata, `approval_cards` assertions, and `bot-eval` commands
- [`evals/baselines/README.md`](../../evals/baselines/README.md) — injection-pack and safety-pack baselines and known-gap policy
- [Security model: Prompt-injection evaluation pack](../guide/security.md#prompt-injection-evaluation-pack) — how the egress and executor gates are tested

## Quick checklist for a new attack scenario

1. Add both `evals/scenarios/inj-<name>.yaml` and a differently worded `evals/scenarios/inj-<name>-holdout.yaml` twin (distinct `user` prompt and `tools` fixtures).
2. Set `attack` metadata (`version`, `entry_point`, `shape`, `goal`, `gate`) so the runner includes the case in the corpus summary table.
3. Script the fake model to obey the injected instruction after reading untrusted content, and list the target sink(s) in `unsafe_tools` and any sensitive strings in `secrets`.
4. Assert `tool_runs: { <sink>: 0 }`, the expected `permissions`, and either `approval_cards` (when an approval card must appear) or `no_approval_tools` + `requests` (when the gate refuses before showing a card, such as forbidden invisible Unicode characters or protected daemon paths).
5. Use only synthetic domains (`example.com`, `example.test`, `evil.example`, `lookalike.example`) and synthetic tokens.
6. Run `cargo test -p evals` and `cargo run -p evals --bin bot-eval -- run --tier 1 --filter inj- --repeat 3 --out run.jsonl`.
7. If a new scenario reveals an undisclosed gate bypass, do **not** publish exploit details in a PR or public issue — follow [`SECURITY.md`](../../SECURITY.md).

Set `deferred_tools: true` to exercise the deferred surface in scripted
scenarios. A fake MCP tool's optional `description` is ingested as untrusted
catalog metadata. Use `expect.requests` to check the tools offered before and
after activation and `approval_cards[].text_not_contains` to check the entire
approval summary and details for catalog prose.
