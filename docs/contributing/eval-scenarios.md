# Adding prompt-injection & safety eval scenarios

Tier-1 evaluation scenarios live as YAML data files under [`evals/scenarios/`](../../evals/scenarios/) and run hermetically in CI (`cargo test -p evals`) with no external network access and no real provider keys.

To capture a real model turn and replay its sanitized transcript through the same
loop, see [Record and replay](eval-record-replay.md).

For opt-in, budgeted comparisons of real models using fixture tools only, see
[Nightly model comparisons](eval-nightly.md). These are separate from the
hermetic CI suite and from live browser/desktop tasks.

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

## Recent feature fixtures

`safe-recent-*` adds 21 cases with differently worded holdouts (42 files).
They cover deferred discovery/activation, catalog approval text, offline bundle
secret references, Telegram risk/buttons/replay, human computer control and live
password focus, spending admission/stream cuts/peer refusal, secret-looking
notes, redaction of persisted report inputs, and scheduled permission ceilings.

Optional fixture fields exercise production checks without external services:

- `notes: true` registers the real notes tools in the isolated data directory.
- `bot_limits` stores real daily/weekly/monthly limits before the turn.
- `peer_limit_attempt: true` attempts to clear those limits through the actual
  paired-desktop router (including a forged owner header); requires HTTP 403
  and unchanged stored limits. No listening port is used.
- `bundle: {manifest, values}` creates a synthetic archive and runs the real
  inspector/installer, then exposes its serialized configuration through
  `bundle_config`. The bundled program is never executed.
- `diagnostic_report` feeds synthetic check details through the daemon's real
  Markdown report builder and scrubber, checking canaries before the agent
  loop redactor runs. `diagnostic_read` exposes the resulting report.
- `desktop: {human_driving, password_focus}` registers real desktop tools with
  an in-memory OS backend and the real guarded sender. Assert
  `tool_runs: {desktop_input: 0}` to check that no input reached the fake sink.
- `approval_cards[].risk` asserts the daemon risk classification.
- `telegram_buttons: true|false` sends that real approval event through the
  production Telegram bridge against a loopback-only mock Bot API. It checks
  button availability, presses a button twice with distinct update IDs, and
  requires exactly one daemon decision for low risk or zero for high risk.

Import fixtures are also consumed by
`cargo test -p cli recent_eval_import_fixtures_arrive_disabled`: the CLI's real
planner reads their Claude/OpenClaw files and must produce disabled servers
with vault references and redacted previews. Tier 1 separately checks that a
guessed call from source JSON cannot grant a callable MCP tool. Both runners
are needed for import coverage. Import application through a daemon is not
covered by these fixtures.

Redaction cases scan model requests, events, audit rows, stored messages, and
data files. They protect inputs to diagnostics/report exports; they do not run
the desktop UI's HTML/Markdown export renderer.

Definition pinning is not yet on `main`: the proposed
[pinning ADR](../design/mcp-tool-pinning-adr.md) points to open PR #633. A
changed-definition/re-approval scenario needs that implementation before it can
be added honestly. These fixtures do not claim pinning coverage and introduce
no expected-failure exemptions.
