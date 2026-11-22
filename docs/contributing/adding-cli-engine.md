# Adding a coding CLI adapter

Read the [engine security and support guide](../guide/cli-engines.md) first.
Do not enable an engine until its approval mode can be enforced. Unknown
version floors and authentication evidence must stay unknown.

`cli_provider::adapter::EngineAdapter` is the native session contract.
`Driver` implements it for Claude, Codex, Cursor, OpenCode, Gemini and
Antigravity. `run_native_turn` dispatches through that contract; the shared
launcher still owns pinned executables, environment filtering, working-directory
checks, cancellation and protocol frame limits. The confined Claude/Codex
path is separate and its existing fail-closed checks remain in force.

1. Add fixed vendor metadata to `registry.rs`: commands, probe arguments,
   install descriptors, documented version floor and fallback models. Never
   interpolate user or model text into installer commands.
2. Add the driver to `Driver::ALL` and define its transport and approval mapping
   in `levels.rs`. Unsupported levels must fall back to Ask or be refused;
   never silently widen them. Retired engines stay hidden unless installed.
3. Add protocol-specific behaviour to the appropriate shared decoder and a
   synthetic transcript to the Rust fixture in `tests/fixtures/native_cli.rs`.
   Keep fixture controls in the sealed test environment. Never extend the
   production environment allowlist for a fixture.
4. Run `cargo test -p cli-provider` and
   `cargo clippy -p cli-provider --all-targets -- -D warnings`, using your assigned
   external Cargo target directory. `adapter_conformance` iterates every driver:
   a new driver must pass the same stream, cancellation, caller-timeout cleanup,
   credential isolation, workspace confinement and output-boundary assertions.

The suite treats protocol text as untrusted data. It checks that stderr and
reasoning metadata do not become assistant text, oversized frames fail closed,
and exit without a protocol completion produces a stable `cli_protocol_error`.
It does not strip arbitrary assistant text: UI rendering and downstream tool
permissions remain responsible for treating that text as untrusted.

The fixtures are not vendor compatibility evidence. Real CLI sessions,
installers and model calls are not exercised. Existing
engine-specific tests cover version-floor refusal, authentication parsing,
approval forwarding and descendant process cleanup. Setting
`BOT_CLI_CONFORMANCE_REAL=1` opts into the same read-only detection contract
against installed CLIs (version and authentication probes, no login or prompt).
This is disabled by default. Record exact versions and commands separately
when claiming vendor verification.
