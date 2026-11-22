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

Hermetic fake-provider tests cover budgets, timeouts, retained assertions,
permission/injection metrics, report privacy and regression decisions. Real
provider billing and the scheduled GitHub run require maintainer opt-in and are
not part of the ordinary test suite.
