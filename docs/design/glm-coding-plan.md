# GLM Coding Plan through CLI engines: draft

Status: proposed for review in [issue #682](https://github.com/emreocell/Praxi-Bot/issues/682).
This describes future behavior, not a supported configuration today.

Z.ai documents Claude Code with its Anthropic-compatible endpoint and a Coding
Plan key. Its [OpenCode guide](https://docs.z.ai/devpack/tool/opencode) selects
the Coding Plan in the CLI's login menu. Z.ai's [standard API quick start](https://docs.z.ai/guides/overview/quick-start)
uses a separate standard key and endpoint. A direct provider preset is a separate
product and is outside the issue maintainer's selected scope. Do not send a plan
key to a direct Praxi provider.

## Proposed boundary

- A local owner explicitly selects a per-engine configuration. Store the token
  in the secret store under a fixed engine-specific name; persist only a reference
  and a validated endpoint in owner settings, never in a bot, prompt or history.
  Writes, tests and removal must use owner-only routes, including on paired hosts.
- For Claude, resolve the secret only for a Claude run, after clearing the child
  environment. Set only `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` from the
  selected configuration. Default to Z.ai's documented HTTPS endpoint; reject
  URLs with credentials, fragments or other unexpected components. A configurable
  endpoint requires an explicit security review of SSRF, redirects and token
  exfiltration; a fixed endpoint is safer for the first implementation.
- Never put a token in arguments, temporary settings files, diagnostic messages,
  events or model context. Diagnostics may show variable names as `<redacted>`.
  Preserve isolated settings, strict MCP, protected config paths and process-tree
  termination. Do not inherit a daemon environment key or enable extra CLI tools.
- GLM model IDs must be passed as CLI model values only after checking the actual
  installed CLI and Z.ai's current model documentation. Do not silently substitute
  Claude aliases for GLM models. Detect and report incompatible CLI versions.
- OpenCode requires a separate design: its documented interactive login saves
  credentials in its own store, and Praxi's governed mode is disabled. Do not
  pretend its authentication or environment contract matches Claude's. Review
  native-tools mode and its child processes before enabling a plan credential.

An environment variable is visible to the CLI and potentially its descendants;
the claim that a token is unavailable to tools needs evidence for each execution
mode and platform. If the CLI can expose it through a native tool or subprocess,
that mode must fail closed until a stronger isolation mechanism exists. Redaction
alone is not a substitute for isolation.

## Acceptance evidence before implementation is complete

Use a fake CLI and a fake endpoint with synthetic secrets. Verify owner-only
access, secret-store persistence and deletion, endpoint validation, engine-scoped
injection, absence from other children, errors, logs, events, history and model
context. Extend leak tests and exercise both governed and native execution paths;
cover child-process exposure as well as diagnostics. Verify model selection with
the installed CLI without sending a real secret, and run a separately authorized
live test only if credentials are available. Document unsupported modes and limits.
