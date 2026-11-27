# Adding a coding CLI adapter

Read the [engine security and support guide](../guide/cli-engines.md) first.
Do not enable an engine until its approval mode can be enforced. Unknown
version floors and authentication evidence must stay unknown.

## The data path (a plain ACP agent)

A coding CLI that speaks the Agent Client Protocol over stdio and needs no special handling
is **a data file, not Rust**: add `providers/cli/<id>.toml` and list it in `EMBEDDED` in
`crates/cli-provider/src/acp_data.rs`. Start from `cline.toml` (a level the CLI can hold)
or `auggie.toml` (a CLI whose permission behaviour is unknown, so only Full access is
offered). The schema is strict, and `cargo test -p cli-provider` rejects a file that:

- has an unknown key, a command that is not a bare executable name, or an argument that is
  not fixed text (no `{prompt}`, no `$VAR`, no spaces, no shell characters);
- installs anything but `npm install -g <package>@<x.y.z>`: the package must be a plain npm
  registry name (optional `@scope/`, lower case, no path, no second `@`, at most 214
  characters) and `install.version` an exact release, never a range or a tag. The command is
  always **unverified, copy only**: Praxi shows it and the owner runs it. It becomes
  one-click only when a reviewed change records a real install;
- maps a level to a wider one, or marks Full as not enforceable;
- uses an argument below Full that is not on the allow-list of known-safe tokens
  (`SAFE_SINGLES` and `SAFE_PAIRS` in `acp_data.rs`; the ACP start argv counts as below Full).
  Permissive flags (`--yolo`, `--auto-approve true`, `--force`, ...) are only for Full, and
  no argument anywhere carries `/ : @ , =`;
- claims anything but `support = "experimental"`, or states no `unverified` facts;
- reuses an id, command or alias of another engine.

A new file needs: the official docs it was read from (`evidence`), the facts it does not
claim (`unverified`), the credential folders (`protected_dirs`, on one line), and the same
folders added to the file tools' and never-list protections (a test in `builtin-tools` and
`permission-engine` fails otherwise). Add the id to the engine lists of the UI
(`modelBrand.ts`) and the guide (`docs/guide/cli-engines.md` and its Turkish page). The
files `tests/acp_data_engines.rs` runs every entry against the fake ACP agent (argv, level
mapping, refused levels and models, exact permission options), and `adapter_conformance`
runs the same stream, cancellation and confinement checks as for every driver.

Use the Rust path below when the CLI needs engine-specific behaviour (its own sign-in
probe or error mapping, a model list command, a launch flag before the subcommand).

`cli_provider::adapter::EngineAdapter` is the native session contract.
`Driver` implements it for Claude, Codex, Cursor, OpenCode, Gemini and
Antigravity. `run_native_turn` dispatches through that contract; the shared
launcher still owns pinned executables, environment filtering, working-directory
checks, cancellation and protocol frame limits. The confined Claude/Codex
path is separate and its existing fail-closed checks remain in force.

1. Add fixed vendor metadata to `registry.rs`: commands, probe arguments,
   install descriptors, documented version floor and fallback models. Never
   interpolate user or model text into installer commands.
2. Add the driver to the built-in list in `levels.rs` (`Driver::BUILT_IN`, private; every
   other loop uses `Driver::all()`, which also holds the data-driven engines) and define its transport and approval mapping
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
