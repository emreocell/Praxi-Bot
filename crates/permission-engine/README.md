# permission-engine

The permission engine resolves whether an agent action is allowed, denied, or
must wait for a person's approval. The decision comes from code and policy,
not from the model. This crate supplies the policy rules and safety checks;
callers are responsible for applying the decisions and presenting approvals.

## Entry points

- [`PermissionResolver::resolve`](src/resolver.rs) combines global, bot, and
  sub-agent policies for an `ActionRequest` and returns a `Decision`:
  `Allow`, `Ask`, or `Deny`.
- [`Policy`, `Rule`, `Level`, and `Limits`](src/policy.rs) describe
  category-level rules, initiator-specific overrides, and permission presets.
  `AllowWithLimits` currently produces an ask when the resolver cannot
  evaluate those limits.
- [`NeverList`](src/never_list.rs) parses shell commands and their wrappers
  to detect catastrophic commands and recursive deletes targeting protected
  paths. It is a safety check, **not** a sandbox.
- [`ContentExposure`, `egress_must_ask`, and
  `executes_must_ask`](src/egress.rs) track untrusted/private content and
  determine when an outgoing action or program execution requires a fresh
  approval.
- [`AutonomyMode` and `balanced_lane`](src/modes.rs) decide which otherwise
  ordinary policy-level asks may be skipped in Balanced mode; they do not
  override denials or mandatory approval gates.
- [`is_host_status_command`](src/host_info.rs) recognizes a narrow set of
  read-only host-status commands with restricted flags.

## Invariants

- A sub-agent must never be more permissive than its parent. Money actions
  and never-list matches must never be automatically allowed, even under a
  Full preset. See [`resolver.rs`](src/resolver.rs).
- Unsupported `AllowWithLimits` constraints fail closed to an `Ask` decision
  rather than silently allowing the operation. See
  [`policy.rs`](src/policy.rs) and [`resolver.rs`](src/resolver.rs).
- After reading untrusted content, the egress and execution checks can require
  approval despite an ordinary grant. Untrusted-only exposure permits the
  configured search provider; combining untrusted and private exposure makes
  even that search ask, unless the explicit egress waiver is set. The egress
  waiver does **not** waive the program-execution check. See
  [`egress.rs`](src/egress.rs).

## Tests

From the workspace root, run `cargo test -p permission-engine`.
The crate includes focused unit tests for policy resolution, protected command
patterns, autonomy modes, and content-exposure decisions.
